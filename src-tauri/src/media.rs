//! macOS local-media pause helper. Called from the pipeline on record start so
//! music / podcasts / video playback don't bleed into the microphone.
//!
//! Approach: shell out to a single osascript that iterates over known apps
//! and tells each one to pause IF it is running and currently playing. We
//! only target apps that expose an AppleScript "player state" + "pause"
//! interface — apps that don't (e.g. QQ Music, browser tabs) are out of
//! scope; documented in INSTALL_macOS.md.
//!
//! No auto-resume on record stop. Symmetric resume is doable (cache the set
//! of paused apps, send `play`) but it's surprising if the user manually
//! paused something before recording and the app then auto-resumes it.

#[cfg(target_os = "macos")]
const PAUSE_SCRIPT: &str = r#"
-- Ask System Events for the names of currently-running processes. We match
-- target apps against THIS list (plain string comparison) instead of using
-- `application "Name" is running`, because referencing an app by name that
-- isn't installed (e.g. Spotify on a machine without it) pops a modal
-- "Where is <app>?" locator dialog. Matching against the live process list
-- never creates a specifier for an absent app, so no dialog.
tell application "System Events" to set runningApps to name of every process
set targetApps to {"Spotify", "Music", "Podcasts", "QuickTime Player", "VLC", "IINA", "TV"}
set paused to {}
repeat with appName in targetApps
    if runningApps contains appName then
        try
            -- `tell application <variable>` can't resolve app-specific terms
            -- (player state / playing / pause) at compile time, which is a hard
            -- parse error (-2741). `using terms from application "Music"` lends
            -- the compiler the iTunes-family vocabulary; events still go to the
            -- dynamic appName. Apps without that vocabulary throw at runtime and
            -- are swallowed by the try.
            using terms from application "Music"
                tell application appName
                    if player state is playing then
                        pause
                        set end of paused to appName
                    end if
                end tell
            end using terms from
        end try
    end if
end repeat
return paused as string
"#;

