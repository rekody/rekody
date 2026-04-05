//! Speech-to-text engines for Chamgei.
//!
//! Provides a trait-based abstraction over STT backends:
//! - Local Whisper inference via whisper-rs
//! - Cloud Whisper via Groq API (optional)

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, info};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Suppress whisper.cpp's C-level stderr output temporarily.
/// Returns a guard that restores stderr when dropped.
#[cfg(unix)]
fn suppress_stderr() -> Option<SuppressStderr> {
    use std::os::unix::io::AsRawFd;
    let stderr_fd = std::io::stderr().as_raw_fd();
    let saved_fd = unsafe { libc::dup(stderr_fd) };
    if saved_fd < 0 {
        return None;
    }
    let devnull = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .ok()?;
    unsafe { libc::dup2(devnull.as_raw_fd(), stderr_fd) };
    Some(SuppressStderr {
        saved_fd,
        stderr_fd,
    })
}

#[cfg(unix)]
struct SuppressStderr {
    saved_fd: i32,
    stderr_fd: i32,
}

#[cfg(unix)]
impl Drop for SuppressStderr {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved_fd, self.stderr_fd);
            libc::close(self.saved_fd);
        }
    }
}

#[cfg(not(unix))]
fn suppress_stderr() -> Option<()> {
    None
}

#[derive(Debug, Error)]
pub enum SttError {
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("transcription failed: {0}")]
    TranscriptionFailed(String),
    #[error("API error: {0}")]
    ApiError(String),
}

/// Raw transcription result from an STT engine.
#[derive(Debug, Clone)]
pub struct Transcript {
    /// The raw transcribed text.
    pub text: String,
    /// Transcription latency in milliseconds.
    pub latency_ms: u64,
}

/// Trait for speech-to-text engines.
pub trait SttEngine: Send + Sync {
    /// Transcribe audio samples (16kHz mono f32) to text.
    fn transcribe(
        &self,
        samples: &[f32],
    ) -> impl std::future::Future<Output = Result<Transcript>> + Send;
}

/// Available Whisper model sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhisperModel {
    /// ~75MB, fastest, lowest accuracy.
    Tiny,
    /// ~250MB, good balance (default).
    #[default]
    Small,
    /// ~750MB, better accuracy.
    Medium,
    /// ~1.5GB, best accuracy.
    Large,
}

impl WhisperModel {
    /// Returns the expected GGML model filename for this model size.
    ///
    /// Uses English-only variants where available for better performance.
    pub fn file_name(self) -> &'static str {
        match self {
            WhisperModel::Tiny => "ggml-tiny.en.bin",
            WhisperModel::Small => "ggml-small.en.bin",
            WhisperModel::Medium => "ggml-medium.en.bin",
            WhisperModel::Large => "ggml-large.bin",
        }
    }
}

/// Local Whisper STT engine using whisper.cpp via whisper-rs.
///
/// Loads a GGML Whisper model and performs on-device transcription.
/// On macOS, GPU acceleration via Metal is used automatically when
/// available through whisper-rs defaults.
pub struct LocalWhisperEngine {
    model: WhisperModel,
    ctx: WhisperContext,
}

// Safety: WhisperContext internally manages thread safety for the whisper.cpp
// context. We only call into it via `create_state()` which produces an
// independent state object, so sharing the context across threads is safe.
unsafe impl Send for LocalWhisperEngine {}
unsafe impl Sync for LocalWhisperEngine {}

