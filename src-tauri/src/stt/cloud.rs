use anyhow::Result;
use async_trait::async_trait;

use super::whisper_compat::WhisperCompatProvider;
use super::{SttConfig, SttProvider, TranscriptEvent};

/// Cloud STT provider that proxies audio through the talkmore-web API.
/// Auth token is passed via the api_key field. Quota is enforced server-side.
pub struct CloudSttProvider {
    stt_config: Option<SttConfig>,
    audio_buffer: Vec<u8>,
    client: reqwest::Client,
    api_base_url: String,
}

/// Max audio buffer: ~24 MB PCM ≈ 12.5 min at 16kHz 16-bit mono.
const MAX_AUDIO_BYTES: usize = 24 * 1024 * 1024;

impl CloudSttProvider {
    pub fn new(api_base_url: String) -> Self {
        Self {
            stt_config: None,
            audio_buffer: Vec::new(),
            client: reqwest::Client::new(),
            api_base_url,
        }
    }

    pub fn with_client(api_base_url: String, client: reqwest::Client) -> Self {
        Self {
            stt_config: None,
            audio_buffer: Vec::new(),
            client,
            api_base_url,
        }
    }
}

#[async_trait]
impl SttProvider for CloudSttProvider {
    async fn connect(&mut self, config: &SttConfig) -> Result<()> {
        if config.api_key.is_empty() {
            anyhow::bail!("Cloud STT: session token is missing. Please sign in first.");
        }
        self.stt_config = Some(config.clone());
        self.audio_buffer.clear();
        tracing::info!("Cloud STT provider ready (buffering mode)");
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &[u8]) -> Result<()> {
        if self.audio_buffer.len() + chunk.len() > MAX_AUDIO_BYTES {
            anyhow::bail!("Cloud STT: audio exceeds maximum length (~12 min)");
        }
        self.audio_buffer.extend_from_slice(chunk);
        Ok(())
    }

    async fn recv_transcript(&mut self) -> Result<Option<TranscriptEvent>> {
        // Batch provider: the transcript is produced in disconnect(), not
        // streamed. Returning Ok(None) immediately makes the pipeline's select!
        // loop busy-spin at 100% CPU (this branch is always instantly ready),
        // pegging a core for the whole recording and freezing the UI on longer
        // takes. Never resolve, so the loop only wakes on incoming audio chunks.
        std::future::pending::<Result<Option<TranscriptEvent>>>().await
    }

    async fn disconnect(&mut self) -> Result<Option<String>> {
        let config = match &self.stt_config {
            Some(c) => c.clone(),
            None => return Ok(None),
        };

        if self.audio_buffer.is_empty() {
            tracing::info!("Cloud STT: no audio buffered, skipping");
            return Ok(None);
        }

        let audio_len_secs = self.audio_buffer.len() as f64 / (config.sample_rate as f64 * 2.0);
        let wav_data = WhisperCompatProvider::build_wav(&self.audio_buffer, config.sample_rate);
        self.audio_buffer.clear();
        tracing::info!(
            "Cloud STT: sending {:.1}s of audio for transcription",
            audio_len_secs
        );

        let file_part = reqwest::multipart::Part::bytes(wav_data)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;

        let mut form = reqwest::multipart::Form::new().part("audio", file_part);

        if let Some(ref lang) = config.language {
            form = form.text("language", lang.clone());
        }

        let resp = self
            .client
            .post(format!("{}/api/proxy/stt", self.api_base_url))
            .header("Authorization", format!("Bearer {}", config.api_key))
            .multipart(form)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            if status.as_u16() == 403 {
                let msg = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["error"].as_str().map(String::from))
                    .unwrap_or_else(|| "STT quota exceeded".to_string());
                anyhow::bail!("{}", msg);
            }
            let truncate_at = body
                .char_indices()
                .take_while(|&(i, _)| i < 200)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(body.len());
            let sanitized = &body[..truncate_at];
            tracing::error!("Cloud STT HTTP {}: {}", status, sanitized);
            anyhow::bail!("Cloud STT error ({}): {}", status, sanitized);
        }

        let v: serde_json::Value = serde_json::from_str(&body)?;
        let text = v["text"].as_str().unwrap_or("").trim().to_string();

        tracing::info!("Cloud STT transcription: {} chars", text.len());

        if text.is_empty() {
            Ok(None)
        } else {
            Ok(Some(text))
        }
    }

    fn name(&self) -> &str {
        "Cloud"
    }
}
