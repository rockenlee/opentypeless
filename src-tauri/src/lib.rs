pub mod agent;
pub mod app_detector;
pub mod audio;
pub mod llm;
pub mod media;
pub mod notify;
pub mod output;
pub mod pipeline;
pub mod sfx;
pub mod storage;
pub mod stt;
pub mod update;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_store::StoreExt;
use tracing_subscriber::EnvFilter;

use std::sync::{Arc, Mutex};

/// Default cloud API base URL. Override with the `API_BASE_URL` environment variable.
pub const DEFAULT_API_BASE_URL: &str = "https://www.opentypeless.com";

/// Read the cloud API base URL from the environment, falling back to the compiled default.
pub fn api_base_url() -> String {
    std::env::var("API_BASE_URL").unwrap_or_else(|_| DEFAULT_API_BASE_URL.to_string())
}

/// Cached hotkey mode to avoid loading config from disk on every keypress.
/// Updated whenever config is saved.
struct HotkeyModeCache(Arc<Mutex<String>>);

/// Cached close_to_tray setting to avoid blocking I/O in the window close handler.
struct CloseToTrayCache(Arc<Mutex<bool>>);

/// Cached (modifiers, key) of the translate-hotkey shortcut. The dispatcher
/// uses this to tell apart "start/stop recording" presses from "toggle
/// translate mode" presses. We store mods+key rather than the `Shortcut`
/// itself because `Shortcut`'s `PartialEq` compares unique instance IDs
/// (not key bindings) — comparing two `Shortcut`s built from the same
/// mods+key returns false. `None` = no translate hotkey is bound.
struct TranslateHotkeyCache(Arc<Mutex<Option<(Modifiers, Code)>>>);

/// Cached (modifiers, key) of the agent-hotkey shortcut. Pressing this fires
/// a "forced agent" recording — whole transcript routed to Hermes regardless
/// of trigger-word prefix. Same comparison rationale as `TranslateHotkeyCache`.
struct AgentHotkeyCache(Arc<Mutex<Option<(Modifiers, Code)>>>);

/// Cached mouse trigger settings to avoid disk I/O on every mouse event.
struct MouseTriggersEnabledCache(Arc<Mutex<bool>>);
struct MouseMiddleClickActionCache(Arc<Mutex<String>>);
struct MouseMiddleDoubleClickActionCache(Arc<Mutex<String>>);
struct MouseMiddleRightActionCache(Arc<Mutex<String>>);
struct MouseLeftMiddleActionCache(Arc<Mutex<String>>);

/// Platform-agnostic mouse button events forwarded to the gesture state machine.
#[derive(Debug)]
enum MouseRawEvent {
    LeftDown,
    LeftUp,
    MiddleDown,
    MiddleUp,
    RightDown,
    RightUp,
}

// CoreGraphics/CoreFoundation FFI for macOS-native event tap.
// Only subscribes to mouse button event types — avoids the keyboard/TSM crash
// that rdev triggers by processing all event types on a background thread.
#[cfg(target_os = "macos")]
#[allow(clippy::duplicated_attributes)]
#[link(name = "CoreGraphics", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: *const std::ffi::c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *mut std::ffi::c_void;
    fn CGEventPost(tap_location: u32, event: *mut std::ffi::c_void);
    fn CFRelease(cf: *mut std::ffi::c_void);
    // Reads the current pressed state of a mouse button without installing an
    // event tap — needs no Accessibility grant for mouse buttons, and leaves
    // nothing for the OS to disable. Used by the polling mouse listener.
    fn CGEventSourceButtonState(state_id: u32, button: u32) -> bool;
}

/// Session token for cloud providers. Set by the frontend after Better Auth login.
/// The Rust pipeline reads this when creating cloud STT/LLM providers.
pub struct SessionTokenStore(pub Arc<Mutex<String>>);

/// Managed tray icon handle for dynamic menu/tooltip updates.
pub struct TrayHandle {
    pub tray: Mutex<tauri::tray::TrayIcon>,
}

#[derive(serde::Serialize)]
struct AudioCaptureTestResult {
    duration_ms: u64,
    chunks: usize,
    bytes: usize,
    max_volume: f32,
}

/// Persisted window position and size.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// Build (or rebuild) the system tray menu based on current state.
fn build_tray_menu(
    app: &tauri::AppHandle,
    is_recording: bool,
    window_visible: bool,
) -> Result<Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let show_hide = MenuItem::with_id(
        app,
        "show_hide",
        if window_visible {
            "Hide Window"
        } else {
            "Show Window"
        },
        true,
        None::<&str>,
    )?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let record = MenuItem::with_id(
        app,
        "record",
        if is_recording {
            "Stop Recording"
        } else {
            "Start Recording"
        },
        true,
        None::<&str>,
    )?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let history = MenuItem::with_id(app, "history", "History", true, None::<&str>)?;
    let account = MenuItem::with_id(app, "account", "Account", true, None::<&str>)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let about = MenuItem::with_id(app, "about", "About OpenTypeless", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_hide, &sep1, &record, &sep2, &settings, &history, &account, &sep3, &about, &quit,
        ],
    )?;
    Ok(menu)
}

/// Update the tray tooltip (when `tooltip` is `Some`) and rebuild its menu —
/// always on the main thread.
///
/// Tray (NSStatusItem) mutations are main-thread-only and dispatch
/// *synchronously*. Doing them from a worker thread while holding the `tray`
/// lock deadlocks: the worker blocks inside the sync dispatch while holding
/// `tray`, and the main thread (which also locks `tray`, e.g. from its tray-menu
/// event handlers) blocks trying to acquire it. `run_on_main_thread` returns
/// immediately, so `tray` is only ever locked on the main thread and the two can
/// never deadlock.
pub fn update_tray(app: &tauri::AppHandle, tooltip: Option<String>) {
    let app_handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let is_recording = app_handle
            .try_state::<pipeline::PipelineHandle>()
            .map(|p| p.current_state() == pipeline::PipelineState::Recording)
            .unwrap_or(false);
        let window_visible = app_handle
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);

        if let Some(tray_handle) = app_handle.try_state::<TrayHandle>() {
            if let Ok(tray) = tray_handle.tray.lock() {
                if let Some(tip) = tooltip.as_deref() {
                    let _ = tray.set_tooltip(Some(tip));
                }
                if let Ok(menu) = build_tray_menu(&app_handle, is_recording, window_visible) {
                    let _ = tray.set_menu(Some(menu));
                }
            }
        }
    });
}

/// Rebuild the tray menu based on current pipeline/window state.
pub fn refresh_tray(app: &tauri::AppHandle) {
    update_tray(app, None);
}

#[tauri::command]
async fn start_recording(state: tauri::State<'_, pipeline::PipelineHandle>) -> Result<(), String> {
    state.start().await.map_err(|e| e.to_string())
}

/// List available input device names for the microphone picker in Settings.
#[tauri::command]
fn list_input_devices() -> Vec<String> {
    crate::audio::capture::list_input_device_names()
}

#[tauri::command]
async fn stop_recording(state: tauri::State<'_, pipeline::PipelineHandle>) -> Result<(), String> {
    state.stop().await.map_err(|e| e.to_string())
}

#[tauri::command]
fn abort_recording(state: tauri::State<'_, pipeline::PipelineHandle>) -> Result<(), String> {
    state.abort();
    Ok(())
}

#[tauri::command]
async fn test_audio_capture() -> Result<AudioCaptureTestResult, String> {
    let started = std::time::Instant::now();
    let (mut handle, mut audio_rx) =
        audio::AudioCaptureHandle::start(audio::AudioConfig::default())
            .map_err(|e| e.to_string())?;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3));
    tokio::pin!(deadline);

    let mut chunks = 0usize;
    let mut bytes = 0usize;
    let mut max_volume = 0.0f32;

    loop {
        tokio::select! {
            maybe_chunk = audio_rx.recv() => {
                match maybe_chunk {
                    Some(chunk) => {
                        chunks += 1;
                        bytes += chunk.len();
                        max_volume = max_volume.max(handle.get_volume());
                    }
                    None => break,
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                max_volume = max_volume.max(handle.get_volume());
            }
            _ = &mut deadline => {
                break;
            }
        }
    }

    handle.stop();
    Ok(AudioCaptureTestResult {
        duration_ms: started.elapsed().as_millis() as u64,
        chunks,
        bytes,
        max_volume,
    })
}