impl LocalWhisperEngine {
    /// Create a new local Whisper engine.
    ///
    /// # Arguments
    /// * `model` - The Whisper model size to use.
    /// * `model_path` - Path to the GGML model file on disk.
    ///
    /// # Errors
    /// Returns `SttError::ModelNotFound` if the model file does not exist or
    /// cannot be loaded by whisper-rs.
    pub fn new(model: WhisperModel, model_path: &str) -> Result<Self> {
        let path = Path::new(model_path);
        if !path.exists() {
            return Err(SttError::ModelNotFound(format!(
                "model file not found at: {}",
                model_path
            ))
            .into());
        }

        info!(
            model_size = ?model,
            path = model_path,
            "loading whisper model"
        );

        let ctx_params = WhisperContextParameters::default();
        let _guard = suppress_stderr(); // suppress whisper.cpp C-level output
        let ctx = WhisperContext::new_with_params(model_path, ctx_params).map_err(|e| {
            SttError::ModelNotFound(format!(
                "failed to load whisper model at {}: {}",
                model_path, e
            ))
        })?;
        drop(_guard); // restore stderr

        info!("whisper model loaded successfully");

        Ok(Self { model, ctx })
    }

    /// Build [`FullParams`] with sensible low-latency defaults for dictation.
    fn build_params(&self) -> FullParams<'_, '_> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        // Language: English only for lower latency
        params.set_language(Some("en"));

        // Single-segment mode for minimal latency
        params.set_single_segment(true);

        // No timestamps needed for dictation
        params.set_print_timestamps(false);
        params.set_token_timestamps(false);

        // Suppress non-speech tokens (reduce hallucinations on silence)
        params.set_suppress_non_speech_tokens(true);

        // Disable printing to stdout — we capture via the API
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);

        // Use all available performance cores
        let num_cpus = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);
        params.set_n_threads(num_cpus);

        params
    }
}

impl SttEngine for LocalWhisperEngine {
    async fn transcribe(&self, samples: &[f32]) -> Result<Transcript> {
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                latency_ms: 0,
            });
        }

        debug!(
            num_samples = samples.len(),
            duration_secs = samples.len() as f64 / 16000.0,
            model = ?self.model,
            "starting transcription"
        );

        let start = Instant::now();
        let _guard = suppress_stderr(); // suppress whisper.cpp/Metal C-level output

        // Create an independent state for this transcription call.
        // This allows concurrent transcriptions from different async tasks
        // without locking, since each state is independent.
        let mut state = self.ctx.create_state().map_err(|e| {
            SttError::TranscriptionFailed(format!("failed to create whisper state: {}", e))
        })?;

        let params = self.build_params();

        // Run the full whisper inference pipeline.
        state.full(params, samples).map_err(|e| {
            SttError::TranscriptionFailed(format!("whisper inference failed: {}", e))
        })?;

        // Collect all segments into the output text.
        let n_segments = state.full_n_segments().map_err(|e| {
            SttError::TranscriptionFailed(format!("failed to get segment count: {}", e))
        })?;

        let mut text = String::new();
        for i in 0..n_segments {
            let segment_text = state.full_get_segment_text(i).map_err(|e| {
                SttError::TranscriptionFailed(format!(
                    "failed to get text for segment {}: {}",
                    i, e
                ))
            })?;
            text.push_str(&segment_text);
        }

        let latency_ms = start.elapsed().as_millis() as u64;

        // Trim leading/trailing whitespace that whisper often produces
        let text = text.trim().to_string();

        info!(
            latency_ms,
            text_len = text.len(),
            segments = n_segments,
            "transcription complete"
        );

        Ok(Transcript { text, latency_ms })
    }
}

// ---------------------------------------------------------------------------
// Groq Cloud Whisper Engine
// ---------------------------------------------------------------------------

/// Response payload from the Groq transcription API.
#[derive(Debug, Deserialize)]
struct GroqTranscriptionResponse {
    text: String,
}

/// Cloud-based STT engine that sends audio to Groq's Whisper API.
///
/// Requires a valid Groq API key. Audio is encoded as a WAV file in memory
/// and uploaded via multipart/form-data.
pub struct GroqWhisperEngine {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GroqWhisperEngine {
    /// Create a new Groq Whisper engine.
    ///
    /// # Arguments
    /// * `api_key` - Groq API key for authentication.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "whisper-large-v3".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create a new Groq Whisper engine with a custom model name.
    ///
    /// # Arguments
    /// * `api_key` - Groq API key for authentication.
    /// * `model` - The Whisper model identifier to use.
    pub fn with_model(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::Client::new(),
        }
    }
}

