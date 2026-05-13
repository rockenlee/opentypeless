//! macOS native notification helper via `osascript display notification`.
//! Used to surface Agent results when the user is focused on another app —
//! the agent-result window auto-shows + focuses, but if it lands behind a
//! full-screen app the user can miss it entirely.
//!
//! `display notification` is sandbox-safe (no Accessibility permission
//! needed; only requires that the user hasn't disabled notifications for
//! the calling app, which is handled by macOS Notification Center the
//! first time it's invoked).

#[cfg(target_os = "macos")]
const NOTIFY_TITLE: &str = "OpenTypeless · Agent";

/// Display a system notification with a short body preview. The full
/// response is in the Agent panel window that opens alongside. Safe to
/// call from any thread; spawns its own to avoid blocking the pipeline.
pub fn show_agent_notification(body: &str) {
    #[cfg(target_os = "macos")]
    {
        // Trim to ~140 chars for the notification body so macOS doesn't
        // truncate awkwardly.
        let mut preview = body.trim().to_string();
        if preview.chars().count() > 140 {
            preview = preview.chars().take(137).collect::<String>() + "...";
        }
        if preview.is_empty() {
            preview = "(empty response)".to_string();
        }

        // AppleScript string-escape — the only chars we need to worry about
        // for this command are backslash and double-quote.
        let escaped_body = preview.replace('\\', "\\\\").replace('"', "\\\"");
        let escaped_title = NOTIFY_TITLE.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            r#"display notification "{escaped_body}" with title "{escaped_title}" sound name "Glass""#
        );

        std::thread::spawn(move || {
            let result = std::process::Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .output();
            if let Ok(out) = result {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    tracing::debug!(
                        "agent notification osascript exit {:?}: {}",
                        out.status.code(),
                        stderr.trim()
                    );
                }
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = body;
    }
}