#[tauri::command]
fn check_accessibility_permission() -> bool {
    pipeline::is_accessibility_trusted()
}

#[tauri::command]
fn request_accessibility_permission() -> bool {
    pipeline::request_accessibility_permission()
}

#[tauri::command]
async fn get_config(
    state: tauri::State<'_, storage::ConfigManager>,
) -> Result<storage::AppConfig, String> {
    state.load().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_config(
    state: tauri::State<'_, storage::ConfigManager>,
    cache: tauri::State<'_, HotkeyModeCache>,
    close_tray_cache: tauri::State<'_, CloseToTrayCache>,
    config: storage::AppConfig,
) -> Result<(), String> {
    *cache.0.lock().unwrap_or_else(|e| e.into_inner()) = config.hotkey_mode.clone();
    *close_tray_cache.0.lock().unwrap_or_else(|e| e.into_inner()) = config.close_to_tray;
    state.save(&config).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn test_stt_connection(
    api_key: String,
    provider: String,
    token_store: tauri::State<'_, SessionTokenStore>,
) -> Result<bool, String> {
    if provider.is_empty() {
        return Ok(false);
    }

    // Cloud provider: verify session token + Pro status via API
    if provider == "cloud" {
        let token = token_store
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if token.is_empty() {
            return Ok(false);
        }
        let api_base = api_base_url();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/api/subscription/status", api_base))
            .header("Authorization", format!("Bearer {}", token))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        return Ok(body["plan"].as_str() == Some("pro"));
    }

    if api_key.is_empty() {
        return Ok(false);
    }

    match provider.as_str() {
        "deepgram" => {
            let client = reqwest::Client::new();
            let resp = client
                .get("https://api.deepgram.com/v1/projects")
                .header("Authorization", format!("Token {}", api_key))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            Ok(resp.status().is_success())
        }
        "assemblyai" => {
            let client = reqwest::Client::new();
            let resp = client
                .get("https://api.assemblyai.com/v2/transcript?limit=1")
                .header("Authorization", api_key)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            Ok(resp.status().is_success())
        }
        "qwen-asr" | "dashscope-stream" => {
            let client = reqwest::Client::new();
            let resp = client
                .get("https://dashscope.aliyuncs.com/compatible-mode/v1/models")
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            Ok(resp.status().is_success())
        }
        "glm-asr" | "openai-whisper" | "groq-whisper" | "siliconflow" => {
            // All four use Whisper-compatible file upload API
            let (endpoint, model, extra_fields): (&str, &str, &[(&str, &str)]) =
                match provider.as_str() {
                    "glm-asr" => (
                        "https://open.bigmodel.cn/api/paas/v4/audio/transcriptions",
                        "glm-asr-2512",
                        &[("stream", "false")][..],
                    ),
                    "openai-whisper" => (
                        "https://api.openai.com/v1/audio/transcriptions",
                        "whisper-1",
                        &[][..],
                    ),
                    "groq-whisper" => (
                        "https://api.groq.com/openai/v1/audio/transcriptions",
                        "whisper-large-v3-turbo",
                        &[][..],
                    ),
                    _ => (
                        "https://api.siliconflow.cn/v1/audio/transcriptions",
                        "FunAudioLLM/SenseVoiceSmall",
                        &[][..],
                    ),
                };

            let silent_pcm = vec![0u8; 3200]; // 0.1s at 16kHz 16-bit mono
            let wav = stt::whisper_compat::WhisperCompatProvider::build_wav(&silent_pcm, 16000);

            let file_part = reqwest::multipart::Part::bytes(wav)
                .file_name("test.wav")
                .mime_str("audio/wav")
                .map_err(|e| e.to_string())?;
            let mut form = reqwest::multipart::Form::new()
                .text("model", model.to_string())
                .part("file", file_part);
            for &(key, value) in extra_fields {
                form = form.text(key.to_string(), value.to_string());
            }

            let client = reqwest::Client::new();
            let resp = client
                .post(endpoint)
                .header("Authorization", format!("Bearer {}", api_key))
                .multipart(form)
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            Ok(resp.status().is_success())
        }
        _ => Err(format!("Unknown STT provider: {}", provider)),
    }
}

#[tauri::command]
async fn test_agent(config: storage::AppConfig) -> Result<String, String> {
    agent::test_agent(config).await.map_err(|e| e.to_string())
}

// Backward-compat alias for callers that still send the old command name.
#[tauri::command]
async fn test_hermes_agent(config: storage::AppConfig) -> Result<String, String> {
    test_agent(config).await
}

#[tauri::command]
async fn test_agent_route(
    app: tauri::AppHandle,
    config: storage::AppConfig,
    text: String,
) -> Result<String, String> {
    let prompt = agent::parse_agent_prompt(&text)
        .ok_or_else(|| "Text does not start with an agent trigger word".to_string())?;
    let request = agent::AgentRequest {
        prompt,
        app_context: app_detector::AppContext::default(),
        selected_text: None,
        config,
    };
    let response = agent::run_agent(request).await.map_err(|e| e.to_string())?;
    pipeline::show_agent_result_window(&app, response.clone()).map_err(|e| e.to_string())?;
    Ok(response)
}

#[tauri::command]
async fn test_hermes_route(
    app: tauri::AppHandle,
    config: storage::AppConfig,
    text: String,
) -> Result<String, String> {
    test_agent_route(app, config, text).await
}

#[tauri::command]
fn get_last_agent_result() -> Option<String> {
    pipeline::latest_agent_result()
}

#[tauri::command]
async fn test_llm_connection(
    api_key: String,
    provider: String,
    base_url: String,
    model: String,
    token_store: tauri::State<'_, SessionTokenStore>,
) -> Result<bool, String> {
    if provider.is_empty() {
        return Ok(false);
    }

    // Cloud provider: verify session token + Pro status via API
    if provider == "cloud" {
        let token = token_store
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if token.is_empty() {
            return Ok(false);
        }
        let api_base = api_base_url();
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("{}/api/subscription/status", api_base))
            .header("Authorization", format!("Bearer {}", token))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        return Ok(body["plan"].as_str() == Some("pro"));
    }

    if api_key.is_empty() || base_url.is_empty() {
        return Ok(false);
    }

    // Validate base_url is a proper HTTP(S) URL
    let parsed = url::Url::parse(&base_url).map_err(|e| format!("Invalid base URL: {e}"))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err("Base URL must use http or https scheme".to_string());
    }

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    Ok(resp.status().is_success())
}

#[tauri::command]
async fn fetch_llm_models(api_key: String, base_url: String) -> Result<Vec<String>, String> {
    if base_url.is_empty() {
        return Ok(vec![]);
    }

    // Validate base_url is a proper HTTP(S) URL
    let parsed = url::Url::parse(&base_url).map_err(|e| format!("Invalid base URL: {e}"))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err("Base URL must use http or https scheme".to_string());
    }

    let client = reqwest::Client::new();
    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Ok(vec![]);
    }

    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;

    // OpenAI-compatible: { data: [{ id: "model-name" }] }
    // Ollama-compatible: { models: [{ name: "model-name" }] }
    let mut models: Vec<String> = Vec::new();

    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        for item in data {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                models.push(id.to_string());
            }
        }
    } else if let Some(data) = body.get("models").and_then(|d| d.as_array()) {
        for item in data {
            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                models.push(name.to_string());
            }
        }
    }

    models.sort();
    Ok(models)
}

