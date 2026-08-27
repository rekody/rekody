//! Speech-to-text engines for rekody.
//!
//! Provides a trait-based abstraction over STT backends:
//! - Local Whisper inference via whisper-rs
//! - Cloud Whisper via Groq API (optional)

use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

/// Nemotron cache-aware streaming engine (feature `nemotron`).
#[cfg(feature = "nemotron")]
pub mod nemotron;

/// Decode-time dictionary term biasing for the Nemotron streaming engine
/// (feature `nemotron`): SentencePiece term encoding plus the trie-based
/// logits processor. Spec: docs/design/nemotron-term-biasing-spec.md.
#[cfg(feature = "nemotron")]
pub mod biasing;

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
    /// A user-supplied endpoint that Rekody will not send audio to.
    #[error("{0}")]
    InvalidEndpoint(String),
    /// The endpoint answered, but not with an OpenAI transcription response.
    /// Kept separate from [`SttError::ApiError`] so a misconfigured custom
    /// endpoint reads as "this is not a transcription API" rather than as a
    /// raw serde parse failure.
    #[error("{0}")]
    UnexpectedResponse(String),
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

    /// Bias decoding toward custom vocabulary terms for subsequent
    /// transcriptions. The pipeline calls this with the user's personal
    /// dictionary before each dictation; engines without a biasing
    /// mechanism keep this default no-op. An empty slice clears any
    /// previously set terms.
    fn set_bias_terms(&self, _terms: &[String]) {}
}

// ---------------------------------------------------------------------------
// Dictionary term biasing
// ---------------------------------------------------------------------------

/// Byte cap for the terms-only Whisper biasing prompt (local decoder context
/// and the Groq `prompt` field). Whisper conditions on at most 224 prompt
/// tokens and keeps the TAIL when the prompt is longer, which would silently
/// drop the user's first terms; 800 bytes stays safely inside that window.
/// Bytes are at least as strict as characters, so multibyte terms only make
/// the prompt shorter.
const MAX_BIAS_PROMPT_BYTES: usize = 800;

/// Join dictionary terms into a Whisper biasing prompt.
///
/// A Whisper prompt is decoder context, not an instruction: the model
/// conditions on it as if that text preceded the audio, so the output is the
/// terms joined with ", " and nothing else. Terms are taken in file order
/// until the next one would pass [`MAX_BIAS_PROMPT_BYTES`]. Interior null
/// bytes are stripped because whisper-rs panics on them. Returns `None` when
/// no usable terms exist, so callers skip the prompt entirely.
fn build_bias_prompt(terms: &[String]) -> Option<String> {
    let mut prompt = String::new();
    for term in terms {
        let term = term.trim().replace('\0', "");
        if term.is_empty() {
            continue;
        }
        let sep_len = if prompt.is_empty() { 0 } else { 2 };
        if prompt.len() + sep_len + term.len() > MAX_BIAS_PROMPT_BYTES {
            break;
        }
        if !prompt.is_empty() {
            prompt.push_str(", ");
        }
        prompt.push_str(&term);
    }
    (!prompt.is_empty()).then_some(prompt)
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
    /// ~574MB, distilled large-v3 with 4 decoder layers (~8x faster decode)
    /// quantized to 5-bit. Near-large accuracy at ~1/3 the size. Recommended
    /// for most users who want fast + accurate.
    Turbo,
    /// ~1.5GB, best accuracy.
    Large,
}

impl WhisperModel {
    /// Returns the GGML model filename.
    ///
    /// rekody uses the multilingual variants for all models so that 100+
    /// languages work without re-downloading. The `.en`-only variants are not
    /// downloaded or used.
    pub fn file_name(self) -> &'static str {
        self.multilingual_file_name()
    }

    /// Returns the multilingual GGML model filename.
    ///
    /// Multilingual variants support auto language detection across 99+ languages.
    /// Download from: https://huggingface.co/ggerganov/whisper.cpp
    pub fn multilingual_file_name(self) -> &'static str {
        match self {
            WhisperModel::Tiny => "ggml-tiny.bin",
            WhisperModel::Small => "ggml-small.bin",
            WhisperModel::Medium => "ggml-medium.bin",
            WhisperModel::Turbo => "ggml-large-v3-turbo-q5_0.bin",
            WhisperModel::Large => "ggml-large.bin",
        }
    }
}

