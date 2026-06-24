use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{SttConfig, SttProvider, TranscriptEvent};

const WS_ENDPOINT: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference";
const CONNECT_TIMEOUT_SECS: u64 = 10;
const TASK_START_TIMEOUT_SECS: u64 = 8;
const RECV_TIMEOUT_SECS: u64 = 10;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct DashscopeStreamProvider {
    ws: Option<WsStream>,
    task_id: String,
}

impl DashscopeStreamProvider {
    pub fn new() -> Self {
        Self {
            ws: None,
            task_id: String::new(),
        }
    }

    fn new_task_id() -> String {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        format!("{:010}{:09}", d.as_secs(), d.subsec_nanos())
    }
}

#[async_trait]
impl SttProvider for DashscopeStreamProvider {
    async fn connect(&mut self, config: &SttConfig) -> Result<()> {
        if config.api_key.is_empty() {
            anyhow::bail!("DashScope 实时转写: API key 为空");
        }

        let request = http::Request::builder()
            .uri(WS_ENDPOINT)
            .header("Authorization", format!("bearer {}", config.api_key))
            .header("Host", "dashscope.aliyuncs.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .body(())?;

        let mut ws = tokio::time::timeout(
            std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS),
            connect_async(request),
        )
        .await
        .map_err(|_| anyhow!("DashScope WebSocket 连接超时 ({}s)", CONNECT_TIMEOUT_SECS))?
        .map_err(|e| anyhow!("DashScope WebSocket 连接失败: {}", e))?
        .0;

        let task_id = Self::new_task_id();

        let mut parameters = serde_json::json!({
            "format": "pcm",
            "sample_rate": config.sample_rate,
            "enable_punctuation_prediction": true,
            "enable_inverse_text_normalization": true,
        });

        if let Some(lang) = config.language.as_deref() {
            if !lang.is_empty() && lang != "multi" {
                parameters["language_hints"] = serde_json::json!([lang]);
            }
        }

        let run_task = serde_json::json!({
            "header": {
                "action": "run-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "task_group": "audio",
                "task": "asr",
                "function": "recognition",
                "model": "paraformer-realtime-v2",
                "parameters": parameters,
                "input": {}
            }
        });

        ws.send(Message::Text(run_task.to_string()))
            .await
            .map_err(|e| anyhow!("DashScope: 发送 run-task 失败: {}", e))?;

        // Wait for task-started with a hard timeout
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(TASK_START_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                anyhow::bail!(
                    "DashScope: 等待 task-started 超时 ({}s)",
                    TASK_START_TIMEOUT_SECS
                );
            }
            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let v: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| anyhow!("DashScope: JSON 解析失败: {}", e))?;
                    match v["header"]["event"].as_str().unwrap_or("") {
                        "task-started" => {
                            tracing::info!("DashScope 实时流: 任务已启动 ({})", task_id);
                            break;
                        }
                        "task-failed" => {
                            let msg = v["header"]["error_message"]
                                .as_str()
                                .unwrap_or("未知错误")
                                .to_string();
                            anyhow::bail!("DashScope: 任务启动失败: {}", msg);
                        }
                        other => {
                            tracing::warn!("DashScope: 启动阶段收到意外事件: {}", other);
                        }
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) => {
                    anyhow::bail!("DashScope: WebSocket 在 task-started 前关闭");
                }
                Ok(Some(Err(e))) => {
                    anyhow::bail!("DashScope: WebSocket 错误: {}", e);
                }
                Ok(None) => {
                    anyhow::bail!("DashScope: WebSocket 流提前结束");
                }
                Err(_) => {
                    anyhow::bail!(
                        "DashScope: 等待 task-started 超时 ({}s)",
                        TASK_START_TIMEOUT_SECS
                    );
                }
                _ => {}
            }
        }

        self.ws = Some(ws);
        self.task_id = task_id;
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &[u8]) -> Result<()> {
        if let Some(ws) = &mut self.ws {
            ws.send(Message::Binary(chunk.to_vec()))
                .await
                .map_err(|e| anyhow!("DashScope: 发送音频失败: {}", e))?;
        }
        Ok(())
    }

    async fn recv_transcript(&mut self) -> Result<Option<TranscriptEvent>> {
        let ws = match &mut self.ws {
            Some(ws) => ws,
            None => return Ok(None),
        };

        let msg =
            tokio::time::timeout(std::time::Duration::from_secs(RECV_TIMEOUT_SECS), ws.next())
                .await;

        let msg = match msg {
            Err(_) => {
                // Timeout — no data from DashScope for RECV_TIMEOUT_SECS seconds.
                // Return an error so the pipeline can clean up rather than hanging.
                return Err(anyhow!(
                    "DashScope: 接收超时 ({}s)，网络可能已断开",
                    RECV_TIMEOUT_SECS
                ));
            }
            Ok(inner) => inner,
        };

        match msg {
            Some(Ok(Message::Text(text))) => {
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => return Ok(None),
                };
                match v["header"]["event"].as_str().unwrap_or("") {
                    "result-generated" => {
                        let sentence = &v["payload"]["output"]["sentence"];
                        let transcript = sentence["text"].as_str().unwrap_or("").trim().to_string();
                        if transcript.is_empty() {
                            return Ok(None);
                        }
                        let is_final = sentence["sentence_end"].as_bool().unwrap_or(false);
                        if is_final {
                            Ok(Some(TranscriptEvent::Final {
                                text: transcript,
                                confidence: 1.0,
                            }))
                        } else {
                            Ok(Some(TranscriptEvent::Partial { text: transcript }))
                        }
                    }
                    "task-finished" => {
                        tracing::info!("DashScope 实时流: 任务完成");
                        Ok(None)
                    }
                    "task-failed" => {
                        let msg = v["header"]["error_message"]
                            .as_str()
                            .unwrap_or("DashScope 任务失败")
                            .to_string();
                        Ok(Some(TranscriptEvent::Error { message: msg }))
                    }
                    _ => Ok(None),
                }
            }
            Some(Ok(Message::Close(_))) => {
                tracing::info!("DashScope 实时流: WebSocket 关闭");
                Ok(None)
            }
            Some(Err(e)) => Ok(Some(TranscriptEvent::Error {
                message: e.to_string(),
            })),
            None => Ok(None),
            _ => Ok(None),
        }
    }

    async fn disconnect(&mut self) -> Result<Option<String>> {
        let mut ws = match self.ws.take() {
            Some(ws) => ws,
            None => return Ok(None),
        };

        let finish = serde_json::json!({
            "header": {
                "action": "finish-task",
                "task_id": self.task_id,
                "streaming": "duplex"
            },
            "payload": { "input": {} }
        });
        let _ = ws.send(Message::Text(finish.to_string())).await;

        // Drain remaining messages — cap at 5s to avoid stalling the pipeline
        let mut trailing = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let v: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    match v["header"]["event"].as_str().unwrap_or("") {
                        "result-generated" => {
                            let sentence = &v["payload"]["output"]["sentence"];
                            if sentence["sentence_end"].as_bool().unwrap_or(false) {
                                let t = sentence["text"].as_str().unwrap_or("").trim().to_string();
                                if !t.is_empty() {
                                    if !trailing.is_empty() {
                                        trailing.push(' ');
                                    }
                                    trailing.push_str(&t);
                                }
                            }
                        }
                        "task-finished" => break,
                        "task-failed" => {
                            tracing::error!("DashScope: disconnect 阶段任务失败");
                            break;
                        }
                        _ => {}
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Err(_) => break,
                Ok(Some(Err(e))) => {
                    tracing::error!("DashScope: disconnect 阶段 WebSocket 错误: {}", e);
                    break;
                }
                _ => {}
            }
        }

        let _ = ws.close(None).await;
        tracing::info!("DashScope 实时流: 已断开");

        if trailing.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trailing))
        }
    }

    fn name(&self) -> &str {
        "DashScope Paraformer 实时"
    }
}
