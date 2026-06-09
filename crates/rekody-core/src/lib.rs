//! Core pipeline orchestration for rekody voice dictation.
//!
//! Wires together all pipeline stages: hotkey → audio → VAD → STT → LLM → injection.

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use rekody_audio::AudioConfig;
pub use rekody_hotkey::{ActivationMode, HotkeyConfig, HotkeyEvent, TriggerKey};
pub use rekody_inject::InjectionMethod;
pub use rekody_stt::WhisperModel;

pub mod bench;
pub mod command_mode;
pub mod config_tui;
pub mod context;
pub mod corrections;
pub mod dictionary;
pub mod history;
pub mod history_tui;
pub mod key_tui;
pub mod onboarding;
pub mod prompts;
pub mod skill;
pub mod skill_tui;
pub mod snippets;
pub mod stats;

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

/// Top-level rekody configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RekodyConfig {
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
    /// BCP-47 language code for transcription (e.g. `"en"`, `"sw"`, `"fr"`).
    ///
    /// `None` (default, omit from config) = auto-detect:
    ///   - Deepgram: `language=multi` — real-time multilingual, 100+ languages
    ///   - Groq Whisper: language field omitted — Whisper auto-detects
    ///   - Local Whisper: `set_language(None)` — requires a multilingual model file
    ///
    /// Setting this to a specific code slightly improves accuracy and speed when
    /// the language is known in advance.
    #[serde(default)]
    pub stt_language: Option<String>,
    /// VAD sensitivity (RMS threshold, ~0.01 for most mics).
    pub vad_threshold: f32,
    /// If true, capture every audio frame from press to release without VAD
    /// gating. Useful for transcribing low-energy input (e.g. phone-speaker
    /// playback into the mic). Default: false (VAD enabled).
    #[serde(default)]
    pub record_all_audio: bool,
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

    /// Which key triggers dictation: "option_space" (default) or "fn_key".
    /// "fn_key" requires System Settings → Keyboard → "Press 🌐 key to" → "Do Nothing".
    #[serde(default = "default_trigger_key")]
    pub trigger_key: String,

    /// Maximum continuous recording duration in seconds (deadman switch).
    /// `0` means no limit. Default: 300 (5 minutes).
    #[serde(default = "default_max_recording_secs")]
    pub max_recording_secs: u64,
}

fn default_stt_engine() -> String {
    "local".into()
}

fn default_cohere_stt_port() -> u16 {
    8099
}

fn default_trigger_key() -> String {
    "option_space".into()
}

fn default_max_recording_secs() -> u64 {
    300
}

impl Default for RekodyConfig {
    fn default() -> Self {
        Self {
            activation_mode: "push_to_talk".into(),
            providers: vec![],
            llm_provider: "groq".into(),
            groq_api_key: None,
            cerebras_api_key: None,
            whisper_model: "turbo".into(),
            stt_engine: "local".into(),
            deepgram_api_key: None,
            cohere_stt_port: 8099,
            vad_threshold: 0.01,
            record_all_audio: false,
            injection_method: "clipboard".into(),
            llm_enabled: None,
            stt_language: None,
            trigger_key: default_trigger_key(),
            max_recording_secs: default_max_recording_secs(),
        }
    }
}

/// Load configuration from TOML file, falling back to defaults.
///
/// After loading, legacy fields are migrated into the new `providers` array
/// so the rest of the pipeline only needs to handle one format.
pub fn load_config(path: &str) -> Result<RekodyConfig> {
    let metadata = std::fs::metadata(path);
    if let Ok(meta) = &metadata
        && meta.len() > 1_048_576
    {
        anyhow::bail!("config file too large (max 1MB)");
    }
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let mut config: RekodyConfig = toml::from_str(&contents)?;
            migrate_legacy_config(&mut config);
            Ok(config)
        }
        Err(_) => {
            tracing::info!("no config file found, using defaults");
            Ok(RekodyConfig::default())
        }
    }
}

