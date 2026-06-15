use anyhow::Result;
use async_trait::async_trait;

use super::{SttConfig, SttProvider, TranscriptEvent};

/// Configuration for a Whisper-compatible HTTP file-upload STT provider.
pub struct WhisperCompatConfig {
    pub provider_name: &'static str,
    pub endpoint: &'static str,
    pub model: &'static str,
    /// Extra form text fields (e.g. GLM-ASR needs "stream"="false").
    pub extra_fields: &'static [(&'static str, &'static str)],
    /// Whether this provider accepts the OpenAI `prompt` field for vocabulary hints.
    pub supports_prompt: bool,
}

/// Max audio buffer: ~24 MB PCM ≈ 12.5 min at 16kHz 16-bit mono.
/// Keeps the resulting WAV under 25 MB (OpenAI/Groq limit).
const MAX_AUDIO_BYTES: usize = 24 * 1024 * 1024;

/// Generic provider for any OpenAI Whisper-compatible transcription API.
/// Works with: OpenAI, Groq, SiliconFlow, GLM-ASR.
pub struct WhisperCompatProvider {
    provider_config: WhisperCompatConfig,
    stt_config: Option<SttConfig>,
    audio_buffer: Vec<u8>,
    client: reqwest::Client,
}

impl WhisperCompatProvider {
    pub fn new(provider_config: WhisperCompatConfig) -> Self {
        Self {
            provider_config,
            stt_config: None,
            audio_buffer: Vec::new(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_client(provider_config: WhisperCompatConfig, client: reqwest::Client) -> Self {
        Self {
            provider_config,
            stt_config: None,
            audio_buffer: Vec::new(),
            client,
        }
    }

    /// Build a WAV file from raw PCM 16-bit mono audio. Public so test helpers can reuse it.
    pub fn build_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
        let data_len = pcm.len() as u32;
        let channels: u16 = 1;
        let bits_per_sample: u16 = 16;
        let byte_rate = sample_rate * (channels as u32) * (bits_per_sample as u32) / 8;
        let block_align = channels * bits_per_sample / 8;
        let file_size = 36 + data_len;

        let mut wav = Vec::with_capacity(44 + pcm.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits_per_sample.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(pcm);
        wav
    }
}

#[async_trait]
impl SttProvider for WhisperCompatProvider {
    async fn connect(&mut self, config: &SttConfig) -> Result<()> {
        if config.api_key.is_empty() {
            anyhow::bail!("{} API key is empty", self.provider_config.provider_name);
        }
        self.stt_config = Some(config.clone());
        self.audio_buffer.clear();
        tracing::info!(
            "{} provider ready (buffering mode)",
            self.provider_config.provider_name
        );
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &[u8]) -> Result<()> {
        if self.audio_buffer.len() + chunk.len() > MAX_AUDIO_BYTES {
            anyhow::bail!(
                "{}: audio exceeds maximum length (~12 min)",
                self.provider_config.provider_name
            );
        }
        self.audio_buffer.extend_from_slice(chunk);
        Ok(())
    }

    async fn recv_transcript(&mut self) -> Result<Option<TranscriptEvent>> {
        // File-based provider: the transcript is produced in disconnect(), not
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
            tracing::info!(
                "{}: no audio buffered, skipping",
                self.provider_config.provider_name
            );
            return Ok(None);
        }

        let audio_len_secs = self.audio_buffer.len() as f64 / (config.sample_rate as f64 * 2.0);
        let wav_data = Self::build_wav(&self.audio_buffer, config.sample_rate);
        self.audio_buffer.clear();
        tracing::info!(
            "{}: sending {:.1}s of audio for transcription",
            self.provider_config.provider_name,
            audio_len_secs
        );

        let file_part = reqwest::multipart::Part::bytes(wav_data)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.provider_config.model.to_string())
            .part("file", file_part);

        // Language hint
        if let Some(ref lang) = config.language {
            if lang != "multi" {
                form = form.text("language", lang.clone());
            }
        }

        // Vocabulary hotwords: pass as `prompt` to bias the model toward correct spellings.
        // Whisper's prompt context window is ~244 tokens. We cap at 40 entries and prefer
        // entries that contain non-CJK characters (Whisper handles Chinese natively;
        // the prompt helps most with proper nouns, brand names, and mixed-script terms).
        if self.provider_config.supports_prompt && !config.hotwords.is_empty() {
            let prompt = build_whisper_prompt(&config.hotwords);
            if !prompt.is_empty() {
                form = form.text("prompt", prompt);
            }
        }

        // Provider-specific extra fields
        for &(key, value) in self.provider_config.extra_fields {
            form = form.text(key.to_string(), value.to_string());
        }

        let resp = self
            .client
            .post(self.provider_config.endpoint)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .multipart(form)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            // Truncate at a valid UTF-8 char boundary to avoid panic on multi-byte chars
            let truncate_at = body
                .char_indices()
                .take_while(|&(i, _)| i < 200)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(body.len());
            let sanitized = &body[..truncate_at];
            tracing::error!(
                "{} HTTP {}: {}",
                self.provider_config.provider_name,
                status,
                sanitized
            );
            anyhow::bail!(
                "{} error ({}): {}",
                self.provider_config.provider_name,
                status,
                sanitized
            );
        }

        let v: serde_json::Value = serde_json::from_str(&body)?;
        let text = v["text"].as_str().unwrap_or("").trim().to_string();

        tracing::info!(
            "{} transcription: {} chars",
            self.provider_config.provider_name,
            text.len()
        );

        if text.is_empty() {
            Ok(None)
        } else {
            Ok(Some(text))
        }
    }

    fn name(&self) -> &str {
        self.provider_config.provider_name
    }
}

/// Build a Whisper-compatible `prompt` string from dictionary hotwords.
///
/// Strategy: Whisper's prompt context is ~244 tokens. We prioritise entries that
/// contain at least one non-CJK character (brand names, English proper nouns,
/// mixed-script terms) because Whisper handles pure Chinese natively, while
/// uncommon spellings benefit most from prompting. We cap at 40 entries so the
/// resulting string stays comfortably within the token budget.
fn build_whisper_prompt(hotwords: &[String]) -> String {
    const MAX_ENTRIES: usize = 40;

    // Score: entries with non-CJK chars come first (brand names, abbreviations, etc.)
    let mut scored: Vec<&str> = hotwords.iter().map(String::as_str).collect();
    scored.sort_by_key(|w| {
        let has_non_cjk = w.chars().any(|c| !matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}'));
        if has_non_cjk { 0usize } else { 1usize }
    });

    scored
        .into_iter()
        .take(MAX_ENTRIES)
        .collect::<Vec<_>>()
        .join(", ")
}