/// Local Whisper STT engine using whisper.cpp via whisper-rs.
///
/// Loads a GGML Whisper model and performs on-device transcription.
///
/// Acceleration, precisely: whisper-rs 0.13 ships `default = []` and its
/// build script sets `GGML_METAL=OFF`, so Metal is never compiled in on any
/// target. The only accelerator Rekody enables is Core ML, and only on
/// macOS/aarch64 (see this crate's Cargo.toml), which puts the ENCODER on the
/// Neural Engine. Everywhere else, encoder and decoder both run on the CPU
/// with Accelerate BLAS. That is why model-size defaults are chosen per
/// architecture rather than globally (#141).
pub struct LocalWhisperEngine {
    model: WhisperModel,
    ctx: WhisperContext,
    /// BCP-47 language code to force (e.g. `"en"`, `"sw"`, `"fr"`).
    /// `None` enables auto language detection (requires a multilingual model file).
    language: Option<String>,
    /// Comma-joined dictionary terms set as the decoder's initial prompt,
    /// built by [`build_bias_prompt`]. `None` = no biasing. Behind a mutex
    /// because `transcribe` takes `&self` and the pipeline refreshes the
    /// terms per dictation.
    bias_prompt: Mutex<Option<String>>,
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
    /// Defaults to English-only transcription. Use [`LocalWhisperEngine::with_language`]
    /// to enable auto-detection or a specific language.
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

