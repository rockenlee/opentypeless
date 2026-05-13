use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::app_detector::AppContext;
use crate::storage::AppConfig;

const AGENT_PREFIXES: &[&str] = &[
    // ordered longest-first so e.g. "ask hermes" wins over the bare "hermes"
    "ask hermes",
    "ask agent",
    "ask claude",
    "ask gemini",
    "hermes",
    "agent",
    "claude",
    "gemini",
];
const AGENT_TIMEOUT: Duration = Duration::from_secs(120);
const PROMPT_PLACEHOLDER: &str = "{prompt}";

/// Per-preset defaults: (default binary name, default args template).
/// `{prompt}` in the args template is replaced with the actual prompt string
/// at invocation time. Returns `None` for `custom` (user supplies everything).
fn preset_defaults(preset: &str) -> Option<(&'static str, &'static str)> {
    match preset.trim().to_lowercase().as_str() {
        "hermes" => Some(("hermes", "-z {prompt}")),
        "claude" => Some(("claude", "--print {prompt}")),
        "gemini" => Some(("gemini", "--prompt {prompt}")),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub prompt: String,
    pub app_context: AppContext,
    pub selected_text: Option<String>,
    pub config: AppConfig,
}

/// Detect whether the transcript starts with a known agent trigger word and,
/// if so, return the prompt with the prefix stripped. Returns None for plain
/// dictation that should go through the normal LLM polish path.
pub fn parse_agent_prompt(raw_text: &str) -> Option<String> {
    let trimmed = raw_text.trim();
    let lower = trimmed.to_lowercase();

    for prefix in AGENT_PREFIXES {
        if lower.starts_with(prefix) {
            let prompt = trimmed[prefix.len()..]
                .trim_start_matches(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            ':' | '：' | ',' | '，' | '.' | '。' | '!' | '！' | '?' | '？'
                        )
                })
                .trim();
            if !prompt.is_empty() {
                return Some(prompt.to_string());
            }
        }
    }

    None
}

// Backward-compat alias for the old name (pipeline.rs used to call this).
#[allow(dead_code)]
pub fn parse_hermes_prompt(raw_text: &str) -> Option<String> {
    parse_agent_prompt(raw_text)
}

pub async fn run_agent(request: AgentRequest) -> Result<String> {
    run_agent_async(request).await
}

// Backward-compat alias.
#[allow(dead_code)]
pub async fn run_hermes(request: AgentRequest) -> Result<String> {
    run_agent(request).await
}

pub async fn test_agent(config: AppConfig) -> Result<String> {
    let request = AgentRequest {
        prompt: "Say only: agent test ok".to_string(),
        app_context: AppContext::default(),
        selected_text: None,
        config,
    };
    run_agent(request).await
}

#[allow(dead_code)]
pub async fn test_hermes(config: AppConfig) -> Result<String> {
    test_agent(config).await
}

pub fn runtime_label(config: &AppConfig) -> String {
    let preset = if config.agent_preset.trim().is_empty() {
        "hermes".to_string()
    } else {
        config.agent_preset.clone()
    };
    let command = resolve_command(config);
    let cwd = resolve_cwd(config).unwrap_or_else(|_| PathBuf::from("."));
    format!(
        "Agent[{}]: {} @ {}",
        preset,
        command.display(),
        cwd.display()
    )
}

