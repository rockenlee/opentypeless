use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;

use super::{prompt, ChunkCallback, LlmConfig, LlmProvider, PolishRequest, PolishResponse};

/// Cloud LLM provider that proxies requests through the talkmore-web API.
/// Auth token is passed via the api_key field in LlmConfig. Quota is enforced server-side.
pub struct CloudLlmProvider {
    client: Client,
}

impl Default for CloudLlmProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CloudLlmProvider {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub fn with_client(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl LlmProvider for CloudLlmProvider {
    async fn polish(
        &self,
        config: &LlmConfig,
        req: &PolishRequest,
        on_chunk: Option<&ChunkCallback>,
    ) -> Result<PolishResponse> {
        if config.api_key.is_empty() {
            anyhow::bail!("Cloud LLM: session token is missing. Please sign in first.");
        }

        let has_selected_text = req
            .selected_text
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty());

        let system_prompt = if req.edit_selection {
            prompt::build_edit_selection_prompt()
        } else {
            prompt::build_system_prompt(
                req.app_type,
                &req.dictionary,
                req.translate_enabled,
                &req.target_lang,
                has_selected_text,
            )
        };

        let mut messages = vec![serde_json::json!({ "role": "system", "content": system_prompt })];
        if has_selected_text {
            messages.push(serde_json::json!({
                "role": "user",
                "content": format!("<selected_text>\n{}\n</selected_text>", req.selected_text.as_ref().unwrap())
            }));
        }
        // In Edit Selection mode the voice input is an EDIT INSTRUCTION, so tag
        // it as <instruction>; normal dictation stays <transcription>.
        let input_tag = if req.edit_selection {
            "instruction"
        } else {
            "transcription"
        };
        messages.push(serde_json::json!({
            "role": "user",
            "content": format!("<{tag}>\n{}\n</{tag}>", req.raw_text, tag = input_tag)
        }));

        let api_base_url = crate::api_base_url();

        let body = serde_json::json!({
            "messages": messages,
            "stream": on_chunk.is_some()
        });

        let response = self
            .client
            .post(format!("{}/api/proxy/llm", api_base_url))
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 403 {
                let msg = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v["error"].as_str().map(String::from))
                    .unwrap_or_else(|| "LLM quota exceeded".to_string());
                anyhow::bail!("{}", msg);
            }
            let truncate_at = text
                .char_indices()
                .take_while(|&(i, _)| i < 200)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(text.len());
            let sanitized = &text[..truncate_at];
            anyhow::bail!("Cloud LLM error ({}): {}", status, sanitized);
        }

        if let Some(callback) = on_chunk {
            let mut full_text = String::new();
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" {
                            break;
                        }
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(content) = v["choices"][0]["delta"]["content"].as_str() {
                                if !content.is_empty() {
                                    full_text.push_str(content);
                                    callback(content);
                                }
                            }
                        }
                    }
                }
            }

            Ok(PolishResponse {
                polished_text: full_text,
            })
        } else {
            let v: serde_json::Value = response.json().await?;
            let text = v["text"]
                .as_str()
                .or_else(|| v["choices"][0]["message"]["content"].as_str())
                .unwrap_or("")
                .to_string();

            Ok(PolishResponse {
                polished_text: text,
            })
        }
    }

    fn name(&self) -> &str {
        "Cloud"
    }
}
