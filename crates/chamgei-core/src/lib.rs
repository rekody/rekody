//! Core pipeline orchestration for Chamgei voice dictation.
//!
//! Wires together all pipeline stages: hotkey → audio → VAD → STT → LLM → injection.

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use chamgei_audio::AudioConfig;
pub use chamgei_hotkey::{ActivationMode, HotkeyConfig, HotkeyEvent};
pub use chamgei_inject::InjectionMethod;
pub use chamgei_llm::Provider;
pub use chamgei_stt::WhisperModel;

pub mod command_mode;
pub mod context;
pub mod corrections;
pub mod dictionary;
pub mod history;
pub mod onboarding;
pub mod prompts;
pub mod snippets;
pub mod stats;
pub mod status;

/// Configuration for a single LLM provider.
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider name: "groq", "cerebras", "together", "openrouter",
    /// "fireworks", "openai", "ollama", "lm-studio", "vllm", or "custom".
    pub name: String,
    /// API key (leave empty for local providers like Ollama).
    #[serde(default)]
    pub api_key: String,
    /// Model identifier (e.g., "openai/gpt-oss-20b", "llama3.1-8b").
    pub model: String,
    /// Custom base URL (only needed for "custom" provider or overrides).
    /// If omitted, the preset URL for the named provider is used.
    #[serde(default)]
    pub base_url: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("model", &self.model)
            .field("api_key", &if self.api_key.is_empty() { "[empty]" } else { "[REDACTED]" })
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// Top-level Chamgei configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChamgeiConfig {
    /// Hotkey activation mode.
    pub activation_mode: String,
    /// Ordered list of LLM providers to try (first = highest priority).
    /// Falls back to the next provider on failure.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,

    // --- Legacy fields (still supported for simple setups) ---
    /// Preferred LLM provider name (legacy, use `providers` instead).
    #[serde(default)]
    pub llm_provider: String,
    /// Groq API key (legacy, use `providers` instead).
    #[serde(default)]
    pub groq_api_key: Option<String>,
    /// Cerebras API key (legacy, use `providers` instead).
    #[serde(default)]
    pub cerebras_api_key: Option<String>,

    /// Whisper model size.
    pub whisper_model: String,
    /// STT engine: "local" (default, whisper.cpp), "groq", or "deepgram".
    #[serde(default = "default_stt_engine")]
    pub stt_engine: String,
    /// Deepgram API key (only needed if stt_engine = "deepgram").
    #[serde(default)]
    pub deepgram_api_key: Option<String>,
    /// VAD sensitivity (RMS threshold, ~0.01 for most mics).
    pub vad_threshold: f32,
    /// Text injection method.
    pub injection_method: String,
}

fn default_stt_engine() -> String {
    "local".into()
}

impl Default for ChamgeiConfig {
    fn default() -> Self {
        Self {
            activation_mode: "push_to_talk".into(),
            providers: vec![],
            llm_provider: "groq".into(),
            groq_api_key: None,
            cerebras_api_key: None,
            whisper_model: "tiny".into(),
            stt_engine: "local".into(),
            deepgram_api_key: None,
            vad_threshold: 0.01,
            injection_method: "clipboard".into(),
        }
    }
}

/// Load configuration from TOML file, falling back to defaults.
pub fn load_config(path: &str) -> Result<ChamgeiConfig> {
    let metadata = std::fs::metadata(path);
    if let Ok(meta) = &metadata {
        if meta.len() > 1_048_576 {
            anyhow::bail!("config file too large (max 1MB)");
        }
    }
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(_) => {
            tracing::info!("no config file found, using defaults");
            Ok(ChamgeiConfig::default())
        }
    }
}

/// Parse the activation mode string from config into [`ActivationMode`].
fn parse_activation_mode(s: &str) -> ActivationMode {
    match s.to_lowercase().as_str() {
        "toggle" => ActivationMode::Toggle,
        _ => ActivationMode::PushToTalk,
    }
}

/// Parse the injection method string from config into [`InjectionMethod`].
fn parse_injection_method(s: &str) -> InjectionMethod {
    match s.to_lowercase().as_str() {
        "native" => InjectionMethod::Native,
        _ => InjectionMethod::Clipboard,
    }
}