#[tauri::command]
async fn bench_stt_connection(
    api_key: String,
    provider: String,
    token_store: tauri::State<'_, SessionTokenStore>,
) -> Result<u32, String> {
    if provider.is_empty() {
        return Err("No provider specified".to_string());
    }

    if provider == "cloud" {
        let token = token_store
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if token.is_empty() {
            return Err("Not signed in".to_string());
        }
        let api_base = api_base_url();
        let client = reqwest::Client::new();
        let t0 = std::time::Instant::now();
        let resp = client
            .get(format!("{}/api/subscription/status", api_base))
            .header("Authorization", format!("Bearer {}", token))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let elapsed = t0.elapsed().as_millis() as u32;
        if !resp.status().is_success() {
            return Err("Request failed".to_string());
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if body["plan"].as_str() != Some("pro") {
            return Err("Pro plan required".to_string());
        }
        return Ok(elapsed);
    }

    if api_key.is_empty() {
        return Err("API key is empty".to_string());
    }

    match provider.as_str() {
        "deepgram" => {
            let client = reqwest::Client::new();
            let t0 = std::time::Instant::now();
            let resp = client
                .get("https://api.deepgram.com/v1/projects")
                .header("Authorization", format!("Token {}", api_key))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let elapsed = t0.elapsed().as_millis() as u32;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            Ok(elapsed)
        }
        "assemblyai" => {
            let client = reqwest::Client::new();
            let t0 = std::time::Instant::now();
            let resp = client
                .get("https://api.assemblyai.com/v2/transcript?limit=1")
                .header("Authorization", api_key)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let elapsed = t0.elapsed().as_millis() as u32;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            Ok(elapsed)
        }
        "qwen-asr" | "dashscope-stream" => {
            let client = reqwest::Client::new();
            let t0 = std::time::Instant::now();
            let resp = client
                .get("https://dashscope.aliyuncs.com/compatible-mode/v1/models")
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let elapsed = t0.elapsed().as_millis() as u32;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let preview: String = body.chars().take(200).collect();
                return Err(format!("HTTP {}: {}", status, preview));
            }
            Ok(elapsed)
        }
        "glm-asr" | "openai-whisper" | "groq-whisper" | "siliconflow" => {
            let (endpoint, model, extra_fields): (&str, &str, &[(&str, &str)]) =
                match provider.as_str() {
                    "glm-asr" => (
                        "https://open.bigmodel.cn/api/paas/v4/audio/transcriptions",
                        "glm-asr-2512",
                        &[("stream", "false")][..],
                    ),
                    "openai-whisper" => (
                        "https://api.openai.com/v1/audio/transcriptions",
                        "whisper-1",
                        &[][..],
                    ),
                    "groq-whisper" => (
                        "https://api.groq.com/openai/v1/audio/transcriptions",
                        "whisper-large-v3-turbo",
                        &[][..],
                    ),
                    _ => (
                        "https://api.siliconflow.cn/v1/audio/transcriptions",
                        "FunAudioLLM/SenseVoiceSmall",
                        &[][..],
                    ),
                };

            let silent_pcm = vec![0u8; 3200]; // 0.1s at 16kHz 16-bit mono
            let wav = stt::whisper_compat::WhisperCompatProvider::build_wav(&silent_pcm, 16000);

            let file_part = reqwest::multipart::Part::bytes(wav)
                .file_name("test.wav")
                .mime_str("audio/wav")
                .map_err(|e| e.to_string())?;
            let mut form = reqwest::multipart::Form::new()
                .text("model", model.to_string())
                .part("file", file_part);
            for &(key, value) in extra_fields {
                form = form.text(key.to_string(), value.to_string());
            }

            let client = reqwest::Client::new();
            let t0 = std::time::Instant::now();
            let resp = client
                .post(endpoint)
                .header("Authorization", format!("Bearer {}", api_key))
                .multipart(form)
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let elapsed = t0.elapsed().as_millis() as u32;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            Ok(elapsed)
        }
        _ => Err(format!("Unknown STT provider: {}", provider)),
    }
}

#[tauri::command]
async fn bench_llm_connection(
    api_key: String,
    provider: String,
    base_url: String,
    model: String,
    token_store: tauri::State<'_, SessionTokenStore>,
) -> Result<u32, String> {
    if provider.is_empty() {
        return Err("No provider specified".to_string());
    }

    if provider == "cloud" {
        let token = token_store
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if token.is_empty() {
            return Err("Not signed in".to_string());
        }
        let api_base = api_base_url();
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false
        });
        let t0 = std::time::Instant::now();
        let resp = client
            .post(format!("{}/api/proxy/llm", api_base))
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let elapsed = t0.elapsed().as_millis() as u32;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        return Ok(elapsed);
    }

    if api_key.is_empty() || base_url.is_empty() {
        return Err("API key or base URL is empty".to_string());
    }

    let parsed = url::Url::parse(&base_url).map_err(|e| format!("Invalid base URL: {e}"))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err("Base URL must use http or https scheme".to_string());
    }

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 1
    });

    let t0 = std::time::Instant::now();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let elapsed = t0.elapsed().as_millis() as u32;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    Ok(elapsed)
}