/// Migrate legacy flat API key fields into the `providers` array.
///
/// This is a one-way in-memory migration — it does NOT rewrite the config
/// file. Users can run `rekody config edit` to adopt the new format.
fn migrate_legacy_config(config: &mut RekodyConfig) {
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

/// Build a [`rekody_llm::ProviderChain`] from the configuration.
///
/// Providers are added in priority order based on the preferred provider
/// setting. If API keys are missing, providers are still added but will
/// report themselves as unavailable at runtime.
/// Create an [`OpenAICompatibleProvider`] from a [`ProviderConfig`].
fn make_provider(pc: &ProviderConfig) -> rekody_llm::OpenAICompatibleProvider {
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

    rekody_llm::OpenAICompatibleProvider::new(&pc.name, base_url, &pc.api_key, &pc.model)
}

fn build_provider_chain(config: &RekodyConfig) -> rekody_llm::ProviderChain {
    // By the time we get here, legacy fields have already been migrated into
    // `config.providers` by `migrate_legacy_config()` in `load_config()`.
    let mut chain = rekody_llm::ProviderChain::new();
    for pc in &config.providers {
        tracing::info!(provider = %pc.name, model = %pc.model, "adding LLM provider");
        chain = match pc.name.to_lowercase().as_str() {
            // On-device Apple Foundation Models (macOS 26+) via the rekody-fm
            // helper. Pseudo-provider: no api_key/model. Unavailable (helper
            // absent / not eligible) → chain falls through to the next tier.
            "apple" | "apple-foundation" | "foundation" => {
                chain.add(rekody_llm::AppleFoundationProvider::new())
            }
            "gemini" => chain.add(rekody_llm::presets::gemini(&pc.api_key, &pc.model)),
            "anthropic" => chain.add(rekody_llm::presets::anthropic(&pc.api_key, &pc.model)),
            _ => chain.add(make_provider(pc)),
        };
    }
    // Final tier: never lose the user's words. If every configured provider is
    // unavailable or errors (e.g. Apple FM on a non-eligible machine), return
    // the raw transcript rather than nothing.
    chain.add(rekody_llm::RawTranscriptFallback::new())
}

/// Returns `true` if LLM post-processing should run for this config.
///
/// Logic:
/// - Explicit `llm_enabled = false` → always skip.
/// - Explicit `llm_enabled = true` → run if providers exist.
/// - `llm_enabled` omitted (None, the default):
///   - Deepgram STT → skip (smart_format already produces clean output).
///   - All other STT engines → run if providers exist.
pub fn has_llm_providers(config: &RekodyConfig) -> bool {
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
    std::env::var("REKODY_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| {
                    std::path::PathBuf::from(h)
                        .join(".local")
                        .join("share")
                        .join("rekody")
                        .join("models")
                })
                .unwrap_or_else(|_| std::path::PathBuf::from("models"))
        })
}

/// Wraps the different STT engine types behind a common enum.
enum SttBackend {
    Local(rekody_stt::LocalWhisperEngine),
    Groq(rekody_stt::GroqWhisperEngine),
    Deepgram(rekody_stt::DeepgramEngine),
    Cohere(rekody_stt::CohereLocalEngine),
}

impl SttBackend {
    async fn transcribe(&self, samples: &[f32]) -> Result<rekody_stt::Transcript> {
        use rekody_stt::SttEngine;
        match self {
            SttBackend::Local(e) => e.transcribe(samples).await,
            SttBackend::Groq(e) => e.transcribe(samples).await,
            SttBackend::Deepgram(e) => e.transcribe(samples).await,
            SttBackend::Cohere(e) => e.transcribe(samples).await,
        }
    }
}

/// The main rekody pipeline orchestrator.
pub struct Pipeline {
    pub config: RekodyConfig,
    provider_chain: rekody_llm::ProviderChain,
    injection_method: InjectionMethod,
    stt: SttBackend,
    history: std::sync::Mutex<history::History>,
}

