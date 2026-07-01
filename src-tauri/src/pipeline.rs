use anyhow::Result;
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::Manager;
use tauri::{WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Notify;

use crate::agent::{self, AgentRequest};
use crate::app_detector;
use crate::audio::{AudioCaptureHandle, AudioConfig};
use crate::llm::{self, LlmConfig, PolishRequest};
use crate::output::{self, OutputMode};
use crate::storage;
use crate::stt::{self, SttConfig, TranscriptEvent};
use crate::SessionTokenStore;

// ─── Timing constants ───

/// On macOS, verify whether the process has been granted Accessibility (Assistive Access)
/// permission. enigo uses CGEventPost under the hood, which requires this permission;
/// without it all synthesised key events are silently dropped by the OS.
/// Returns true on all non-macOS platforms (no permission needed).
pub fn is_accessibility_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "ApplicationServices", kind = "framework")]
        extern "C" {
            fn AXIsProcessTrusted() -> u8;
        }
        unsafe { AXIsProcessTrusted() != 0 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Request Accessibility permission. On macOS, if not already trusted, opens the
/// Privacy & Security → Accessibility settings pane so the user can grant it. The
/// previous implementation called `AXIsProcessTrustedWithOptions` via raw FFI to
/// pop the system prompt, but constructing the options CFDictionary by hand was
/// fragile (CFDictionaryCreate returning NULL → SIGSEGV inside
/// AXIsProcessTrustedWithOptions), especially under Rosetta. Returns the current
/// trusted status. On non-macOS platforms always returns true.
pub fn request_accessibility_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        let trusted = is_accessibility_trusted();
        if !trusted {
            // Open the macOS Accessibility settings pane directly. Safe shell-out,
            // no FFI, works the same on native arm64 and Rosetta x86_64.
            let _ = std::process::Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status();
        }
        trusted
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Delay before capturing selected text to ensure hotkey modifiers are released.
const SELECTED_TEXT_CAPTURE_DELAY_MS: u64 = 60;
/// Delay after simulating Ctrl+C to let the clipboard update.
const CLIPBOARD_COPY_SETTLE_MS: u64 = 100;
/// Delay after dispatching the Edit Selection replace paste (Cmd+V) before the
/// read-only probe re-reads the selection. `output.type_text` only DISPATCHES the
/// keystroke (osascript returns once it's posted, not applied), so a slow target
/// app (Electron, browser contenteditable) may not have replaced the selection
/// yet; without this settle the probe re-reads the still-present original and
/// misreports a successful in-place replace as a read-only rejection.
const EDIT_SELECTION_PASTE_SETTLE_MS: u64 = 220;
/// Interval for polling audio volume during recording.
const VOLUME_POLL_INTERVAL_MS: u64 = 50;
/// Timeout for STT finalization after recording stops.
const STT_FINALIZE_TIMEOUT_SECS: u64 = 120;

static LAST_AGENT_RESULT: OnceLock<Arc<Mutex<Option<String>>>> = OnceLock::new();

fn last_agent_result_slot() -> &'static Arc<Mutex<Option<String>>> {
    LAST_AGENT_RESULT.get_or_init(|| Arc::new(Mutex::new(None)))
}

pub fn latest_agent_result() -> Option<String> {
    last_agent_result_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn show_agent_result_window(app_handle: &tauri::AppHandle, response: String) -> Result<()> {
    *last_agent_result_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(response.clone());

    let window = if let Some(window) = app_handle.get_webview_window("agent-result") {
        window
    } else {
        WebviewWindowBuilder::new(
            app_handle,
            "agent-result",
            WebviewUrl::App("index.html#agent-result".into()),
        )
        .title("Agent Response")
        .inner_size(720.0, 560.0)
        .min_inner_size(420.0, 320.0)
        .resizable(true)
        .decorations(true)
        .visible(true)
        .build()?
    };

    let _ = window.show();
    let _ = window.set_focus();

    // The React listener may still be mounting in a newly-created window.
    // Re-emitting is idempotent on the frontend and avoids a startup race.
    let first = response.clone();
    let second = response.clone();
    let third = response;
    let w1 = window.clone();
    let w2 = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let _ = w1.emit("agent:result", first);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let _ = w2.emit("agent:result", second);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let _ = window.emit("agent:result", third);
    });

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineState {
    Idle,
    Recording,
    Transcribing,
    Polishing,
    Outputting,
}

impl PipelineState {
    fn as_u8(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Recording => 1,
            Self::Transcribing => 2,
            Self::Polishing => 3,
            Self::Outputting => 4,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Recording,
            2 => Self::Transcribing,
            3 => Self::Polishing,
            4 => Self::Outputting,
            _ => Self::Idle,
        }
    }
}

/// Outcome of a `start()` attempt, so callers can tell whether THIS specific
/// call actually began a recording — which a global `state == Recording` check
/// cannot (another concurrent start may already hold Recording). Edit Selection
/// mode is armed only on `Started`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartOutcome {
    /// This call won the Idle→Recording CAS and started a recording.
    Started,
    /// The CAS was a no-op because the pipeline was already active — this call
    /// started nothing.
    NoopAlreadyActive,
    /// This call won the CAS but bailed before recording became active (missing
    /// STT key, STT connect failure, audio failure, or abort during setup);
    /// state has been reset to Idle.
    FailedBeforeRecording,
}

pub struct PipelineHandle {
    app_handle: tauri::AppHandle,
    state: Arc<AtomicU8>,
    audio_handle: Arc<Mutex<Option<AudioCaptureHandle>>>,
    audio_volume: Arc<Mutex<f32>>,
    accumulated_text: Arc<Mutex<String>>,
    stt_done: Arc<Notify>,
    /// Fired by stop()/abort() to tell the STT consume task to finalize. We
    /// cannot depend on the audio channel closing: on macOS the cpal stream does
    /// not reliably release its sender on drop, so the channel can stay open
    /// forever and the consume loop would hang waiting for EOF (the "stuck on
    /// Transcribing" bug).
    finalize_stt: Arc<Notify>,
    abort_flag: Arc<AtomicBool>,
    /// Set by `start_for_agent()` so the upcoming stop() routes the transcript
    /// through Hermes regardless of trigger-word prefix. Reset at the end of
    /// stop() (so a subsequent normal start/stop is unaffected).
    force_agent_mode: Arc<AtomicBool>,
    /// Set by `start_for_edit_selection()` so the upcoming stop() treats the
    /// transcript as an INSTRUCTION on the current selection and replaces the
    /// selection in-place (Edit Selection mode). Reset (swapped) inside stop()
    /// and in abort() so normal dictation is never affected.
    force_edit_selection_mode: Arc<AtomicBool>,
    preloaded_config: Arc<Mutex<Option<storage::AppConfig>>>,
    preloaded_app_ctx: Arc<Mutex<Option<app_detector::AppContext>>>,
    preloaded_dictionary: Arc<Mutex<Option<Vec<String>>>>,
    preloaded_selected_text: Arc<Mutex<Option<String>>>,
    recording_start: Arc<Mutex<Option<std::time::Instant>>>,
    shared_client: reqwest::Client,
    /// Serializes start()/stop() so that stop() waits for start() to finish
    /// its setup before reading shared state (preloaded_config, audio_handle, etc.).
    /// Without this, a quick press-release in hold mode causes stop() to run
    /// while start() is still connecting to STT, finding empty fields.
    pipeline_lock: tokio::sync::Mutex<()>,
}

