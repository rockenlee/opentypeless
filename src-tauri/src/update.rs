//! Lightweight update reminder.
//!
//! On startup we ask GitHub for the latest *published* release and, if it is
//! newer than the running build, surface a reminder (a macOS notification plus
//! an `update:available` event the UI can show as a banner). This NEVER
//! downloads or installs anything — the user updates manually. Every failure
//! mode (offline, no release published yet → 404, malformed JSON) is silent so
//! it can never nag or break startup.

use tauri::{AppHandle, Emitter};

const RELEASES_API: &str = "https://api.github.com/repos/rockenlee/opentypeless/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/rockenlee/opentypeless/releases";

/// Parse "x.y.z" (optionally "vx.y.z", with an optional pre-release suffix on
/// the patch like "4-rc1") into a comparable tuple. Returns None for anything
/// that isn't a 3-part numeric version.
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()?
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

pub async fn check_for_update(app: AppHandle, client: reqwest::Client) {
    let current = env!("CARGO_PKG_VERSION");

    let resp = match client
        .get(RELEASES_API)
        .header("User-Agent", "OpenTypeless-UpdateCheck")
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("update check: request failed: {e}");
            return;
        }
    };

    if !resp.status().is_success() {
        // 404 == no published release yet (the common case while releases are held).
        tracing::debug!("update check: HTTP {}", resp.status());
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("update check: bad json: {e}");
            return;
        }
    };

    let tag = body["tag_name"].as_str().unwrap_or_default();
    let url = body["html_url"].as_str().unwrap_or(RELEASES_PAGE);

    let (Some(latest), Some(cur)) = (parse_version(tag), parse_version(current)) else {
        tracing::debug!("update check: unparseable version (latest={tag}, current={current})");
        return;
    };

    if latest > cur {
        tracing::info!("update available: {tag} (running {current})");
        crate::notify::show_update_notification(tag);
        let _ = app.emit(
            "update:available",
            serde_json::json!({ "version": tag, "url": url }),
        );
    } else {
        tracing::debug!("update check: up to date (latest {tag}, running {current})");
    }
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn parses_plain_and_v_prefixed() {
        assert_eq!(parse_version("0.2.4"), Some((0, 2, 4)));
        assert_eq!(parse_version("v0.2.4"), Some((0, 2, 4)));
        assert_eq!(parse_version(" v1.10.0 "), Some((1, 10, 0)));
    }

    #[test]
    fn parses_patch_with_suffix() {
        assert_eq!(parse_version("v0.2.4-rc1"), Some((0, 2, 4)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn ordering_is_correct() {
        assert!(parse_version("v0.2.5") > parse_version("v0.2.4"));
        assert!(parse_version("v0.3.0") > parse_version("v0.2.9"));
        assert!(parse_version("v1.0.0") > parse_version("v0.9.9"));
        assert!(!(parse_version("v0.2.4") > parse_version("v0.2.4")));
    }
}