impl Pipeline {
    pub fn new(config: RekodyConfig) -> Result<Self> {
        let provider_chain = build_provider_chain(&config);
        let injection_method = parse_injection_method(&config.injection_method);

        // Initialize the STT engine based on config.
        let lang = config.stt_language.clone();
        let stt = match config.stt_engine.to_lowercase().as_str() {
            "groq" => {
                let key = config.groq_api_key.clone().unwrap_or_default();
                tracing::info!(language = ?lang, "using Groq cloud STT (Whisper Large v3)");
                SttBackend::Groq(rekody_stt::GroqWhisperEngine::with_language(key, lang))
            }
            "deepgram" => {
                let key = config.deepgram_api_key.clone().unwrap_or_default();
                // Default to "multi" (Nova-3 multilingual) when no language is pinned.
                let dg_lang = lang.unwrap_or_else(|| "multi".to_string());
                tracing::info!(language = %dg_lang, "using Deepgram cloud STT (Nova-3)");
                SttBackend::Deepgram(rekody_stt::DeepgramEngine::with_language(key, dg_lang))
            }
            "cohere" => {
                tracing::info!(port = config.cohere_stt_port, "using Cohere local STT");
                SttBackend::Cohere(rekody_stt::CohereLocalEngine::new(config.cohere_stt_port))
            }
            _ => {
                // Default: local Whisper.
                // Use multilingual model file when no language is pinned or language != "en".
                let whisper_model = match config.whisper_model.to_lowercase().as_str() {
                    "tiny" => WhisperModel::Tiny,
                    "small" => WhisperModel::Small,
                    "medium" => WhisperModel::Medium,
                    "large" => WhisperModel::Large,
                    _ => WhisperModel::Turbo,
                };
                let model_dir = resolve_model_dir();
                let is_english_only = lang.as_deref() == Some("en");
                let model_file = if is_english_only {
                    whisper_model.file_name()
                } else {
                    whisper_model.multilingual_file_name()
                };
                let model_path = model_dir.join(model_file);
                let model_path_str = model_path.to_string_lossy();
                tracing::info!(language = ?lang, model = model_file, "using local Whisper STT");
                let engine = rekody_stt::LocalWhisperEngine::with_language(
                    whisper_model,
                    &model_path_str,
                    lang,
                )?;
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
}

impl Pipeline {
    /// Start the dictation pipeline.
    ///
    /// This method runs indefinitely, listening for hotkey events and
    /// processing audio through the full pipeline:
    ///
    /// hotkey → audio capture → VAD → STT → (LLM) → text injection
    pub async fn run(&self) -> Result<()> {
        tracing::info!("rekody pipeline starting");

        // 1. Parse hotkey config and start listener.
        let trigger_key = match self.config.trigger_key.to_lowercase().as_str() {
            "fn_key" | "fn" => TriggerKey::FnKey,
            _ => TriggerKey::OptionSpace,
        };
        let hotkey_config = HotkeyConfig {
            activation_mode: parse_activation_mode(&self.config.activation_mode),
            trigger_key,
            max_recording_secs: self.config.max_recording_secs,
        };
        let mut hotkey_rx = rekody_hotkey::start_listener(hotkey_config)?;
        tracing::info!("hotkey listener started");

        // 2. Create audio capture and open the device stream.
        let audio_config = AudioConfig {
            vad_threshold: self.config.vad_threshold,
            record_all_audio: self.config.record_all_audio,
        };
        let audio_capture = rekody_audio::AudioCapture::new(audio_config.clone());
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
                            tracing::info!(source = "hotkey", "recording started");
                            audio_capture.start_recording();
                        }
                        Some(HotkeyEvent::RecordStop) => {
                            tracing::info!(source = "hotkey", "recording stopped");
                            audio_capture.stop_recording();
                        }
                        Some(HotkeyEvent::CommandMode) => {
                            tracing::info!("command mode activated (not yet implemented)");
                        }
                        Some(HotkeyEvent::CycleSkill) => {
                            // ⌥Space+Tab: advance the sticky skill. The next
                            // dictation picks it up via the fresh-read in
                            // process_segment. Surface the new selection.
                            let now = skill::cycle_active();
                            tracing::info!(
                                skill = now.as_deref().unwrap_or("Auto"),
                                "skill cycled"
                            );
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

        tracing::info!("rekody pipeline stopped");
        Ok(())
    }

    /// Process a single audio segment through the STT → LLM → injection
    /// stages of the pipeline.
    async fn process_segment(
        &self,
        segment: &rekody_audio::AudioSegment,
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

            // Resolve the system prompt. A user-selected skill (or an
            // app-triggered skill) overrides the built-in per-app prompt;
            // otherwise fall back to the context-specific prompt. Read fresh
            // each dictation so `rekody skill use …` takes effect without a
            // daemon restart.
            let resolved_skill =
                skill::resolve(&app_context.app_name, app_context.bundle_id.as_deref());
            let skill_name = resolved_skill.as_ref().map(|r| r.name.clone());
            let base_prompt = match resolved_skill {
                Some(r) => r.prompt,
                None => prompts::get_prompt_for_app(
                    &app_context.app_name,
                    app_context.bundle_id.as_deref(),
                ),
            };
            if let Some(name) = &skill_name {
                tracing::debug!(skill = %name, "applying skill prompt");
            }
            // Append the user's personal-dictionary terms so the model
            // preserves jargon/proper-nouns (e.g. "rekody" not "record").
            // Read fresh each dictation; no-op when the dictionary is empty.
            let dict = dictionary::Dictionary::load_or_empty();
            let system_prompt = dictionary::inject_vocabulary_prompt(&base_prompt, &dict);

            // Send through the LLM provider chain.
            match self
                .provider_chain
                .format(&transcript.text, &app_context, &system_prompt)
                .await
            {
                Ok(formatted) => {
                    // `skill` field lets the UI surface which skill shaped this
                    // dictation (empty when no skill applied).
                    tracing::info!(
                        provider = %formatted.provider,
                        latency_ms = formatted.latency_ms,
                        skill = skill_name.as_deref().unwrap_or(""),
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
        rekody_inject::inject_text(&final_text, self.injection_method)?;
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

#[cfg(test)]
mod prompt_composition_tests {
    //! End-to-end check that the dictation pipeline's system-prompt assembly
    //! composes correctly: a skill's prompt (body + output-hygiene tail) with
    //! the user's personal-dictionary vocabulary appended. Mirrors exactly what
    //! `process_segment` builds (skill base → `inject_vocabulary_prompt`).

    use crate::dictionary::{Dictionary, inject_vocabulary_prompt};
    use crate::skill::Skill;

    #[test]
    fn skill_body_hygiene_and_vocabulary_all_present() {
        // A skill as parsed from a .md file (REPLACE model, no inherit_base).
        let skill = Skill {
            name: "notes".into(),
            description: "Clean bulleted notes".into(),
            triggers: vec![],
            inherit_base: false,
            body: "Turn the transcription into clean bulleted notes.".into(),
        };
        let base = skill.system_prompt();

        let mut dict = Dictionary::new();
        dict.add_term("rekody");
        dict.add_term("Kalenjin");
        let final_prompt = inject_vocabulary_prompt(&base, &dict);

        // Skill body survives.
        assert!(final_prompt.contains("Turn the transcription into clean bulleted notes."));
        // Output-hygiene tail (appended by Skill::system_prompt) survives.
        assert!(final_prompt.contains("OUTPUT RULES"));
        // Vocabulary section + both terms are appended.
        assert!(final_prompt.contains("preserve these terms exactly"));
        assert!(final_prompt.contains("rekody"));
        assert!(final_prompt.contains("Kalenjin"));
        // Ordering: skill content comes before the vocabulary block.
        let body_idx = final_prompt.find("clean bulleted notes").unwrap();
        let vocab_idx = final_prompt.find("preserve these terms").unwrap();
        assert!(body_idx < vocab_idx);
    }

    #[test]
    fn empty_dictionary_leaves_prompt_unchanged() {
        let skill = Skill {
            name: "x".into(),
            description: String::new(),
            triggers: vec![],
            inherit_base: false,
            body: "BODY".into(),
        };
        let base = skill.system_prompt();
        let composed = inject_vocabulary_prompt(&base, &Dictionary::new());
        assert_eq!(composed, base);
    }
}

#[cfg(test)]
mod apple_fm_tests {
    //! End-to-end check of the Apple Foundation Models provider through the
    //! `rekody-fm` helper. Ignored by default — requires the helper installed
    //! (`make fm-helper`) and Apple Intelligence enabled on this machine, so it
    //! never runs in CI. Run manually: `cargo test -p rekody-core --bin rekody
    //! apple_fm_end_to_end -- --ignored --nocapture`.
    use rekody_llm::{AppContext, AppleFoundationProvider, LlmProvider};

    #[tokio::test]
    #[ignore = "needs rekody-fm helper + Apple Intelligence; manual only"]
    async fn apple_fm_end_to_end() {
        let provider = AppleFoundationProvider::new();
        assert!(
            provider.is_available().await,
            "rekody-fm helper not found — run `make fm-helper`"
        );
        let ctx = AppContext {
            app_name: "Test".into(),
            bundle_id: None,
        };
        let out = provider
            .format(
                "um so yeah we should uh ship the the rollback script today you know",
                &ctx,
                "Rewrite the raw voice transcription as clean text. Remove filler \
                 words and fix grammar. Respond with ONLY the cleaned text.",
            )
            .await
            .expect("FM format failed");
        eprintln!("FM provider output: {:?}", out.text);
        assert!(!out.text.is_empty());
        assert_eq!(out.provider, "apple-foundation-models");
        // Filler should be gone.
        assert!(!out.text.to_lowercase().contains(" um "));
    }
}