async fn run_agent_async(request: AgentRequest) -> Result<String> {
    let command = resolve_command(&request.config);
    if command.as_os_str().is_empty() {
        return Err(anyhow!(
            "Agent command not configured. Open Settings → Agent and set a binary path (or pick a preset)."
        ));
    }
    let args_template = resolve_args(&request.config);
    let cwd = resolve_cwd(&request.config)?;
    let prompt = build_prompt(&request);

    let preset_label = if request.config.agent_preset.trim().is_empty() {
        "agent".to_string()
    } else {
        request.config.agent_preset.clone()
    };

    let mut cmd = Command::new(&command);
    // Tokenise the args template by whitespace, substituting the literal
    // `{prompt}` token with the actual prompt string. Tokens are passed as
    // separate argv entries so shell quoting isn't a concern.
    let mut had_placeholder = false;
    for token in args_template.split_whitespace() {
        if token == PROMPT_PLACEHOLDER {
            cmd.arg(&prompt);
            had_placeholder = true;
        } else {
            cmd.arg(token);
        }
    }
    // If the user's args template forgot `{prompt}`, append the prompt at the
    // end (so e.g. just "-z" still works — easy to typo).
    if !had_placeholder {
        cmd.arg(&prompt);
    }

    let child = cmd
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "failed to launch {} command `{}`",
                preset_label,
                command.display()
            )
        })?;

    let output = match timeout(AGENT_TIMEOUT, child.wait_with_output()).await {
        Ok(result) => result.context("failed to wait for agent output")?,
        Err(_) => {
            return Err(anyhow!(
                "{} timed out after {} seconds",
                preset_label,
                AGENT_TIMEOUT.as_secs()
            ));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(anyhow!(
            "{} exited with status {}{}",
            preset_label,
            output.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }

    let response = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if response.is_empty() {
        return Err(anyhow!("{} returned an empty response", preset_label));
    }

    Ok(response)
}

fn resolve_cwd(config: &AppConfig) -> Result<PathBuf> {
    let configured = config.agent_cwd.trim();
    if !configured.is_empty() {
        return Ok(PathBuf::from(configured));
    }

    // Legacy env var name kept for users who set it.
    if let Ok(cwd) = std::env::var("OPENTYPELESS_AGENT_CWD")
        .or_else(|_| std::env::var("OPENTYPELESS_HERMES_CWD"))
    {
        if !cwd.trim().is_empty() {
            return Ok(PathBuf::from(cwd.trim()));
        }
    }

    std::env::current_dir().context("failed to resolve current directory for agent")
}

fn resolve_command(config: &AppConfig) -> PathBuf {
    // Explicit override always wins.
    let configured = config.agent_command.trim();
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }

    // Env vars next.
    if let Ok(command) =
        std::env::var("OPENTYPELESS_AGENT_COMMAND").or_else(|_| std::env::var("OPENTYPELESS_HERMES_COMMAND"))
    {
        let trimmed = command.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    let preset = config.agent_preset.trim().to_lowercase();
    let preset = if preset.is_empty() { "hermes" } else { preset.as_str() };
    let default_bin = match preset_defaults(preset) {
        Some((bin, _)) => bin,
        None => return PathBuf::new(), // "custom" preset and nothing configured
    };

    // Look in well-known per-user install locations. Each of these is where
    // the corresponding agent's official installer or `npm install -g` tend
    // to drop their binary. Resolved via $HOME so the binary is shareable
    // across machines (no hard-coded usernames).
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let candidates = [
            home.join(format!("miniconda3/bin/{default_bin}")),
            home.join(format!("anaconda3/bin/{default_bin}")),
            home.join(format!(".local/bin/{default_bin}")),
            home.join(format!(".cargo/bin/{default_bin}")),
            home.join(format!(".bun/bin/{default_bin}")),
            home.join(format!(".npm-global/bin/{default_bin}")),
        ];
        for candidate in candidates {
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // Fallback: bare binary name → relies on PATH lookup.
    PathBuf::from(default_bin)
}

fn resolve_args(config: &AppConfig) -> String {
    let configured = config.agent_args.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    let preset = config.agent_preset.trim().to_lowercase();
    let preset = if preset.is_empty() { "hermes" } else { preset.as_str() };
    match preset_defaults(preset) {
        Some((_, args)) => args.to_string(),
        None => PROMPT_PLACEHOLDER.to_string(), // custom with empty args = just pass prompt
    }
}

fn build_prompt(request: &AgentRequest) -> String {
    let mut prompt = String::new();
    prompt.push_str(&request.prompt);

    if !request.app_context.app_name.trim().is_empty()
        || !request.app_context.window_title.trim().is_empty()
    {
        prompt.push_str("\n\nContext from OpenTypeless:\n");
        if !request.app_context.app_name.trim().is_empty() {
            prompt.push_str("- Active app: ");
            prompt.push_str(request.app_context.app_name.trim());
            prompt.push('\n');
        }
        if !request.app_context.window_title.trim().is_empty() {
            prompt.push_str("- Window title: ");
            prompt.push_str(request.app_context.window_title.trim());
            prompt.push('\n');
        }
    }

    if let Some(selected_text) = request
        .selected_text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push_str("\nSelected text:\n");
        prompt.push_str(selected_text);
        prompt.push('\n');
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::parse_agent_prompt;

    #[test]
    fn parses_supported_agent_prefixes() {
        assert_eq!(
            parse_agent_prompt("hermes summarize this").as_deref(),
            Some("summarize this")
        );
        assert_eq!(
            parse_agent_prompt("Ask Agent fix the failing test").as_deref(),
            Some("fix the failing test")
        );
        assert_eq!(
            parse_agent_prompt("hermes帮我看一下今天的天气").as_deref(),
            Some("帮我看一下今天的天气")
        );
        assert_eq!(
            parse_agent_prompt("Hermes，解释这个报错").as_deref(),
            Some("解释这个报错")
        );
        assert_eq!(
            parse_agent_prompt("claude write me a haiku").as_deref(),
            Some("write me a haiku")
        );
        assert_eq!(
            parse_agent_prompt("Ask Gemini what's 2+2").as_deref(),
            Some("what's 2+2")
        );
    }

    #[test]
    fn ignores_regular_dictation() {
        assert_eq!(parse_agent_prompt("please write this normally"), None);
        assert_eq!(parse_agent_prompt("hermes"), None);
        assert_eq!(parse_agent_prompt("agent"), None);
    }
}