/// Build a [`chamgei_llm::ProviderChain`] from the configuration.
///
/// Providers are added in priority order based on the preferred provider
/// setting. If API keys are missing, providers are still added but will
/// report themselves as unavailable at runtime.
/// Create an [`OpenAICompatibleProvider`] from a [`ProviderConfig`].
fn make_provider(pc: &ProviderConfig) -> chamgei_llm::OpenAICompatibleProvider {
    // If a custom base_url is set, use it. Otherwise resolve from preset name.
    let base_url = pc.base_url.clone().unwrap_or_else(|| {
        match pc.name.to_lowercase().as_str() {
            "groq" => "https://api.groq.com/openai/v1/chat/completions",
            "cerebras" => "https://api.cerebras.ai/v1/chat/completions",
            "together" => "https://api.together.xyz/v1/chat/completions",
            "openrouter" => "https://openrouter.ai/api/v1/chat/completions",
            "fireworks" => "https://api.fireworks.ai/inference/v1/chat/completions",
            "openai" => "https://api.openai.com/v1/chat/completions",
            "ollama" => "http://localhost:11434/v1/chat/completions",
            "lm-studio" => "http://localhost:1234/v1/chat/completions",
            "vllm" => "http://localhost:8000/v1/chat/completions",
            _ => "http://localhost:11434/v1/chat/completions", // default to ollama
        }
        .to_string()
    });

    chamgei_llm::OpenAICompatibleProvider::new(
        &pc.name,
        base_url,
        &pc.api_key,
        &pc.model,
    )
}

fn build_provider_chain(config: &ChamgeiConfig) -> chamgei_llm::ProviderChain {
    let mut chain = chamgei_llm::ProviderChain::new();

    if !config.providers.is_empty() {
        // New config format: explicit provider list in priority order.
        for pc in &config.providers {
            tracing::info!(
                provider = %pc.name,
                model = %pc.model,
                "adding LLM provider"
            );
            match pc.name.to_lowercase().as_str() {
                "gemini" => {
                    chain = chain.add(chamgei_llm::presets::gemini(&pc.api_key, &pc.model));
                }
                "anthropic" => {
                    chain = chain.add(chamgei_llm::presets::anthropic(&pc.api_key, &pc.model));
                }
                _ => {
                    chain = chain.add(make_provider(pc));
                }
            }
        }
    } else {
        // Legacy config format: single llm_provider + api key fields.
        let groq_key = config.groq_api_key.clone().unwrap_or_default();
        let cerebras_key = config.cerebras_api_key.clone().unwrap_or_default();

        match config.llm_provider.to_lowercase().as_str() {
            "groq" if !groq_key.is_empty() => {
                chain = chain.add(chamgei_llm::presets::groq(&groq_key, "openai/gpt-oss-20b"));
            }
            "cerebras" if !cerebras_key.is_empty() => {
                chain = chain.add(chamgei_llm::presets::cerebras(&cerebras_key, "llama3.1-8b"));
            }
            _ => {
                // Add both if keys exist
                if !groq_key.is_empty() {
                    chain = chain.add(chamgei_llm::presets::groq(&groq_key, "openai/gpt-oss-20b"));
                }
                if !cerebras_key.is_empty() {
                    chain = chain.add(chamgei_llm::presets::cerebras(&cerebras_key, "llama3.1-8b"));
                }
            }
        }
    }

    chain
}

/// Returns `true` if at least one LLM provider is configured.
fn has_llm_providers(config: &ChamgeiConfig) -> bool {
    if !config.providers.is_empty() {
        return true;
    }
    config
        .cerebras_api_key
        .as_ref()
        .is_some_and(|k| !k.is_empty())
        || config
            .groq_api_key
            .as_ref()
            .is_some_and(|k| !k.is_empty())
}

/// Resolve the Whisper model directory from env or defaults.
fn resolve_model_dir() -> std::path::PathBuf {
    std::env::var("CHAMGEI_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| {
                    std::path::PathBuf::from(h)
                        .join(".local")
                        .join("share")
                        .join("chamgei")
                        .join("models")
                })
                .unwrap_or_else(|_| std::path::PathBuf::from("models"))
        })
}