        Ok(Self {
            model,
            ctx,
            language: Some("en".to_string()),
            bias_prompt: Mutex::new(None),
        })
    }

    /// Create a new local Whisper engine with a specific language or auto-detection.
    ///
    /// # Arguments
    /// * `model` - The Whisper model size to use.
    /// * `model_path` - Path to the GGML model file on disk. For auto-detection or
    ///   non-English languages, use a **multilingual** model (`ggml-tiny.bin`, not
    ///   `ggml-tiny.en.bin`). See [`WhisperModel::multilingual_file_name`].
    /// * `language` - BCP-47 language code to force (e.g. `"sw"` for Swahili), or
    ///   `None` to auto-detect the spoken language.
    ///
    /// # Errors
    /// Returns `SttError::ModelNotFound` if the model file does not exist.
    pub fn with_language(
        model: WhisperModel,
        model_path: &str,
        language: Option<String>,
    ) -> Result<Self> {
        let mut engine = Self::new(model, model_path)?;
        engine.language = language;
        Ok(engine)
    }

    /// Build [`FullParams`] tuned for the audio length being transcribed.
    ///
    /// `long_audio` selects the decoding profile:
    /// - `false` (≤25 s): single-segment, no timestamps — minimal latency
    ///   for the push-to-talk dictation path.
    /// - `true` (>25 s): multi-segment with whisper.cpp's sliding-window
    ///   mechanism, timestamps enabled, and standard hallucination guards
    ///   (`no_speech_thold`, `logprob_thold`). Required because
    ///   `single_segment` truncates anything past Whisper's 30 s window.
    fn build_params(&self, long_audio: bool) -> FullParams<'_, '_> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        // Set language: None enables Whisper's built-in auto language detection.
        params.set_language(self.language.as_deref());

        // Bias decoding toward the user's dictionary terms: Whisper treats
        // the initial prompt as text that preceded the audio, nudging it to
        // spell those terms correctly. whisper-rs copies the string into the
        // params, so the lock guard is released right after this call.
        if let Ok(guard) = self.bias_prompt.lock()
            && let Some(prompt) = guard.as_deref()
        {
            params.set_initial_prompt(prompt);
        }

        if long_audio {
            // Let whisper.cpp run multi-segment with its sliding-window decoder.
            // Timestamps anchor segment-boundary decisions across the windows.
            params.set_single_segment(false);
            params.set_print_timestamps(true);
            params.set_token_timestamps(true);

            // Standard Whisper hallucination guards. These thresholds match
            // the upstream whisper.cpp defaults for long-form audio and help
            // suppress repeated/looped output on silence or noise.
            // (Methods are named `_thold` in whisper-rs 0.13.)
            params.set_no_speech_thold(0.6);
            params.set_logprob_thold(-1.0);
        } else {
            // Single-segment mode for minimal latency on short dictation clips.
            params.set_single_segment(true);

            // No timestamps needed for dictation
            params.set_print_timestamps(false);
            params.set_token_timestamps(false);
        }

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

        // whisper.cpp processes audio at 16 kHz. The C engine has a 30 s window
        // when `single_segment` is set; pick a 25 s threshold so we leave a
        // small buffer below that limit before switching to multi-segment.
        const LONG_AUDIO_THRESHOLD_SECS: f64 = 25.0;
        let duration_secs = samples.len() as f64 / 16000.0;
        let long_audio = duration_secs > LONG_AUDIO_THRESHOLD_SECS;

        debug!(
            num_samples = samples.len(),
            duration_secs,
            model = ?self.model,
            long_audio,
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

        let params = self.build_params(long_audio);

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

    fn set_bias_terms(&self, terms: &[String]) {
        if let Ok(mut prompt) = self.bias_prompt.lock() {
            *prompt = build_bias_prompt(terms);
        }
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible transcription engine (Groq, OpenAI, Together, vLLM, ...)
// ---------------------------------------------------------------------------

/// Response payload from an OpenAI-compatible transcription endpoint.
///
/// `response_format=json` is documented to return exactly `{"text": "..."}`.
/// Extra fields are ignored, so an endpoint that returns the verbose shape
/// still works as long as it carries `text`.
#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// Path every OpenAI-compatible transcription API exposes, relative to the
/// API base URL.
const TRANSCRIPTIONS_PATH: &str = "/audio/transcriptions";

/// Groq's API base. Groq is a preset of the generic engine, not a separate
/// implementation: it was already the OpenAI-compatible shape.
const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

/// Whisper model Groq serves.
const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3";

/// Longest error body echoed back to the user. Enough to recognise an HTML
/// login page or a JSON error, short enough not to flood a notification.
const MAX_ERROR_BODY_CHARS: usize = 300;

/// Is this host on the loopback interface?
///
/// The one case where plain http is acceptable: a transcription server the
/// user runs on their own machine never puts audio on a network.
fn is_loopback_host(host: &str) -> bool {
    // Strip the brackets reqwest keeps on IPv6 literals.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    // `localhost` and anything under it (`foo.localhost`) resolve to loopback
    // by RFC 6761.
    bare == "localhost" || bare.ends_with(".localhost")
}

/// Turn a user-supplied API base URL into the transcription endpoint Rekody
/// will POST audio to, or refuse with a message that says what is wrong.
///
/// The user's voice goes to this address, so the rules are strict and the
/// refusals are specific:
///
/// * https is required, because audio in flight must be encrypted;
/// * plain http is allowed only on loopback, so self-hosting still works;
/// * a base URL already ending in `/audio/transcriptions` is taken as-is,
///   because pasting the full endpoint is the obvious mistake to forgive.
///
/// Never called with anything but user input, and never logs it beyond the
/// host, which is what the UI shows anyway.
pub fn resolve_transcription_endpoint(base_url: &str) -> Result<reqwest::Url, SttError> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(SttError::InvalidEndpoint(
            "no API base URL is set for the custom speech-to-text provider. \
             Add one in Settings, or set custom_stt_base_url in config.toml."
                .to_string(),
        ));
    }

    let url = reqwest::Url::parse(trimmed).map_err(|_| {
        SttError::InvalidEndpoint(format!(
            "\"{trimmed}\" is not a valid URL. It should look like \
             https://api.openai.com/v1"
        ))
    })?;

    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(SttError::InvalidEndpoint(format!(
            "Rekody only sends audio over https (or http on localhost), \
             and this URL uses {scheme}."
        )));
    }

    let host = url
        .host_str()
        .ok_or_else(|| {
            SttError::InvalidEndpoint(format!("\"{trimmed}\" has no host to send audio to."))
        })?
        .to_string();

    // Credentials in the URL are dropped when the endpoint is rebuilt below,
    // which would show up as an unexplained 401. Say so instead, and keep
    // secrets out of a string that gets displayed in Settings and logged as
    // a destination host.
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SttError::InvalidEndpoint(
            "put the API key in the key field, not in the URL. Rekody sends it \
             as an Authorization header."
                .to_string(),
        ));
    }

    if scheme == "http" && !is_loopback_host(&host) {
        return Err(SttError::InvalidEndpoint(format!(
            "Rekody will not send your voice to {host} over plain http, \
             which anyone on the network can read. Use https, or http on \
             localhost for a server you run yourself."
        )));
    }

    // Forgive a pasted full endpoint; otherwise append the standard path.
    let path = url.path().trim_end_matches('/');
    if path.ends_with(TRANSCRIPTIONS_PATH) {
        return Ok(url);
    }
    // The query is carried over: an Azure-style endpoint puts its api-version
    // there, and dropping it turns into an unexplained 404 from the server.
    let query = match url.query() {
        Some(q) if !q.is_empty() => format!("?{q}"),
        _ => String::new(),
    };
    let joined = format!(
        "{}://{}{}{}{}",
        scheme,
        authority(&url),
        path,
        TRANSCRIPTIONS_PATH,
        query
    );
    reqwest::Url::parse(&joined).map_err(|_| {
        SttError::InvalidEndpoint(format!(
            "could not build a transcription endpoint from \"{trimmed}\"."
        ))
    })
}

/// `host` or `host:port`, as it belongs in a URL.
fn authority(url: &reqwest::Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        (None, _) => String::new(),
    }
}