impl PipelineHandle {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self {
            app_handle,
            state: Arc::new(AtomicU8::new(PipelineState::Idle.as_u8())),
            audio_handle: Arc::new(Mutex::new(None)),
            audio_volume: Arc::new(Mutex::new(0.0)),
            accumulated_text: Arc::new(Mutex::new(String::new())),
            stt_done: Arc::new(Notify::new()),
            finalize_stt: Arc::new(Notify::new()),
            abort_flag: Arc::new(AtomicBool::new(false)),
            force_agent_mode: Arc::new(AtomicBool::new(false)),
            force_edit_selection_mode: Arc::new(AtomicBool::new(false)),
            preloaded_config: Arc::new(Mutex::new(None)),
            preloaded_app_ctx: Arc::new(Mutex::new(None)),
            preloaded_dictionary: Arc::new(Mutex::new(None)),
            preloaded_selected_text: Arc::new(Mutex::new(None)),
            recording_start: Arc::new(Mutex::new(None)),
            shared_client: reqwest::Client::new(),
            pipeline_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn set_state(&self, new_state: PipelineState) {
        self.state.store(new_state.as_u8(), Ordering::SeqCst);
        let _ = self.app_handle.emit("pipeline:state", new_state);

        // Update tray tooltip + menu to reflect pipeline state. Dispatched onto
        // the main thread (never locking `tray` from this worker thread) — see
        // crate::update_tray for the deadlock this avoids.
        let tooltip = match new_state {
            PipelineState::Recording => "OpenTypeless - Recording...",
            PipelineState::Transcribing => "OpenTypeless - Transcribing...",
            PipelineState::Polishing => "OpenTypeless - Polishing...",
            PipelineState::Outputting => "OpenTypeless - Outputting...",
            PipelineState::Idle => "OpenTypeless",
        };
        crate::update_tray(&self.app_handle, Some(tooltip.to_string()));

        // Drive the recording capsule's visibility from Rust. The capsule is a
        // separate webview window that was supposed to show/hide itself in JS
        // (useCapsuleResize) on the pipeline:state event — but on macOS the
        // hidden WKWebView didn't reliably react, so the capsule never appeared
        // while recording (and its in-webview duration timer never ran, so the
        // max-length auto-stop didn't fire either). Showing/hiding the window
        // from the main thread here is deterministic and independent of the
        // webview's JS. Window ops are main-thread-only (run_on_main_thread).
        let app_for_capsule = self.app_handle.clone();
        let _ = self.app_handle.run_on_main_thread(move || {
            if let Some(capsule) = app_for_capsule.get_webview_window("capsule") {
                let was_visible = capsule.is_visible().unwrap_or(false);
                if matches!(new_state, PipelineState::Idle) {
                    let _ = capsule.hide();
                } else {
                    let _ = capsule.show();
                    let _ = capsule.set_always_on_top(true);
                }
                tracing::info!(
                    "capsule visibility: state={:?} was_visible={} now_visible={:?}",
                    new_state,
                    was_visible,
                    capsule.is_visible()
                );
            } else {
                tracing::warn!("capsule window NOT FOUND (state={:?})", new_state);
            }
        });
    }

    pub fn current_state(&self) -> PipelineState {
        PipelineState::from_u8(self.state.load(Ordering::SeqCst))
    }

    /// Immediately abort the pipeline regardless of current state.
    /// Stops audio capture, forces state to Idle, and signals any
    /// ongoing stop() to exit early via abort_flag.
    pub fn abort(&self) {
        tracing::info!(
            "Pipeline abort requested (current state: {:?})",
            self.current_state()
        );

        // Set abort flag so any running stop() exits early
        self.abort_flag.store(true, Ordering::SeqCst);

        // Stop audio capture, then signal the STT task to finalize. abort_flag is
        // already set above, so the task discards the take instead of transcribing.
        {
            let mut handle = self.audio_handle.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut h) = *handle {
                h.stop();
            }
            *handle = None;
        }
        self.finalize_stt.notify_one();

        // Recording aborted — resume any media we paused on start.
        crate::media::resume_local_media();

        // Unblock stop() if it's waiting on stt_done.notified()
        self.stt_done.notify_one();

        // Clear accumulated text
        self.accumulated_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        // Reset force_agent so a subsequent normal start() isn't poisoned by
        // a previously-armed agent recording that got aborted before stop().
        self.force_agent_mode.store(false, Ordering::SeqCst);
        // Same for edit-selection mode.
        self.force_edit_selection_mode
            .store(false, Ordering::SeqCst);

        // Force state to Idle — emits pipeline:state event to sync frontend
        self.set_state(PipelineState::Idle);
    }

    /// Capture selected text from the foreground app by simulating Ctrl+C / Cmd+C.
    /// Must be called when no hotkey modifier keys are physically held down.
    /// Called from async context via block_in_place, so std::thread::sleep is acceptable.
    fn capture_selected_text(&self) -> Option<String> {
        let mut clipboard = arboard::Clipboard::new().ok()?;
        // Back up whatever the user currently has so the synthetic Cmd+C below
        // doesn't destroy it. get_text() only sees text; if the clipboard holds
        // an image (or text-less content), back up the image too so we can put it
        // back. Without this, a non-text clipboard was silently wiped and the
        // captured selection left behind as residue.
        let backup_text = clipboard.get_text().ok();
        let backup_image = if backup_text.is_none() {
            clipboard.get_image().ok()
        } else {
            None
        };

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("osascript")
                .args([
                    "-e",
                    r#"tell application "System Events" to keystroke "c" using command down"#,
                ])
                .output();
        }

        #[cfg(not(target_os = "macos"))]
        if let Ok(mut enigo) = Enigo::new(&EnigoSettings::default()) {
            let modifier = Key::Control;

            let pressed = enigo.key(modifier, Direction::Press).is_ok();
            if pressed {
                let _ = enigo.key(Key::Unicode('c'), Direction::Click);
                let _ = enigo.key(modifier, Direction::Release);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(CLIPBOARD_COPY_SETTLE_MS));

        let selected = clipboard.get_text().ok();

        // Restore the user's clipboard exactly as we found it: text first, then
        // image; if we could back up neither (empty clipboard, or content arboard
        // can't read such as copied files), clear it so we at least don't leave
        // the captured selection behind as residue.
        if let Some(ref b) = backup_text {
            let _ = clipboard.set_text(b);
        } else if let Some(img) = backup_image {
            let _ = clipboard.set_image(img);
        } else {
            let _ = clipboard.clear();
        }

        tracing::info!(
            "Selected text capture: backup_len={}, selected_len={}",
            backup_text.as_deref().map(|s| s.len()).unwrap_or(0),
            selected.as_deref().map(|s| s.len()).unwrap_or(0)
        );

        // On macOS, if Cmd+C had no effect (e.g., no Accessibility permission),
        // the clipboard is unchanged, so selected == backup — return None to avoid
        // passing stale clipboard content to the LLM as if it were selected text.
        match &selected {
            Some(s) if !s.trim().is_empty() => {
                if backup_text.as_deref() == Some(s.as_str()) {
                    tracing::debug!(
                        "Selected text equals clipboard backup — Cmd+C had no effect, ignoring"
                    );
                    None
                } else {
                    Some(s.clone())
                }
            }
            _ => None,
        }
    }

    async fn load_config(&self) -> storage::AppConfig {
        self.app_handle
            .state::<storage::ConfigManager>()
            .load()
            .await
            .unwrap_or_default()
    }

    /// Start a recording that, on stop(), will be routed through the Hermes
    /// agent regardless of whether the transcript begins with a trigger-word
    /// prefix. The whole transcript becomes the agent prompt. Use this when
    /// the user explicitly invokes the agent (via dedicated hotkey).
    pub async fn start_for_agent(&self) -> Result<()> {
        self.force_agent_mode.store(true, Ordering::SeqCst);
        self.start().await
    }

    /// Start a recording that, on stop(), applies the transcript as an
    /// instruction to the currently-selected text and replaces the selection
    /// in-place (Edit Selection mode). Selected text is captured regardless of
    /// the `selected_text_enabled` setting because the user explicitly invoked
    /// this mode via its dedicated hotkey.
    pub async fn start_for_edit_selection(&self) -> Result<()> {
        // Arm Edit Selection INSIDE start_and_report, under pipeline_lock, and
        // only after the Idle→Recording CAS has won and setup fully succeeded.
        // Arming in this caller (after the lock was released) raced two ways:
        //   * a concurrent abort() (which takes NO lock) could reset the flag and
        //     then be clobbered by a late store(true) here — leaking Edit
        //     Selection into the NEXT recording; and
        //   * a concurrent stop() could acquire the freed lock and swap the flag
        //     to false before this store ran — silently dropping the edit and
        //     routing the instruction to normal dictation.
        // Passing the intent down makes the arm atomic with the CAS: stop() can't
        // take the lock until we return, and only the `Started` path arms.
        self.start_and_report(true).await.map(|_| ())
    }

    pub async fn start(&self) -> Result<()> {
        self.start_and_report(false).await.map(|_| ())
    }

