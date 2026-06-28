use anyhow::Result;
use async_trait::async_trait;

use super::{OutputMode, TextOutput};

/// Delay after writing to clipboard before simulating paste.
const CLIPBOARD_SETTLE_MS: u64 = 80;

pub struct ClipboardOutput {
    /// macOS: name of the target application to activate before pasting.
    /// Empty string means "paste to whatever is frontmost".
    pub app_name: String,
}

impl Default for ClipboardOutput {
    fn default() -> Self {
        Self::new("")
    }
}

impl ClipboardOutput {
    pub fn new(app_name: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
        }
    }
}

#[async_trait]
impl TextOutput for ClipboardOutput {
    async fn type_text(&self, text: &str) -> Result<()> {
        let text = text.to_string();
        #[allow(unused_variables)]
        let app_name = self.app_name.clone();
        tokio::task::spawn_blocking(move || {
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|e| anyhow::anyhow!("Failed to access clipboard: {}", e))?;

            clipboard
                .set_text(&text)
                .map_err(|e| anyhow::anyhow!("Failed to set clipboard: {}", e))?;

            std::thread::sleep(std::time::Duration::from_millis(CLIPBOARD_SETTLE_MS));

            // On macOS: just send Cmd+V to whatever app is currently frontmost.
            // We deliberately do NOT `activate` the detected app first. The capsule
            // never steals focus (verified), so the frontmost window IS already the
            // user's target app. A `tell application "X" to activate` step only
            // risks yanking focus to the WRONG app — `app_name` is the frontmost
            // *process* name, which doesn't always resolve to the same AppleScript
            // application, and the user may have moved on during transcription.
            // That stray activate was the "it switches my focused app after
            // pasting" bug.
            #[cfg(target_os = "macos")]
            {
                tracing::debug!(
                    "Clipboard: Cmd+V into current frontmost app (recorded in '{}')",
                    app_name
                );

                // Send Cmd+V via System Events.
                let paste_script = r#"tell application "System Events" to keystroke "v" using command down"#;
                let paste_result = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(paste_script)
                    .output();

                match paste_result {
                    Ok(out) if out.status.success() => {}
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let stderr_trim = stderr.trim();
                        tracing::warn!(
                            "osascript Cmd+V exit {:?}: {}",
                            out.status.code(),
                            stderr_trim
                        );

                        // macOS error code 1002 from `keystroke`: "not allowed to
                        // send keystrokes". Despite the name, this is gated on
                        // ACCESSIBILITY permission, NOT Automation/System Events.
                        // The user typically grants Automation → System Events
                        // first and then is baffled why paste still fails.
                        if stderr_trim.contains("(1002)")
                            || stderr_trim.contains("not allowed to send keystrokes")
                        {
                            // Best-effort: open the Accessibility settings pane
                            // directly so the user doesn't have to hunt for it.
                            let _ = std::process::Command::new("open")
                                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                                .status();
                            anyhow::bail!(
                                "Auto-paste blocked: OpenTypeless needs ACCESSIBILITY permission (not Automation). I've opened System Settings → Privacy & Security → Accessibility — add OpenTypeless and turn it on. Text is on the clipboard; press ⌘V to paste manually for now."
                            );
                        }

                        if stderr_trim.contains("1743") || stderr_trim.contains("not authorized") {
                            anyhow::bail!(
                                "Auto-paste blocked: OpenTypeless needs 'System Events' Automation permission. Open System Settings → Privacy & Security → Automation → OpenTypeless → enable System Events. Text is on the clipboard; press ⌘V to paste manually for now."
                            );
                        }

                        // Surface the actual osascript stderr so the user can show it
                        // to us if there's a less-common failure mode.
                        anyhow::bail!(
                            "Auto-paste failed (exit {:?}): {}. Text is on the clipboard — press ⌘V to paste manually.",
                            out.status.code(),
                            if stderr_trim.is_empty() { "(no stderr)" } else { stderr_trim }
                        );
                    }
                    Err(e) => {
                        tracing::warn!("osascript Cmd+V launch failed: {e}");
                        anyhow::bail!(
                            "Auto-paste failed: osascript could not be launched ({e}). Text is on the clipboard — press ⌘V to paste manually."
                        );
                    }
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                // Dismiss active IME composition before Ctrl+V to prevent the
                // IME from intercepting the paste keystroke.
                #[cfg(target_os = "windows")]
                {
                    super::dismiss_ime_composition();
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }

                use enigo::{Direction, Enigo, Key, Keyboard, Settings};
                let mut enigo = Enigo::new(&Settings::default())
                    .map_err(|e| anyhow::anyhow!("Failed to create Enigo: {:?}", e))?;

                enigo
                    .key(Key::Control, Direction::Press)
                    .map_err(|e| anyhow::anyhow!("Key press error: {:?}", e))?;
                enigo
                    .key(Key::Unicode('v'), Direction::Click)
                    .map_err(|e| anyhow::anyhow!("Key click error: {:?}", e))?;
                enigo
                    .key(Key::Control, Direction::Release)
                    .map_err(|e| anyhow::anyhow!("Key release error: {:?}", e))?;
            }

            Ok(())
        })
        .await?
    }

    fn mode(&self) -> OutputMode {
        OutputMode::Clipboard
    }
}