/// Wraps the different STT engine types behind a common enum.
enum SttBackend {
    Local(chamgei_stt::LocalWhisperEngine),
    Groq(chamgei_stt::GroqWhisperEngine),
    Deepgram(chamgei_stt::DeepgramEngine),
}

impl SttBackend {
    async fn transcribe(&self, samples: &[f32]) -> Result<chamgei_stt::Transcript> {
        use chamgei_stt::SttEngine;
        match self {
            SttBackend::Local(e) => e.transcribe(samples).await,
            SttBackend::Groq(e) => e.transcribe(samples).await,
            SttBackend::Deepgram(e) => e.transcribe(samples).await,
        }
    }
}

/// The main Chamgei pipeline orchestrator.
pub struct Pipeline {
    pub config: ChamgeiConfig,
    provider_chain: chamgei_llm::ProviderChain,
    injection_method: InjectionMethod,
    stt: SttBackend,
    history: std::sync::Mutex<history::History>,
}

impl Pipeline {
    pub fn new(config: ChamgeiConfig) -> Result<Self> {
        let provider_chain = build_provider_chain(&config);
        let injection_method = parse_injection_method(&config.injection_method);

        // Initialize the STT engine based on config.
        let stt = match config.stt_engine.to_lowercase().as_str() {
            "groq" => {
                let key = config.groq_api_key.clone().unwrap_or_default();
                tracing::info!("using Groq cloud STT (Whisper Large v3)");
                SttBackend::Groq(chamgei_stt::GroqWhisperEngine::new(key))
            }
            "deepgram" => {
                let key = config.deepgram_api_key.clone().unwrap_or_default();
                tracing::info!("using Deepgram cloud STT (Nova-2)");
                SttBackend::Deepgram(chamgei_stt::DeepgramEngine::new(key))
            }
            _ => {
                // Default: local Whisper
                let whisper_model = match config.whisper_model.to_lowercase().as_str() {
                    "tiny" => WhisperModel::Tiny,
                    "medium" => WhisperModel::Medium,
                    "large" => WhisperModel::Large,
                    _ => WhisperModel::Small,
                };
                let model_dir = resolve_model_dir();
                let model_path = model_dir.join(whisper_model.file_name());
                let model_path_str = model_path.to_string_lossy();
                let engine = chamgei_stt::LocalWhisperEngine::new(whisper_model, &model_path_str)?;
                SttBackend::Local(engine)
            }
        };

        let history = std::sync::Mutex::new(history::History::load());

        Ok(Self {
            config,
            provider_chain,
            injection_method,
            stt,
            history,
        })
    }

