//! macOS local-media pause helper. Called from the pipeline on record start so
//! music / podcasts / video playback don't bleed into the microphone, and on
//! record stop to resume exactly what we paused.
//!
//! Scope: only apps that expose the iTunes-family AppleScript interface
//! (`player state` + `pause` / `play`) — Spotify, Apple Music, Podcasts, Apple
//! TV, VLC, IINA. Apps without it (QQ Music, NetEase, browser tabs) are NOT
//! controlled: the only lever for them is the system play/pause media key,
//! which is a blind toggle that proved unreliable (it would start playback
//! when nothing was playing, and fights the Bluetooth HFP mode switch on
//! AirPods). The robust answer for those is to record with the built-in
//! microphone so playback isn't disturbed — see the microphone preference.

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
-- Return one app name per line. `paused as string` would concatenate the list
-- with no separator ("SpotifyMusic"); a linefeed delimiter lets the caller
-- recover the individual names to resume later.
set AppleScript's text item delimiters to linefeed
return paused as string
"#;

/// Records which scriptable apps the most recent record-start paused, so
/// record-stop can resume exactly those — and nothing the user paused
/// themselves. Sequential by construction (the pipeline state machine forbids
/// overlapping recordings).
#[cfg(target_os = "macos")]
static PAUSED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Best-effort: pause any currently-playing scriptable media apps. Returns
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
            // Names of scriptable apps the AppleScript pass paused (one per line).
            let scriptable_apps: Vec<String> = match output {
                Ok(out) if out.status.success() => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let apps: Vec<String> = stdout
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !apps.is_empty() {
                        tracing::info!("Paused media in: {}", apps.join(", "));
                    }
                    apps
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    tracing::debug!(
                        "media pause osascript exit {:?}: {}",
                        out.status.code(),
                        stderr.trim()
                    );
                    Vec::new()
                }
                Err(e) => {
                    tracing::debug!("media pause: osascript spawn failed: {e}");
                    Vec::new()
                }
            };

            // Remember what we paused so resume_local_media() can restore exactly
            // these — and only these — when the recording ends.
            *PAUSED.lock().unwrap_or_else(|e| e.into_inner()) = scriptable_apps;
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux / Windows pause is doable via playerctl / SMTC but out of
        // scope right now — primary use case is macOS.
    }
}

/// Resume whatever `pause_local_media()` paused for the current recording.
///
/// Reads and clears the remembered state, so it only ever resumes apps WE
/// paused — never something the user paused themselves. A no-op if the last
/// record start paused nothing. Spawned off-thread; resume is not latency-critical.
pub fn resume_local_media() {
    #[cfg(target_os = "macos")]
    {
        let apps = std::mem::take(&mut *PAUSED.lock().unwrap_or_else(|e| e.into_inner()));
        if apps.is_empty() {
            return; // nothing to resume
        }
        std::thread::spawn(move || {
            for app in &apps {
                // App names come from our own fixed target list (echoed back by
                // the pause script), never user input — safe to inline.
                let script = format!(
                    "using terms from application \"Music\"\n\
                         tell application \"{app}\" to play\n\
                     end using terms from"
                );
                let _ = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(&script)
                    .output();
            }
            tracing::info!("Resumed media in: {}", apps.join(", "));
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No-op on non-macOS (pause is macOS-only for now).
    }
}
