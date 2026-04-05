//! Core pipeline orchestration for Chamgei voice dictation.
//!
//! Wires together all pipeline stages: hotkey → audio → VAD → STT → LLM → injection.

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use chamgei_audio::AudioConfig;
pub use chamgei_hotkey::{ActivationMode, HotkeyConfig, HotkeyEvent};
pub use chamgei_inject::InjectionMethod;
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
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "[empty]"
                } else {
                    "[REDACTED]"
                },
            )
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
    /// STT engine: "local" (default, whisper.cpp), "groq", "deepgram", or "cohere".
    #[serde(default = "default_stt_engine")]
    pub stt_engine: String,
    /// Deepgram API key (only needed if stt_engine = "deepgram").
    #[serde(default)]
    pub deepgram_api_key: Option<String>,
    /// Port for the local Cohere STT server (only needed if stt_engine = "cohere").
    #[serde(default = "default_cohere_stt_port")]
    pub cohere_stt_port: u16,
    /// VAD sensitivity (RMS threshold, ~0.01 for most mics).
    pub vad_threshold: f32,
    /// Text injection method.
    pub injection_method: String,
    /// Whether to run LLM post-processing on transcripts.
    ///
    /// - `None` (default / omitted from config): auto — LLM is disabled when
    ///   `stt_engine = "deepgram"` (Deepgram's `smart_format` already produces
    ///   clean, punctuated output), enabled for all other engines.
    /// - `Some(true)`: always run LLM regardless of STT engine.
    /// - `Some(false)`: always skip LLM.
    ///
    /// Set via `llm_enabled = true` / `llm_enabled = false` in config.toml.
    #[serde(default)]
    pub llm_enabled: Option<bool>,
}

fn default_stt_engine() -> String {
    "local".into()
}

fn default_cohere_stt_port() -> u16 {
    8099
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
            cohere_stt_port: 8099,
            vad_threshold: 0.01,
            injection_method: "clipboard".into(),
            llm_enabled: None,
        }
    }
}

/// Load configuration from TOML file, falling back to defaults.
///
/// After loading, legacy fields are migrated into the new `providers` array
/// so the rest of the pipeline only needs to handle one format.
pub fn load_config(path: &str) -> Result<ChamgeiConfig> {
    let metadata = std::fs::metadata(path);
    if let Ok(meta) = &metadata
        && meta.len() > 1_048_576
    {
        anyhow::bail!("config file too large (max 1MB)");
    }
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let mut config: ChamgeiConfig = toml::from_str(&contents)?;
            migrate_legacy_config(&mut config);
            Ok(config)
        }
        Err(_) => {
            tracing::info!("no config file found, using defaults");
            Ok(ChamgeiConfig::default())
        }
    }
}