    /// Same as `start()` but reports whether THIS call actually began a
    /// recording (see `StartOutcome`). `arm_edit_selection` requests Edit
    /// Selection mode: the flag is set (and `edit_selection:active` emitted)
    /// atomically under `pipeline_lock` on the `Started` path only — never on a
    /// CAS no-op or a pre-recording failure, so it can't leak into another
    /// recording this call did not start.
    async fn start_and_report(&self, arm_edit_selection: bool) -> Result<StartOutcome> {
        // Hold pipeline_lock for the entire setup so stop() cannot read
        // partially-initialised state (preloaded_config, audio_handle, etc.).
        let _guard = self.pipeline_lock.lock().await;

        // Reset abort flag for new recording
        self.abort_flag.store(false, Ordering::SeqCst);

        // Atomic CAS: only one caller can transition Idle → Recording. A failed
        // CAS means another start already holds the pipeline — this call started
        // nothing, so it must NOT arm any per-recording mode.
        if self
            .state
            .compare_exchange(
                PipelineState::Idle.as_u8(),
                PipelineState::Recording.as_u8(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return Ok(StartOutcome::NoopAlreadyActive);
        }
        // Route through set_state so the recording capsule is actually shown
        // (and the tray updated) — emitting pipeline:state directly here used to
        // bypass set_state's capsule show/hide, which is why the capsule stayed
        // invisible the whole time we were recording.
        self.set_state(PipelineState::Recording);
        crate::sfx::play_cue("Pop"); // activation cue when recording starts

        // Clear accumulated text
        self.accumulated_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        // P0-2: Load config BEFORE starting audio capture — fail fast on missing API key
        let config_data = self.load_config().await;
        *self
            .preloaded_config
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(config_data.clone());
        *self
            .preloaded_app_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(app_detector::detect_current_app());
        let dict_words = self
            .app_handle
            .state::<storage::DictionaryStore>()
            .words()
            .await;
        *self
            .preloaded_dictionary
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(dict_words);

        tracing::debug!(
            "Pipeline using config: stt_provider={}, stt_key_len={}, stt_lang={}",
            config_data.stt_provider,
            config_data.stt_api_key.len(),
            config_data.stt_language
        );

        // Pause locally-playing music / video so the mic doesn't pick it up.
        // Best-effort, non-blocking (spawns its own thread internally), only
        // targets apps with AppleScript player-state support.
        if config_data.auto_pause_media {
            crate::media::pause_local_media();
        }

        // Guard: empty API key — bail before starting audio (skip for cloud provider)
        if config_data.stt_api_key.is_empty() && config_data.stt_provider != "cloud" {
            let _ = self.app_handle.emit(
                "pipeline:error",
                "STT API key is not configured. Please set it in Settings → Speech Recognition.",
            );
            *self
                .preloaded_config
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self
                .preloaded_app_ctx
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self
                .preloaded_dictionary
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.set_state(PipelineState::Idle);
            return Ok(StartOutcome::FailedBeforeRecording);
        }

        // P0-3: Pre-connect STT provider before spawning task
        let stt_api_key = if config_data.stt_provider == "cloud" {
            self.app_handle
                .state::<SessionTokenStore>()
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        } else {
            config_data.stt_api_key.clone()
        };

        let stt_hotwords = self
            .preloaded_dictionary
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();

        let stt_config = SttConfig {
            api_key: stt_api_key,
            language: if config_data.stt_language == "multi" {
                None
            } else {
                Some(config_data.stt_language.clone())
            },
            smart_format: true,
            sample_rate: 16000,
            hotwords: stt_hotwords,
        };

        let mut provider =
            stt::create_provider(&config_data.stt_provider, Some(self.shared_client.clone()));
        if let Err(e) = provider.connect(&stt_config).await {
            tracing::error!("STT connect failed: {}", e);
            let _ = self
                .app_handle
                .emit("pipeline:error", format!("STT connection failed: {e}"));
            *self
                .preloaded_config
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self
                .preloaded_app_ctx
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self
                .preloaded_dictionary
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.set_state(PipelineState::Idle);
            return Ok(StartOutcome::FailedBeforeRecording);
        }

        // Start audio capture on dedicated thread
        let config = AudioConfig {
            preferred_input_device: config_data.preferred_input_device.clone(),
            ..AudioConfig::default()
        };
        let (handle, mut audio_rx) = match AudioCaptureHandle::start(config) {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Audio capture failed: {}", e);
                let _ = self
                    .app_handle
                    .emit("pipeline:error", format!("Audio capture failed: {e}"));
                *self
                    .preloaded_config
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                *self
                    .preloaded_app_ctx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                *self
                    .preloaded_dictionary
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                self.set_state(PipelineState::Idle);
                return Ok(StartOutcome::FailedBeforeRecording);
            }
        };

        // Store the audio handle's volume reference.
        // Check abort_flag first — if abort() was called while we were connecting
        // to STT, don't store the handle (it would be orphaned with nobody to stop it).
        if self.abort_flag.load(Ordering::SeqCst) {
            tracing::info!("Pipeline aborted during setup, discarding audio capture");
            // handle drops here, stopping the capture thread
            self.set_state(PipelineState::Idle);
            return Ok(StartOutcome::FailedBeforeRecording);
        }
        let audio_vol = handle.get_volume();
        *self.audio_volume.lock().unwrap_or_else(|e| e.into_inner()) = audio_vol;
        *self.audio_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);

