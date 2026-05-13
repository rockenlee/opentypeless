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
set targetApps to {"Spotify", "Music", "Podcasts", "QuickTime Player", "VLC", "IINA", "TV"}
set paused to {}
repeat with appName in targetApps
    try
        if application appName is running then
            tell application appName
                try
                    if player state is playing then
                        pause
                        set end of paused to appName
                    end if
                end try
            end tell
        end if
    end try
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
            match output {
                Ok(out) if out.status.success() => {
                    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !stdout.is_empty() {
                        tracing::info!("Paused media in: {}", stdout);
                    }
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    tracing::debug!(
                        "media pause osascript exit {:?}: {}",
                        out.status.code(),
                        stderr.trim()
                    );
                }
                Err(e) => tracing::debug!("media pause: osascript spawn failed: {e}"),
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux / Windows pause is doable via playerctl / SMTC but out of
        // scope right now — primary use case is macOS.
    }
}
