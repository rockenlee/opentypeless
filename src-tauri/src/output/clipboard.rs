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
        let app_name = self.app_name.clone();
        tokio::task::spawn_blocking(move || {
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|e| anyhow::anyhow!("Failed to access clipboard: {}", e))?;

            clipboard
                .set_text(&text)
                .map_err(|e| anyhow::anyhow!("Failed to set clipboard: {}", e))?;

            std::thread::sleep(std::time::Duration::from_millis(CLIPBOARD_SETTLE_MS));

            // On macOS: activate the target app then Cmd+V via osascript.
            // When app_name is known, we activate it first — this is critical when the
            // Tauri window has stolen focus during a long operation (e.g. Hermes agent).
            // Skip activation when the detected app is OpenTypeless itself (happens when the
            // user clicks the capsule instead of using the hotkey) — pasting into ourselves
            // is never what the user wants.
            #[cfg(target_os = "macos")]
            {
                // Two-step paste so the target-app `activate` step is best-effort
                // (it requires per-target Automation grants — OpenTypeless → Cursor,
                // OpenTypeless → WeChat, etc. — that the user typically hasn't given)
                // while the actual Cmd+V keystroke is the critical step (only needs
                // OpenTypeless → System Events).
                //
                // If activate fails (no permission for that target app, or the app
                // isn't running), we log + continue. The keystroke lands on whatever
                // is currently frontmost — usually still the user's target app,
                // because OpenTypeless is a small background utility that doesn't
                // typically steal focus.
                let lowered = app_name.to_lowercase();
                let should_activate = !app_name.is_empty()
                    && !lowered.contains("opentypeless")
                    && !lowered.contains("tauri");
                let escaped_name = app_name.replace('\\', "\\\\").replace('"', "\\\"");

                // Step 1 (best-effort): bring target app to front.
                if should_activate {
                    let activate_script = format!(r#"tell application "{escaped_name}" to activate"#);
                    let activate_result = std::process::Command::new("osascript")
                        .arg("-e")
                        .arg(&activate_script)
                        .output();
                    match activate_result {
                        Ok(out) if out.status.success() => {
                            // Tiny delay for the app to actually take focus before keystroke.
                            std::thread::sleep(std::time::Duration::from_millis(80));
                        }
                        Ok(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            tracing::warn!(
                                "osascript activate '{}' exit {:?}: {} — continuing without activate",
                                app_name,
                                out.status.code(),
                                stderr.trim()
                            );
                            // No bail — keystroke might still work on current frontmost.
                        }
                        Err(e) => {
                            tracing::warn!("osascript activate launch failed: {e} — continuing");
                        }
                    }
                }

                // Step 2 (critical): send Cmd+V via System Events.
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