/// Cloud (or self-hosted) STT engine speaking OpenAI's
/// `/v1/audio/transcriptions`.
///
/// One implementation covers Groq, OpenAI, Together, Fireworks, a local
/// vLLM or LM Studio, and anything else that answers the same shape. Audio
/// is encoded as a WAV file in memory and uploaded via multipart/form-data.
pub struct OpenAiCompatEngine {
    /// Provider name for logs and error messages, e.g. `"Groq"`. Never a key.
    label: String,
    /// Full transcriptions endpoint, already validated.
    endpoint: reqwest::Url,
    api_key: String,
    model: String,
    client: reqwest::Client,
    /// BCP-47 language code hint (e.g. `"en"`, `"sw"`). `None` = auto-detect.
    language: Option<String>,
    /// Comma-joined dictionary terms sent as the multipart `prompt` field,
    /// built by [`build_bias_prompt`]. `None` = field omitted.
    bias_prompt: Mutex<Option<String>>,
}

impl OpenAiCompatEngine {
    /// Groq preset: Groq's own base URL and Whisper Large v3.
    ///
    /// Kept as a named constructor so `stt_engine = "groq"` configs that
    /// predate the generic engine behave byte for byte as they did.
    pub fn groq(api_key: String, language: Option<String>) -> Self {
        Self::preset(
            "Groq",
            GROQ_BASE_URL,
            GROQ_DEFAULT_MODEL.to_string(),
            api_key,
            language,
        )
    }

    /// Groq preset with a different model name on Groq's endpoint.
    pub fn groq_with_model(api_key: String, model: String) -> Self {
        Self::preset("Groq", GROQ_BASE_URL, model, api_key, None)
    }

    /// A preset whose base URL Rekody chose, so validation cannot fail.
    fn preset(
        label: &str,
        base_url: &str,
        model: String,
        api_key: String,
        language: Option<String>,
    ) -> Self {
        let endpoint =
            resolve_transcription_endpoint(base_url).expect("built-in preset base URLs are valid");
        Self::build(label.to_string(), endpoint, model, api_key, language)
    }

    /// An endpoint the user supplied. Fails before any audio is recorded
    /// rather than at the moment they speak.
    pub fn custom(
        base_url: &str,
        model: String,
        api_key: String,
        language: Option<String>,
    ) -> Result<Self, SttError> {
        let endpoint = resolve_transcription_endpoint(base_url)?;
        if model.trim().is_empty() {
            return Err(SttError::InvalidEndpoint(
                "the custom speech-to-text provider needs a model name, \
                 for example whisper-1."
                    .to_string(),
            ));
        }
        let label = endpoint.host_str().unwrap_or("the endpoint").to_string();
        Ok(Self::build(label, endpoint, model, api_key, language))
    }

    fn build(
        label: String,
        endpoint: reqwest::Url,
        model: String,
        api_key: String,
        language: Option<String>,
    ) -> Self {
        Self {
            label,
            endpoint,
            api_key,
            model,
            client: reqwest::Client::new(),
            language,
            bias_prompt: Mutex::new(None),
        }
    }

    /// The host this engine sends audio to. Surfaced in the UI before the
    /// first recording so nobody has to read a config file to find out where
    /// their voice is going.
    pub fn destination_host(&self) -> &str {
        self.endpoint.host_str().unwrap_or("")
    }

    /// The full endpoint, for diagnostics such as `rekody doctor`.
    pub fn endpoint(&self) -> &reqwest::Url {
        &self.endpoint
    }
}