/// Migrate legacy flat API key fields into the `providers` array.
///
/// This is a one-way in-memory migration — it does NOT rewrite the config
/// file. Users can run `chamgei config edit` to adopt the new format.
fn migrate_legacy_config(config: &mut ChamgeiConfig) {
    // Only migrate if providers array is empty (new format not yet used).
    if !config.providers.is_empty() {
        return;
    }

    let groq_key = config.groq_api_key.clone().unwrap_or_default();
    let cerebras_key = config.cerebras_api_key.clone().unwrap_or_default();

    match config.llm_provider.to_lowercase().as_str() {
        "groq" if !groq_key.is_empty() => {
            config.providers.push(ProviderConfig {
                name: "groq".into(),
                api_key: groq_key,
                model: "openai/gpt-oss-20b".into(),
                base_url: None,
            });
        }
        "cerebras" if !cerebras_key.is_empty() => {
            config.providers.push(ProviderConfig {
                name: "cerebras".into(),
                api_key: cerebras_key,
                model: "llama3.1-8b".into(),
                base_url: None,
            });
        }
        _ => {
            // Add whichever keys exist, groq first.
            if !groq_key.is_empty() {
                config.providers.push(ProviderConfig {
                    name: "groq".into(),
                    api_key: groq_key,
                    model: "openai/gpt-oss-20b".into(),
                    base_url: None,
                });
            }
            if !cerebras_key.is_empty() {
                config.providers.push(ProviderConfig {
                    name: "cerebras".into(),
                    api_key: cerebras_key,
                    model: "llama3.1-8b".into(),
                    base_url: None,
                });
            }
        }
    }

    if !config.providers.is_empty() {
        tracing::debug!(
            count = config.providers.len(),
            "migrated legacy LLM config to providers array"
        );
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

    chamgei_llm::OpenAICompatibleProvider::new(&pc.name, base_url, &pc.api_key, &pc.model)
}

fn build_provider_chain(config: &ChamgeiConfig) -> chamgei_llm::ProviderChain {
    // By the time we get here, legacy fields have already been migrated into
    // `config.providers` by `migrate_legacy_config()` in `load_config()`.
    let mut chain = chamgei_llm::ProviderChain::new();
    for pc in &config.providers {
        tracing::info!(provider = %pc.name, model = %pc.model, "adding LLM provider");
        chain = match pc.name.to_lowercase().as_str() {
            "gemini" => chain.add(chamgei_llm::presets::gemini(&pc.api_key, &pc.model)),
            "anthropic" => chain.add(chamgei_llm::presets::anthropic(&pc.api_key, &pc.model)),
            _ => chain.add(make_provider(pc)),
        };
    }
    chain
}

/// Returns `true` if LLM post-processing should run for this config.
///
/// Logic:
/// - Explicit `llm_enabled = false` → always skip.
/// - Explicit `llm_enabled = true` → run if providers exist.
/// - `llm_enabled` omitted (None, the default):
///   - Deepgram STT → skip (smart_format already produces clean output).
///   - All other STT engines → run if providers exist.
pub fn has_llm_providers(config: &ChamgeiConfig) -> bool {
    if config.providers.is_empty() {
        return false;
    }
    match config.llm_enabled {
        Some(false) => false,
        Some(true) => true,
        None => config.stt_engine.to_lowercase() != "deepgram",
    }
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
    Cohere(chamgei_stt::CohereLocalEngine),
}

impl SttBackend {
    async fn transcribe(&self, samples: &[f32]) -> Result<chamgei_stt::Transcript> {
        use chamgei_stt::SttEngine;
        match self {
            SttBackend::Local(e) => e.transcribe(samples).await,
            SttBackend::Groq(e) => e.transcribe(samples).await,
            SttBackend::Deepgram(e) => e.transcribe(samples).await,
            SttBackend::Cohere(e) => e.transcribe(samples).await,
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
    /// Optional status manager for UI feedback (Tauri or other frontends).
    status_manager: Option<status::StatusManager>,
    /// Optional external event receiver (e.g. from Tauri UI toggle button).
    external_rx: Option<
        tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<chamgei_hotkey::HotkeyEvent>>,
    >,
}

/// Handle for sending events to the pipeline from external sources (e.g. UI).
#[derive(Clone)]
pub struct PipelineControl {
    tx: tokio::sync::mpsc::UnboundedSender<chamgei_hotkey::HotkeyEvent>,
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
                tracing::info!("using Deepgram cloud STT (Nova-3)");
                SttBackend::Deepgram(chamgei_stt::DeepgramEngine::new(key))
            }
            "cohere" => {
                tracing::info!(port = config.cohere_stt_port, "using Cohere local STT");
                SttBackend::Cohere(chamgei_stt::CohereLocalEngine::new(config.cohere_stt_port))
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
            status_manager: None,
            external_rx: None,
        })
    }

    /// Attach a [`StatusManager`] so the pipeline reports state transitions.
    pub fn with_status_manager(mut self, manager: status::StatusManager) -> Self {
        self.status_manager = Some(manager);
        self
    }

    /// Create a [`PipelineControl`] handle that can send events into the pipeline
    /// from external sources (e.g. a UI toggle button).
    pub fn create_control(&mut self) -> PipelineControl {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.external_rx = Some(tokio::sync::Mutex::new(rx));
        PipelineControl { tx }
    }
}

impl PipelineControl {
    /// Send a start recording event.
    pub fn start_recording(&self) {
        let _ = self.tx.send(chamgei_hotkey::HotkeyEvent::RecordStart);
    }

    /// Send a stop recording event.
    pub fn stop_recording(&self) {
        let _ = self.tx.send(chamgei_hotkey::HotkeyEvent::RecordStop);
    }
}

impl Pipeline {
    /// Update the status manager (if attached).
    fn set_status(&self, status: status::PipelineStatus) {
        if let Some(ref mgr) = self.status_manager {
            mgr.set_status(status);
        }
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

        // 3. Main event loop — listen for hotkey events, external UI events,
        //    and audio segments concurrently using tokio::select!.
        let mut external_rx_guard = if let Some(ref ext) = self.external_rx {
            Some(ext.lock().await)
        } else {
            None
        };

        loop {
            // Helper: handle a hotkey/UI event
            macro_rules! handle_event {
                ($event:expr, $source:expr) => {
                    match $event {
                        HotkeyEvent::RecordStart => {
                            tracing::info!(source = $source, "recording started");
                            self.set_status(status::PipelineStatus::Recording);
                            audio_capture.start_recording();
                        }
                        HotkeyEvent::RecordStop => {
                            tracing::info!(source = $source, "recording stopped");
                            audio_capture.stop_recording();
                        }
                        HotkeyEvent::CommandMode => {
                            tracing::info!("command mode activated (not yet implemented)");
                        }
                    }
                };
            }

            tokio::select! {
                hotkey_event = hotkey_rx.recv() => {
                    match hotkey_event {
                        Some(evt) => handle_event!(evt, "hotkey"),
                        None => {
                            tracing::warn!("hotkey channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Listen for events from the UI (toggle button).
                ext_event = async {
                    match external_rx_guard.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match ext_event {
                        Some(evt) => handle_event!(evt, "ui"),
                        None => {
                            tracing::debug!("external event channel closed");
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
                            self.set_status(status::PipelineStatus::Processing);

                            if let Err(e) = self.process_segment(&audio_segment, llm_enabled).await {
                                tracing::error!(error = %e, "failed to process audio segment");
                                self.set_status(status::PipelineStatus::Error(e.to_string()));
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
        self.set_status(status::PipelineStatus::Injecting);
        tracing::debug!(
            method = ?self.injection_method,
            text_len = final_text.len(),
            "injecting text"
        );
        chamgei_inject::inject_text(&final_text, self.injection_method)?;
        tracing::info!("text injected successfully");
        self.set_status(status::PipelineStatus::Idle);

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
