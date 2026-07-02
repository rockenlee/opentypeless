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
        let _app_name = self.app_name.clone();
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
                    _app_name
                );

                // Paste with a low-level Cmd+V (CGEvent, HID tap) instead of an
                // osascript `keystroke` — so we only ever need ACCESSIBILITY, never
                // the separate Automation / "System Events" permission that used to
                // confuse users. A CGEvent key is silently dropped without
                // Accessibility, so check first and surface a clear message.
                if !crate::pipeline::is_accessibility_trusted() {
                    let _ = std::process::Command::new("open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                        .status();
                    anyhow::bail!(
                        "Auto-paste blocked: OpenTypeless needs ACCESSIBILITY permission. I've opened System Settings → Privacy & Security → Accessibility — add OpenTypeless and turn it on. Text is on the clipboard; press ⌘V to paste manually for now."
                    );
                }
                crate::post_cmd_key(9); // kVK_ANSI_V
            }

            #[cfg(not(target_os = "macos"))]
            {
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