/// Hand-written so the API key can never reach a log line, a panic message,
/// or a `{:?}` in someone's debugging session. Mirrors `ProviderConfig`.
impl std::fmt::Debug for OpenAiCompatEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatEngine")
            .field("label", &self.label)
            .field("endpoint", &self.endpoint.as_str())
            .field("model", &self.model)
            .field("language", &self.language)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "[empty]"
                } else {
                    "[REDACTED]"
                },
            )
            .finish()
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
/// `language` is an optional BCP-47 code (e.g. `"en"`, `"sw"`). When `None`,
/// the language field is omitted and Groq Whisper auto-detects the language.
///
/// `prompt` is an optional terms-only biasing prompt (see
/// [`build_bias_prompt`]). When `None`, the prompt field is omitted.
///
/// Returns `(content_type_header, body_bytes)`.
fn build_multipart_body(
    wav_data: &[u8],
    model: &str,
    language: Option<&str>,
    prompt: Option<&str>,
) -> (String, Vec<u8>) {
    let boundary = "----RekodyBoundary9876543210";
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

    // language field — only included when explicitly set; omitting it triggers auto-detection
    if let Some(lang) = language {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"language\"\r\n\r\n");
        body.extend_from_slice(lang.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    // prompt field: dictionary terms as decoder context, biasing Whisper
    // toward the user's vocabulary. Omitted when the dictionary is empty.
    if let Some(prompt) = prompt {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"prompt\"\r\n\r\n");
        body.extend_from_slice(prompt.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    // response_format field
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
    body.extend_from_slice(b"json\r\n");

    // closing boundary
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let content_type = format!("multipart/form-data; boundary={boundary}");
    (content_type, body)
}

/// Shorten a response body for an error message, on a character boundary.
fn snippet(body: &str) -> String {
    let clean = body.trim();
    if clean.is_empty() {
        return "an empty body".to_string();
    }
    let mut out: String = clean.chars().take(MAX_ERROR_BODY_CHARS).collect();
    if out.chars().count() < clean.chars().count() {
        out.push('…');
    }
    out
}

impl SttEngine for OpenAiCompatEngine {
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
            host = %self.destination_host(),
            "starting cloud transcription"
        );

        let start = Instant::now();

        // Encode samples to WAV in memory
        let wav_data = encode_wav(samples);

        // Build the multipart body (language=None → the provider
        // auto-detects; bias_prompt=None → no prompt field).
        let bias_prompt = self.bias_prompt.lock().ok().and_then(|guard| guard.clone());
        let (content_type, body) = build_multipart_body(
            &wav_data,
            &self.model,
            self.language.as_deref(),
            bias_prompt.as_deref(),
        );

        let mut request = self
            .client
            .post(self.endpoint.clone())
            .header("Content-Type", content_type)
            .body(body);
        // A self-hosted server usually wants no key at all. Sending an empty
        // bearer token makes some of them reject the request outright.
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let response = request.send().await.map_err(|e| {
            SttError::ApiError(format!(
                "could not reach {} at {}: {e}",
                self.label,
                self.destination_host()
            ))
        })?;

        let status = response.status();
        // Read as text first so a failure can quote what actually came back.
        // A misconfigured endpoint usually answers with HTML or a plain
        // error, and "expected value at line 1 column 1" helps nobody.
        let raw = response.text().await.map_err(|e| {
            SttError::ApiError(format!(
                "{} sent a response Rekody could not read: {e}",
                self.label
            ))
        })?;

        if !status.is_success() {
            return Err(SttError::ApiError(format!(
                "{} returned {}: {}",
                self.label,
                status,
                snippet(&raw)
            ))
            .into());
        }

        let parsed: TranscriptionResponse = serde_json::from_str(&raw).map_err(|_| {
            SttError::UnexpectedResponse(format!(
                "{} answered, but not with a transcription. Rekody expected \
                 JSON with a \"text\" field from {}, and got: {}",
                self.label,
                self.endpoint,
                snippet(&raw)
            ))
        })?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let text = parsed.text.trim().to_string();

        info!(
            latency_ms,
            text_len = text.len(),
            model = %self.model,
            host = %self.destination_host(),
            "cloud transcription complete"
        );

        Ok(Transcript { text, latency_ms })
    }

    fn set_bias_terms(&self, terms: &[String]) {
        if let Ok(mut prompt) = self.bias_prompt.lock() {
            *prompt = build_bias_prompt(terms);
        }
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
        let boundary = "----RekodyBoundary9876543210";
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
///
/// By default uses `language=multi` which enables Nova-3's real-time multilingual
/// detection across 100+ languages. Pass a specific BCP-47 code to pin to one
/// language (slightly faster and more accurate for that language).
pub struct DeepgramEngine {
    api_key: String,
    model: String,
    client: reqwest::Client,
    /// BCP-47 language code, or `"multi"` for auto-detection (default).
    language: String,
    /// Dictionary terms sent as one `keyterm` query param each, capped at
    /// [`DEEPGRAM_MAX_KEYTERMS`]. Empty = no biasing.
    bias_terms: Mutex<Vec<String>>,
}

impl DeepgramEngine {
    /// Create a new Deepgram STT engine with Nova-3 multilingual auto-detection.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model: "nova-3".to_string(),
            client: reqwest::Client::new(),
            language: "multi".to_string(),
            bias_terms: Mutex::new(Vec::new()),
        }
    }

    /// Create a new Deepgram engine with a custom model.
    pub fn with_model(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            client: reqwest::Client::new(),
            language: "multi".to_string(),
            bias_terms: Mutex::new(Vec::new()),
        }
    }

    /// Create a new Deepgram engine pinned to a specific language.
    ///
    /// Use a BCP-47 code (e.g. `"en"`, `"sw"`, `"fr"`) or `"multi"` for
    /// auto-detection. Pinning to a language is slightly faster and more accurate
    /// when you know the speaker's language in advance.
    pub fn with_language(api_key: String, language: String) -> Self {
        Self {
            api_key,
            model: "nova-3".to_string(),
            client: reqwest::Client::new(),
            language,
            bias_terms: Mutex::new(Vec::new()),
        }
    }
}