#[tauri::command]
async fn get_history(
    state: tauri::State<'_, storage::HistoryStore>,
    limit: u32,
    offset: u32,
) -> Result<Vec<storage::HistoryEntry>, String> {
    state.list(limit, offset).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn clear_history(state: tauri::State<'_, storage::HistoryStore>) -> Result<(), String> {
    state.clear().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_history_entry(
    state: tauri::State<'_, storage::HistoryStore>,
    id: i64,
) -> Result<(), String> {
    state.remove(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_dictionary(
    state: tauri::State<'_, storage::DictionaryStore>,
) -> Result<Vec<storage::DictionaryEntry>, String> {
    state.list().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_dictionary_entry(
    state: tauri::State<'_, storage::DictionaryStore>,
    word: String,
    pronunciation: Option<String>,
) -> Result<(), String> {
    let word = word.trim().to_string();
    if word.is_empty() {
        return Err("Word cannot be empty".to_string());
    }
    if word.len() > 100 {
        return Err("Word is too long (max 100 characters)".to_string());
    }
    if let Some(ref p) = pronunciation {
        if p.len() > 100 {
            return Err("Pronunciation is too long (max 100 characters)".to_string());
        }
    }
    state
        .add(&word, pronunciation.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_dictionary_entry(
    state: tauri::State<'_, storage::DictionaryStore>,
    id: i64,
) -> Result<(), String> {
    state.remove(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_session_token(
    state: tauri::State<'_, SessionTokenStore>,
    token: String,
) -> Result<(), String> {
    *state.0.lock().unwrap_or_else(|e| e.into_inner()) = token;
    Ok(())
}

#[tauri::command]
async fn set_auto_start(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, storage::ConfigManager>,
    enabled: bool,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(|e| e.to_string())?;
    } else {
        autolaunch.disable().map_err(|e| e.to_string())?;
    }
    let mut config = config_state.load().await.map_err(|e| e.to_string())?;
    config.auto_start = enabled;
    config_state
        .save(&config)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Re-register both the recording hotkey and the translate-toggle hotkey from
/// the supplied config. Unregisters everything first to ensure a clean slate.
/// Also updates `TranslateHotkeyCache` so the dispatcher can recognise the
/// translate press.
fn reregister_all_hotkeys(
    app: &tauri::AppHandle,
    config: &storage::AppConfig,
) -> Result<(), String> {
    app.global_shortcut()
        .unregister_all()
        .map_err(|e| e.to_string())?;

    let recording = parse_hotkey(&config.hotkey).unwrap_or_else(default_shortcut);
    if let Err(e) = app.global_shortcut().register(recording) {
        tracing::warn!(
            "Failed to register recording hotkey '{}' (may be occupied): {e}",
            config.hotkey
        );
    }

    let translate = if config.translate_hotkey.trim().is_empty() {
        None
    } else {
        parse_hotkey(&config.translate_hotkey)
    };
    let translate_signature = translate.as_ref().map(|s| (s.mods, s.key));
    if let Some(t) = translate {
        if let Err(e) = app.global_shortcut().register(t) {
            tracing::warn!(
                "Failed to register translate hotkey '{}' (may be occupied): {e}",
                config.translate_hotkey
            );
        }
    }
    if let Some(cache) = app.try_state::<TranslateHotkeyCache>() {
        *cache.0.lock().unwrap_or_else(|e| e.into_inner()) = translate_signature;
    }

    let agent = if config.agent_hotkey.trim().is_empty() {
        None
    } else {
        parse_hotkey(&config.agent_hotkey)
    };
    let agent_signature = agent.as_ref().map(|s| (s.mods, s.key));
    if let Some(a) = agent {
        if let Err(e) = app.global_shortcut().register(a) {
            tracing::warn!(
                "Failed to register agent hotkey '{}' (may be occupied): {e}",
                config.agent_hotkey
            );
        }
    }
    if let Some(cache) = app.try_state::<AgentHotkeyCache>() {
        *cache.0.lock().unwrap_or_else(|e| e.into_inner()) = agent_signature;
    }
    Ok(())
}

#[tauri::command]
async fn update_hotkey(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, storage::ConfigManager>,
    hotkey: String,
) -> Result<(), String> {
    // Validate before mutating state — empty string is allowed (means "no binding").
    if !hotkey.trim().is_empty() && parse_hotkey(&hotkey).is_none() {
        return Err(format!("Invalid hotkey: {}", hotkey));
    }

    let mut config = config_state.load().await.map_err(|e| e.to_string())?;
    config.hotkey = hotkey;
    config_state
        .save(&config)
        .await
        .map_err(|e| e.to_string())?;
    reregister_all_hotkeys(&app, &config)?;
    Ok(())
}

#[tauri::command]
async fn update_translate_hotkey(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, storage::ConfigManager>,
    hotkey: String,
) -> Result<(), String> {
    // Empty string is valid → disables the translate hotkey entirely.
    if !hotkey.trim().is_empty() && parse_hotkey(&hotkey).is_none() {
        return Err(format!("Invalid hotkey: {}", hotkey));
    }

    let mut config = config_state.load().await.map_err(|e| e.to_string())?;
    config.translate_hotkey = hotkey;
    config_state
        .save(&config)
        .await
        .map_err(|e| e.to_string())?;
    reregister_all_hotkeys(&app, &config)?;
    Ok(())
}

#[tauri::command]
async fn update_agent_hotkey(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, storage::ConfigManager>,
    hotkey: String,
) -> Result<(), String> {
    // Empty string is valid → disables the agent hotkey entirely.
    if !hotkey.trim().is_empty() && parse_hotkey(&hotkey).is_none() {
        return Err(format!("Invalid hotkey: {}", hotkey));
    }

    let mut config = config_state.load().await.map_err(|e| e.to_string())?;
    config.agent_hotkey = hotkey;
    config_state
        .save(&config)
        .await
        .map_err(|e| e.to_string())?;
    reregister_all_hotkeys(&app, &config)?;
    Ok(())
}

/// Temporarily unregister all global shortcuts so the webview can capture key events.
#[tauri::command]
fn pause_hotkey(app: tauri::AppHandle) -> Result<(), String> {
    app.global_shortcut()
        .unregister_all()
        .map_err(|e| e.to_string())
}

/// Re-register both the recording hotkey and the translate-toggle hotkey from
/// config after the in-app recorder is done capturing keys. Critical: this
/// must register BOTH bindings, otherwise cancelling the translate-hotkey
/// recorder would silently drop the translate binding until app restart.
#[tauri::command]
async fn resume_hotkey(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, storage::ConfigManager>,
) -> Result<(), String> {
    let config = config_state.load().await.map_err(|e| e.to_string())?;
    reregister_all_hotkeys(&app, &config)
}

#[tauri::command]
async fn update_mouse_triggers(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, storage::ConfigManager>,
    enabled: bool,
    middle_click_action: String,
    middle_double_click_action: String,
    middle_right_action: String,
    left_middle_action: String,
) -> Result<(), String> {
    let mut cfg = config_state.load().await.map_err(|e| e.to_string())?;
    cfg.mouse_triggers_enabled = enabled;
    cfg.mouse_middle_click_action = middle_click_action.clone();
    cfg.mouse_middle_double_click_action = middle_double_click_action.clone();
    cfg.mouse_middle_right_action = middle_right_action.clone();
    cfg.mouse_left_middle_action = left_middle_action.clone();
    config_state.save(&cfg).await.map_err(|e| e.to_string())?;

    // Acquire all five caches simultaneously so a concurrent dispatch_mouse_action
    // cannot observe a partially-updated set (e.g. enabled=true but stale action).
    if let (Some(c_en), Some(c_mc), Some(c_mdc), Some(c_mr), Some(c_lm)) = (
        app.try_state::<MouseTriggersEnabledCache>(),
        app.try_state::<MouseMiddleClickActionCache>(),
        app.try_state::<MouseMiddleDoubleClickActionCache>(),
        app.try_state::<MouseMiddleRightActionCache>(),
        app.try_state::<MouseLeftMiddleActionCache>(),
    ) {
        let mut g_en = c_en.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut g_mc = c_mc.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut g_mdc = c_mdc.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut g_mr = c_mr.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut g_lm = c_lm.0.lock().unwrap_or_else(|e| e.into_inner());
        *g_en = enabled;
        *g_mc = middle_click_action;
        *g_mdc = middle_double_click_action;
        *g_mr = middle_right_action;
        *g_lm = left_middle_action;
    }
    Ok(())
}

// ─── Hotkey parsing ───

fn default_shortcut() -> Shortcut {
    let default_hotkey = storage::AppConfig::default().hotkey;
    let fallback = {
        #[cfg(target_os = "macos")]
        {
            Shortcut::new(Some(Modifiers::ALT), Code::Slash)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Shortcut::new(Some(Modifiers::CONTROL), Code::Slash)
        }
    };
    parse_hotkey(&default_hotkey).unwrap_or(fallback)
}

/// Dispatch an action triggered by a mouse gesture. Reads the action string
/// from the cached config and calls the appropriate pipeline method.
async fn dispatch_mouse_action(gesture: String, app_handle: tauri::AppHandle) {
    let enabled = match app_handle.try_state::<MouseTriggersEnabledCache>() {
        Some(c) => *c.0.lock().unwrap_or_else(|e| e.into_inner()),
        None => {
            tracing::warn!("mouse gesture '{}' dropped: trigger cache missing", gesture);
            return;
        }
    };
    tracing::info!(
        "mouse gesture '{}' received (triggers_enabled={})",
        gesture,
        enabled
    );
    if !enabled {
        return;
    }

    let action = match gesture.as_str() {
        "middle_single" => match app_handle.try_state::<MouseMiddleClickActionCache>() {
            Some(c) => c.0.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => return,
        },
        "middle_double" => match app_handle.try_state::<MouseMiddleDoubleClickActionCache>() {
            Some(c) => c.0.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => return,
        },
        "middle_right" => match app_handle.try_state::<MouseMiddleRightActionCache>() {
            Some(c) => c.0.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => return,
        },
        "left_middle" => match app_handle.try_state::<MouseLeftMiddleActionCache>() {
            Some(c) => c.0.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => return,
        },
        _ => return,
    };

    match action.as_str() {
        "recording" => {
            let pipeline = app_handle.state::<pipeline::PipelineHandle>();
            if pipeline.current_state() == pipeline::PipelineState::Idle {
                if let Err(e) = pipeline.start().await {
                    tracing::error!("Mouse trigger: start recording failed: {}", e);
                    let _ = app_handle.emit("pipeline:error", e.to_string());
                }
            } else if let Err(e) = pipeline.stop().await {
                tracing::error!("Mouse trigger: stop recording failed: {}", e);
                let _ = app_handle.emit("pipeline:error", e.to_string());
            }
        }
        "translate" => {
            let config_state = app_handle.state::<storage::ConfigManager>();
            let mut cfg = match config_state.load().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Mouse trigger: failed to load config: {}", e);
                    return;
                }
            };
            cfg.translate_enabled = !cfg.translate_enabled;
            let new_state = cfg.translate_enabled;
            let target_lang = cfg.target_lang.clone();
            if let Err(e) = config_state.save(&cfg).await {
                tracing::error!("Mouse trigger: failed to save config: {}", e);
                return;
            }
            let _ = app_handle.emit(
                "translate:toggled",
                serde_json::json!({ "enabled": new_state, "target_lang": target_lang }),
            );
        }
        "agent" => {
            let pipeline = app_handle.state::<pipeline::PipelineHandle>();
            if pipeline.current_state() == pipeline::PipelineState::Idle {
                if let Err(e) = pipeline.start_for_agent().await {
                    tracing::error!("Mouse trigger: agent start failed: {}", e);
                    let _ = app_handle.emit("pipeline:error", e.to_string());
                }
            } else if let Err(e) = pipeline.stop().await {
                tracing::error!("Mouse trigger: agent stop failed: {}", e);
                let _ = app_handle.emit("pipeline:error", e.to_string());
            }
        }
        "confirm" => {
            // Simulate a Return key press+release at the HID level so the
            // frontmost app receives it as if the user pressed Enter.
            #[cfg(target_os = "macos")]
            unsafe {
                const K_VK_RETURN: u16 = 36;
                let ev_down = CGEventCreateKeyboardEvent(std::ptr::null(), K_VK_RETURN, true);
                if !ev_down.is_null() {
                    CGEventPost(0, ev_down); // kCGHIDEventTap
                    CFRelease(ev_down);
                }
                let ev_up = CGEventCreateKeyboardEvent(std::ptr::null(), K_VK_RETURN, false);
                if !ev_up.is_null() {
                    CGEventPost(0, ev_up);
                    CFRelease(ev_up);
                }
            }
            #[cfg(target_os = "windows")]
            {
                use enigo::{Direction, Enigo, Key, Keyboard, Settings};
                if let Ok(mut enigo) = Enigo::new(&Settings::default()) {
                    let _ = enigo.key(Key::Return, Direction::Click);
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                let _ = rdev::simulate(&rdev::EventType::KeyPress(rdev::Key::Return));
                let _ = rdev::simulate(&rdev::EventType::KeyRelease(rdev::Key::Return));
            }
        }
        _ => {}
    }
}

/// State machine that processes raw mouse events and dispatches gesture actions.
/// Runs in a dedicated tokio task fed by a platform-specific listener.
async fn mouse_event_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<MouseRawEvent>,
    app_handle: tauri::AppHandle,
) {
    let double_click_window = tokio::time::Duration::from_millis(300);

    let mut left_down = false;
    let mut middle_down = false;
    let mut right_down = false;
    let mut click_count: u8 = 0;
    let mut chord_dispatched = false; // prevent a chord from also counting as a click
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        let timeout_fut = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            biased;

            maybe_event = rx.recv() => {
                let event = match maybe_event {
                    Some(e) => e,
                    None => break, // channel closed
                };
                match event {
                    MouseRawEvent::LeftDown => {
                        left_down = true;
                        if middle_down {
                            // Left+Middle chord — cancel pending click, fire immediately
                            click_count = 0;
                            deadline = None;
                            chord_dispatched = true;
                            let h = app_handle.clone();
                            tauri::async_runtime::spawn(dispatch_mouse_action("left_middle".to_owned(), h));
                        }
                    }
                    MouseRawEvent::LeftUp => {
                        left_down = false;
                    }
                    MouseRawEvent::MiddleDown => {
                        middle_down = true;
                        if right_down {
                            // Middle+Right chord
                            click_count = 0;
                            deadline = None;
                            chord_dispatched = true;
                            let h = app_handle.clone();
                            tauri::async_runtime::spawn(dispatch_mouse_action("middle_right".to_owned(), h));
                        } else if left_down {
                            // Left+Middle chord
                            click_count = 0;
                            deadline = None;
                            chord_dispatched = true;
                            let h = app_handle.clone();
                            tauri::async_runtime::spawn(dispatch_mouse_action("left_middle".to_owned(), h));
                        }
                    }
                    MouseRawEvent::MiddleUp => {
                        middle_down = false;
                        if chord_dispatched {
                            chord_dispatched = false;
                            continue;
                        }
                        click_count += 1;
                        deadline = Some(tokio::time::Instant::now() + double_click_window);
                    }
                    MouseRawEvent::RightDown => {
                        right_down = true;
                        if middle_down {
                            click_count = 0;
                            deadline = None;
                            chord_dispatched = true;
                            let h = app_handle.clone();
                            tauri::async_runtime::spawn(dispatch_mouse_action("middle_right".to_owned(), h));
                        }
                    }
                    MouseRawEvent::RightUp => {
                        right_down = false;
                    }
                }
            }

            _ = timeout_fut, if deadline.is_some() => {
                let gesture = if click_count >= 2 { "middle_double" } else { "middle_single" };
                let h = app_handle.clone();
                tauri::async_runtime::spawn(dispatch_mouse_action(gesture.to_owned(), h));
                click_count = 0;
                deadline = None;
            }
        }
    }
}

/// macOS: drive the global mouse triggers by polling physical mouse-button
/// state (CGEventSourceButtonState) at ~125 Hz. Unlike a CGEventTap this needs
/// no Accessibility grant and there is no tap for the OS to disable — which is
/// what made the previous tap-based listener die a few seconds after launch.
#[cfg(target_os = "macos")]
fn start_mouse_listener(app_handle: tauri::AppHandle) {
    use std::sync::OnceLock;

    static SENDER: OnceLock<tokio::sync::mpsc::UnboundedSender<MouseRawEvent>> = OnceLock::new();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MouseRawEvent>();
    if SENDER.set(tx).is_err() {
        // rx is dropped here — the existing event loop keeps running with the
        // original app_handle. Calling this function more than once is a bug:
        // the new app_handle is silently discarded.
        tracing::error!(
            "start_mouse_listener called more than once; new app_handle discarded. \
             Mouse triggers remain bound to the original handle."
        );
        return;
    }

    // Channel to report tap creation success/failure back to the tokio task.
    let (tap_tx, tap_rx) = std::sync::mpsc::channel::<bool>();

    std::thread::spawn(move || unsafe {
        // Poll physical mouse-button state instead of installing a CGEventTap.
        //
        // A global tap at kCGHIDEventTap level requires Accessibility trust, and
        // on this Apple-Development-signed build macOS revokes that trust a few
        // seconds after a Finder launch — silently disabling the tap (the disable
        // event is never even delivered to the callback) with no way to revive
        // it (re-enabling from the callback, a runloop timer, and a watchdog
        // thread were all verified to fail). CGEventSourceButtonState merely
        // *reads* the current button state: it installs no tap, so there is
        // nothing for the OS to disable, and for mouse buttons it needs no
        // Accessibility grant. We sample at ~125 Hz and feed edge transitions
        // into the same channel the gesture state machine already consumes.
        //
        // (Typing the result into other apps still needs Accessibility — that is
        // surfaced separately by the pipeline, not by this listener.)
        const SOURCE_STATE: u32 = 0; // kCGEventSourceStateCombinedSessionState
                                     // CGMouseButton: left = 0, right = 1, center (middle) = 2
        let (mut left, mut middle, mut right) = (false, false, false);
        tracing::info!("Mouse trigger poller started (CGEventSourceButtonState)");
        // No tap, no permission gate — the poller is always live.
        let _ = tap_tx.send(true);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(8));
            let (nl, nm, nr) = (
                CGEventSourceButtonState(SOURCE_STATE, 0),
                CGEventSourceButtonState(SOURCE_STATE, 2),
                CGEventSourceButtonState(SOURCE_STATE, 1),
            );
            let tx = match SENDER.get() {
                Some(t) => t,
                None => continue,
            };
            // Emit left/right edges before middle so chord detection in
            // mouse_event_loop sees the modifier button already pressed.
            if nl && !left {
                let _ = tx.send(MouseRawEvent::LeftDown);
            }
            if nr && !right {
                let _ = tx.send(MouseRawEvent::RightDown);
            }
            if nm && !middle {
                let _ = tx.send(MouseRawEvent::MiddleDown);
            }
            if !nm && middle {
                let _ = tx.send(MouseRawEvent::MiddleUp);
            }
            if !nl && left {
                let _ = tx.send(MouseRawEvent::LeftUp);
            }
            if !nr && right {
                let _ = tx.send(MouseRawEvent::RightUp);
            }
            left = nl;
            middle = nm;
            right = nr;
        }
    });

    tauri::async_runtime::spawn(async move {
        // Give the background thread up to 2s to report tap status, then emit to frontend.
        let tap_ok = tokio::task::spawn_blocking(move || {
            tap_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);

        let _ = app_handle.emit("mouse:tap_active", tap_ok);
        if !tap_ok {
            tracing::warn!("Mouse tap inactive — emitting warning to frontend");
        }

        mouse_event_loop(rx, app_handle).await;
    });
}

/// Windows: poll mouse button state via GetAsyncKeyState at ~125 Hz.
/// Unlike rdev::listen which installs both WH_KEYBOARD_LL and WH_MOUSE_LL
/// hooks, polling avoids installing any keyboard hooks entirely — the keyboard
/// hook conflicts with Windows IME (Input Method Editors), causing crashes and
/// garbled input when Chinese/Japanese/Korean IMEs are active.
#[cfg(target_os = "windows")]
fn start_mouse_listener(app_handle: tauri::AppHandle) {
    use std::sync::OnceLock;

    static SENDER: OnceLock<tokio::sync::mpsc::UnboundedSender<MouseRawEvent>> = OnceLock::new();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MouseRawEvent>();
    if SENDER.set(tx).is_err() {
        tracing::error!("start_mouse_listener called more than once; new app_handle discarded.");
        return;
    }

    let (tap_tx, tap_rx) = std::sync::mpsc::channel::<bool>();

    std::thread::spawn(move || {
        const VK_LBUTTON: i32 = 0x01;
        const VK_RBUTTON: i32 = 0x02;
        const VK_MBUTTON: i32 = 0x04;
        let (mut left, mut middle, mut right) = (false, false, false);
        tracing::info!("Mouse trigger poller started (GetAsyncKeyState)");
        let _ = tap_tx.send(true);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(8));
            let (nl, nm, nr) = unsafe {
                (
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(VK_LBUTTON)
                        < 0,
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(VK_MBUTTON)
                        < 0,
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(VK_RBUTTON)
                        < 0,
                )
            };
            let tx = match SENDER.get() {
                Some(t) => t,
                None => continue,
            };
            if nl && !left {
                let _ = tx.send(MouseRawEvent::LeftDown);
            }
            if nr && !right {
                let _ = tx.send(MouseRawEvent::RightDown);
            }
            if nm && !middle {
                let _ = tx.send(MouseRawEvent::MiddleDown);
            }
            if !nm && middle {
                let _ = tx.send(MouseRawEvent::MiddleUp);
            }
            if !nl && left {
                let _ = tx.send(MouseRawEvent::LeftUp);
            }
            if !nr && right {
                let _ = tx.send(MouseRawEvent::RightUp);
            }
            left = nl;
            middle = nm;
            right = nr;
        }
    });

    tauri::async_runtime::spawn(async move {
        let tap_ok = tokio::task::spawn_blocking(move || {
            tap_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);

        let _ = app_handle.emit("mouse:tap_active", tap_ok);
        if !tap_ok {
            tracing::warn!("Mouse tap inactive — emitting warning to frontend");
        }

        mouse_event_loop(rx, app_handle).await;
    });
}

/// Linux: use rdev for global mouse event listening.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn start_mouse_listener(app_handle: tauri::AppHandle) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<MouseRawEvent>();
    let (tap_tx, tap_rx) = std::sync::mpsc::channel::<bool>();

    std::thread::spawn(move || {
        if let Err(e) = rdev::listen(move |event| {
            let raw = match event.event_type {
                rdev::EventType::ButtonPress(rdev::Button::Left) => Some(MouseRawEvent::LeftDown),
                rdev::EventType::ButtonRelease(rdev::Button::Left) => Some(MouseRawEvent::LeftUp),
                rdev::EventType::ButtonPress(rdev::Button::Middle) => {
                    Some(MouseRawEvent::MiddleDown)
                }
                rdev::EventType::ButtonRelease(rdev::Button::Middle) => {
                    Some(MouseRawEvent::MiddleUp)
                }
                rdev::EventType::ButtonPress(rdev::Button::Right) => Some(MouseRawEvent::RightDown),
                rdev::EventType::ButtonRelease(rdev::Button::Right) => Some(MouseRawEvent::RightUp),
                _ => None,
            };
            if let Some(ev) = raw {
                let _ = tx.send(ev);
            }
        }) {
            tracing::error!("rdev mouse listener error: {:?}", e);
            let _ = tap_tx.send(false);
        }
    });

    tauri::async_runtime::spawn(async move {
        let tap_ok = tokio::task::spawn_blocking(move || {
            tap_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap_or(true)
        })
        .await
        .unwrap_or(true);

        let _ = app_handle.emit("mouse:tap_active", tap_ok);
        if !tap_ok {
            tracing::warn!("Mouse tap inactive — emitting warning to frontend");
        }

        mouse_event_loop(rx, app_handle).await;
    });
}

fn build_shortcut_handler(
    app_handle: tauri::AppHandle,
) -> impl Fn(&tauri::AppHandle, &Shortcut, tauri_plugin_global_shortcut::ShortcutEvent)
       + Send
       + Sync
       + 'static {
    move |_app, shortcut, event| {
        let handle = app_handle.clone();

        // Check whether this press is the translate-toggle hotkey. If so,
        // flip `translate_enabled` in config and notify the frontend — never
        // touch the recording pipeline. Translate hotkey reacts to Pressed
        // only (a toggle, like Caps Lock).
        //
        // Compare on (mods, key) — NOT on `Shortcut` equality, because that
        // compares unique instance IDs which always differ between the cached
        // copy and the one delivered by the global-hotkey plugin's callback.
        let is_translate_shortcut = handle
            .try_state::<TranslateHotkeyCache>()
            .and_then(|cache| {
                cache
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|(mods, key)| *mods == shortcut.mods && *key == shortcut.key)
            })
            .unwrap_or(false);

        if is_translate_shortcut {
            if event.state == ShortcutState::Pressed {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let config_state = handle.state::<storage::ConfigManager>();
                    let mut cfg = match config_state.load().await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::error!("Translate toggle: failed to load config: {}", e);
                            return;
                        }
                    };
                    cfg.translate_enabled = !cfg.translate_enabled;
                    let new_state = cfg.translate_enabled;
                    let target_lang = cfg.target_lang.clone();
                    if let Err(e) = config_state.save(&cfg).await {
                        tracing::error!("Translate toggle: failed to save config: {}", e);
                        return;
                    }
                    tracing::info!(
                        "Translate toggled via hotkey: enabled={}, target_lang={}",
                        new_state,
                        target_lang
                    );
                    // Emit so frontend can show a toast + refresh its mirror of config.
                    let _ = handle.emit(
                        "translate:toggled",
                        serde_json::json!({
                            "enabled": new_state,
                            "target_lang": target_lang,
                        }),
                    );
                });
            }
            return;
        }

        // Agent hotkey: start a forced-agent recording (Idle→Recording) or
        // stop an in-flight one (Recording→Transcribing). Same toggle semantics
        // as the recording hotkey when hotkey_mode is "toggle", but always uses
        // `start_for_agent()` so stop() routes through Hermes regardless of
        // STT trigger-word recognition. Only Pressed matters here.
        let is_agent_shortcut = handle
            .try_state::<AgentHotkeyCache>()
            .and_then(|cache| {
                cache
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|(mods, key)| *mods == shortcut.mods && *key == shortcut.key)
            })
            .unwrap_or(false);
        if is_agent_shortcut {
            if event.state == ShortcutState::Pressed {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let pipeline = handle.state::<pipeline::PipelineHandle>();
                    if pipeline.current_state() == pipeline::PipelineState::Idle {
                        tracing::info!("Agent hotkey: starting forced-agent recording");
                        if let Err(e) = pipeline.start_for_agent().await {
                            tracing::error!("Agent start failed: {}", e);
                            let _ = handle.emit("pipeline:error", e.to_string());
                        }
                    } else if let Err(e) = pipeline.stop().await {
                        tracing::error!("Agent stop failed: {}", e);
                        let _ = handle.emit("pipeline:error", e.to_string());
                    }
                });
            }
            return;
        }

        // Otherwise this is the recording shortcut — original behavior.
        match event.state {
            ShortcutState::Pressed => {
                let hotkey_mode = handle
                    .state::<HotkeyModeCache>()
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                tauri::async_runtime::spawn(async move {
                    let pipeline = handle.state::<pipeline::PipelineHandle>();

                    if hotkey_mode == "toggle" {
                        if pipeline.current_state() == pipeline::PipelineState::Idle {
                            if let Err(e) = pipeline.start().await {
                                tracing::error!("Failed to start recording: {}", e);
                                let _ = handle.emit("pipeline:error", e.to_string());
                            }
                        } else if let Err(e) = pipeline.stop().await {
                            tracing::error!("Failed to stop recording: {}", e);
                            let _ = handle.emit("pipeline:error", e.to_string());
                        }
                    } else if let Err(e) = pipeline.start().await {
                        tracing::error!("Failed to start recording: {}", e);
                        let _ = handle.emit("pipeline:error", e.to_string());
                    }
                });
            }
            ShortcutState::Released => {
                let hotkey_mode = handle
                    .state::<HotkeyModeCache>()
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if hotkey_mode != "toggle" {
                    tauri::async_runtime::spawn(async move {
                        let pipeline = handle.state::<pipeline::PipelineHandle>();
                        if let Err(e) = pipeline.stop().await {
                            tracing::error!("Failed to stop recording: {}", e);
                            let _ = handle.emit("pipeline:error", e.to_string());
                        }
                    });
                }
            }
        }
    }
}