        *self
            .recording_start
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());

        // Volume monitoring task
        let app_handle = self.app_handle.clone();
        let audio_handle_ref = self.audio_handle.clone();
        let state_ref = self.state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(VOLUME_POLL_INTERVAL_MS)).await;
                let current = PipelineState::from_u8(state_ref.load(Ordering::SeqCst));
                if current != PipelineState::Recording {
                    break;
                }
                let vol = audio_handle_ref
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|h| h.get_volume())
                    .unwrap_or(0.0);
                let _ = app_handle.emit("audio:volume", vol);
            }
        });

        // Selected text will be captured in stop() after hotkey is released,
        // so Ctrl+C simulation won't conflict with held keys.
        *self
            .preloaded_selected_text
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;

        // STT streaming task — provider is already connected
        let app_handle = self.app_handle.clone();
        let accumulated = self.accumulated_text.clone();
        let stt_done = self.stt_done.clone();
        let finalize_stt = self.finalize_stt.clone();
        let abort_flag = self.abort_flag.clone();

        tokio::spawn(async move {
            // Forward audio to STT until EITHER the audio channel closes (clean
            // EOF) OR stop()/abort() fires finalize_stt. Relying on the channel
            // closing alone is NOT safe: on macOS the cpal stream may not release
            // its sender on drop, so the channel can stay open forever — without
            // finalize_stt the loop would hang and the pipeline would be stuck on
            // "Transcribing" forever.
            loop {
                tokio::select! {
                    chunk = audio_rx.recv() => {
                        match chunk {
                            Some(data) => {
                                let _ = provider.send_audio(&data).await;
                            }
                            // Audio channel closed (clean EOF).
                            None => break,
                        }
                    }
                    _ = finalize_stt.notified() => {
                        // Explicit stop/abort signal: drain whatever real audio is
                        // still buffered, then stop reading and finalize.
                        while let Ok(data) = audio_rx.try_recv() {
                            let _ = provider.send_audio(&data).await;
                        }
                        break;
                    }
                    transcript = provider.recv_transcript() => {
                        match transcript {
                            Ok(Some(TranscriptEvent::Partial { text })) => {
                                let _ = app_handle.emit("stt:partial", &text);
                            }
                            Ok(Some(TranscriptEvent::Final { text, .. })) => {
                                let mut acc = accumulated.lock().unwrap_or_else(|e| e.into_inner());
                                acc.push_str(&text);
                                acc.push(' ');
                                let current = acc.clone();
                                drop(acc);
                                let _ = app_handle.emit("stt:final", &current);
                            }
                            Ok(Some(TranscriptEvent::Error { message })) => {
                                tracing::error!("STT error: {}", message);
                                let _ = app_handle.emit("pipeline:error", format!("STT error: {message}"));
                                break;
                            }
                            Err(e) => {
                                tracing::error!("STT recv error: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Finalize exactly once, however the loop exited — skipped on abort
            // (abort discards the take). For batch providers (qwen-asr) this is
            // where the transcription HTTP request actually happens.
            if !abort_flag.load(Ordering::SeqCst) {
                match provider.disconnect().await {
                    Ok(Some(text)) => {
                        let mut acc = accumulated.lock().unwrap_or_else(|e| e.into_inner());
                        acc.push_str(&text);
                        let current = acc.clone();
                        drop(acc);
                        let _ = app_handle.emit("stt:final", &current);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::error!("STT disconnect error: {}", e);
                        let _ = app_handle.emit("pipeline:error", format!("STT error: {e}"));
                    }
                }
            }

            // Signal that STT processing is complete
            stt_done.notify_one();
        });

        // Arm Edit Selection mode now that the recording is fully set up, while
        // still holding pipeline_lock. Doing it here (not in the caller after the
        // lock drops) makes arming atomic with the CAS: a concurrent stop() can't
        // acquire the lock to swap the flag until we return, and a concurrent
        // abort() ordered after this correctly wins (there is no late store to
        // clobber it). Only reached on the `Started` path — every early return
        // above (CAS no-op / pre-recording failure) skips it.
        if arm_edit_selection {
            self.force_edit_selection_mode.store(true, Ordering::SeqCst);
            // Tell the UI we're in Edit Selection mode from the moment recording
            // starts, so the capsule shows "editing selection" instead of looking
            // identical to normal dictation. Size is emitted later, once the
            // selection is captured at stop().
            let _ = self.app_handle.emit(
                "edit_selection:active",
                serde_json::json!({ "active": true }),
            );
        }

        Ok(StartOutcome::Started)
    }

    pub async fn stop(&self) -> Result<()> {
        // Acquire pipeline_lock so we wait for start() to finish its setup
        // (load_config, connect STT, start audio) before reading shared state.
        // Released before the long stt_done wait so start() isn't blocked 120s.
        let guard = self.pipeline_lock.lock().await;

        // Atomic CAS: only one caller can transition Recording → Transcribing
        if self
            .state
            .compare_exchange(
                PipelineState::Recording.as_u8(),
                PipelineState::Transcribing.as_u8(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return Ok(());
        }
        let _ = self
            .app_handle
            .emit("pipeline:state", PipelineState::Transcribing);
        // Update tray for transcribing state (dispatched to the main thread).
        crate::update_tray(
            &self.app_handle,
            Some("OpenTypeless - Transcribing...".to_string()),
        );
        crate::sfx::play_cue("Bottle"); // exit cue when recording stops

        let stop_start = std::time::Instant::now();

        // Capture selected text now — hotkey is released so Ctrl+C won't conflict.
        // Small delay to ensure hotkey modifiers are fully released (especially in toggle mode).
        let config_data = self
            .preloaded_config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        // Edit Selection mode is armed by its dedicated hotkey. Read (and reset)
        // it now so we can force selection capture below even when the passive
        // `selected_text_enabled` setting is off. Swapping here also guarantees
        // the flag never leaks into the next recording, even on early return.
        let force_edit_selection = self.force_edit_selection_mode.swap(false, Ordering::SeqCst);
        let selected_text = if config_data.selected_text_enabled || force_edit_selection {
            tokio::time::sleep(std::time::Duration::from_millis(
                SELECTED_TEXT_CAPTURE_DELAY_MS,
            ))
            .await;
            tokio::task::block_in_place(|| self.capture_selected_text())
        } else {
            None
        };
        tracing::info!(
            "Selected text result: len={}",
            selected_text.as_deref().map(|s| s.len()).unwrap_or(0)
        );
        *self
            .preloaded_selected_text
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selected_text;

        // Stop audio capture, then explicitly signal the STT task to finalize.
        // We can't rely on the audio channel closing: on macOS the cpal stream
        // may not release its sender on drop, so the channel can stay open
        // forever and the consume loop would hang on EOF.
        {
            let mut handle = self.audio_handle.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut h) = *handle {
                h.stop();
            }
            *handle = None;
        }
        self.finalize_stt.notify_one();

        // Recording is over — resume any media we paused on start.
        crate::media::resume_local_media();

        // P2-1: Pre-build LLM resources while waiting for STT
        let preloaded_config = self
            .preloaded_config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let config = match preloaded_config {
            Some(c) => c,
            None => self.load_config().await,
        };
        let app_ctx = self
            .preloaded_app_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(app_detector::detect_current_app);
        let dictionary_words = self
            .preloaded_dictionary
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_default();
        let selected_text = self
            .preloaded_selected_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();

        // All shared state has been taken — release the lock so a new start()
        // isn't blocked by the long stt_done wait that follows.
        drop(guard);

        // Always use batch output: keyboard mode uses output_text() after full LLM
        // response arrives. Streaming chunk-by-chunk clipboard paste was unreliable
        // on Windows — each Ctrl+V is async and the next set_text() could overwrite
        // the clipboard before the target app processed the previous paste, producing
        // garbled output that differed from what History recorded.

        // Pre-build LLM provider and Enigo while STT is still processing
        let pre_llm = if config.polish_enabled
            && (!config.llm_api_key.is_empty() || config.llm_provider == "cloud")
        {
            let llm_api_key = if config.llm_provider == "cloud" {
                self.app_handle
                    .state::<SessionTokenStore>()
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            } else {
                config.llm_api_key.clone()
            };

            let llm_config = LlmConfig {
                api_key: llm_api_key,
                model: config.llm_model.clone(),
                base_url: config.llm_base_url.clone(),
                max_tokens: 4096,
                temperature: 0.3,
            };
            let provider =
                llm::create_provider(&config.llm_provider, Some(self.shared_client.clone()));
            Some((llm_config, provider))
        } else {
            None
        };

        // Wait for STT task to finish (handles both streaming and file-based providers)
        // Timeout after 120s to support long recordings
        let stt_done = self.stt_done.clone();
        tokio::select! {
            _ = stt_done.notified() => {
                tracing::debug!("STT task completed");
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(STT_FINALIZE_TIMEOUT_SECS)) => {
                tracing::warn!("STT task timed out after {}s, using accumulated text so far", STT_FINALIZE_TIMEOUT_SECS);
            }
        }

        let stt_elapsed = stop_start.elapsed();
        tracing::info!(
            "[Pipeline Timing] STT finalize: {}ms",
            stt_elapsed.as_millis()
        );

        // Check if pipeline was aborted while waiting for STT
        if self.abort_flag.load(Ordering::SeqCst) {
            tracing::info!("Pipeline aborted after STT wait, skipping LLM and output");
            return Ok(());
        }

        let raw_text = self
            .accumulated_text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .trim()
            .to_string();

        if raw_text.is_empty() {
            // If the recording was extremely short (e.g. user double-tapped the hotkey
            // or accidentally clicked the capsule twice), treat it as a misfire and
            // silently return to Idle instead of yelling "No speech detected" at them.
            let recording_duration = self
                .recording_start
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .map(|t| t.elapsed());
            let was_a_misfire = recording_duration
                .map(|d| d < std::time::Duration::from_millis(500))
                .unwrap_or(false);
            if !was_a_misfire {
                if force_edit_selection {
                    // Edit Selection must stay in its own, localized failure lane —
                    // never fall back to the normal dictation English error.
                    self.emit_edit_result("fail", "no_speech", "");
                } else {
                    let _ = self
                        .app_handle
                        .emit("pipeline:error", "No speech detected. Please try again.");
                }
            } else {
                tracing::info!(
                    "Recording too short ({:?}) — treating as misfire, no error",
                    recording_duration
                );
            }
            // Clear recording_start so the next start() gets a fresh instant
            *self
                .recording_start
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.set_state(PipelineState::Idle);
            return Ok(());
        }

        let mut final_text;
        let llm_elapsed;
        let mut agent_response: Option<String> = None;

        // Route explicit agent commands before the normal dictation polish path.
        // Two ways to land on Hermes:
        //   1. User pressed the dedicated agent hotkey → `force_agent_mode` is
        //      set, the WHOLE transcript becomes the agent prompt (no prefix
        //      stripping). This bypasses STT mishearing of "Hermes"/"agent".
        //   2. Transcript starts with a trigger word like "hermes" / "agent" /
        //      "ask hermes" / "ask agent" — strip the prefix and use the rest.
        //
        // `force_agent_mode` is `swap`-ped so the flag is auto-reset for the
        // next recording, even if we abort early below.
        let force_agent = self.force_agent_mode.swap(false, Ordering::SeqCst);
        let hermes_prompt = if force_agent {
            if raw_text.is_empty() {
                None
            } else {
                Some(raw_text.clone())
            }
        } else if config.agent_enabled {
            agent::parse_agent_prompt(&raw_text)
        } else {
            None
        };

        // Polish with LLM (resources already pre-built), or run Hermes when requested.
        // Check abort before entering LLM/agent and output
        if self.abort_flag.load(Ordering::SeqCst) {
            tracing::info!("Pipeline aborted before LLM/agent/output");
            return Ok(());
        }

        // Edit Selection mode: the transcript is an instruction on the current
        // selection. This is a self-contained path — it validates the target,
        // replaces the selection in-place on success, or surfaces a visible
        // error / clipboard fallback otherwise, then returns to Idle. It does
        // not fall through to the normal dictation/agent output below.
        if force_edit_selection {
            return self
                .run_edit_selection(&raw_text, selected_text, &app_ctx, &config, pre_llm)
                .await;
        }

        if let Some(prompt) = hermes_prompt {
            self.set_state(PipelineState::Polishing);
            let agent_start = std::time::Instant::now();
            let _ = self.app_handle.emit("llm:chunk", "Running agent...\n");
            let _ = self
                .app_handle
                .emit("agent:status", agent::runtime_label(&config));

            let request = AgentRequest {
                prompt,
                app_context: app_ctx.clone(),
                selected_text: selected_text.clone(),
                config: config.clone(),
            };

            match agent::run_agent(request).await {
                Ok(response) => {
                    if self.abort_flag.load(Ordering::SeqCst) {
                        tracing::info!("Pipeline aborted after agent response, skipping output");
                        return Ok(());
                    }
                    llm_elapsed = agent_start.elapsed();
                    // Agent results are shown in a dedicated window — do not type/paste.
                    if let Err(e) = show_agent_result_window(&self.app_handle, response.clone()) {
                        tracing::error!("Failed to show agent result window: {}", e);
                        let _ = self
                            .app_handle
                            .emit("pipeline:error", format!("Agent result window failed: {e}"));
                    }
                    // Native notification so the user sees the result even if
                    // they're in another app and miss the agent panel surfacing.
                    if config.agent_notification {
                        crate::notify::show_agent_notification(&response);
                    }
                    final_text = raw_text.clone();
                    // Store full agent response in history entry (set below)
                    agent_response = Some(response);
                }
                Err(e) => {
                    tracing::error!("Agent run failed: {}", e);
                    final_text = raw_text.clone();
                    llm_elapsed = agent_start.elapsed();
                    let err_text = e.to_string();
                    let _ = self
                        .app_handle
                        .emit("pipeline:error", format!("Agent failed: {err_text}"));
                    // Also notify on failure — same reason: user may be elsewhere.
                    if config.agent_notification {
                        crate::notify::show_agent_notification(&format!(
                            "Agent failed: {err_text}"
                        ));
                    }
                }
            }

            tracing::info!("[Pipeline Timing] Agent: {}ms", llm_elapsed.as_millis());
        } else if let Some((llm_config, provider)) = pre_llm {
            self.set_state(PipelineState::Polishing);
            let llm_start = std::time::Instant::now();

            // on_chunk only drives the UI transcript display; actual output happens
            // in batch after the full response arrives (see output_text below).
            let app_handle = self.app_handle.clone();
            let on_chunk: llm::ChunkCallback = Box::new(move |chunk: &str| {
                let _ = app_handle.emit("llm:chunk", chunk);
            });

            let req = PolishRequest {
                raw_text: raw_text.clone(),
                app_type: app_ctx.app_type,
                dictionary: dictionary_words,
                translate_enabled: config.translate_enabled,
                target_lang: config.target_lang.clone(),
                selected_text,
                edit_selection: false,
            };

            match provider.polish(&llm_config, &req, Some(&on_chunk)).await {
                Ok(response) => {
                    // Check abort after LLM returns — skip output if cancelled during polish
                    if self.abort_flag.load(Ordering::SeqCst) {
                        tracing::info!("Pipeline aborted after LLM polish, skipping output");
                        return Ok(());
                    }
                    final_text = response.polished_text;
                    // LLM polish can come back empty — the model returns no content
                    // for a short/garbled take, or the streaming reply carries
                    // neither content nor reasoning_content. Outputting "" looks
                    // exactly like "nothing happened" (the user's complaint). Fall
                    // back to the raw transcript so a recording always lands text.
                    if final_text.trim().is_empty() {
                        tracing::warn!(
                            "LLM polish returned empty — falling back to raw transcript"
                        );
                        final_text = raw_text.clone();
                    }
                    llm_elapsed = llm_start.elapsed();

                    if let Err(e) = self
                        .output_text(&final_text, &app_ctx.app_name, &config)
                        .await
                    {
                        tracing::error!("Output failed: {}", e);
                        let _ = self
                            .app_handle
                            .emit("pipeline:error", format!("Output failed: {e}"));
                    }
                }
                Err(e) => {
                    // Check abort after LLM error — skip fallback output if cancelled
                    if self.abort_flag.load(Ordering::SeqCst) {
                        tracing::info!("Pipeline aborted after LLM error, skipping output");
                        return Ok(());
                    }
                    tracing::error!("LLM polish failed: {}, outputting raw text", e);
                    final_text = raw_text.clone();
                    llm_elapsed = llm_start.elapsed();

                    let _ = self
                        .app_handle
                        .emit("pipeline:error", format!("LLM polishing failed: {e}"));
                    if let Err(e) = self
                        .output_text(&final_text, &app_ctx.app_name, &config)
                        .await
                    {
                        tracing::error!("Output failed: {}", e);
                        let _ = self
                            .app_handle
                            .emit("pipeline:error", format!("Output failed: {e}"));
                    }
                }
            }

            tracing::info!(
                "[Pipeline Timing] LLM polish: {}ms",
                llm_elapsed.as_millis()
            );
        } else {
            llm_elapsed = std::time::Duration::ZERO;
            final_text = raw_text.clone();
            if let Err(e) = self
                .output_text(&final_text, &app_ctx.app_name, &config)
                .await
            {
                tracing::error!("Output failed: {}", e);
                let _ = self
                    .app_handle
                    .emit("pipeline:error", format!("Output failed: {e}"));
            }
        }

        let total_elapsed = stop_start.elapsed();

        // Compute recording duration
        let duration_ms = self
            .recording_start
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .map(|start| start.elapsed().as_millis() as i64);

        tracing::info!(
            "[Pipeline Timing] Total stop(): {}ms (STT: {}ms, LLM: {}ms, Output+Save: {}ms)",
            total_elapsed.as_millis(),
            stt_elapsed.as_millis(),
            llm_elapsed.as_millis(),
            total_elapsed.as_millis() - stt_elapsed.as_millis() - llm_elapsed.as_millis(),
        );

        // Emit timing to frontend
        let _ = self.app_handle.emit(
            "pipeline:timing",
            serde_json::json!({
                "stt_ms": stt_elapsed.as_millis() as u64,
                "llm_ms": llm_elapsed.as_millis() as u64,
                "total_ms": total_elapsed.as_millis() as u64,
                "recording_ms": duration_ms,
            }),
        );

        // Save to history
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let entry = storage::HistoryEntry {
            id: 0, // auto-increment
            created_at: now,
            app_name: app_ctx.app_name,
            app_type: format!("{:?}", app_ctx.app_type),
            raw_text,
            polished_text: final_text,
            language: None,
            duration_ms,
            agent_response,
        };
        if let Err(e) = self
            .app_handle
            .state::<storage::HistoryStore>()
            .add(entry)
            .await
        {
            tracing::error!("Failed to save history: {}", e);
        }

        self.set_state(PipelineState::Idle);
        Ok(())
    }

    async fn output_text(
        &self,
        text: &str,
        app_name: &str,
        config: &storage::AppConfig,
    ) -> Result<()> {
        self.set_state(PipelineState::Outputting);

        let mode = if config.output_mode != "keyboard" {
            OutputMode::Clipboard
        } else {
            OutputMode::Keyboard
        };

        // On macOS, keyboard output uses CGEventPost via enigo which requires
        // Accessibility permission. Clipboard mode uses osascript which does not.
        if mode == OutputMode::Keyboard && !is_accessibility_trusted() {
            anyhow::bail!("ACCESSIBILITY_REQUIRED");
        }

        let output = output::create_output(mode, app_name);
        output.type_text(text).await?;

        let _ = self.app_handle.emit("pipeline:target_app", app_name);

        Ok(())
    }

    /// Edit Selection mode: apply the spoken `instruction` to the captured
    /// `selected` text via the LLM, then replace the current selection in-place.
    /// Every failure mode (no selection, LLM disabled/empty/error, target moved,
    /// paste blocked) surfaces a visible error and/or a clipboard fallback — a
    /// clipboard fallback is NEVER reported as a successful in-place replace.
    async fn run_edit_selection(
        &self,
        instruction: &str,
        selected_text: Option<String>,
        app_ctx: &app_detector::AppContext,
        _config: &storage::AppConfig,
        pre_llm: Option<(LlmConfig, Box<dyn llm::LlmProvider>)>,
    ) -> Result<()> {
        // 1. A selection is mandatory. Distinguish "permission blocked capture"
        //    from "nothing selected" — never blame the user for a permission gap.
        let selected = match selected_text {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                if !is_accessibility_trusted() {
                    let _ = request_accessibility_permission();
                    self.emit_edit_result("fail", "permission_capture", "");
                } else {
                    self.emit_edit_result("fail", "no_selection", "");
                }
                self.set_state(PipelineState::Idle);
                return Ok(());
            }
        };

        // Publish the selection size so the capsule can show "editing selection
        // · N chars / N words" once it is known.
        let words = edit_selection_word_count(&selected);
        let chars = selected.chars().count();
        let _ = self.app_handle.emit(
            "edit_selection:size",
            serde_json::json!({ "chars": chars, "words": words }),
        );

        // 2. Length cap: refuse oversized selections up front (words for spaced
        //    text, chars as a CJK/no-space fallback).
        if edit_selection_too_long(&selected) {
            self.emit_edit_result("fail", "too_long", "");
            self.set_state(PipelineState::Idle);
            return Ok(());
        }

        // 3. The rewrite needs the LLM. If polishing is off or no key is set,
        //    pre_llm is None — surface it instead of silently doing nothing.
        let (llm_config, provider) = match pre_llm {
            Some(x) => x,
            None => {
                self.emit_edit_result("fail", "llm_disabled", "");
                self.set_state(PipelineState::Idle);
                return Ok(());
            }
        };

        // 4. Run the instruction against the selection using the DEDICATED Edit
        //    Selection prompt (edit_selection: true) — not the dictation rules.
        self.set_state(PipelineState::Polishing);
        let req = PolishRequest {
            raw_text: instruction.to_string(),
            app_type: app_ctx.app_type,
            dictionary: Vec::new(),
            // Translation in Edit Selection is driven by the spoken instruction,
            // not the global translate toggle — keep it off here.
            translate_enabled: false,
            target_lang: String::new(),
            selected_text: Some(selected.clone()),
            edit_selection: true,
        };
        let mut result = match provider.polish(&llm_config, &req, None).await {
            Ok(r) => r.polished_text,
            Err(e) => {
                tracing::error!("Edit Selection LLM failed: {}", e);
                self.emit_edit_result("fail", "llm_failed", "");
                self.set_state(PipelineState::Idle);
                return Ok(());
            }
        };

        // If the model broke the "single final replacement text" contract (added
        // a "(Note: …)"/explanation/Markdown, or — for translation — left source
        // words untranslated), retry ONCE with an explicit corrective. This is
        // the primary fix for the "翻译成英文 → We … rewrite功能 today. (Note: …)"
        // failure; the deterministic sanitize below is the belt-and-suspenders.
        if !is_edit_selection_no_edit(&result) && edit_selection_output_nonconformant(&result) {
            tracing::warn!("Edit Selection: non-conformant model output — retrying once");
            let retry_req = PolishRequest {
                raw_text: format!(
                    "{instruction}\n\nYour previous answer included notes, explanations, alternatives, Markdown, or left words untranslated. Redo it now: output ONLY the final replacement text, fully in the requested language, with no notes, parentheticals, alternatives, quotes, or Markdown."
                ),
                app_type: app_ctx.app_type,
                dictionary: Vec::new(),
                translate_enabled: false,
                target_lang: String::new(),
                selected_text: Some(selected.clone()),
                edit_selection: true,
            };
            if let Ok(r2) = provider.polish(&llm_config, &retry_req, None).await {
                if !r2.polished_text.trim().is_empty() {
                    result = r2.polished_text;
                }
            }
        }

        if self.abort_flag.load(Ordering::SeqCst) {
            tracing::info!("Edit Selection aborted after LLM, skipping replace");
            self.set_state(PipelineState::Idle);
            return Ok(());
        }

        // The model emits the NO_EDIT sentinel when the instruction is empty,
        // unintelligible, or unrelated — surface a failure instead of pasting a
        // chat-style answer into the user's document.
        if is_edit_selection_no_edit(&result) {
            self.emit_edit_result("fail", "no_edit", "");
            self.set_state(PipelineState::Idle);
            return Ok(());
        }

        // Deterministic final cleanup so what we paste is directly-replaceable
        // text (strips any residual "(Note: …)" / code fence the retry left).
        let final_text = sanitize_edit_selection_output(&result);
        if final_text.trim().is_empty() {
            self.emit_edit_result("fail", "no_result", "");
            self.set_state(PipelineState::Idle);
            return Ok(());
        }

        // 5. Validate the target + that the ORIGINAL selection is still present,
        //    then replace it in-place (or fall back).
        self.replace_current_selection(&final_text, app_ctx, &selected)
            .await;
        self.set_state(PipelineState::Idle);
        Ok(())
    }

    /// Replace the current selection with `text`, but ONLY when we can prove it
    /// is safe: the foreground app is unchanged AND the current selection still
    /// matches the text we originally captured. If the app moved, the selection
    /// was lost/changed, permission is missing, or the paste fails, we copy to
    /// the clipboard and surface a visible error — a clipboard fallback is
    /// deliberately NEVER reported as a successful in-place replace.
    async fn replace_current_selection(
        &self,
        text: &str,
        captured: &app_detector::AppContext,
        original_selection: &str,
    ) {
        self.set_state(PipelineState::Outputting);

        // Re-detect the foreground app. If focus moved away from the app we
        // captured the selection from, a blind paste would land in the wrong
        // place — fall back to the clipboard instead.
        let current = tokio::task::block_in_place(app_detector::detect_current_app);
        if edit_selection_target_changed(&captured.app_name, &current.app_name) {
            self.copy_to_clipboard_fallback(text, "focus_changed", &current.app_name);
            return;
        }

        // Synthetic paste (and the re-capture below) needs Accessibility on
        // macOS; without it the keystroke is silently dropped. Fall back to the
        // clipboard rather than pretend.
        if !is_accessibility_trusted() {
            self.copy_to_clipboard_fallback(text, "permission_replace", "");
            let _ = request_accessibility_permission();
            return;
        }

        // Re-capture the CURRENT selection and verify it still matches what we
        // captured before the LLM ran. This closes the "same app but selection
        // lost" hole: if the user clicked elsewhere in the same app (deselecting
        // or moving the caret), a paste would silently land at the caret. If the
        // selection is gone, unreadable, or no longer matches, we must NOT paste
        // — fall back to the clipboard. capture_selected_text() restores the
        // user's clipboard afterwards, so this probe leaves no residue.
        let current_selection = tokio::task::block_in_place(|| self.capture_selected_text());
        if !edit_selection_still_replaceable(original_selection, current_selection.as_deref()) {
            self.copy_to_clipboard_fallback(text, "selection_lost", "");
            return;
        }

        // Both guards passed: the same app is frontmost and the original
        // selection is still present. Paste over it → in-place replace. The
        // clipboard output sets the text before pasting, so on a paste failure
        // the result is already on the clipboard for a manual retry.
        let output = output::create_output(OutputMode::Clipboard, &captured.app_name);
        match output.type_text(text).await {
            Ok(()) => {
                // A successful Cmd+V keystroke does NOT prove the target accepted
                // the paste: a read-only / paste-rejecting field silently ignores
                // it while leaving the original selection in place. Probe once
                // more — if the original selection is STILL there unchanged, the
                // content was not replaced, so this is a rejected paste and must
                // NOT be reported as success.
                //
                // Give the target app time to actually APPLY the paste before we
                // probe — otherwise a slow app that hasn't replaced the selection
                // yet looks identical to a read-only reject and we'd wrongly tell
                // the user nothing changed (risking a double-paste) on a replace
                // that lands a moment later.
                tokio::time::sleep(std::time::Duration::from_millis(
                    EDIT_SELECTION_PASTE_SETTLE_MS,
                ))
                .await;
                let after_paste = tokio::task::block_in_place(|| self.capture_selected_text());
                if edit_selection_paste_rejected(original_selection, after_paste.as_deref()) {
                    tracing::warn!(
                        "Edit Selection: paste rejected by '{}' (selection unchanged after paste) — treating as read-only, falling back",
                        captured.app_name
                    );
                    self.copy_to_clipboard_fallback(text, "readonly", &captured.app_name);
                    return;
                }
                tracing::info!(
                    "Edit Selection: replaced selection in '{}'",
                    captured.app_name
                );
                // Keep target_app for the generic target display, and emit a
                // distinct success result so the UI can confirm the in-place
                // replace and show the undo hint.
                let _ = self
                    .app_handle
                    .emit("pipeline:target_app", captured.app_name.clone());
                self.emit_edit_result("success", "", &captured.app_name);
            }
            Err(e) => {
                // ClipboardOutput already placed `text` on the clipboard, so this
                // is a fallback: the paste keystroke itself failed (often a
                // permission gap). Surface it as a localized fallback, not success.
                tracing::error!("Edit Selection paste failed: {}", e);
                self.copy_to_clipboard_fallback(text, "paste_failed", &captured.app_name);
            }
        }
    }

    /// Put `text` on the clipboard and emit a localized-by-the-frontend Edit
    /// Selection FALLBACK result (`code` → i18n key, `app` for interpolation).
    /// A fallback keeps the result on the clipboard but is NEVER a success.
    fn copy_to_clipboard_fallback(&self, text: &str, code: &str, app: &str) {
        let copied = arboard::Clipboard::new()
            .and_then(|mut c| c.set_text(text))
            .is_ok();
        if !copied {
            tracing::warn!("Edit Selection: clipboard fallback failed to set text");
        }
        self.emit_edit_result("fallback", code, app);
    }

    /// Emit an Edit Selection outcome for the frontend to localize and surface.
    /// `status` is "success" | "fallback" | "fail"; `code` maps to an i18n key
    /// (empty for success); `app` is the target app name for interpolation.
    fn emit_edit_result(&self, status: &str, code: &str, app: &str) {
        let _ = self.app_handle.emit(
            "edit_selection:result",
            serde_json::json!({ "status": status, "code": code, "app": app }),
        );
    }

    /// P1-2: Pre-warm HTTP connection pool by issuing a HEAD request to the STT endpoint.
    /// Call once after app startup to avoid cold-start TLS handshake on first recording.
    pub async fn pre_warm(&self) {
        let config = self.load_config().await;

        // Pre-warm STT endpoint
        let stt_endpoint = match config.stt_provider.as_str() {
            "cloud" => {
                let base = crate::api_base_url();
                format!("{}/api/proxy/stt", base)
            }
            "glm-asr" => "https://open.bigmodel.cn/api/paas/v4/audio/transcriptions".to_string(),
            "openai-whisper" => "https://api.openai.com/v1/audio/transcriptions".to_string(),
            "groq-whisper" => "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            "siliconflow" => "https://api.siliconflow.cn/v1/audio/transcriptions".to_string(),
            "deepgram" => "https://api.deepgram.com/v1/listen".to_string(),
            "assemblyai" => "https://api.assemblyai.com/v2/transcript".to_string(),
            "qwen-asr" | "dashscope-stream" => {
                "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation".to_string()
            }
            _ => {
                tracing::debug!(
                    "Unknown STT provider '{}', skipping pre-warm",
                    config.stt_provider
                );
                return;
            }
        };
        tracing::debug!("Pre-warming HTTP connection to {}", stt_endpoint);
        let _ = self
            .shared_client
            .head(&stt_endpoint)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
        tracing::debug!("STT connection pre-warm complete");

        // Pre-warm LLM endpoint if polish is enabled
        if config.polish_enabled {
            let llm_url = if config.llm_provider == "cloud" {
                let base = crate::api_base_url();
                format!("{}/api/proxy/llm", base)
            } else {
                config.llm_base_url.clone()
            };
            tracing::debug!("Pre-warming LLM connection to {}", llm_url);
            let _ = self
                .shared_client
                .head(&llm_url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await;
            tracing::debug!("LLM connection pre-warm complete");
        }
    }
}

/// Whether the foreground app changed since we captured the selection. An empty
/// captured name means detection failed at capture time — we can't prove the
/// target is the same, so treat it as changed and fall back to the clipboard
/// rather than risk pasting the replacement into the wrong window.
fn edit_selection_target_changed(captured_app: &str, current_app: &str) -> bool {
    captured_app.is_empty() || captured_app != current_app
}

/// Collapse runs of whitespace to single spaces and trim, so a selection
/// re-captured via a second Cmd/Ctrl+C compares equal to the original despite
/// trailing newlines or copy-time whitespace jitter. Intentionally lenient on
/// whitespace only — any difference in the actual characters means the
/// selection changed and we must fall back.
fn normalize_selection(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether the current selection is still the one we captured and can therefore
/// be safely replaced in-place. `current` is the re-captured selection (None =
/// nothing selected / capture failed). Returns false for a missing, empty, or
/// mismatched selection — every one of those must fall back to the clipboard
/// instead of pasting blindly at the caret.
fn edit_selection_still_replaceable(original: &str, current: Option<&str>) -> bool {
    match current {
        Some(c) => {
            let c = normalize_selection(c);
            !c.is_empty() && c == normalize_selection(original)
        }
        None => false,
    }
}

/// After a paste, whether the target REJECTED it (e.g. a read-only field): the
/// original selection is still present unchanged, so nothing was replaced. A
/// successful `Cmd+V` keystroke alone never proves replacement — only a content
/// change does. Same predicate as `edit_selection_still_replaceable`, but named
/// for the post-paste check where a still-present original selection means
/// FAILURE (reject → fall back), not success. On a real replace the selection is
/// gone/changed (`after_paste` is None or differs) → returns false → success.
fn edit_selection_paste_rejected(original: &str, after_paste: Option<&str>) -> bool {
    edit_selection_still_replaceable(original, after_paste)
}

/// Word count for Edit Selection length limits: whitespace-separated tokens.
fn edit_selection_word_count(selected: &str) -> usize {
    selected.split_whitespace().count()
}

/// Whether a selection exceeds the Edit Selection size cap. Uses a word cap for
/// spaced text and a character cap as a CJK / no-whitespace fallback (a long
/// Chinese passage can be one "word" but thousands of characters).
fn edit_selection_too_long(selected: &str) -> bool {
    const MAX_WORDS: usize = 1000;
    const MAX_CHARS: usize = 6000;
    edit_selection_word_count(selected) > MAX_WORDS || selected.chars().count() > MAX_CHARS
}

/// Whether the model returned the NO_EDIT sentinel (instruction empty,
/// unintelligible, or unrelated) — map to a visible failure, never paste it.
fn is_edit_selection_no_edit(result: &str) -> bool {
    result.trim() == crate::llm::prompt::EDIT_SELECTION_NO_EDIT
}

/// Deterministically clean a model edit result down to directly-pastable text:
/// unwrap a fully-fenced code block and strip a trailing meta-note like
/// "(Note: …)" / "（注：…）" / a final "Note:" line. Intentionally conservative —
/// only very high-confidence meta patterns are removed so legitimate content is
/// never corrupted. The real fix for mixed-language / explanatory output is the
/// prompt plus a one-shot retry; this is the belt-and-suspenders last pass.
fn sanitize_edit_selection_output(raw: &str) -> String {
    let mut s = raw.trim().to_string();

    // 1. Unwrap a fully-fenced ```lang … ``` block.
    if s.starts_with("```") && s.trim_end().ends_with("```") {
        if let Some(first_nl) = s.find('\n') {
            let inner = &s[first_nl + 1..];
            if let Some(close) = inner.rfind("```") {
                s = inner[..close].trim().to_string();
            }
        }
    }

    // 2. Strip a trailing parenthetical meta-note "(Note: …)" / "（注：…）".
    //    Requires the colon form so ordinary asides like "(note the wording)"
    //    are left untouched. Only strips when the note runs to the very end.
    // `to_ascii_lowercase()` is byte-length-preserving, so indices found in it
    // are valid slice boundaries in `s` (unlike full Unicode `to_lowercase`).
    for (close, marker) in [(')', "(note:"), ('）', "（注："), ('）', "（注:")] {
        let lower = s.to_ascii_lowercase();
        if let Some(idx) = lower.rfind(marker) {
            // The note must be the trailing segment (closes at/near the end).
            if s.trim_end().ends_with(close) {
                let before = s[..idx].trim_end();
                if !before.is_empty() {
                    s = before.to_string();
                }
            }
        }
    }

    // 3. Strip a trailing standalone "Note: …" line (no parens).
    let lower = s.to_ascii_lowercase();
    if let Some(idx) = lower.rfind("\nnote:") {
        let before = s[..idx].trim_end();
        if !before.is_empty() {
            s = before.to_string();
        }
    }

    s.trim().to_string()
}

/// Whether a model edit result is non-conformant (contains meta-notes/fences),
/// i.e. sanitizing changed it — a signal to retry once for a clean single output.
fn edit_selection_output_nonconformant(raw: &str) -> bool {
    sanitize_edit_selection_output(raw) != raw.trim()
}

#[cfg(test)]
mod tests {
    use super::{
        edit_selection_output_nonconformant, edit_selection_paste_rejected,
        edit_selection_still_replaceable, edit_selection_target_changed, edit_selection_too_long,
        is_edit_selection_no_edit, sanitize_edit_selection_output,
    };

    #[test]
    fn target_unchanged_when_same_app() {
        assert!(!edit_selection_target_changed("TextEdit", "TextEdit"));
    }

    // ── Length cap + NO_EDIT sentinel ──

    #[test]
    fn length_ok_for_normal_selection() {
        assert!(!edit_selection_too_long(
            "Make this sentence a bit more polite."
        ));
        // ~999 words is under the word cap.
        let words = vec!["word"; 999].join(" ");
        assert!(!edit_selection_too_long(&words));
    }

    #[test]
    fn too_long_over_word_cap() {
        let words = vec!["word"; 1001].join(" ");
        assert!(edit_selection_too_long(&words));
    }

    #[test]
    fn too_long_over_char_cap_for_cjk() {
        // A single "word" (no spaces) but > 6000 chars — the CJK/no-space fallback.
        let cjk = "字".repeat(6001);
        assert_eq!(super::edit_selection_word_count(&cjk), 1);
        assert!(edit_selection_too_long(&cjk));
    }

    #[test]
    fn no_edit_sentinel_detected() {
        assert!(is_edit_selection_no_edit("NO_EDIT"));
        assert!(is_edit_selection_no_edit("  NO_EDIT\n"));
        assert!(!is_edit_selection_no_edit("Make it shorter."));
        assert!(!is_edit_selection_no_edit("NO_EDITING here"));
    }

    // ── Output sanitize / non-conformance (Turing's translate failure) ──

    #[test]
    fn sanitize_strips_turing_translation_note() {
        // Turing's exact failing output — the trailing "(Note: …)" must be removed.
        let raw = "We completed the selected text rewrite功能 today. (Note: The word \"功能\" means \"function\" or \"feature\", so a more accurate translation would be: \"We completed the selected text rewrite feature today.\")";
        let out = sanitize_edit_selection_output(raw);
        assert!(!out.contains("Note:"), "note must be stripped: {out}");
        assert!(
            !out.contains("more accurate"),
            "explanation stripped: {out}"
        );
        assert!(out.starts_with("We completed the selected text rewrite"));
        // And it is flagged non-conformant → triggers the one-shot retry.
        assert!(edit_selection_output_nonconformant(raw));
    }

    #[test]
    fn sanitize_unwraps_code_fence() {
        assert_eq!(
            sanitize_edit_selection_output("```\nHello there\n```"),
            "Hello there"
        );
        assert_eq!(
            sanitize_edit_selection_output("```text\nfixed line\n```"),
            "fixed line"
        );
    }

    #[test]
    fn sanitize_strips_chinese_note() {
        let out = sanitize_edit_selection_output("最终替换文本。（注：这里是解释）");
        assert_eq!(out, "最终替换文本。");
    }

    #[test]
    fn sanitize_leaves_clean_output_untouched() {
        // Conformant results — including a legit non-colon aside — pass through.
        let clean = "We finished the Edit Selection feature today.";
        assert_eq!(sanitize_edit_selection_output(clean), clean);
        assert!(!edit_selection_output_nonconformant(clean));
        let aside = "Press the button (note the color) before saving.";
        assert_eq!(sanitize_edit_selection_output(aside), aside);
        assert!(!edit_selection_output_nonconformant(aside));
    }

    #[test]
    fn target_changed_when_focus_moved() {
        assert!(edit_selection_target_changed("TextEdit", "Safari"));
    }

    #[test]
    fn target_changed_when_capture_unknown() {
        // Empty captured name = detection failed → cannot prove same target,
        // so we must fall back rather than blind-paste.
        assert!(edit_selection_target_changed("", "TextEdit"));
        assert!(edit_selection_target_changed("", ""));
    }

    // ── Finding 1: same app but selection lost/changed must fall back ──

    #[test]
    fn replaceable_when_selection_matches() {
        assert!(edit_selection_still_replaceable(
            "hello world",
            Some("hello world")
        ));
    }

    #[test]
    fn replaceable_ignores_whitespace_jitter() {
        // A re-capture may pick up a trailing newline or collapsed spaces; that
        // is still the same selection and should replace.
        assert!(edit_selection_still_replaceable(
            "hello world",
            Some("  hello   world\n")
        ));
    }

    #[test]
    fn not_replaceable_when_selection_missing() {
        // Same app is frontmost, but nothing is selected / re-capture failed →
        // must fall back, never paste at the caret.
        assert!(!edit_selection_still_replaceable("hello world", None));
    }

    #[test]
    fn not_replaceable_when_selection_empty() {
        assert!(!edit_selection_still_replaceable("hello world", Some("")));
        assert!(!edit_selection_still_replaceable(
            "hello world",
            Some("   \n")
        ));
    }

    #[test]
    fn not_replaceable_when_selection_changed() {
        // User clicked elsewhere and a different range is now selected.
        assert!(!edit_selection_still_replaceable(
            "hello world",
            Some("goodbye world")
        ));
    }

    // ── Runtime finding: read-only / paste-rejected target must not be success ──

    #[test]
    fn paste_rejected_when_selection_unchanged() {
        // Read-only field: Cmd+V was a no-op, the original selection is still
        // there unchanged → the paste was rejected, never report success.
        assert!(edit_selection_paste_rejected(
            "readonly paste target",
            Some("readonly paste target")
        ));
    }

    #[test]
    fn paste_rejected_ignores_whitespace_jitter() {
        // Re-capture whitespace jitter must not fool the reject check.
        assert!(edit_selection_paste_rejected(
            "readonly paste target",
            Some("  readonly   paste target\n")
        ));
    }

    #[test]
    fn paste_accepted_when_selection_gone() {
        // Normal textarea/contenteditable/TextEdit: after a real paste the caret
        // sits after the inserted text with nothing selected → re-capture is
        // None → not rejected → success path preserved.
        assert!(!edit_selection_paste_rejected(
            "readonly paste target",
            None
        ));
    }

    #[test]
    fn paste_accepted_when_content_changed() {
        // The field now holds the replacement (a different selection or none of
        // the original) → not rejected → success.
        assert!(!edit_selection_paste_rejected(
            "readonly paste target",
            Some("the rewritten text")
        ));
        assert!(!edit_selection_paste_rejected(
            "readonly paste target",
            Some("")
        ));
    }
}

/// Repeatable model-acceptance fixture for the 5 Edit Selection instruction
/// classes, including Turing's failing translate case. IGNORED by default so
/// normal `cargo test` and CI need no API key. It mirrors `run_edit_selection`'s
/// retry-once + sanitize logic and asserts every result is directly-pastable
/// (no notes/Markdown; translation leaves no CJK residue).
///
/// Run it (no key is stored in the repo — supply your own via env):
///   OT_EDIT_FIXTURE_BASE_URL=https://openrouter.ai/api/v1 \
///   OT_EDIT_FIXTURE_MODEL=google/gemini-2.5-flash \
///   OT_EDIT_FIXTURE_API_KEY=sk-... \
///   cargo test --lib edit_selection_model_fixture -- --ignored --nocapture
#[cfg(test)]
mod edit_selection_model_fixture {
    use super::{edit_selection_output_nonconformant, sanitize_edit_selection_output};
    use crate::llm::{self, LlmConfig, PolishRequest};

    async fn run_case(
        provider: &dyn llm::LlmProvider,
        cfg: &LlmConfig,
        selected: &str,
        instruction: &str,
    ) -> String {
        let mk = |raw: String| PolishRequest {
            raw_text: raw,
            app_type: llm::AppType::General,
            dictionary: Vec::new(),
            translate_enabled: false,
            target_lang: String::new(),
            selected_text: Some(selected.to_string()),
            edit_selection: true,
        };
        let mut out = provider
            .polish(cfg, &mk(instruction.to_string()), None)
            .await
            .expect("polish failed")
            .polished_text;
        if edit_selection_output_nonconformant(&out) {
            let retry = format!(
                "{instruction}\n\nYour previous answer included notes, explanations, alternatives, Markdown, or left words untranslated. Redo it now: output ONLY the final replacement text, fully in the requested language, with no notes, parentheticals, alternatives, quotes, or Markdown."
            );
            if let Ok(r2) = provider.polish(cfg, &mk(retry), None).await {
                if !r2.polished_text.trim().is_empty() {
                    out = r2.polished_text;
                }
            }
        }
        sanitize_edit_selection_output(&out)
    }

    #[ignore = "requires a live LLM; supply OT_EDIT_FIXTURE_* env vars"]
    #[tokio::test]
    async fn five_instruction_classes_produce_replaceable_text() {
        let (base, key, model) = match (
            std::env::var("OT_EDIT_FIXTURE_BASE_URL"),
            std::env::var("OT_EDIT_FIXTURE_API_KEY"),
            std::env::var("OT_EDIT_FIXTURE_MODEL"),
        ) {
            (Ok(b), Ok(k), Ok(m)) if !k.is_empty() => (b, k, m),
            _ => {
                eprintln!("skipping: set OT_EDIT_FIXTURE_BASE_URL/API_KEY/MODEL to run");
                return;
            }
        };
        let cfg = LlmConfig {
            api_key: key,
            model,
            base_url: base,
            max_tokens: 4096,
            temperature: 0.3,
        };
        let provider = llm::create_provider("openai", Some(reqwest::Client::new()));

        // 1. Translate — Turing's exact failing case.
        let translated = run_case(
            provider.as_ref(),
            &cfg,
            "今天我们完成了选区改写功能",
            "翻译成英文",
        )
        .await;
        eprintln!("translate => {translated}");
        assert!(!translated.trim().is_empty());
        assert!(!edit_selection_output_nonconformant(&translated));
        assert!(!translated.contains("Note:"));
        assert!(
            !translated
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "translation left CJK residue: {translated}"
        );

        // 2-5. shorten / expand / more polite / fix typos — must be clean,
        //      directly-pastable text (no notes/Markdown/alternatives).
        for (selected, instruction) in [
            (
                "This is a fairly long-winded sentence that could honestly be tightened up quite a bit.",
                "改短一点",
            ),
            ("The cat sat on the mat.", "扩写成两句"),
            ("send me the report now", "改得更礼貌"),
            ("hey can u send me teh fil", "修正错别字"),
        ] {
            let out = run_case(provider.as_ref(), &cfg, selected, instruction).await;
            eprintln!("{instruction} => {out}");
            assert!(!out.trim().is_empty(), "empty for {instruction}");
            assert!(
                !edit_selection_output_nonconformant(&out),
                "non-conformant for {instruction}: {out}"
            );
            assert!(!out.contains("Note:"), "note leaked for {instruction}: {out}");
            assert!(!out.contains("```"), "markdown fence for {instruction}: {out}");
        }
    }
}
