//! Tiny UI sound cues for record start / stop.
//!
//! Plays a macOS system sound off-thread via `afplay` so it never blocks the
//! pipeline. The sounds live in `/System/Library/Sounds` and ship with every
//! macOS install, so this works on any machine the app is distributed to — no
//! bundled audio assets required.

/// Play a macOS system sound by file name (e.g. "Pop", "Bottle"), best-effort.
/// Spawned off-thread; silently does nothing if `afplay` or the sound is absent.
#[cfg(target_os = "macos")]
pub fn play_cue(name: &'static str) {
    std::thread::spawn(move || {
        let path = format!("/System/Library/Sounds/{name}.aiff");
        let _ = std::process::Command::new("afplay").arg(path).status();
    });
}

#[cfg(not(target_os = "macos"))]
pub fn play_cue(_name: &'static str) {}