/// Maximum number of dictionary terms forwarded as Deepgram `keyterm` query
/// params. Covers any realistic personal dictionary while keeping the
/// request URL well under practical URL-length limits.
const DEEPGRAM_MAX_KEYTERMS: usize = 50;

/// Build the query parameters for Deepgram's `/v1/listen` endpoint.
///
/// Each dictionary term becomes one Nova-3 `keyterm` param, which boosts
/// recognition of rare words at decode time. Older models (nova-2 and
/// earlier) use the `keywords` param instead; rekody pins Deepgram to
/// Nova-3. Only the first [`DEEPGRAM_MAX_KEYTERMS`] terms are sent, in file
/// order. reqwest URL-encodes every pair, so multi-word terms and special
/// characters are safe.
fn build_deepgram_query<'a>(
    model: &'a str,
    language: &'a str,
    bias_terms: &'a [String],
) -> Vec<(&'static str, &'a str)> {
    let mut query: Vec<(&'static str, &'a str)> = vec![
        ("model", model),
        ("language", language),
        ("smart_format", "true"),
        ("punctuate", "true"),
    ];
    query.extend(
        bias_terms
            .iter()
            .take(DEEPGRAM_MAX_KEYTERMS)
            .map(|term| ("keyterm", term.as_str())),
    );
    query
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

        // Use reqwest query params so values are URL-encoded automatically,
        // preventing parameter injection if the config contains special characters.
        // language="multi" enables Nova-3's real-time multilingual auto-detection;
        // dictionary terms ride along as `keyterm` params (see
        // `build_deepgram_query`).
        let bias_terms = self
            .bias_terms
            .lock()
            .map(|terms| terms.clone())
            .unwrap_or_default();
        let response = self
            .client
            .post("https://api.deepgram.com/v1/listen")
            .query(&build_deepgram_query(
                &self.model,
                &self.language,
                &bias_terms,
            ))
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

    fn set_bias_terms(&self, terms: &[String]) {
        if let Ok(mut stored) = self.bias_terms.lock() {
            *stored = terms
                .iter()
                .map(|term| term.trim())
                .filter(|term| !term.is_empty())
                .map(str::to_owned)
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    // ----- build_bias_prompt -----

    #[test]
    fn bias_prompt_joins_terms_with_commas() {
        let prompt = build_bias_prompt(&terms(&["Rekody", "Chamgei", "Core ML"]));
        assert_eq!(prompt.as_deref(), Some("Rekody, Chamgei, Core ML"));
    }

    #[test]
    fn bias_prompt_empty_and_whitespace_terms_yield_none() {
        assert_eq!(build_bias_prompt(&[]), None);
        assert_eq!(build_bias_prompt(&terms(&["", "   "])), None);
        // Whitespace padding is trimmed off surviving terms.
        assert_eq!(
            build_bias_prompt(&terms(&["  Rekody ", ""])).as_deref(),
            Some("Rekody")
        );
    }

    #[test]
    fn bias_prompt_caps_total_length_in_file_order() {
        // 8-byte terms cost 10 bytes each after the first (", " separator),
        // so the 800-byte cap admits exactly the first 80 terms.
        let many: Vec<String> = (0..200).map(|i| format!("term{i:04}")).collect();
        let prompt = build_bias_prompt(&many).unwrap();
        assert!(prompt.len() <= MAX_BIAS_PROMPT_BYTES);
        assert!(prompt.starts_with("term0000, term0001"));
        assert!(prompt.ends_with("term0079"));
        assert!(!prompt.contains("term0080"));
    }

    #[test]
    fn bias_prompt_strips_null_bytes() {
        // whisper-rs's set_initial_prompt panics on interior null bytes.
        assert_eq!(
            build_bias_prompt(&terms(&["Re\0kody"])).as_deref(),
            Some("Rekody")
        );
        assert_eq!(build_bias_prompt(&terms(&["\0"])), None);
    }

    // ----- Deepgram query construction -----

    #[test]
    fn deepgram_query_has_no_keyterms_when_empty() {
        let query = build_deepgram_query("nova-3", "multi", &[]);
        assert_eq!(
            query,
            vec![
                ("model", "nova-3"),
                ("language", "multi"),
                ("smart_format", "true"),
                ("punctuate", "true"),
            ]
        );
    }

    #[test]
    fn deepgram_query_adds_one_keyterm_per_term() {
        let bias = terms(&["Rekody", "Kipkemboi"]);
        let query = build_deepgram_query("nova-3", "en", &bias);
        let keyterms: Vec<&str> = query
            .iter()
            .filter(|(key, _)| *key == "keyterm")
            .map(|(_, value)| *value)
            .collect();
        assert_eq!(keyterms, vec!["Rekody", "Kipkemboi"]);
        // Base params are untouched.
        assert!(query.contains(&("model", "nova-3")));
        assert!(query.contains(&("language", "en")));
    }

    #[test]
    fn deepgram_query_caps_keyterms() {
        let many: Vec<String> = (0..80).map(|i| format!("term{i}")).collect();
        let query = build_deepgram_query("nova-3", "multi", &many);
        let count = query.iter().filter(|(key, _)| *key == "keyterm").count();
        assert_eq!(count, DEEPGRAM_MAX_KEYTERMS);
        // File order: the first terms survive the cap.
        assert!(query.contains(&("keyterm", "term0")));
        assert!(!query.contains(&("keyterm", "term50")));
    }

    #[test]
    fn deepgram_set_bias_terms_stores_trimmed_nonempty_terms() {
        let engine = DeepgramEngine::new("test-key".to_string());
        engine.set_bias_terms(&terms(&["  Rekody ", "   ", "Chamgei"]));
        let stored = engine.bias_terms.lock().unwrap().clone();
        assert_eq!(stored, terms(&["Rekody", "Chamgei"]));
        // Empty input clears previous terms.
        engine.set_bias_terms(&[]);
        assert!(engine.bias_terms.lock().unwrap().is_empty());
    }

    // ----- Groq multipart body -----

    #[test]
    fn groq_multipart_includes_prompt_field_when_terms_exist() {
        let wav = encode_wav(&[0.0f32; 16]);
        let (_, body) =
            build_multipart_body(&wav, "whisper-large-v3", None, Some("Rekody, Chamgei"));
        let body_text = String::from_utf8_lossy(&body);
        assert!(body_text.contains("name=\"prompt\""));
        assert!(body_text.contains("Rekody, Chamgei"));
    }

    #[test]
    fn groq_multipart_omits_prompt_field_when_none() {
        let wav = encode_wav(&[0.0f32; 16]);
        let (_, body) = build_multipart_body(&wav, "whisper-large-v3", Some("en"), None);
        let body_text = String::from_utf8_lossy(&body);
        assert!(!body_text.contains("name=\"prompt\""));
        // Existing fields are unaffected.
        assert!(body_text.contains("name=\"model\""));
        assert!(body_text.contains("name=\"language\""));
    }

    #[test]
    fn groq_set_bias_terms_builds_prompt_and_clears_on_empty() {
        let engine = OpenAiCompatEngine::groq("test-key".to_string(), None);
        engine.set_bias_terms(&terms(&["Rekody"]));
        assert_eq!(
            engine.bias_prompt.lock().unwrap().as_deref(),
            Some("Rekody")
        );
        engine.set_bias_terms(&[]);
        assert_eq!(*engine.bias_prompt.lock().unwrap(), None);
    }

    // ----- OpenAI-compatible presets -----

    /// The Groq preset must keep hitting the exact URL and model the
    /// separate GroqWhisperEngine did, or existing `stt_engine = "groq"`
    /// configs would quietly change behaviour.
    #[test]
    fn groq_preset_is_unchanged_by_the_generalisation() {
        let engine = OpenAiCompatEngine::groq("k".to_string(), Some("en".to_string()));
        assert_eq!(
            engine.endpoint().as_str(),
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
        assert_eq!(engine.model, "whisper-large-v3");
        assert_eq!(engine.language.as_deref(), Some("en"));
        assert_eq!(engine.destination_host(), "api.groq.com");
    }

    #[test]
    fn groq_with_model_keeps_groqs_endpoint() {
        let engine =
            OpenAiCompatEngine::groq_with_model("k".into(), "whisper-large-v3-turbo".into());
        assert_eq!(engine.model, "whisper-large-v3-turbo");
        assert_eq!(engine.destination_host(), "api.groq.com");
    }

    // ----- Endpoint guard -----

    #[test]
    fn https_endpoints_get_the_transcriptions_path_appended() {
        let url = resolve_transcription_endpoint("https://api.openai.com/v1").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.openai.com/v1/audio/transcriptions"
        );
        // A trailing slash must not produce a doubled one.
        let url = resolve_transcription_endpoint("https://api.openai.com/v1/").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.openai.com/v1/audio/transcriptions"
        );
    }

    /// Pasting the full endpoint is the obvious mistake, and it is harmless.
    #[test]
    fn a_full_endpoint_is_taken_as_given() {
        let url =
            resolve_transcription_endpoint("https://api.together.xyz/v1/audio/transcriptions")
                .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.together.xyz/v1/audio/transcriptions"
        );
    }

    /// Self-hosting must keep working, so loopback over plain http is the
    /// one allowed exception.
    #[test]
    fn plain_http_is_allowed_only_on_loopback() {
        for base in [
            "http://localhost:8000/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:8000/v1",
            "http://whisper.localhost/v1",
        ] {
            assert!(
                resolve_transcription_endpoint(base).is_ok(),
                "{base} should be allowed"
            );
        }
    }

    /// The guard that matters: a user's voice never goes to a remote host in
    /// the clear.
    #[test]
    fn plain_http_to_a_remote_host_is_refused() {
        let err = resolve_transcription_endpoint("http://example.com/v1").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("example.com"),
            "message must name the host: {msg}"
        );
        assert!(msg.contains("https"), "message must say what to do: {msg}");
        // 10.0.0.5 is private but not loopback: still a network hop.
        assert!(resolve_transcription_endpoint("http://10.0.0.5:8000/v1").is_err());
    }

    /// A key belongs in the key field. Embedding it in the URL would be
    /// dropped when the endpoint is rebuilt, surfacing as an unexplained
    /// 401, and would put a secret in a string the UI displays.
    #[test]
    fn credentials_in_the_url_are_refused() {
        for base in [
            "https://user:pass@api.openai.com/v1",
            "https://sk-secret@api.openai.com/v1",
        ] {
            let err = resolve_transcription_endpoint(base).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("key field"), "unhelpful: {msg}");
            assert!(
                !msg.contains("pass"),
                "the message must not echo the secret: {msg}"
            );
            assert!(
                !msg.contains("sk-secret"),
                "the message must not echo the secret: {msg}"
            );
        }
    }

    /// Short and normalised IPv4 forms are still loopback once the URL
    /// parser has expanded them. The Swift side agrees; see CustomEndpoint.
    #[test]
    fn short_loopback_forms_are_accepted() {
        for base in [
            "http://127.1/v1",
            "http://127.0.0.1/v1",
            "http://LOCALHOST:8000/v1",
        ] {
            assert!(
                resolve_transcription_endpoint(base).is_ok(),
                "{base} should be allowed"
            );
        }
        // A hostname that merely starts with a loopback address is not one.
        assert!(resolve_transcription_endpoint("http://127.0.0.1.evil.com/v1").is_err());
    }

    #[test]
    fn non_http_schemes_and_junk_are_refused() {
        for base in ["ftp://example.com", "file:///etc/passwd", "not a url", ""] {
            assert!(
                resolve_transcription_endpoint(base).is_err(),
                "{base:?} should be refused"
            );
        }
    }

    /// A custom provider with no model name cannot work, and saying so at
    /// construction beats failing at the moment someone speaks.
    /// An Azure-style endpoint carries its api-version in the query. Dropping
    /// it produced a 404 the user could not explain.
    #[test]
    fn the_query_string_survives_the_path_append() {
        let url =
            resolve_transcription_endpoint("https://x.openai.azure.com/v1?api-version=2026-01-01")
                .unwrap();
        assert_eq!(
            url.as_str(),
            "https://x.openai.azure.com/v1/audio/transcriptions?api-version=2026-01-01"
        );
        // A full endpoint keeps its query untouched, as before.
        let url = resolve_transcription_endpoint(
            "https://x.openai.azure.com/v1/audio/transcriptions?api-version=2026-01-01",
        )
        .unwrap();
        assert!(url.as_str().ends_with("?api-version=2026-01-01"));
    }

    #[test]
    fn custom_requires_a_model_name() {
        let err =
            OpenAiCompatEngine::custom("https://api.openai.com/v1", "  ".into(), "k".into(), None)
                .unwrap_err();
        assert!(err.to_string().contains("model name"));
    }

    #[test]
    fn custom_labels_itself_with_the_destination_host() {
        let engine = OpenAiCompatEngine::custom(
            "https://api.openai.com/v1",
            "whisper-1".into(),
            String::new(),
            None,
        )
        .unwrap();
        assert_eq!(engine.destination_host(), "api.openai.com");
        assert_eq!(engine.label, "api.openai.com");
    }

    // ----- Error bodies -----

    /// A misconfigured endpoint must read as "this is not a transcription
    /// API", not as a serde parse error.
    #[test]
    fn snippet_truncates_on_a_character_boundary() {
        assert_eq!(snippet("   "), "an empty body");
        assert_eq!(snippet("  hello  "), "hello");
        let long = "é".repeat(MAX_ERROR_BODY_CHARS + 50);
        let out = snippet(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_ERROR_BODY_CHARS + 1);
    }

    #[test]
    fn loopback_detection_covers_the_forms_people_type() {
        for host in ["localhost", "127.0.0.1", "127.1.2.3", "::1", "[::1]"] {
            assert!(is_loopback_host(host), "{host} is loopback");
        }
        for host in ["example.com", "10.0.0.5", "0.0.0.0", "localhost.evil.com"] {
            assert!(!is_loopback_host(host), "{host} is not loopback");
        }
    }
}