/// Encode f32 samples (16 kHz mono) as a WAV file in memory (PCM 16-bit).
fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let num_samples = samples.len();
    let data_size = (num_samples * 2) as u32; // 2 bytes per i16 sample
    let file_size = 36 + data_size; // total file size minus 8-byte RIFF header preamble

    let mut buf = Vec::with_capacity(44 + data_size as usize);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt sub-chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&16000u32.to_le_bytes()); // sample rate
    buf.extend_from_slice(&32000u32.to_le_bytes()); // byte rate (16000 * 1 * 2)
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align (1 * 2)
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data sub-chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());

    for &s in samples {
        // Clamp to [-1.0, 1.0] then scale to i16 range
        let clamped = s.clamp(-1.0, 1.0);
        let val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&val.to_le_bytes());
    }

    buf
}

/// Build a multipart/form-data body manually (avoids the `multipart` feature).
///
/// Returns `(content_type_header, body_bytes)`.
fn build_multipart_body(wav_data: &[u8], model: &str) -> (String, Vec<u8>) {
    let boundary = "----ChamgeiBoundary9876543210";
    let mut body = Vec::new();

    // file field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav_data);
    body.extend_from_slice(b"\r\n");

    // model field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(model.as_bytes());
    body.extend_from_slice(b"\r\n");

    // language field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"language\"\r\n\r\n");
    body.extend_from_slice(b"en\r\n");

    // response_format field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
    body.extend_from_slice(b"json\r\n");

    // closing boundary
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let content_type = format!("multipart/form-data; boundary={boundary}");
    (content_type, body)
}

impl SttEngine for GroqWhisperEngine {
    async fn transcribe(&self, samples: &[f32]) -> Result<Transcript> {
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                latency_ms: 0,
            });
        }

        debug!(
            num_samples = samples.len(),
            duration_secs = samples.len() as f64 / 16000.0,
            model = %self.model,
            "starting Groq cloud transcription"
        );

        let start = Instant::now();

        // Encode samples to WAV in memory
        let wav_data = encode_wav(samples);

        // Build the multipart body
        let (content_type, body) = build_multipart_body(&wav_data, &self.model);

        // Send to Groq API
        let response = self
            .client
            .post("https://api.groq.com/openai/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", content_type)
            .body(body)
            .send()
            .await
            .map_err(|e| SttError::ApiError(format!("request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read response body".to_string());
            return Err(SttError::ApiError(format!(
                "Groq API returned {}: {}",
                status, error_body
            ))
            .into());
        }

        let groq_resp: GroqTranscriptionResponse = response
            .json()
            .await
            .map_err(|e| SttError::ApiError(format!("failed to parse response: {}", e)))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let text = groq_resp.text.trim().to_string();

        info!(
            latency_ms,
            text_len = text.len(),
            model = %self.model,
            "Groq cloud transcription complete"
        );

        Ok(Transcript { text, latency_ms })
    }
}

// ---------------------------------------------------------------------------
// Cohere Local STT Engine
// ---------------------------------------------------------------------------

/// Response from the Cohere local transcription server.
#[derive(Debug, Deserialize)]
struct CohereTranscriptionResponse {
    text: String,
}

/// Local STT engine that connects to a Cohere transcription server.
///
/// Sends audio as a WAV file via multipart/form-data POST to a local HTTP
/// server running at `http://localhost:{port}/transcribe`.
pub struct CohereLocalEngine {
    port: u16,
    client: reqwest::Client,
}

impl CohereLocalEngine {
    /// Create a new Cohere local STT engine.
    ///
    /// # Arguments
    /// * `port` - The port the local Cohere transcription server listens on.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            client: reqwest::Client::new(),
        }
    }
}