fn parse_hotkey(s: &str) -> Option<Shortcut> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return None;
    }

    let mut modifiers = Modifiers::empty();
    let key_str = parts.last()?;

    for &part in &parts[..parts.len() - 1] {
        match part.to_lowercase().as_str() {
            "alt" => modifiers |= Modifiers::ALT,
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "meta" | "super" | "win" | "cmd" => modifiers |= Modifiers::META,
            _ => return None,
        }
    }

    let code = match key_str.to_lowercase().as_str() {
        "space" => Code::Space,
        "tab" => Code::Tab,
        "enter" | "return" => Code::Enter,
        "backspace" => Code::Backspace,
        "escape" | "esc" => Code::Escape,
        "delete" => Code::Delete,
        "insert" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "arrowup" | "up" => Code::ArrowUp,
        "arrowdown" | "down" => Code::ArrowDown,
        "arrowleft" | "left" => Code::ArrowLeft,
        "arrowright" | "right" => Code::ArrowRight,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "/" | "slash" => Code::Slash,
        "\\" | "backslash" => Code::Backslash,
        "." | "period" => Code::Period,
        "," | "comma" => Code::Comma,
        ";" | "semicolon" => Code::Semicolon,
        "'" | "quote" => Code::Quote,
        "`" | "backquote" => Code::Backquote,
        "-" | "minus" => Code::Minus,
        "=" | "equal" => Code::Equal,
        "[" | "bracketleft" => Code::BracketLeft,
        "]" | "bracketright" => Code::BracketRight,
        _ => return None,
    };

    let mods = if modifiers.is_empty() {
        None
    } else {
        Some(modifiers)
    };
    Some(Shortcut::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hotkey_ctrl_slash() {
        let s = parse_hotkey("Ctrl+/");
        assert!(s.is_some());
        let s = s.unwrap();
        assert_eq!(s.mods, Modifiers::CONTROL);
        assert_eq!(s.key, Code::Slash);
    }

    #[test]
    fn test_parse_hotkey_ctrl_shift_a() {
        let s = parse_hotkey("Ctrl+Shift+A");
        assert!(s.is_some());
        let s = s.unwrap();
        assert_eq!(s.mods, Modifiers::CONTROL | Modifiers::SHIFT);
        assert_eq!(s.key, Code::KeyA);
    }

    #[test]
    fn test_parse_hotkey_case_insensitive() {
        let s = parse_hotkey("cTrL+/");
        assert!(s.is_some());
        let s = s.unwrap();
        assert_eq!(s.mods, Modifiers::CONTROL);
        assert_eq!(s.key, Code::Slash);
    }

    #[test]
    fn test_parse_hotkey_f_keys() {
        for (key, expected) in [("F1", Code::F1), ("F12", Code::F12)] {
            let s = parse_hotkey(&format!("Ctrl+{}", key));
            assert!(s.is_some(), "Failed to parse Ctrl+{}", key);
            assert_eq!(s.unwrap().key, expected);
        }
    }

    #[test]
    fn test_parse_hotkey_meta_modifier() {
        for name in ["Meta", "Super", "Win", "Cmd"] {
            let s = parse_hotkey(&format!("{}+A", name));
            assert!(s.is_some(), "Failed to parse {}+A", name);
            assert_eq!(s.unwrap().mods, Modifiers::SUPER);
        }
    }

    #[test]
    fn test_parse_hotkey_no_modifier() {
        let s = parse_hotkey("A");
        assert!(s.is_some());
        assert_eq!(s.unwrap().mods, Modifiers::empty());
    }

    #[test]
    fn test_parse_hotkey_invalid_key() {
        let s = parse_hotkey("Alt+InvalidKey");
        assert!(s.is_none());
    }

    #[test]
    fn test_parse_hotkey_empty_string() {
        let s = parse_hotkey("");
        assert!(s.is_none());
    }

    #[test]
    fn test_parse_hotkey_digits() {
        let s = parse_hotkey("Ctrl+0");
        assert!(s.is_some());
        assert_eq!(s.unwrap().key, Code::Digit0);

        let s = parse_hotkey("Ctrl+9");
        assert!(s.is_some());
        assert_eq!(s.unwrap().key, Code::Digit9);
    }

    #[test]
    fn test_parse_hotkey_navigation_keys() {
        for (key, expected) in [
            ("Enter", Code::Enter),
            ("Tab", Code::Tab),
            ("Escape", Code::Escape),
            ("Backspace", Code::Backspace),
            ("Delete", Code::Delete),
            ("Up", Code::ArrowUp),
            ("Down", Code::ArrowDown),
        ] {
            let s = parse_hotkey(&format!("Alt+{}", key));
            assert!(s.is_some(), "Failed to parse Alt+{}", key);
            assert_eq!(s.unwrap().key, expected);
        }
    }
}
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive(
                "opentypeless=debug"
                    .parse()
                    .expect("static directive is valid"),
            ),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Deep-link URL forwarding is handled automatically by the
            // "deep-link" feature of single-instance plugin.
            // Just focus the main window so the user sees the result.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            // Open devtools only when the "devtools" feature is explicitly enabled
            #[cfg(feature = "devtools")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
                if let Some(window) = app.get_webview_window("capsule") {
                    window.open_devtools();
                }
            }

            let app_handle = app.handle().clone();

            // Keep the recording capsule above normal windows. A single direct
            // Rust call here is more reliable than the static alwaysOnTop config,
            // and — unlike asserting always-on-top from the capsule's JS resize
            // effect — it cannot block the window's show() call (doing so from JS
            // before show() is what stopped the capsule from appearing at all).
            #[cfg(target_os = "macos")]
            if let Some(capsule) = app.get_webview_window("capsule") {
                let _ = capsule.set_always_on_top(true);
            }

            // Initialize data directory and database
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("opentypeless.db");

            // Initialize stores
            let config_manager = storage::ConfigManager::new(app_handle.clone());
            let history_store = storage::HistoryStore::new(db_path.clone())
                .map_err(|e| anyhow::anyhow!("Failed to init history store: {}", e))?;
            let dictionary_store = storage::DictionaryStore::new(db_path)
                .map_err(|e| anyhow::anyhow!("Failed to init dictionary store: {}", e))?;
            let pipeline_handle = pipeline::PipelineHandle::new(app_handle.clone());

            // Load initial config to get hotkey
            let initial_config =
                tauri::async_runtime::block_on(config_manager.load()).unwrap_or_default();
            let shortcut = parse_hotkey(&initial_config.hotkey).unwrap_or_else(default_shortcut);
            let translate_shortcut = if initial_config.translate_hotkey.trim().is_empty() {
                None
            } else {
                parse_hotkey(&initial_config.translate_hotkey)
            };
            let translate_signature = translate_shortcut.as_ref().map(|s| (s.mods, s.key));
            let agent_shortcut = if initial_config.agent_hotkey.trim().is_empty() {
                None
            } else {
                parse_hotkey(&initial_config.agent_hotkey)
            };
            let agent_signature = agent_shortcut.as_ref().map(|s| (s.mods, s.key));

            app.manage(config_manager);
            app.manage(history_store);
            app.manage(dictionary_store);
            app.manage(pipeline_handle);

            // Lightweight update reminder: a few seconds after launch, ask GitHub
            // whether a newer release exists and, if so, notify the user. Non-
            // blocking and silent on any failure — never downloads or installs.
            {
                let update_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    crate::update::check_for_update(update_app, reqwest::Client::new()).await;
                });
            }

            app.manage(HotkeyModeCache(Arc::new(Mutex::new(
                initial_config.hotkey_mode.clone(),
            ))));
            app.manage(CloseToTrayCache(Arc::new(Mutex::new(
                initial_config.close_to_tray,
            ))));
            app.manage(TranslateHotkeyCache(Arc::new(Mutex::new(
                translate_signature,
            ))));
            app.manage(AgentHotkeyCache(Arc::new(Mutex::new(agent_signature))));
            app.manage(SessionTokenStore(Arc::new(Mutex::new(String::new()))));
            app.manage(MouseTriggersEnabledCache(Arc::new(Mutex::new(
                initial_config.mouse_triggers_enabled,
            ))));
            app.manage(MouseMiddleClickActionCache(Arc::new(Mutex::new(
                initial_config.mouse_middle_click_action.clone(),
            ))));
            app.manage(MouseMiddleDoubleClickActionCache(Arc::new(Mutex::new(
                initial_config.mouse_middle_double_click_action.clone(),
            ))));
            app.manage(MouseMiddleRightActionCache(Arc::new(Mutex::new(
                initial_config.mouse_middle_right_action.clone(),
            ))));
            app.manage(MouseLeftMiddleActionCache(Arc::new(Mutex::new(
                initial_config.mouse_left_middle_action.clone(),
            ))));
            start_mouse_listener(app_handle.clone());

            // Sync auto-start state with system
            {
                use tauri_plugin_autostart::ManagerExt;
                let autolaunch = app.handle().autolaunch();
                let is_enabled = autolaunch.is_enabled().unwrap_or(false);
                if initial_config.auto_start && !is_enabled {
                    let _ = autolaunch.enable();
                } else if !initial_config.auto_start && is_enabled {
                    let _ = autolaunch.disable();
                }
            }

            // Register global shortcut from config
            let handler = build_shortcut_handler(app_handle.clone());
            app.handle().plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(handler)
                    .build(),
            )?;
            if let Err(e) = app.global_shortcut().register(shortcut) {
                tracing::warn!(
                    "Failed to register shortcut '{}' (may be occupied): {e}",
                    initial_config.hotkey
                );
            }
            if let Some(t) = translate_shortcut {
                if let Err(e) = app.global_shortcut().register(t) {
                    tracing::warn!(
                        "Failed to register translate hotkey '{}' (may be occupied): {e}",
                        initial_config.translate_hotkey
                    );
                }
            }
            if let Some(a) = agent_shortcut {
                if let Err(e) = app.global_shortcut().register(a) {
                    tracing::warn!(
                        "Failed to register agent hotkey '{}' (may be occupied): {e}",
                        initial_config.agent_hotkey
                    );
                }
            }

            // System tray
            let tray_menu = build_tray_menu(&app_handle, false, true)
                .map_err(|e| anyhow::anyhow!("Failed to build tray menu: {}", e))?;

            let mut tray_builder = TrayIconBuilder::new();
            // default_window_icon() can be None depending on platform/bundle; fall
            // back instead of .expect() so a missing icon can't crash startup — a
            // GUI-subsystem Windows build would otherwise just silently vanish.
            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            } else {
                tracing::warn!("default window icon missing — tray uses platform default");
            }
            let tray = tray_builder
                .menu(&tray_menu)
                .tooltip("OpenTypeless")
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    "show_hide" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let visible = window.is_visible().unwrap_or(false);
                            if visible {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                            refresh_tray(app);
                        }
                    }
                    "record" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let pipeline = handle.state::<pipeline::PipelineHandle>();
                            if pipeline.current_state() == pipeline::PipelineState::Idle {
                                if let Err(e) = pipeline.start().await {
                                    tracing::error!("Tray start recording failed: {}", e);
                                }
                            } else if pipeline.current_state() == pipeline::PipelineState::Recording
                            {
                                if let Err(e) = pipeline.stop().await {
                                    tracing::error!("Tray stop recording failed: {}", e);
                                }
                            }
                        });
                    }
                    "settings" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit("tray:settings", ());
                            let _ = window.show();
                            let _ = window.set_focus();
                            refresh_tray(app);
                        }
                    }
                    "history" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit("tray:history", ());
                            let _ = window.show();
                            let _ = window.set_focus();
                            refresh_tray(app);
                        }
                    }
                    "account" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit("navigate", "#/account");
                            let _ = window.show();
                            let _ = window.set_focus();
                            refresh_tray(app);
                        }
                    }
                    "about" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.emit("tray:about", ());
                            let _ = window.show();
                            let _ = window.set_focus();
                            refresh_tray(app);
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    let should_show = matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } | TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        }
                    );
                    if should_show {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                            refresh_tray(app);
                        }
                    }
                })
                .build(app)?;

            app.manage(TrayHandle {
                tray: Mutex::new(tray),
            });

            // Close-to-tray: intercept window close
            if let Some(main_window) = app.get_webview_window("main") {
                let handle = app.handle().clone();
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        let close_to_tray = *handle
                            .state::<CloseToTrayCache>()
                            .0
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if close_to_tray {
                            api.prevent_close();
                            // Save window state before hiding (skip if minimized)
                            if let Some(w) = handle.get_webview_window("main") {
                                if let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) {
                                    if pos.x > -1000
                                        && pos.y > -1000
                                        && size.width >= 720
                                        && size.height >= 480
                                    {
                                        let ws = WindowState {
                                            x: pos.x,
                                            y: pos.y,
                                            width: size.width,
                                            height: size.height,
                                        };
                                        if let Ok(store) = handle.store("settings.json") {
                                            if let Ok(val) = serde_json::to_value(&ws) {
                                                store.set("window_state", val);
                                                let _ = store.save();
                                            }
                                        }
                                    }
                                }
                                let _ = w.hide();
                            }
                            refresh_tray(&handle);
                        }
                    }
                });
            }

            // Restore window state from previous session
            if let Ok(store) = app.handle().store("settings.json") {
                if let Some(val) = store.get("window_state") {
                    if let Ok(ws) = serde_json::from_value::<WindowState>(val.clone()) {
                        // Validate: skip if coordinates are off-screen (e.g. -32000 from minimized state)
                        if ws.x > -1000 && ws.y > -1000 && ws.width >= 720 && ws.height >= 480 {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.set_position(tauri::Position::Physical(
                                    tauri::PhysicalPosition::new(ws.x, ws.y),
                                ));
                                let _ = window.set_size(tauri::Size::Physical(
                                    tauri::PhysicalSize::new(ws.width, ws.height),
                                ));
                            }
                        }
                    }
                }
            }

            // Start minimized: only show window if not configured to start minimized
            if !initial_config.start_minimized {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }

            tracing::info!("OpenTypeless started");

            // P1-2: Pre-warm HTTP connection pool in background
            let warm_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                let pipeline = warm_handle.state::<pipeline::PipelineHandle>();
                pipeline.pre_warm().await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            abort_recording,
            list_input_devices,
            test_audio_capture,
            check_accessibility_permission,
            request_accessibility_permission,
            get_config,
            update_config,
            test_stt_connection,
            test_llm_connection,
            test_agent,
            test_agent_route,
            test_hermes_agent,
            test_hermes_route,
            get_last_agent_result,
            bench_stt_connection,
            bench_llm_connection,
            fetch_llm_models,
            get_history,
            clear_history,
            delete_history_entry,
            get_dictionary,
            add_dictionary_entry,
            remove_dictionary_entry,
            update_hotkey,
            update_translate_hotkey,
            update_agent_hotkey,
            update_mouse_triggers,
            pause_hotkey,
            resume_hotkey,
            set_auto_start,
            set_session_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
