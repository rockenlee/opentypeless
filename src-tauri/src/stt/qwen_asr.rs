use anyhow::Result;
use async_trait::async_trait;
use base64::Engine as _;

use super::{SttConfig, SttProvider, TranscriptEvent};

const ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation";
const MODEL: &str = "qwen3-asr-flash";
/// ~6 MB PCM ≈ 3 min at 16kHz 16-bit mono. Base64 expands by 4/3 so the
/// encoded payload stays under the 10 MB DashScope limit.
const MAX_AUDIO_BYTES: usize = 6 * 1024 * 1024;

pub struct QwenAsrProvider {
    stt_config: Option<SttConfig>,
    audio_buffer: Vec<u8>,
    client: reqwest::Client,
}

impl QwenAsrProvider {
    pub fn new() -> Self {
        Self {
            stt_config: None,
            audio_buffer: Vec::new(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self {
            stt_config: None,
            audio_buffer: Vec::new(),
            client,
        }
    }

    fn build_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
        let data_len = pcm.len() as u32;
        let byte_rate = sample_rate * 2; // mono 16-bit
        let file_size = 36 + data_len;
        let mut wav = Vec::with_capacity(44 + pcm.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(pcm);
        wav
    }
}

#[async_trait]
impl SttProvider for QwenAsrProvider {
    async fn connect(&mut self, config: &SttConfig) -> Result<()> {
        if config.api_key.is_empty() {
            anyhow::bail!("Qwen ASR API key is empty");
        }
        self.stt_config = Some(config.clone());
        self.audio_buffer.clear();
        Ok(())
    }

    async fn send_audio(&mut self, chunk: &[u8]) -> Result<()> {
        if self.audio_buffer.len() + chunk.len() > MAX_AUDIO_BYTES {
            anyhow::bail!("Qwen ASR: audio exceeds maximum length (~3 min)");
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
            return Ok(None);
        }

        let audio_len_secs = self.audio_buffer.len() as f64 / (config.sample_rate as f64 * 2.0);

        // Silence guard: measure the recorded signal (i16 LE PCM). When the mic
        // captured essentially nothing — wrong/dead input device, Bluetooth
        // headset in its case, muted mic — ASR models don't return empty; they
        // HALLUCINATE a stock phrase ("Thank you.", "Thanks for watching."). So
        // we detect near-silence here and return None, which the pipeline shows
        // as an honest "No speech detected" instead of a bogus transcript.
        let (peak, rms) = {
            let mut peak: i32 = 0;
            let mut sum_sq: f64 = 0.0;
            let mut n: usize = 0;
            for pair in self.audio_buffer.chunks_exact(2) {
                let s = i16::from_le_bytes([pair[0], pair[1]]) as i32;
                peak = peak.max(s.abs());
                sum_sq += (s as f64) * (s as f64);
                n += 1;
            }
            let rms = if n > 0 { (sum_sq / n as f64).sqrt() } else { 0.0 };
            (peak, rms)
        };
        // Peak well under ~1.5% of full scale over the whole take means there
        // was no real speech. Conservative — ordinary quiet speech far exceeds it.
        const SILENCE_PEAK_THRESHOLD: i32 = 500;
        if peak < SILENCE_PEAK_THRESHOLD {
            tracing::warn!(
                "Qwen ASR: audio is near-silent (peak={}, rms={:.1}, {:.1}s) — skipping STT \
                 to avoid a hallucinated transcript. Check the input microphone.",
                peak,
                rms,
                audio_len_secs
            );
            self.audio_buffer.clear();
            return Ok(None);
        }

        let wav_data = Self::build_wav(&self.audio_buffer, config.sample_rate);
        self.audio_buffer.clear();

        tracing::info!(
            "Qwen ASR: sending {:.1}s of audio (peak={}, rms={:.1})",
            audio_len_secs,
            peak,
            rms
        );

        let b64 = base64::engine::general_purpose::STANDARD.encode(&wav_data);
        let audio_uri = format!("data:audio/wav;base64,{b64}");

        // Build language hint. DashScope accepts "zh", "en", etc.; omit for auto-detect.
        let language = config
            .language
            .as_deref()
            .filter(|l| *l != "multi" && !l.is_empty())
            .map(|l| l.to_string());

        let mut asr_options = serde_json::json!({ "enable_itn": true });
        if let Some(ref lang) = language {
            asr_options["language"] = serde_json::Value::String(lang.clone());
        }

        // DashScope native format: audio content goes under input.messages, not top-level messages.
        let body = serde_json::json!({
            "model": MODEL,
            "input": {
                "messages": [{
                    "role": "user",
                    "content": [{ "type": "audio", "audio": audio_uri }]
                }]
            },
            "parameters": asr_options
        });

        let resp = self
            .client
            .post(ENDPOINT)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;

        if !status.is_success() {
            let preview: String = text.chars().take(200).collect();
            tracing::error!("Qwen ASR HTTP {}: {}", status, preview);
            anyhow::bail!("Qwen ASR error ({}): {}", status, preview);
        }

        let v: serde_json::Value = serde_json::from_str(&text)?;
        // Truncate by CHARS, not bytes: the response is UTF-8 (Chinese is 3
        // bytes/char), so a byte slice like &text[..500] panics when byte 500
        // lands mid-character — which is exactly what froze long recordings
        // (longer transcript → response > 500 bytes → slice → panic → the STT
        // task dies without signalling stt_done → stop() hangs on "Transcribing").
        let preview: String = text.chars().take(500).collect();
        tracing::debug!("Qwen ASR raw response: {}", preview);

        // DashScope returns content as either a plain string or an array of {type,text} objects.
        let content = &v["output"]["choices"][0]["message"]["content"];
        let transcript = if let Some(s) = content.as_str() {
            s.trim().to_string()
        } else if let Some(arr) = content.as_array() {
            // [{type:"text", text:"..."}] format
            arr.iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string()
        } else {
            let preview: String = text.chars().take(300).collect();
            anyhow::bail!("Qwen ASR: unexpected response shape: {}", preview);
        };

        if transcript.is_empty() {
            return Ok(None);
        }

        tracing::info!("Qwen ASR transcription: {} chars", transcript.len());
        Ok(Some(transcript))
    }

    fn name(&self) -> &str {
        "Qwen3-ASR-Flash"
    }
}