    /// Start the dictation pipeline.
    ///
    /// This method runs indefinitely, listening for hotkey events and
    /// processing audio through the full pipeline:
    ///
    /// hotkey → audio capture → VAD → STT → (LLM) → text injection
    pub async fn run(&self) -> Result<()> {
        tracing::info!("chamgei pipeline starting");

        // 1. Parse hotkey config and start listener.
        let hotkey_config = HotkeyConfig {
            activation_mode: parse_activation_mode(&self.config.activation_mode),
        };
        let mut hotkey_rx = chamgei_hotkey::start_listener(hotkey_config)?;
        tracing::info!("hotkey listener started");

        // 2. Create audio capture and open the device stream.
        let audio_config = AudioConfig {
            vad_threshold: self.config.vad_threshold,
        };
        let audio_capture = chamgei_audio::AudioCapture::new(audio_config.clone());
        let mut segment_rx = audio_capture.open(audio_config)?;
        tracing::info!("audio capture initialized");

        let llm_enabled = has_llm_providers(&self.config);
        if llm_enabled {
            tracing::info!("LLM post-processing enabled");
        } else {
            tracing::info!("no LLM API keys configured; raw STT output will be used");
        }

        // 3. Main event loop — listen for hotkey events and audio segments
        //    concurrently using tokio::select!.
        loop {
            tokio::select! {
                hotkey_event = hotkey_rx.recv() => {
                    match hotkey_event {
                        Some(HotkeyEvent::RecordStart) => {
                            tracing::info!("recording started (hotkey)");
                            audio_capture.start_recording();
                        }
                        Some(HotkeyEvent::RecordStop) => {
                            tracing::info!("recording stopped (hotkey)");
                            audio_capture.stop_recording();
                            // Audio segments will arrive through segment_rx
                            // once the VAD detects end-of-speech.
                        }
                        Some(HotkeyEvent::CommandMode) => {
                            tracing::info!("command mode activated (not yet implemented)");
                            // TODO: Implement command mode — select text +
                            // voice instruction for editing.
                        }
                        None => {
                            tracing::warn!("hotkey channel closed, shutting down");
                            break;
                        }
                    }
                }

                segment = segment_rx.recv() => {
                    match segment {
                        Some(audio_segment) => {
                            tracing::info!(
                                duration_secs = audio_segment.duration_secs,
                                samples = audio_segment.samples.len(),
                                "received audio segment, processing"
                            );

                            if let Err(e) = self.process_segment(&audio_segment, llm_enabled).await {
                                tracing::error!(error = %e, "failed to process audio segment");
                            }
                        }
                        None => {
                            tracing::warn!("audio segment channel closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }

        tracing::info!("chamgei pipeline stopped");
        Ok(())
    }

    /// Process a single audio segment through the STT → LLM → injection
    /// stages of the pipeline.
    async fn process_segment(
        &self,
        segment: &chamgei_audio::AudioSegment,
        llm_enabled: bool,
    ) -> Result<()> {
        // --- STT (model already loaded at startup) ---
        use chamgei_stt::SttEngine;
        let transcript = self.stt.transcribe(&segment.samples).await?;

        if transcript.text.is_empty() {
            tracing::debug!("empty transcript, skipping injection");
            return Ok(());
        }

        tracing::info!(
            text = %transcript.text,
            latency_ms = transcript.latency_ms,
            "transcription complete"
        );

        // --- LLM post-processing ---
        let mut llm_latency_ms: Option<u64> = None;
        let mut llm_provider: Option<String> = None;
        let mut app_name = String::from("Unknown");

        let final_text = if llm_enabled {
            // Detect the active application for context-aware formatting.
            let app_context = context::detect_active_app();
            tracing::debug!(
                app = %app_context.app_name,
                bundle = ?app_context.bundle_id,
                "detected active application"
            );
            app_name = app_context.app_name.clone();

            // Get the context-specific system prompt.
            let system_prompt = prompts::get_prompt_for_app(
                &app_context.app_name,
                app_context.bundle_id.as_deref(),
            );

            // Send through the LLM provider chain.
            match self
                .provider_chain
                .format(&transcript.text, &app_context, &system_prompt)
                .await
            {
                Ok(formatted) => {
                    tracing::info!(
                        provider = %formatted.provider,
                        latency_ms = formatted.latency_ms,
                        "LLM formatting complete"
                    );
                    llm_latency_ms = Some(formatted.latency_ms);
                    llm_provider = Some(formatted.provider.clone());
                    // Guard: if LLM returns empty, use raw transcript
                    if formatted.text.trim().is_empty() {
                        tracing::warn!("LLM returned empty text, using raw transcript");
                        transcript.text.clone()
                    } else {
                        formatted.text
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "LLM formatting failed, falling back to raw transcript"
                    );
                    transcript.text.clone()
                }
            }
        } else {
            // No LLM configured — use raw STT output directly.
            transcript.text.clone()
        };

        // --- Text injection ---
        tracing::debug!(
            method = ?self.injection_method,
            text_len = final_text.len(),
            "injecting text"
        );
        chamgei_inject::inject_text(&final_text, self.injection_method)?;
        tracing::info!("text injected successfully");

        // --- Save to history ---
        let entry = history::History::new_entry(
            final_text,
            transcript.text.clone(),
            transcript.latency_ms,
            llm_latency_ms,
            llm_provider,
            app_name,
        );
        if let Ok(mut history) = self.history.lock() {
            history.add(entry);
        } else {
            tracing::warn!("failed to acquire history lock, skipping history save");
        }

        Ok(())
    }
}