impl SttEngine for CohereLocalEngine {
    async fn transcribe(&self, samples: &[f32]) -> Result<Transcript> {
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                latency_ms: 0,
            });
        }

        debug!(
            num_samples = samples.len(),
            duration_secs = samples.len() as f64 / 16000.0,
            port = self.port,
            "starting Cohere local transcription"
        );

        let start = Instant::now();
        let wav_data = encode_wav(samples);

        // Build a simple multipart body with just the audio file.
        let boundary = "----ChamgeiBoundary9876543210";
        let mut body = Vec::new();

        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
        body.extend_from_slice(&wav_data);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let content_type = format!("multipart/form-data; boundary={boundary}");
        let url = format!("http://localhost:{}/transcribe", self.port);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", content_type)
            .body(body)
            .send()
            .await
            .map_err(|e| SttError::ApiError(format!("request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read response body".to_string());
            return Err(SttError::ApiError(format!(
                "Cohere local server returned {}: {}",
                status, error_body
            ))
            .into());
        }

        let cohere_resp: CohereTranscriptionResponse = response
            .json()
            .await
            .map_err(|e| SttError::ApiError(format!("failed to parse response: {}", e)))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let text = cohere_resp.text.trim().to_string();

        info!(
            latency_ms,
            text_len = text.len(),
            port = self.port,
            "Cohere local transcription complete"
        );

        Ok(Transcript { text, latency_ms })
    }
}

// ---------------------------------------------------------------------------
// Deepgram Cloud STT Engine
// ---------------------------------------------------------------------------

/// Response from Deepgram's speech-to-text API.
#[derive(Debug, Deserialize)]
struct DeepgramResponse {
    results: Option<DeepgramResults>,
}

#[derive(Debug, Deserialize)]
struct DeepgramResults {
    channels: Vec<DeepgramChannel>,
}

#[derive(Debug, Deserialize)]
struct DeepgramChannel {
    alternatives: Vec<DeepgramAlternative>,
}

#[derive(Debug, Deserialize)]
struct DeepgramAlternative {
    transcript: String,
}

/// Cloud-based STT engine using Deepgram's Nova-3 API.
///
/// Sends audio as a WAV file to Deepgram's `/v1/listen` endpoint.
/// Requires a valid Deepgram API key (get one at https://console.deepgram.com).
pub struct DeepgramEngine {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl DeepgramEngine {
    /// Create a new Deepgram STT engine with the default Nova-3 model.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "nova-3".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create a new Deepgram engine with a custom model.
    pub fn with_model(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::Client::new(),
        }
    }
}

impl SttEngine for DeepgramEngine {
    async fn transcribe(&self, samples: &[f32]) -> Result<Transcript> {
        if samples.is_empty() {
            return Ok(Transcript {
                text: String::new(),
                latency_ms: 0,
            });
        }

        debug!(
            num_samples = samples.len(),
            duration_secs = samples.len() as f64 / 16000.0,
            model = %self.model,
            "starting Deepgram transcription"
        );

        let start = Instant::now();
        let wav_data = encode_wav(samples);

        let url = format!(
            "https://api.deepgram.com/v1/listen?model={}&language=en&smart_format=true&punctuate=true",
            self.model
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", "audio/wav")
            .body(wav_data)
            .send()
            .await
            .map_err(|e| SttError::ApiError(format!("request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(
                SttError::ApiError(format!("Deepgram returned {}: {}", status, body)).into(),
            );
        }

        let dg_resp: DeepgramResponse = response
            .json()
            .await
            .map_err(|e| SttError::ApiError(format!("failed to parse response: {}", e)))?;

        let text = dg_resp
            .results
            .and_then(|r| r.channels.into_iter().next())
            .and_then(|c| c.alternatives.into_iter().next())
            .map(|a| a.transcript)
            .unwrap_or_default()
            .trim()
            .to_string();

        let latency_ms = start.elapsed().as_millis() as u64;

        info!(
            latency_ms,
            text_len = text.len(),
            model = %self.model,
            "Deepgram transcription complete"
        );

        Ok(Transcript { text, latency_ms })
    }
}