/// Best-effort: pause any currently-playing local media apps. Returns
/// silently on failure (this is a nice-to-have, never a blocker). Spawns
/// off the calling thread so we don't add latency to record start.
pub fn pause_local_media() {
    #[cfg(target_os = "macos")]
    {
        std::thread::spawn(|| {
            let output = std::process::Command::new("osascript")
                .arg("-e")
                .arg(PAUSE_SCRIPT)
                .output();
            // Did the AppleScript pass pause a scriptable app (Spotify/Music/…)?
            let scripted_pause = match output {
                Ok(out) if out.status.success() => {
                    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !stdout.is_empty() {
                        tracing::info!("Paused media in: {}", stdout);
                        true
                    } else {
                        false
                    }
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    tracing::debug!(
                        "media pause osascript exit {:?}: {}",
                        out.status.code(),
                        stderr.trim()
                    );
                    false
                }
                Err(e) => {
                    tracing::debug!("media pause: osascript spawn failed: {e}");
                    false
                }
            };

            // Only fall back to the system play/pause key when the AppleScript
            // pass paused nothing. If it DID pause a scriptable app, sending the
            // media key would re-toggle that same now-playing app back to play.
            // The fallback covers non-scriptable players (QQ Music, browser
            // tabs).
            //
            // The media key is a BLIND toggle (no play/pause query), so we only
            // send it when the default output device is actually rendering audio
            // right now. Without this guard, pressing it while nothing plays
            // would START playback instead of pausing it.
            if !scripted_pause && audio_output_is_active() {
                unsafe { send_play_pause_key() };
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux / Windows pause is doable via playerctl / SMTC but out of
        // scope right now — primary use case is macOS.
    }
}

/// Is the system default output device currently rendering audio?
///
/// Uses the public CoreAudio property `kAudioDevicePropertyDeviceIsRunningSomewhere`,
/// which is true whenever any process is actively playing through the default
/// output device. We query the OUTPUT device specifically, so our own
/// microphone capture (an INPUT device) never trips it. This is the state
/// check that makes the blind media play/pause key safe to send: if nothing is
/// playing we skip it and avoid accidentally starting playback.
///
/// Returns false on any CoreAudio error — the conservative choice, since the
/// reported bug is "starts music when nothing was playing".
#[cfg(target_os = "macos")]
fn audio_output_is_active() -> bool {
    #[repr(C)]
    struct AudioObjectPropertyAddress {
        m_selector: u32,
        m_scope: u32,
        m_element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            in_object_id: u32,
            in_address: *const AudioObjectPropertyAddress,
            in_qualifier_data_size: u32,
            in_qualifier_data: *const std::ffi::c_void,
            io_data_size: *mut u32,
            out_data: *mut std::ffi::c_void,
        ) -> i32;
    }

    // FourCC constants (big-endian char codes).
    const SYSTEM_OBJECT: u32 = 1; // kAudioObjectSystemObject
    const DEFAULT_OUTPUT_DEVICE: u32 = u32::from_be_bytes(*b"dOut"); // kAudioHardwarePropertyDefaultOutputDevice
    const IS_RUNNING_SOMEWHERE: u32 = u32::from_be_bytes(*b"gone"); // kAudioDevicePropertyDeviceIsRunningSomewhere
    const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob"); // kAudioObjectPropertyScopeGlobal
    const ELEMENT_MAIN: u32 = 0; // kAudioObjectPropertyElementMain

    unsafe {
        // 1. Resolve the default output device id.
        let dev_addr = AudioObjectPropertyAddress {
            m_selector: DEFAULT_OUTPUT_DEVICE,
            m_scope: SCOPE_GLOBAL,
            m_element: ELEMENT_MAIN,
        };
        let mut device_id: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &dev_addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut device_id as *mut u32 as *mut std::ffi::c_void,
        );
        if status != 0 || device_id == 0 {
            tracing::debug!("audio_output_is_active: no default output device (status={status})");
            return false;
        }

        // 2. Is that device running (rendering) somewhere?
        let run_addr = AudioObjectPropertyAddress {
            m_selector: IS_RUNNING_SOMEWHERE,
            m_scope: SCOPE_GLOBAL,
            m_element: ELEMENT_MAIN,
        };
        let mut running: u32 = 0;
        let mut rsize = std::mem::size_of::<u32>() as u32;
        let status = AudioObjectGetPropertyData(
            device_id,
            &run_addr,
            0,
            std::ptr::null(),
            &mut rsize,
            &mut running as *mut u32 as *mut std::ffi::c_void,
        );
        if status != 0 {
            tracing::debug!("audio_output_is_active: IsRunningSomewhere query failed (status={status})");
            return false;
        }
        running != 0
    }
}

/// Synthesise a press+release of the hardware "play/pause" media key.
///
/// macOS routes this key to the current "now playing" app via the system
/// media controls, so it works for players that expose no AppleScript
/// interface (QQ Music, browser tabs, NetEase Music, …) — the apps the
/// AppleScript pass above can't touch. It is delivered as an
/// NSEventTypeSystemDefined event (subtype 8), which is the only documented
/// way to emit a media key; plain CGEvent keyboard events don't carry it.
#[cfg(target_os = "macos")]
unsafe fn send_play_pause_key() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    // CoreGraphics: post a CGEvent at the HID tap (0). The CGEventRef we get
    // from -[NSEvent CGEvent] is owned by the autoreleased NSEvent, so we post
    // it without releasing.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventPost(tap: u32, event: *mut std::ffi::c_void);
    }

    const NX_KEYTYPE_PLAY: isize = 16;
    const NS_EVENT_TYPE_SYSTEM_DEFINED: usize = 14;
    const SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
    // data1 low byte encodes key state: 0xA = down, 0xB = up.
    for &state in &[0x0A_isize, 0x0B_isize] {
        let data1: isize = (NX_KEYTYPE_PLAY << 16) | (state << 8);
        let loc = NSPoint { x: 0.0, y: 0.0 };
        let event: *mut Object = msg_send![class!(NSEvent),
            otherEventWithType: NS_EVENT_TYPE_SYSTEM_DEFINED
            location: loc
            modifierFlags: 0usize
            timestamp: 0.0f64
            windowNumber: 0isize
            context: std::ptr::null_mut::<Object>()
            subtype: SUBTYPE_AUX_CONTROL_BUTTONS
            data1: data1
            data2: -1isize
        ];
        if event.is_null() {
            tracing::debug!("send_play_pause_key: NSEvent creation returned nil");
            continue;
        }
        let cg: *mut std::ffi::c_void = msg_send![event, CGEvent];
        if !cg.is_null() {
            CGEventPost(0, cg);
        }
    }
}
