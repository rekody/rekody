//! Core pipeline orchestration for rekody voice dictation.
//!
//! Wires together all pipeline stages: hotkey → audio → VAD → STT →
//! dictionary correction → LLM (optional) → number normalization →
//! snippet expansion → injection.

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use rekody_audio::AudioConfig;
pub use rekody_hotkey::{ActivationMode, HotkeyConfig, HotkeyEvent, TriggerKey};
pub use rekody_inject::InjectionMethod;
pub use rekody_stt::WhisperModel;

/// Human-readable label for the dictation trigger, per platform. macOS uses
/// ⌥+Space; on Windows ⌥Space is unavailable (Alt+Space opens the window menu,
/// Win+Space switches input methods), so the keyboard hook uses Ctrl+Space.
/// Use this anywhere trigger keys are shown to the user so help/onboarding/the
/// live pill all name the key that actually works on the running platform.
#[cfg(target_os = "windows")]
pub const TRIGGER_LABEL: &str = "Ctrl + space";
#[cfg(not(target_os = "windows"))]
pub const TRIGGER_LABEL: &str = "⌥ + space";

/// Whether this build can put the Whisper encoder on the Apple Neural Engine.
///
/// The predicate is the same one that gates the `coreml` feature on
/// `whisper-rs` in `crates/rekody-stt/Cargo.toml`, so it is true exactly when
/// `ensure_coreml_encoder` has an encoder to install and whisper.cpp has a
/// backend to run it on. Everywhere else the encoder runs on CPU: Rekody
/// compiles whisper.cpp with `GGML_METAL=OFF` on every target (whisper-rs
/// 0.13 ships `default = []`), and there is no Neural Engine to fall back to.
pub const HAS_NEURAL_ENGINE: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// The Whisper size Setup preselects, chosen for the machine this build runs
/// on rather than assumed.
///
/// Turbo is `large-v3-turbo`: the decoder is cut from 32 layers to 4, but the
/// ENCODER is byte-for-byte the 32-layer large-v3 encoder (636,968,960
/// parameters in both). whisper.cpp classifies it as `MODEL_LARGE` for that
/// reason. Encoding is also a fixed cost per dictation, not a per-second one,
/// because `whisper_full` zero-pads every utterance to a 30 s window
/// (`WHISPER_CHUNK_SIZE`). So on Apple Silicon, where the encoder runs on the
/// Neural Engine, Turbo really is "fast with near-large accuracy"; on a CPU
/// it is a large-model encode on every sentence and the label is a promise
/// the hardware cannot keep (#141).
///
/// Small is the pick off the CPU cost/accuracy curve: on the one laptop-class
/// Intel Mac in whisper.cpp's benchmark thread (i7-8750H, 8 threads, AVX2
/// BLAS) the published per-window encode times are tiny 0.49 s, small 3.96 s,
/// medium 13.08 s, large 25.56 s, while multilingual WER goes 21.71 (tiny),
/// 13.96 (small), 12.50 (medium). Small buys 36% of the available accuracy
/// for 8% of large's time; medium and turbo cost 3x and 6x more per point
/// gained. Tiny is rejected on quality, not speed: roughly one word in five
/// is not dictation.
///
/// Those numbers are from 2022 and cover one laptop, so they set the ORDER
/// with confidence and the absolute times only loosely. `rekody bench
/// --model <size>` reproduces them on real hardware in a couple of minutes.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const DEFAULT_WHISPER_MODEL: &str = "turbo";
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub const DEFAULT_WHISPER_MODEL: &str = "small";

pub mod bench;
pub mod cleanup_guard;
pub mod command_mode;
pub mod config_tui;
pub mod context;
pub mod corrections;
pub mod dictionary;
pub mod history;
pub mod history_tui;
pub mod hud;
pub mod key_tui;
pub mod numbers;
pub mod onboarding;
pub mod prompts;
pub mod skill;
pub mod skill_tui;
pub mod snippets;
pub mod stats;
#[cfg(feature = "nemotron")]
pub mod streaming;
pub mod stt_catalog;
pub mod training_data;

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

/// The `input_device` preference: one pinned name (today's form) or an
/// ordered fallback chain (issue #122). Untagged so the TOML value is a
/// plain string or a plain array; existing string configs parse unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputDevicePref {
    /// One pinned device name: use it when connected, otherwise the system
    /// default.
    Name(String),
    /// Ordered preference chain: the first connected device captures.
    Chain(Vec<String>),
}

impl InputDevicePref {
    /// The preference as an ordered chain for `rekody-audio`: a single pin
    /// becomes a one-entry chain. Blank entries and the `"system"` sentinel
    /// are kept; resolution skips them.
    pub fn to_chain(&self) -> Vec<String> {
        match self {
            Self::Name(name) => vec![name.clone()],
            Self::Chain(chain) => chain.clone(),
        }
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
    /// STT engine id. Run `rekody engines` for the list this build offers;
    /// `stt_catalog` is the table both that command and this field read.
    #[serde(default = "default_stt_engine")]
    pub stt_engine: String,
    /// Deepgram API key (only needed if stt_engine = "deepgram").
    #[serde(default)]
    pub deepgram_api_key: Option<String>,
    /// Google AI Studio API key (only needed if stt_engine = "gemini").
    ///
    /// The same key the Gemini LLM provider uses, and the same keychain
    /// account (`gemini`), because Google issues one key for both.
    #[serde(default)]
    pub gemini_api_key: Option<String>,
    /// Port for the local Cohere STT server (only needed if stt_engine = "cohere").
    #[serde(default = "default_cohere_stt_port")]
    pub cohere_stt_port: u16,

    // --- Custom OpenAI-compatible STT provider (stt_engine = "custom") ---
    //
    // Opt-in only. These three keys are inert for every other engine, and
    // the pipeline refuses to start rather than guess a destination for
    // someone's voice. See `stt_catalog`'s CUSTOM entry.
    /// API base URL for `stt_engine = "custom"`, e.g.
    /// `"https://api.openai.com/v1"`. Must be https, or http on localhost.
    #[serde(default)]
    pub custom_stt_base_url: Option<String>,
    /// Model id the custom endpoint expects, e.g. `"whisper-1"`.
    #[serde(default)]
    pub custom_stt_model: Option<String>,
    /// API key for the custom endpoint. Optional: a vLLM or LM Studio server
    /// on localhost usually wants none.
    #[serde(default)]
    pub custom_stt_api_key: Option<String>,
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
    /// Preferred microphone(s), matched by name (case-insensitive substring).
    /// `None`/omitted (or `"system"`) follows the OS default input — which
    /// drifts to whatever was connected last (e.g. AirPods). Accepts either
    /// TOML form:
    ///
    /// ```toml
    /// input_device = "Shure MVX2U"                              # one pin
    /// input_device = ["Shure MVX2U", "MacBook Air Microphone"]  # ordered chain
    /// ```
    ///
    /// A single name pins capture to that device, falling back to the system
    /// default when it is not connected. A list is an ordered preference
    /// chain: at each recording start the first connected entry captures;
    /// when none is connected, capture follows the system default and a
    /// warning names what was tried. An empty list behaves like the key
    /// being unset.
    #[serde(default)]
    pub input_device: Option<InputDevicePref>,
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
    /// `0` means no limit. Default: 1800 (30 minutes), matching the app's
    /// Settings picker (5/10/20/30/60 min).
    #[serde(default = "default_max_recording_secs")]
    pub max_recording_secs: u64,

    /// Save each dictation's audio (FLAC) + raw transcript locally under
    /// `~/.local/share/rekody/training-data/` so a personal fine-tuning
    /// dataset is ready when wanted. Local-only — nothing leaves the
    /// machine. Default: on (set `save_training_data = false` to disable);
    /// the daemon announces the capture path at startup and `rekody doctor`
    /// shows the dataset size.
    #[serde(default = "default_save_training_data")]
    pub save_training_data: bool,

    /// Show the floating HUD pill while dictating (requires the `rekody-hud`
    /// helper binary).
    ///
    /// - `None` (default / omitted from config): auto — the HUD runs when the
    ///   helper binary is found next to the daemon executable (or at
    ///   `$REKODY_HUD_BIN`), and is silently off otherwise.
    /// - `Some(true)`: same as auto (the helper binary is still required).
    /// - `Some(false)`: never show the HUD.
    ///
    /// Set via `hud = true` / `hud = false` in config.toml.
    #[serde(default)]
    pub hud: Option<bool>,

    /// Decode-time dictionary term biasing for the Nemotron streaming
    /// engine (`[term_biasing]` table; spec:
    /// docs/design/nemotron-term-biasing-spec.md). Serde-defaulted so every
    /// existing config.toml keeps parsing; when the table is absent the
    /// feature is OFF. `enabled = true` has an effect only when
    /// `stt_engine = "nemotron"`; every other engine ignores the table.
    #[serde(default)]
    pub term_biasing: TermBiasingConfig,
}

/// The `[term_biasing]` config table: engine-level dictionary term biasing
/// on the Nemotron greedy decode path (issue #91). Defaults are the spec's
/// starting values; they stay behind `enabled = false` until the evaluation
/// gate in the spec's section 6 passes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermBiasingConfig {
    /// Feature flag, default false. Only effective when
    /// `stt_engine = "nemotron"`.
    #[serde(default)]
    pub enabled: bool,
    /// Logits added per matched token at trie depth 1. Default 3.0.
    #[serde(default = "default_term_biasing_boost")]
    pub boost: f32,
    /// A token is only boosted when its logit is within this distance of
    /// the step's best logit. Default 10.0 (tuned on the shipped int8
    /// model; see docs/design/nemotron-term-biasing-spec.md and #91).
    #[serde(default = "default_term_biasing_margin")]
    pub margin: f32,
    /// Boost multiplier for tokens at trie depth >= 2, so a started phrase
    /// is carried to completion. Default 3.0 (tuned on the int8 model).
    #[serde(default = "default_term_biasing_depth_factor")]
    pub depth_factor: f32,
    /// Hard cap on the number of dictionary terms biased. Default 200.
    #[serde(default = "default_term_biasing_max_terms")]
    pub max_terms: usize,
}

impl Default for TermBiasingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            boost: default_term_biasing_boost(),
            margin: default_term_biasing_margin(),
            depth_factor: default_term_biasing_depth_factor(),
            max_terms: default_term_biasing_max_terms(),
        }
    }
}

fn default_term_biasing_boost() -> f32 {
    3.0
}

fn default_term_biasing_margin() -> f32 {
    10.0
}

fn default_term_biasing_depth_factor() -> f32 {
    3.0
}

fn default_term_biasing_max_terms() -> usize {
    200
}

fn default_save_training_data() -> bool {
    true
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
    1800
}

impl RekodyConfig {
    /// The `input_device` preference as the ordered chain `rekody-audio`
    /// consumes: unset gives an empty chain (system default), a string gives
    /// one entry, an array passes through in order.
    pub fn input_device_chain(&self) -> Vec<String> {
        self.input_device
            .as_ref()
            .map(InputDevicePref::to_chain)
            .unwrap_or_default()
    }
}

impl Default for RekodyConfig {
    fn default() -> Self {
        Self {
            activation_mode: "both".into(),
            providers: vec![],
            llm_provider: "groq".into(),
            groq_api_key: None,
            cerebras_api_key: None,
            whisper_model: DEFAULT_WHISPER_MODEL.into(),
            stt_engine: "local".into(),
            deepgram_api_key: None,
            gemini_api_key: None,
            cohere_stt_port: 8099,
            custom_stt_base_url: None,
            custom_stt_model: None,
            custom_stt_api_key: None,
            vad_threshold: 0.01,
            record_all_audio: false,
            input_device: None,
            injection_method: "clipboard".into(),
            llm_enabled: None,
            stt_language: None,
            trigger_key: default_trigger_key(),
            max_recording_secs: default_max_recording_secs(),
            save_training_data: true,
            hud: None,
            term_biasing: TermBiasingConfig::default(),
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
///
/// `"both"` (hold = push-to-talk, quick tap = hands-free latch) is the
/// default for new configs; explicit `"push_to_talk"` / `"toggle"` are
/// honored so existing setups keep their exact behavior.
fn parse_activation_mode(s: &str) -> ActivationMode {
    match s.to_lowercase().as_str() {
        "toggle" => ActivationMode::Toggle,
        "push_to_talk" | "ptt" | "hold" => ActivationMode::PushToTalk,
        _ => ActivationMode::Both,
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
/// - `llm_enabled` omitted (None, the default): skip when the STT provider
///   already formats its own text, run otherwise.
///
/// The "already formats its own text" set is not spelled out here. It is
/// [`stt_catalog::SttProvider::formats_own_text`], so a future provider with
/// its own punctuation declares that in the catalog and this function needs
/// no edit. An engine id this build does not know keeps cleanup ON, which is
/// the safe direction: raw transcripts read worse than doubly-cleaned ones.
/// Which Gemini transcription mode a config resolves to.
///
/// The model can return what was said or a tidied version of it, and the two
/// disagree: verbatim keeps "Tuesday, no, Wednesday, um", smart returns
/// "Wednesday" with nothing marking the edit.
///
/// This is not a user setting, and deliberately so. Rekody files each
/// dictation's audio next to its raw transcript as a fine-tuning dataset
/// (`training_data::save_pair`, called with the engine's own text before any
/// cleanup runs), so a smart transcript stored there is a label of words
/// nobody spoke. Offering the mode as a choice would mean either letting a
/// user create that contradiction or silently overriding the choice they
/// made. Deriving it from `save_training_data` means there is one rule, it
/// always holds, and `rekody doctor` can state it.
pub fn gemini_mode_for(save_training_data: bool) -> rekody_stt::GeminiMode {
    if save_training_data {
        rekody_stt::GeminiMode::Verbatim
    } else {
        rekody_stt::GeminiMode::Smart
    }
}

/// Why [`gemini_mode_for`] chose what it chose, in words a user reads.
pub fn gemini_mode_reason(save_training_data: bool) -> &'static str {
    if save_training_data {
        "saving training data, so the transcript keeps every word you said"
    } else {
        "not saving training data, so filler and false starts are tidied away"
    }
}

pub fn has_llm_providers(config: &RekodyConfig) -> bool {
    if config.providers.is_empty() {
        return false;
    }
    match config.llm_enabled {
        Some(false) => false,
        Some(true) => true,
        None => !stt_catalog::find(&config.stt_engine).is_some_and(|p| p.formats_own_text),
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

/// Modification time of the personal dictionary file, `None` when the file
/// (or `HOME`) is missing. Used by the streaming path to notice edits.
#[cfg(feature = "nemotron")]
fn dictionary_mtime() -> Option<std::time::SystemTime> {
    dictionary::Dictionary::default_path()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())
}

/// Re-stat the dictionary file against the recorded mtime. When it moved
/// (edit, create, or delete), update the stamp and return the fresh term
/// list for a `StreamMsg::ReloadTerms`; `None` means nothing changed. One
/// `stat` per utterance start, off the audio hot path.
#[cfg(feature = "nemotron")]
fn dictionary_terms_if_changed(
    last_mtime: &mut Option<std::time::SystemTime>,
) -> Option<Vec<String>> {
    let mtime = dictionary_mtime();
    if mtime == *last_mtime {
        return None;
    }
    *last_mtime = mtime;
    Some(dictionary::Dictionary::load_or_empty().terms().to_vec())
}

/// Wraps the different STT engine types behind a common enum.
enum SttBackend {
    Local(rekody_stt::LocalWhisperEngine),
    /// Any endpoint speaking OpenAI's `/v1/audio/transcriptions`:
    /// the `groq` preset, or a `custom` endpoint the user supplied.
    OpenAiCompat(rekody_stt::OpenAiCompatEngine),
    Deepgram(rekody_stt::DeepgramEngine),
    /// Google `gemini-3.5-transcribe`, batch. The verbatim/smart mode was
    /// resolved in `Pipeline::new` and is fixed for the engine's lifetime.
    Gemini(rekody_stt::GeminiTranscribeEngine),
    Cohere(rekody_stt::CohereLocalEngine),
    /// Nemotron cache-aware streaming (English). The engine itself lives on a
    /// dedicated thread spawned in `run_streaming`; this variant only carries
    /// the model directory and routes `run()` to the streaming event loop.
    #[cfg(feature = "nemotron")]
    NemotronStreaming {
        model_dir: std::path::PathBuf,
    },
}

impl SttBackend {
    async fn transcribe(&self, samples: &[f32]) -> Result<rekody_stt::Transcript> {
        use rekody_stt::SttEngine;
        match self {
            SttBackend::Local(e) => e.transcribe(samples).await,
            SttBackend::OpenAiCompat(e) => e.transcribe(samples).await,
            SttBackend::Deepgram(e) => e.transcribe(samples).await,
            SttBackend::Gemini(e) => e.transcribe(samples).await,
            SttBackend::Cohere(e) => e.transcribe(samples).await,
            #[cfg(feature = "nemotron")]
            SttBackend::NemotronStreaming { .. } => {
                anyhow::bail!("nemotron is a streaming engine; batch transcribe is not supported")
            }
        }
    }

    /// Forward dictionary terms to the engine so decoding is biased toward
    /// the user's vocabulary (local Whisper initial prompt, Deepgram
    /// keyterms, Groq prompt). Engines without a biasing mechanism take the
    /// trait's default no-op.
    fn set_bias_terms(&self, terms: &[String]) {
        use rekody_stt::SttEngine;
        match self {
            SttBackend::Local(e) => e.set_bias_terms(terms),
            SttBackend::OpenAiCompat(e) => e.set_bias_terms(terms),
            SttBackend::Deepgram(e) => e.set_bias_terms(terms),
            SttBackend::Gemini(e) => e.set_bias_terms(terms),
            SttBackend::Cohere(e) => e.set_bias_terms(terms),
            #[cfg(feature = "nemotron")]
            SttBackend::NemotronStreaming { .. } => {}
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

/// Probe the frontmost application off the hot path. Spawned at recording
/// START (the user is about to speak for seconds, so the ~100ms osascript
/// call never delays anything); the run loops resolve it at finalize time
/// via [`resolve_app_probe`]. Capturing at start is also more truthful than
/// probing at finalize: the start app is the one the text lands in.
fn spawn_app_probe() -> tokio::task::JoinHandle<rekody_llm::AppContext> {
    tokio::task::spawn_blocking(context::detect_active_app)
}

/// Tell the user their recording hit the duration cap and was force-stopped.
/// Without this, a hands-free session ending on its own looks like a bug —
/// the user never pressed stop. Fire-and-forget; must never block the
/// audio path.
fn notify_max_recording_stop(max_secs: u64) {
    let minutes = max_secs.div_ceil(60);
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"Recording stopped after {minutes} minutes \
             (the safety cap). Your dictation is still being transcribed.\" \
             with title \"Rekody\""
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    tracing::info!(minutes, "recording stopped at the duration cap");
}

/// Resolve a pending app probe into `cache`, waiting at most 300ms (only
/// sub-second dictations can still be racing it). Returns the cached app,
/// which stays valid for every utterance until the next recording start.
async fn resolve_app_probe(
    probe: &mut Option<tokio::task::JoinHandle<rekody_llm::AppContext>>,
    cache: &mut Option<rekody_llm::AppContext>,
) -> Option<rekody_llm::AppContext> {
    if let Some(handle) = probe.take() {
        match tokio::time::timeout(std::time::Duration::from_millis(300), handle).await {
            Ok(Ok(ctx)) => *cache = Some(ctx),
            _ => tracing::debug!("app-at-start probe did not resolve in time"),
        }
    }
    cache.clone()
}

/// RMS energy of one live-tap chunk, as the pill's waveform reads it.
///
/// Kept as its own function so every engine loop computes the level the same
/// way. Two details are deliberate: `max(1)` rather than an early return, so
/// an empty chunk still yields `sqrt(0.0 / 1.0)`, and the summation order,
/// which fixes the f32 result bit for bit. Both are what the streaming loop
/// has always done, and
/// `mic_level_is_bit_identical_to_the_legacy_streaming_expression` pins
/// them.
#[inline]
fn mic_level_rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// Emit the mic level that drives the pill's waveform and the terminal
/// sparkline.
///
/// EVERY run loop that captures audio must call this once per live-tap chunk.
/// It is the only producer of the `mic level` event that `UiLayer` and
/// `HudLayer` (main.rs) listen for, so a loop that forgets it ships a pill
/// whose bar never moves while the mic is live. That was issue #140.
///
/// Three properties are contractual and must not drift:
///
/// * **Target `rekody_core`.** The default `EnvFilter` carries
///   `rekody_core=debug`; an event raised from another crate would be
///   filtered away before either UI layer saw it.
/// * **Level `debug`.** Keeps default log filtering unchanged. This is the
///   highest-frequency event in the system (~20-100/s while the key is held).
/// * **Field named `rms`.** Both layers look the value up by that name.
#[inline]
fn emit_mic_level(samples: &[f32]) {
    let rms = mic_level_rms(samples);
    tracing::debug!(rms, "mic level");
}

impl Pipeline {
    pub fn new(config: RekodyConfig) -> Result<Self> {
        let provider_chain = build_provider_chain(&config);
        let injection_method = parse_injection_method(&config.injection_method);

        // Initialize the STT engine based on config.
        let lang = config.stt_language.clone();
        // Trimmed to match `stt_catalog::find`, which every other surface
        // uses. Without this a padded `stt_engine = "  deepgram  "` chose
        // one engine here and a different cleanup default there.
        let stt = match config.stt_engine.trim().to_lowercase().as_str() {
            "groq" => {
                let key = config.groq_api_key.clone().unwrap_or_default();
                tracing::info!(language = ?lang, "using Groq cloud STT (Whisper Large v3)");
                SttBackend::OpenAiCompat(rekody_stt::OpenAiCompatEngine::groq(key, lang))
            }
            // The user's own endpoint. Every failure mode is reported here,
            // before a microphone is ever opened, and none of them falls
            // through to another engine: sending someone's voice somewhere
            // they did not choose would be worse than not starting.
            "custom" => {
                let base_url = config.custom_stt_base_url.clone().unwrap_or_default();
                let model = config.custom_stt_model.clone().unwrap_or_default();
                let key = config.custom_stt_api_key.clone().unwrap_or_default();
                let engine = rekody_stt::OpenAiCompatEngine::custom(&base_url, model, key, lang)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "{e}\n\nThe custom speech-to-text provider is configured with \
                                 custom_stt_base_url and custom_stt_model in config.toml, \
                                 or in Settings."
                        )
                    })?;
                tracing::info!(
                    host = %engine.destination_host(),
                    "using a custom OpenAI-compatible STT endpoint"
                );
                SttBackend::OpenAiCompat(engine)
            }
            "deepgram" => {
                let key = config.deepgram_api_key.clone().unwrap_or_default();
                // Default to "multi" (Nova-3 multilingual) when no language is pinned.
                let dg_lang = lang.unwrap_or_else(|| "multi".to_string());
                tracing::info!(language = %dg_lang, "using Deepgram cloud STT (Nova-3)");
                SttBackend::Deepgram(rekody_stt::DeepgramEngine::with_language(key, dg_lang))
            }
            // Google's transcribe model has two modes that disagree about
            // what was said. Verbatim keeps filler and abandoned starts;
            // smart quietly removes them, so "Tuesday, no, Wednesday, um"
            // comes back as "Wednesday" with nothing marking the edit.
            //
            // Rekody saves each dictation's audio next to its raw transcript
            // as a fine-tuning dataset (`training_data::save_pair`, called
            // with the engine's own text before any cleanup runs). A smart
            // transcript filed there is a label that does not match its
            // audio: it teaches an acoustic model that speech contains words
            // nobody said. So the mode is not a setting. Saving training
            // data means verbatim, full stop, and only a user who has turned
            // that capture off gets the tidier text.
            //
            // Resolved once here because the daemon has no config reload:
            // `Pipeline::new` runs at start and `self.config` cannot change
            // under it, so the mode cannot drift out of step with the flag
            // that chose it.
            "gemini" => {
                let key = config.gemini_api_key.clone().unwrap_or_default();
                let mode = gemini_mode_for(config.save_training_data);
                tracing::info!(
                    mode = mode.label(),
                    "using Gemini cloud STT (gemini-3.5-transcribe): {}",
                    gemini_mode_reason(config.save_training_data)
                );
                SttBackend::Gemini(rekody_stt::GeminiTranscribeEngine::new(key, mode))
            }
            "cohere" => {
                tracing::info!(port = config.cohere_stt_port, "using Cohere local STT");
                SttBackend::Cohere(rekody_stt::CohereLocalEngine::new(config.cohere_stt_port))
            }
            "nemotron" => {
                #[cfg(feature = "nemotron")]
                {
                    let model_dir = resolve_model_dir().join("nemotron-en-int8");
                    for file in ["encoder.onnx", "decoder_joint.onnx", "tokenizer.model"] {
                        let path = model_dir.join(file);
                        if !path.exists() {
                            anyhow::bail!(
                                "Nemotron model file missing: {}\n\
                                 Run `rekody setup` and pick the Nemotron streaming engine \
                                 to download it (~881MB).",
                                path.display()
                            );
                        }
                    }
                    tracing::info!(
                        dir = %model_dir.display(),
                        "using Nemotron streaming STT (English, on-device)"
                    );
                    SttBackend::NemotronStreaming { model_dir }
                }
                #[cfg(not(feature = "nemotron"))]
                anyhow::bail!(
                    "this rekody build does not include the Nemotron streaming engine \
                     (compiled without the `nemotron` feature)"
                )
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
        // Default-on local dataset capture must never be covert: announce
        // the path and the off switch at every startup.
        if self.config.save_training_data {
            tracing::info!(
                dir = %training_data::dataset_dir().display(),
                "saving dictation audio + transcripts locally for personal fine-tuning \
                 (save_training_data = false to disable)"
            );
        }
        #[cfg(feature = "nemotron")]
        if let SttBackend::NemotronStreaming { model_dir } = &self.stt {
            return self.run_streaming(model_dir.clone()).await;
        }
        self.run_batch().await
    }

    /// Batch event loop: VAD-segmented audio → STT at key release.
    async fn run_batch(&self) -> Result<()> {
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
        //
        //    The live tap is subscribed BEFORE open() (the processing thread
        //    picks the sender up there) purely to drive the waveform: this
        //    loop transcribes from `segment_rx` and throws the tap's samples
        //    away after taking their level. Without it the pill's bar sits
        //    dead for the whole recording on every Whisper dictation (#140).
        let audio_config = AudioConfig {
            vad_threshold: self.config.vad_threshold,
            record_all_audio: self.config.record_all_audio,
            input_device: self.config.input_device_chain(),
        };
        let audio_capture = rekody_audio::AudioCapture::new(audio_config.clone());
        let mut live_rx = audio_capture.live_chunks();
        let mut segment_rx = audio_capture.open(audio_config)?;
        tracing::info!("audio capture initialized (live tap active)");

        let llm_enabled = has_llm_providers(&self.config);
        if llm_enabled {
            tracing::info!("LLM post-processing enabled");
        } else {
            tracing::info!("no LLM API keys configured; raw STT output will be used");
        }

        // Frontmost-app capture for history tags + cleanup context: probed
        // at recording start, resolved when the segment finalizes.
        let mut app_probe: Option<tokio::task::JoinHandle<rekody_llm::AppContext>> = None;
        let mut app_cache: Option<rekody_llm::AppContext> = None;

        // One-time notice when Tab is pressed without an LLM provider.
        let mut skill_cycle_notice_logged = false;

        // Gate for the waveform tap: a cpal callback in flight at RecordStop
        // can still land a chunk in `live_rx` after the key is up. Dropping
        // those keeps the bar from twitching once the pill says "working…".
        let mut recording = false;

        // 3. Main event loop — listen for hotkey events and audio segments
        //    concurrently using tokio::select!.
        loop {
            tokio::select! {
                hotkey_event = hotkey_rx.recv() => {
                    match hotkey_event {
                        Some(HotkeyEvent::RecordStart) => {
                            tracing::info!(source = "hotkey", "recording started");
                            app_cache = None;
                            app_probe = Some(spawn_app_probe());
                            recording = true;
                            audio_capture.start_recording();
                        }
                        Some(HotkeyEvent::RecordStop) => {
                            tracing::info!(source = "hotkey", "recording stopped");
                            recording = false;
                            audio_capture.stop_recording();
                        }
                        Some(HotkeyEvent::RecordStopForced) => {
                            tracing::warn!(source = "deadman", "recording force-stopped at the duration cap");
                            notify_max_recording_stop(self.config.max_recording_secs);
                            recording = false;
                            audio_capture.stop_recording();
                        }
                        Some(HotkeyEvent::RecordLatched) => {
                            // Display-only: recording already runs; tells the
                            // UI layers to show the hands-free stop hint.
                            tracing::info!("hands-free latched");
                        }
                        Some(HotkeyEvent::CommandMode) => {
                            tracing::info!("command mode activated (not yet implemented)");
                        }
                        Some(HotkeyEvent::CycleSkill) => {
                            if llm_enabled {
                                // ⌥Space+Tab: advance the sticky skill. The next
                                // dictation picks it up via the fresh-read in
                                // process_segment. Surface the new selection.
                                let now = skill::cycle_active();
                                tracing::info!(
                                    skill = now.as_deref().unwrap_or("Auto"),
                                    "skill cycled"
                                );
                            } else if !skill_cycle_notice_logged {
                                // Skills only apply during AI cleanup, so the
                                // gesture stays quiet instead of advertising a
                                // feature that cannot run.
                                skill_cycle_notice_logged = true;
                                tracing::info!(
                                    "skill cycling ignored: skills need AI cleanup \
                                     enabled (add an LLM provider via rekody setup)"
                                );
                            }
                        }
                        None => {
                            tracing::warn!("hotkey channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Waveform only. The transcript comes from `segment_rx`
                // below; these samples exist so the pill and the terminal
                // sparkline can show that the mic is live, and are dropped
                // straight after their level is taken.
                chunk = live_rx.recv() => {
                    match chunk {
                        Some(samples) if recording => emit_mic_level(&samples),
                        Some(_) => {}
                        None => {
                            tracing::warn!("live audio channel closed, shutting down");
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

                            let app_at_start =
                                resolve_app_probe(&mut app_probe, &mut app_cache).await;
                            if let Err(e) = self
                                .process_segment(&audio_segment, llm_enabled, app_at_start)
                                .await
                            {
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

    /// Streaming event loop (Nemotron): decode WHILE the key is held via the
    /// live audio tap, show partials on the status line, inject the final
    /// transcript once at key release (~100ms flush vs 1–3s batch).
    ///
    /// Partials are never typed into the target app — backspace-revision
    /// breaks in terminals, vim, and secure input fields. Display-only.
    #[cfg(feature = "nemotron")]
    async fn run_streaming(&self, model_dir: std::path::PathBuf) -> Result<()> {
        tracing::info!("rekody pipeline starting (streaming mode)");

        // 1. Hotkey listener — same setup as batch.
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

        // 2. Audio capture with the live tap subscribed BEFORE open(), so the
        //    processing thread picks up the sender. The VAD/segment channel
        //    still exists but its segments are ignored in this mode.
        let audio_config = AudioConfig {
            vad_threshold: self.config.vad_threshold,
            record_all_audio: self.config.record_all_audio,
            input_device: self.config.input_device_chain(),
        };
        let audio_capture = rekody_audio::AudioCapture::new(audio_config.clone());
        let mut live_rx = audio_capture.live_chunks();
        let mut segment_rx = audio_capture.open(audio_config)?;
        tracing::info!("audio capture initialized (live tap active)");

        // 3. Decode-time dictionary term biasing (issue #91), config-flagged
        //    and DEFAULT OFF. Disabled means `None` reaches the engine thread
        //    and no logits processor is ever installed: decoding stays
        //    byte-identical to a build without the feature. Enabled builds
        //    the initial settings from the `[term_biasing]` table plus the
        //    personal dictionary; later dictionary edits are picked up per
        //    utterance via the mtime re-stat below (`StreamMsg::ReloadTerms`),
        //    preserving the Dictionary v1 behavior that `rekody dictionary
        //    add` applies without a daemon restart.
        let bias_enabled = self.config.term_biasing.enabled;
        let mut dict_mtime: Option<std::time::SystemTime> = None;
        let bias = if bias_enabled {
            dict_mtime = dictionary_mtime();
            let tb = &self.config.term_biasing;
            let dict = dictionary::Dictionary::load_or_empty();
            tracing::info!(
                terms = dict.terms().len(),
                boost = tb.boost,
                margin = tb.margin,
                "term biasing enabled for the Nemotron streaming engine"
            );
            Some(rekody_stt::biasing::BiasSettings {
                terms: dict.terms().to_vec(),
                boost: tb.boost,
                margin: tb.margin,
                depth_factor: tb.depth_factor,
                max_terms: tb.max_terms,
            })
        } else {
            None
        };

        // 4. Engine thread (model loads there, ~3.4s; samples queue meanwhile).
        let (stream_tx, mut stream_rx) = streaming::spawn(model_dir, bias);

        let llm_enabled = has_llm_providers(&self.config);
        if llm_enabled {
            tracing::info!("LLM post-processing enabled");
        } else {
            tracing::info!("no LLM API keys configured; raw STT output will be used");
        }

        // Gate for forwarding tap chunks: a callback in flight at RecordStop
        // can land a chunk in live_rx AFTER we drain+flush — without this it
        // would leak into the next utterance's buffer.
        let mut recording = false;
        // Utterance audio accumulated for the training-data pair (opt-out
        // via save_training_data = false). ~320KB per 5s of speech.
        let mut utterance_samples: Vec<f32> = Vec::new();
        // Speaking-duration source (streaming Nemotron path): count of 16kHz
        // live-tap samples fed to the engine for this utterance — from
        // RecordStart through the post-release drain. Counted unconditionally
        // (utterance_samples above only fills when save_training_data is on),
        // and samples/sample_rate is exact where hotkey press/release
        // wall-clock instants would drift from the audio actually captured.
        let mut utterance_sample_count: u64 = 0;
        // Frontmost-app capture for history tags + cleanup context: probed
        // at recording start, resolved when the utterance finalizes.
        let mut app_probe: Option<tokio::task::JoinHandle<rekody_llm::AppContext>> = None;
        let mut app_cache: Option<rekody_llm::AppContext> = None;

        // One-time notice when Tab is pressed without an LLM provider.
        let mut skill_cycle_notice_logged = false;

        // Release tail: when the key is released, audio for the last word(s)
        // is still draining through the capture pipeline (cpal callback →
        // resampler → tap). Cutting the mic instantly drops it. Instead we
        // keep capturing for a short window, let the pipeline empty, THEN
        // stop + flush. `stop_deadline` is the absolute time to do that.
        const RELEASE_TAIL: std::time::Duration = std::time::Duration::from_millis(180);
        let mut stop_deadline: Option<tokio::time::Instant> = None;

        loop {
            tokio::select! {
                hotkey_event = hotkey_rx.recv() => {
                    match hotkey_event {
                        Some(HotkeyEvent::RecordStart) => {
                            if stop_deadline.take().is_some() {
                                // Re-pressed within the release tail — the mic
                                // never stopped, so treat it as the same
                                // utterance (forgives a brief accidental lift).
                                tracing::debug!("recording resumed within release tail");
                            } else {
                                tracing::info!(source = "hotkey", "recording started");
                                // Utterance start: re-stat the dictionary file
                                // and rebuild the biasing trie when it changed
                                // (`rekody dictionary add` with no restart).
                                // The reload lands on the engine thread ahead
                                // of this utterance's samples (same channel,
                                // in order), between utterances by design.
                                if bias_enabled
                                    && let Some(terms) =
                                        dictionary_terms_if_changed(&mut dict_mtime)
                                {
                                    let _ = stream_tx
                                        .send(streaming::StreamMsg::ReloadTerms(terms));
                                }
                                recording = true;
                                utterance_samples.clear();
                                utterance_sample_count = 0;
                                app_cache = None;
                                app_probe = Some(spawn_app_probe());
                                audio_capture.start_recording();
                            }
                        }
                        Some(HotkeyEvent::RecordStop) => {
                            // Don't cut the mic yet — the last word(s) may still
                            // be draining through the pipeline. Keep capturing
                            // for RELEASE_TAIL, then stop + flush (deadline
                            // branch below). The tap keeps forwarding meanwhile.
                            stop_deadline =
                                Some(tokio::time::Instant::now() + RELEASE_TAIL);
                        }
                        Some(HotkeyEvent::RecordStopForced) => {
                            tracing::warn!(source = "deadman", "recording force-stopped at the duration cap");
                            notify_max_recording_stop(self.config.max_recording_secs);
                            stop_deadline =
                                Some(tokio::time::Instant::now() + RELEASE_TAIL);
                        }
                        Some(HotkeyEvent::RecordLatched) => {
                            // Display-only: recording already runs; tells the
                            // UI layers to show the hands-free stop hint.
                            tracing::info!("hands-free latched");
                        }
                        Some(HotkeyEvent::CommandMode) => {
                            tracing::info!("command mode activated (not yet implemented)");
                        }
                        Some(HotkeyEvent::CycleSkill) => {
                            if llm_enabled {
                                let now = skill::cycle_active();
                                tracing::info!(
                                    skill = now.as_deref().unwrap_or("Auto"),
                                    "skill cycled"
                                );
                            } else if !skill_cycle_notice_logged {
                                // Skills only apply during AI cleanup, so the
                                // gesture stays quiet instead of advertising a
                                // feature that cannot run.
                                skill_cycle_notice_logged = true;
                                tracing::info!(
                                    "skill cycling ignored: skills need AI cleanup \
                                     enabled (add an LLM provider via rekody setup)"
                                );
                            }
                        }
                        None => {
                            tracing::warn!("hotkey channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Release tail elapsed: the pipeline has drained, so stop the
                // mic, sweep up any final chunks, and flush the utterance.
                () = async {
                    match stop_deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    tracing::info!(source = "hotkey", "recording stopped");
                    audio_capture.stop_recording();
                    while let Ok(chunk) = live_rx.try_recv() {
                        utterance_sample_count += chunk.len() as u64;
                        if self.config.save_training_data {
                            utterance_samples.extend_from_slice(&chunk);
                        }
                        let _ = stream_tx.send(streaming::StreamMsg::Samples(chunk));
                    }
                    recording = false;
                    stop_deadline = None;
                    if stream_tx.send(streaming::StreamMsg::Flush).is_err() {
                        anyhow::bail!("nemotron engine thread died");
                    }
                }

                chunk = live_rx.recv() => {
                    match chunk {
                        // Drop stragglers that arrive after flush (see gate above).
                        Some(samples) if recording => {
                            // Mic level for the UI waveform (picked up by the
                            // tracing UI layer; ~20-100 events/s while held).
                            emit_mic_level(&samples);
                            utterance_sample_count += samples.len() as u64;
                            if self.config.save_training_data {
                                utterance_samples.extend_from_slice(&samples);
                            }
                            if stream_tx.send(streaming::StreamMsg::Samples(samples)).is_err() {
                                anyhow::bail!("nemotron engine thread died");
                            }
                        }
                        Some(_) => {}
                        None => {
                            tracing::warn!("live audio channel closed, shutting down");
                            break;
                        }
                    }
                }

                event = stream_rx.recv() => {
                    match event {
                        Some(streaming::StreamEvent::Partial(text)) => {
                            // Picked up by the UI layer for the status line.
                            tracing::info!(text = %text, "partial transcript");
                        }
                        Some(streaming::StreamEvent::Final { text, latency_ms }) => {
                            // Captured-speech duration for this utterance,
                            // from the unconditional live-tap sample counter.
                            let duration_ms = utterance_sample_count * 1000
                                / u64::from(rekody_audio::TARGET_SAMPLE_RATE);
                            if text.is_empty() {
                                tracing::debug!("empty transcript, skipping injection");
                            } else {
                                let app_at_start =
                                    resolve_app_probe(&mut app_probe, &mut app_cache).await;
                                if self.config.save_training_data
                                    && let Err(e) = training_data::save_pair(
                                        &utterance_samples,
                                        &text,
                                        &self.config.stt_engine,
                                        app_at_start.as_ref().map(|a| a.app_name.as_str()),
                                    ) {
                                        tracing::debug!(error = %e, "training pair not saved");
                                    }
                                if let Err(e) = self
                                    .process_transcript(
                                        &text,
                                        latency_ms,
                                        Some(duration_ms),
                                        llm_enabled,
                                        app_at_start,
                                    )
                                    .await
                                {
                                    tracing::error!(error = %e, "failed to process transcript");
                                }
                            }
                            utterance_samples.clear();
                            utterance_sample_count = 0;
                        }
                        Some(streaming::StreamEvent::Error(msg)) => {
                            tracing::error!(error = %msg, "nemotron streaming error");
                        }
                        None => {
                            tracing::error!("nemotron engine thread died, shutting down");
                            break;
                        }
                    }
                }

                // Drain VAD segments so the channel doesn't back up; the
                // streaming path replaces them entirely.
                segment = segment_rx.recv() => {
                    if segment.is_none() {
                        tracing::warn!("audio segment channel closed, shutting down");
                        break;
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
        app_at_start: Option<rekody_llm::AppContext>,
    ) -> Result<()> {
        // --- STT (model already loaded at startup) ---
        // Personal dictionary terms bias the engine's decoding (local
        // Whisper initial prompt, Deepgram keyterms, Groq prompt) before
        // the deterministic corrector ever sees the transcript. Read fresh
        // per dictation, like the corrector's own read in
        // `process_transcript`; that second read stays so the streaming
        // path keeps working unchanged.
        let dict = dictionary::Dictionary::load_or_empty();
        self.stt.set_bias_terms(dict.terms());
        // Batch transcription has a real pause between the last word and the
        // text, and on a CPU-only encode that pause is measured in seconds.
        // A static "transcribing…" with no sense of duration reads as a hang
        // (#141), so tick a counter for the UI layers for exactly as long as
        // the engine holds the audio. It stops before LLM cleanup, which has
        // its own verb, and the streaming loop never reaches here at all.
        let heartbeat = tokio::spawn(async move {
            let mut secs = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                secs += 1;
                tracing::debug!(secs, "transcription in progress");
            }
        });
        let transcribed = self.stt.transcribe(&segment.samples).await;
        heartbeat.abort();
        let transcript = transcribed?;

        if transcript.text.is_empty() {
            tracing::debug!("empty transcript, skipping injection");
            return Ok(());
        }

        // Opt-in: persist (audio, raw transcript) for personal fine-tuning.
        if self.config.save_training_data
            && let Err(e) = training_data::save_pair(
                &segment.samples,
                &transcript.text,
                &self.config.stt_engine,
                app_at_start.as_ref().map(|a| a.app_name.as_str()),
            )
        {
            tracing::debug!(error = %e, "training pair not saved");
        }

        // Speaking-duration source (batch Whisper path): the segment's exact
        // sample count at 16kHz. The segment holds precisely the audio
        // captured between recording start and stop (VAD-gated unless
        // record_all_audio), so samples/sample_rate is the most truthful
        // measure of captured speech — wall-clock instants would also include
        // pre-speech silence, and everything downstream of here (STT + LLM)
        // is processing time, not speaking time.
        let duration_ms =
            segment.samples.len() as u64 * 1000 / u64::from(rekody_audio::TARGET_SAMPLE_RATE);

        self.process_transcript(
            &transcript.text,
            transcript.latency_ms,
            Some(duration_ms),
            llm_enabled,
            app_at_start,
        )
        .await
    }

    /// Post-STT stages shared by batch and streaming paths: dictionary
    /// correction → LLM cleanup → dictionary correction (again) → number
    /// normalization → snippet expansion → injection → history.
    ///
    /// `duration_ms` is the captured-speech duration (recording start to
    /// stop) computed by the caller from its audio sample count — see the
    /// comments at each call site for the per-path source.
    ///
    /// `app_at_start` is the application that was frontmost when recording
    /// STARTED (probed off the hot path; see `spawn_app_probe`). That is the
    /// app the text lands in, and it is captured for every dictation so
    /// history tags don't depend on cleanup being enabled. `None` falls back
    /// to a synchronous probe on the cleanup path and "Unknown" in history.
    async fn process_transcript(
        &self,
        raw_text: &str,
        stt_latency_ms: u64,
        duration_ms: Option<u64>,
        llm_enabled: bool,
        app_at_start: Option<rekody_llm::AppContext>,
    ) -> Result<()> {
        tracing::info!(
            text = %raw_text,
            latency_ms = stt_latency_ms,
            "transcription complete"
        );

        // Personal dictionary, read fresh each dictation so `rekody
        // dictionary add` takes effect without a daemon restart. The
        // deterministic corrector runs on the raw transcript BEFORE optional
        // LLM cleanup — zero latency, works with cleanup off — and again
        // after cleanup inside `finalize_dictation_text`, protecting term
        // casing against LLM rewrites. The history entry's `raw_transcript`
        // keeps `raw_text` untouched (dataset purity for fine-tuning).
        let dict = dictionary::Dictionary::load_or_empty();
        let corrected_text = dictionary::correct_text(raw_text, &dict);

        // --- LLM post-processing ---
        let mut llm_latency_ms: Option<u64> = None;
        let mut llm_provider: Option<String> = None;
        let mut app_name = app_at_start
            .as_ref()
            .map(|a| a.app_name.clone())
            .unwrap_or_else(|| String::from("Unknown"));

        // Long-form gate: past a few thousand characters, AI cleanup is the
        // wrong tool — local models overflow their context window and can
        // generate for minutes, pinning the stop pipeline at "finishing"
        // while the text sits uninjected. The dictionary-corrected
        // transcript goes straight to injection instead.
        const CLEANUP_MAX_CHARS: usize = 5_000;
        let transcript_chars = corrected_text.chars().count();
        let run_cleanup = llm_enabled && transcript_chars <= CLEANUP_MAX_CHARS;
        if llm_enabled && !run_cleanup {
            tracing::warn!(
                chars = transcript_chars,
                limit = CLEANUP_MAX_CHARS,
                "transcript too long for AI cleanup; injecting corrected text directly"
            );
        }

        let final_text = if run_cleanup {
            // Application context for context-aware formatting: the app
            // captured at recording start, or a synchronous probe when no
            // start capture exists (VAD-only segments).
            let app_context = match app_at_start {
                Some(ctx) => ctx,
                None => context::detect_active_app(),
            };
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
            // preserves jargon/proper-nouns (e.g. "rekody" not "record") —
            // belt and suspenders on top of the deterministic corrector.
            // No-op when the dictionary is empty.
            let system_prompt = dictionary::inject_vocabulary_prompt(&base_prompt, &dict);

            // Send through the LLM provider chain (input already
            // dictionary-corrected).
            match self
                .provider_chain
                .format(&corrected_text, &app_context, &system_prompt)
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
                    // Guards on the cleanup output. Empty → corrected
                    // transcript. Chat-response shaped (the model ANSWERED
                    // the dictation instead of cleaning it) → corrected
                    // transcript, unless a user skill is active — skills
                    // legitimately transform text. See `cleanup_guard`.
                    if formatted.text.trim().is_empty() {
                        tracing::warn!("LLM returned empty text, using raw transcript");
                        corrected_text
                    } else if skill_name.is_none()
                        && let Some(reason) =
                            cleanup_guard::rejects(&corrected_text, &formatted.text)
                    {
                        tracing::warn!(
                            provider = %formatted.provider,
                            %reason,
                            "cleanup output rejected, using corrected transcript"
                        );
                        corrected_text
                    } else {
                        formatted.text
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "LLM formatting failed, falling back to raw transcript"
                    );
                    corrected_text
                }
            }
        } else {
            // No LLM configured (or the transcript is past the long-form
            // gate) — the dictionary-corrected STT output is already the
            // final wording.
            corrected_text
        };

        // Deterministic finishing pass: dictionary correction (again, cheap,
        // protects casing when the LLM ran) → number normalization → snippet
        // expansion. Snippets are read fresh each dictation so
        // `snippets.toml` edits apply without a daemon restart.
        let mut snippet_store = snippets::SnippetStore::new();
        if let Err(e) = snippet_store.load() {
            tracing::debug!(error = %e, "snippets not loaded, expansion skipped");
        }
        let final_text = finalize_dictation_text(&final_text, &dict, &snippet_store);

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
            raw_text.to_string(),
            duration_ms,
            stt_latency_ms,
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

/// Deterministic finishing pass shared by every dictation path, applied to
/// the final text right before injection and the history write:
///
/// 1. Dictionary correction ([`dictionary::correct_text`]) — recases and
///    fixes near-miss spellings of the user's terms. It already ran once on
///    the raw transcript before optional LLM cleanup; this second run is
///    microseconds and protects term casing when the LLM rewrote the text.
/// 2. Number/currency/percent/unit normalization ([`numbers::normalize`]) —
///    the LLM does this inconsistently and the raw path not at all.
/// 3. Snippet expansion ([`snippets::expand_triggers`]) — last, so stored
///    expansions are injected verbatim and never rewritten by the passes
///    above.
///
/// The raw transcript never flows through here: the history entry's
/// `raw_transcript` field keeps the untouched STT output.
fn finalize_dictation_text(
    text: &str,
    dict: &dictionary::Dictionary,
    snippet_store: &snippets::SnippetStore,
) -> String {
    let corrected = dictionary::correct_text(text, dict);
    let normalized = numbers::normalize(&corrected);
    snippets::expand_triggers(&normalized, snippet_store)
}

#[cfg(test)]
mod dictation_finalize_tests {
    //! Pipeline-seam checks for the deterministic post-STT pass:
    //! `process_transcript` feeds the final text through
    //! `finalize_dictation_text` and stores the result as the history
    //! entry's `text`, while `raw_transcript` keeps the raw STT output.

    use crate::dictionary::Dictionary;
    use crate::snippets::SnippetStore;
    use crate::{finalize_dictation_text, history};

    #[test]
    fn corrector_then_snippets_apply_and_raw_transcript_stays_raw() {
        let mut dict = Dictionary::new();
        dict.add_term("Rekody");
        let mut store = SnippetStore::with_path(std::path::PathBuf::from("/dev/null"));
        store.add_snippet("sig", "Best,\nTony Kipkemboi\nrekody.com");

        // What STT actually emits: a mangled product name and a spoken trigger.
        let raw = "tell them recody ships today slash sig";
        let finalized = finalize_dictation_text(raw, &dict, &store);
        assert_eq!(
            finalized,
            "tell them Rekody ships today Best,\nTony Kipkemboi\nrekody.com"
        );

        // The seam `process_transcript` writes: finalized text goes into
        // `text`, the raw transcript is stored untouched.
        let entry = history::History::new_entry(
            finalized.clone(),
            raw.to_string(),
            Some(2000),
            80,
            None,
            None,
            "Mail".to_string(),
        );
        assert_eq!(entry.text, finalized);
        assert_eq!(entry.raw_transcript, raw);
        assert!(entry.raw_transcript.contains("recody"));
        assert!(!entry.raw_transcript.contains("Rekody"));
    }

    #[test]
    fn finalize_without_dictionary_or_snippets_is_identity_for_plain_text() {
        let dict = Dictionary::new();
        let store = SnippetStore::with_path(std::path::PathBuf::from("/dev/null"));
        let text = "nothing to correct here.";
        assert_eq!(finalize_dictation_text(text, &dict, &store), text);
    }

    #[test]
    fn finalize_keeps_number_normalization_in_the_chain() {
        let dict = Dictionary::new();
        let store = SnippetStore::with_path(std::path::PathBuf::from("/dev/null"));
        let out = finalize_dictation_text("that costs fifty dollars", &dict, &store);
        assert_eq!(out, "that costs $50");
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
mod term_biasing_config_tests {
    //! The `[term_biasing]` config table (issue #91): absent table means the
    //! feature is OFF with the spec's default tunables, partial tables fill
    //! in spec defaults, and the nested table round-trips through the same
    //! `toml::to_string_pretty` the config TUI uses to save.

    use crate::RekodyConfig;

    /// The four keys every existing config.toml already has (no serde
    /// defaults on these fields).
    const MINIMAL: &str = r#"
activation_mode = "both"
whisper_model = "turbo"
vad_threshold = 0.01
injection_method = "clipboard"
"#;

    fn parse(extra: &str) -> RekodyConfig {
        toml::from_str(&format!("{MINIMAL}{extra}")).expect("config parses")
    }

    #[test]
    fn absent_table_is_off_with_spec_defaults() {
        let config = parse("");
        assert!(!config.term_biasing.enabled, "flag must default OFF");
        assert_eq!(config.term_biasing.boost, 3.0);
        assert_eq!(config.term_biasing.margin, 10.0);
        assert_eq!(config.term_biasing.depth_factor, 3.0);
        assert_eq!(config.term_biasing.max_terms, 200);
    }

    #[test]
    fn default_config_matches_absent_table() {
        let config = RekodyConfig::default();
        assert!(!config.term_biasing.enabled);
        assert_eq!(config.term_biasing.boost, 3.0);
        assert_eq!(config.term_biasing.margin, 10.0);
        assert_eq!(config.term_biasing.depth_factor, 3.0);
        assert_eq!(config.term_biasing.max_terms, 200);
    }

    #[test]
    fn partial_table_fills_spec_defaults() {
        let config = parse("[term_biasing]\nenabled = true\n");
        assert!(config.term_biasing.enabled);
        assert_eq!(config.term_biasing.boost, 3.0);
        assert_eq!(config.term_biasing.margin, 10.0);
        assert_eq!(config.term_biasing.depth_factor, 3.0);
        assert_eq!(config.term_biasing.max_terms, 200);
    }

    #[test]
    fn full_table_overrides_every_tunable() {
        let config = parse(
            "[term_biasing]\nenabled = true\nboost = 2.5\nmargin = 4.0\n\
             depth_factor = 2.0\nmax_terms = 50\n",
        );
        assert!(config.term_biasing.enabled);
        assert_eq!(config.term_biasing.boost, 2.5);
        assert_eq!(config.term_biasing.margin, 4.0);
        assert_eq!(config.term_biasing.depth_factor, 2.0);
        assert_eq!(config.term_biasing.max_terms, 50);
    }

    #[test]
    fn config_round_trips_through_config_tui_save_path() {
        // `config_tui` saves via `toml::to_string_pretty(&config)`; the new
        // nested table must serialize (no value-after-table ordering issue)
        // and re-parse to the same settings.
        let mut config = RekodyConfig::default();
        config.term_biasing.enabled = true;
        config.term_biasing.boost = 2.0;
        let serialized = toml::to_string_pretty(&config).expect("config serializes");
        let reparsed: RekodyConfig = toml::from_str(&serialized).expect("round-trip parses");
        assert!(reparsed.term_biasing.enabled);
        assert_eq!(reparsed.term_biasing.boost, 2.0);
        assert_eq!(reparsed.term_biasing.max_terms, 200);
    }
}

#[cfg(test)]
mod input_device_config_tests {
    //! The `input_device` key (issue #122): a plain string stays today's
    //! single pin, an array is an ordered preference chain, and both forms
    //! survive the `toml::to_string_pretty` save path the config TUI uses.

    use crate::{InputDevicePref, RekodyConfig};

    /// The four keys every existing config.toml already has (no serde
    /// defaults on these fields).
    const MINIMAL: &str = r#"
activation_mode = "both"
whisper_model = "turbo"
vad_threshold = 0.01
injection_method = "clipboard"
"#;

    fn parse(extra: &str) -> RekodyConfig {
        toml::from_str(&format!("{MINIMAL}{extra}")).expect("config parses")
    }

    #[test]
    fn string_form_parses_as_single_pin() {
        let config = parse("input_device = \"Shure MVX2U\"\n");
        assert_eq!(
            config.input_device,
            Some(InputDevicePref::Name("Shure MVX2U".into()))
        );
        assert_eq!(config.input_device_chain(), vec!["Shure MVX2U".to_string()]);
    }

    #[test]
    fn array_form_parses_as_ordered_chain() {
        let config = parse("input_device = [\"Shure MVX2U\", \"MacBook Air Microphone\"]\n");
        assert_eq!(
            config.input_device,
            Some(InputDevicePref::Chain(vec![
                "Shure MVX2U".into(),
                "MacBook Air Microphone".into(),
            ]))
        );
        assert_eq!(
            config.input_device_chain(),
            vec![
                "Shure MVX2U".to_string(),
                "MacBook Air Microphone".to_string(),
            ]
        );
    }

    #[test]
    fn absent_key_means_system_default() {
        let config = parse("");
        assert_eq!(config.input_device, None);
        assert!(config.input_device_chain().is_empty());
    }

    #[test]
    fn empty_array_behaves_like_unset() {
        let config = parse("input_device = []\n");
        assert_eq!(config.input_device, Some(InputDevicePref::Chain(vec![])));
        assert!(config.input_device_chain().is_empty());
    }

    #[test]
    fn system_sentinel_still_parses() {
        // The setup wizard writes `input_device = "system"`; resolution in
        // rekody-audio skips the sentinel and follows the system default.
        let config = parse("input_device = \"system\"\n");
        assert_eq!(
            config.input_device,
            Some(InputDevicePref::Name("system".into()))
        );
    }

    #[test]
    fn both_forms_round_trip_through_config_tui_save_path() {
        // `config_tui` saves via `toml::to_string_pretty(&config)`; each form
        // must serialize back to the same TOML shape and re-parse equal.
        let pinned = RekodyConfig {
            input_device: Some(InputDevicePref::Name("Shure MVX2U".into())),
            ..RekodyConfig::default()
        };
        let serialized = toml::to_string_pretty(&pinned).expect("string form serializes");
        assert!(serialized.contains("input_device = \"Shure MVX2U\""));
        let reparsed: RekodyConfig = toml::from_str(&serialized).expect("round-trip parses");
        assert_eq!(reparsed.input_device, pinned.input_device);

        let chained = RekodyConfig {
            input_device: Some(InputDevicePref::Chain(vec![
                "Shure MVX2U".into(),
                "MacBook Air Microphone".into(),
            ])),
            ..RekodyConfig::default()
        };
        let serialized = toml::to_string_pretty(&chained).expect("array form serializes");
        assert!(serialized.contains("input_device = ["));
        let reparsed: RekodyConfig = toml::from_str(&serialized).expect("round-trip parses");
        assert_eq!(reparsed.input_device, chained.input_device);
    }
}

#[cfg(test)]
mod stt_engine_compat_tests {
    //! Existing configs must keep working. Every `stt_engine` value Rekody
    //! has ever shipped is exercised here against the behaviour it had
    //! before the provider catalog replaced the hand-maintained lists.

    use super::*;

    fn config_with(engine: &str) -> RekodyConfig {
        RekodyConfig {
            stt_engine: engine.into(),
            providers: vec![ProviderConfig {
                name: "groq".into(),
                api_key: "k".into(),
                model: "m".into(),
                base_url: None,
            }],
            ..Default::default()
        }
    }

    /// The cleanup default used to be spelled `stt_engine != "deepgram"`.
    /// It is now `!formats_own_text`, and must decide identically for every
    /// engine id, including ids this build does not know.
    #[test]
    fn cleanup_default_matches_the_rule_it_replaced() {
        for engine in [
            "local",
            "nemotron",
            "groq",
            "cohere",
            "custom",
            "deepgram",
            "Deepgram",
            "  deepgram  ",
            "an-engine-from-the-future",
        ] {
            let cfg = config_with(engine);
            let expected = engine.trim().to_lowercase() != "deepgram";
            assert_eq!(
                has_llm_providers(&cfg),
                expected,
                "cleanup default changed for stt_engine = {engine:?}"
            );
        }
    }

    /// An explicit `llm_enabled` still wins over the provider's own habits.
    #[test]
    fn explicit_llm_enabled_still_overrides_the_provider() {
        let mut cfg = config_with("deepgram");
        cfg.llm_enabled = Some(true);
        assert!(has_llm_providers(&cfg));
        let mut cfg = config_with("local");
        cfg.llm_enabled = Some(false);
        assert!(!has_llm_providers(&cfg));
    }

    /// A config with no provider chain never runs cleanup, whatever the
    /// engine says.
    #[test]
    fn no_providers_means_no_cleanup() {
        let cfg = RekodyConfig {
            stt_engine: "local".into(),
            ..Default::default()
        };
        assert!(!has_llm_providers(&cfg));
    }

    /// The rule this PR exists for. Saving training data must mean verbatim,
    /// because the dataset stores the engine's own transcript as the label
    /// for the audio beside it, and a smart transcript there is a sentence
    /// the speaker never said.
    #[test]
    fn saving_training_data_forces_verbatim_gemini() {
        assert_eq!(gemini_mode_for(true), rekody_stt::GeminiMode::Verbatim);
        assert_eq!(gemini_mode_for(false), rekody_stt::GeminiMode::Smart);
    }

    /// Rekody ships with training capture on, so verbatim is what a user who
    /// changes nothing gets. If that default ever flips, the mode a fresh
    /// install runs in flips with it, and this test is where that shows up.
    #[test]
    fn the_default_config_puts_gemini_in_verbatim() {
        let cfg = RekodyConfig::default();
        assert!(cfg.save_training_data);
        assert_eq!(
            gemini_mode_for(cfg.save_training_data),
            rekody_stt::GeminiMode::Verbatim
        );
    }

    /// The mode is derived, never stored, so there is no config key that can
    /// contradict it. A hand-edited config cannot ask for smart while the
    /// dataset is being written.
    #[test]
    fn no_config_key_can_override_the_gemini_mode() {
        assert!(!stt_catalog::config_keys("gemini").iter().any(|k| {
            let k = k.to_lowercase();
            k.contains("mode") || k.contains("verbatim") || k.contains("smart")
        }));
        assert_eq!(stt_catalog::config_keys("gemini"), vec!["gemini_api_key"]);
    }

    /// Gemini keeps cleanup on, unlike Deepgram. Verbatim is the default
    /// mode and it returns filler, which is right for a dataset label and
    /// wrong for text about to be typed. Cleanup cannot reach the dataset:
    /// the training pair is captured from the raw transcript first.
    #[test]
    fn gemini_leaves_llm_cleanup_on_by_default() {
        let cfg = RekodyConfig {
            stt_engine: "gemini".into(),
            providers: vec![ProviderConfig {
                name: "ollama".into(),
                api_key: String::new(),
                model: "qwen2.5:3b".into(),
                base_url: None,
            }],
            llm_enabled: None,
            ..Default::default()
        };
        assert!(has_llm_providers(&cfg));
    }

    /// The new custom keys are optional everywhere. A config written by any
    /// earlier version parses unchanged and leaves them unset.
    #[test]
    fn a_config_without_the_custom_keys_still_parses() {
        let toml = r#"
activation_mode = "both"
whisper_model = "turbo"
stt_engine = "deepgram"
deepgram_api_key = "dg_test"
vad_threshold = 0.01
injection_method = "clipboard"
"#;
        let cfg: RekodyConfig = toml::from_str(toml).expect("legacy config must parse");
        assert_eq!(cfg.stt_engine, "deepgram");
        assert_eq!(cfg.deepgram_api_key.as_deref(), Some("dg_test"));
        assert!(cfg.custom_stt_base_url.is_none());
        assert!(cfg.custom_stt_model.is_none());
        assert!(cfg.custom_stt_api_key.is_none());
    }

    /// Every id the catalog offers must be constructible, or the pickers
    /// would offer an engine the pipeline cannot build. The two that need
    /// files on disk or a user-supplied URL are expected to fail loudly,
    /// which is itself the contract: no silent fallback to another engine.
    #[test]
    fn every_catalog_id_is_understood_by_the_pipeline() {
        for provider in stt_catalog::catalog() {
            let cfg = config_with(provider.id);
            let built = Pipeline::new(cfg);
            match provider.id {
                // Needs custom_stt_base_url, which this config does not set.
                "custom" => {
                    let err = built.err().expect("custom must refuse an unset URL");
                    let msg = err.to_string();
                    assert!(
                        msg.contains("base URL"),
                        "custom must say what is missing: {msg}"
                    );
                    assert!(
                        msg.contains("custom_stt_base_url"),
                        "custom must name the setting: {msg}"
                    );
                }
                // Needs model files this test machine may or may not have.
                "nemotron" | "local" => {}
                _ => assert!(
                    built.is_ok(),
                    "{} is offered but cannot be built",
                    provider.id
                ),
            }
        }
    }

    /// The guard that matters most: `custom` must never be reached by
    /// accident. An unset URL fails, and a plain-http remote URL fails, so
    /// no path exists where audio quietly goes somewhere unvetted.
    #[test]
    fn custom_refuses_rather_than_falling_back() {
        let mut cfg = config_with("custom");
        cfg.custom_stt_base_url = Some("http://example.com/v1".into());
        cfg.custom_stt_model = Some("whisper-1".into());
        let err = Pipeline::new(cfg)
            .err()
            .expect("plain http must be refused");
        assert!(err.to_string().contains("example.com"));

        let mut cfg = config_with("custom");
        cfg.custom_stt_base_url = Some("https://api.openai.com/v1".into());
        cfg.custom_stt_model = None;
        let err = Pipeline::new(cfg).err().expect("a missing model must fail");
        assert!(err.to_string().contains("model name"));
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

#[cfg(test)]
mod mic_level_tests {
    //! Apple Silicon neutrality guard for issue #140.
    //!
    //! The streaming loop used to compute its own RMS inline; both loops now
    //! call [`emit_mic_level`]. Moving that expression is the one place a
    //! careless refactor could silently change what the streaming path emits,
    //! so these tests capture the emitted event sequence for a fixed input
    //! under the LEGACY expression and under the shared helper and require
    //! them to be identical, field for field and bit for bit.
    //!
    //! Run with `--nocapture` to print both sequences for an external diff.

    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};

    /// The expression `run_streaming` carried before the extraction, copied
    /// verbatim from lib.rs at 0.5.29 (commit 67721c4). Do not "tidy" it.
    fn legacy_streaming_rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    /// The legacy emit, replayed with `target:` pinned to `rekody_core`.
    ///
    /// The pin is needed because `tracing` derives an event's target from the
    /// module path where the macro EXPANDS, and this replica lives in a test
    /// submodule (`rekody_core::mic_level_tests`) rather than at the crate
    /// root where `run_streaming` and `emit_mic_level` both sit. Pinning
    /// reproduces the production target instead of the test module's.
    ///
    /// That same rule is why the emit stayed in `rekody-core`: raising it
    /// from the audio crate would have retargeted it to `rekody_audio`, which
    /// the default `EnvFilter` (`info,rekody=debug,rekody_core=debug`) drops
    /// before either UI layer runs. `the_event_keeps_its_target_level_and_field_name`
    /// asserts the shipped helper's real, unpinned target.
    fn legacy_emit_mic_level(samples: &[f32]) {
        let rms = legacy_streaming_rms(samples);
        tracing::debug!(target: "rekody_core", rms, "mic level");
    }

    /// One captured event, rendered so a byte-diff of two runs is meaningful.
    /// The rms is printed as its exact bit pattern, not a rounded decimal.
    #[derive(Clone, PartialEq, Eq, Debug)]
    struct Captured(String);

    /// Mirrors `main.rs`'s `EventVisitor` so what this captures is what the
    /// UI layers actually consume: tracing widens the f32 to f64, the visitor
    /// stringifies it, and `on_mic_level` parses that string back to f32.
    /// Both the rendered string and the exact bits are kept.
    #[derive(Default)]
    struct Recorder {
        message: String,
        rms: Option<(String, u32)>,
        other: Vec<String>,
    }

    impl Visit for Recorder {
        fn record_f64(&mut self, field: &Field, value: f64) {
            if field.name() == "rms" {
                self.rms = Some((value.to_string(), (value as f32).to_bits()));
            } else {
                self.other.push(format!("{}=f64:{}", field.name(), value));
            }
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = format!("{value:?}");
            } else {
                self.other.push(format!("{}={value:?}", field.name()));
            }
        }
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<Captured>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let meta = event.metadata();
            let mut r = Recorder::default();
            event.record(&mut r);
            let rms = r
                .rms
                .map(|(text, bits)| format!("{text} (0x{bits:08x})"))
                .unwrap_or_else(|| "<none>".into());
            self.0.lock().unwrap().push(Captured(format!(
                "target={} level={} msg={} rms={} extra={:?}",
                meta.target(),
                meta.level(),
                r.message,
                rms,
                r.other
            )));
        }
    }

    /// Replay `chunks` through `emit`, gating exactly the way the streaming
    /// select arm does (`Some(samples) if recording` emits, `Some(_)` drops),
    /// and return the events that reached the subscriber.
    fn replay(chunks: &[(bool, Vec<f32>)], emit: impl Fn(&[f32])) -> Vec<Captured> {
        use tracing_subscriber::layer::SubscriberExt;
        let cap = Capture::default();
        let subscriber = tracing_subscriber::registry().with(cap.clone());
        tracing::subscriber::with_default(subscriber, || {
            for (recording, samples) in chunks {
                if *recording {
                    emit(samples);
                }
            }
        });
        cap.0.lock().unwrap().clone()
    }

    /// A fixed, deterministic stand-in for a held dictation: an amplitude
    /// envelope over three tones, sliced into the run lengths the 48 kHz ->
    /// 16 kHz resampler actually produces (341/342 samples), plus the edge
    /// cases the loop can legitimately hand us.
    fn fixture_chunks() -> Vec<(bool, Vec<f32>)> {
        let mut signal: Vec<f32> = Vec::with_capacity(16_000 * 3);
        for n in 0..(16_000 * 3) {
            let t = n as f32 / 16_000.0;
            let env = (t * 2.7).sin().abs() * 0.35 + 0.004;
            let s = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.6
                + (2.0 * std::f32::consts::PI * 517.0 * t).sin() * 0.3
                + (2.0 * std::f32::consts::PI * 1_310.0 * t).sin() * 0.1;
            signal.push(s * env);
        }

        // Edge cases first: an empty run (`max(1)` territory), a single
        // sample, silence, both clipping rails, and a denormal run.
        let mut chunks: Vec<(bool, Vec<f32>)> = vec![
            (true, Vec::new()),
            (true, vec![0.5]),
            (true, vec![0.0; 341]),
            (true, vec![1.0; 342]),
            (true, vec![-1.0; 341]),
            (true, vec![f32::MIN_POSITIVE; 480]),
        ];

        // The steady state: alternating 341/342-sample runs, the shape
        // FftFixedIn(48000 -> 16000, 1024) emits.
        let mut pos = 0usize;
        let mut take_342 = false;
        while pos < signal.len() {
            let len = if take_342 { 342 } else { 341 };
            let end = (pos + len).min(signal.len());
            chunks.push((true, signal[pos..end].to_vec()));
            pos = end;
            take_342 = !take_342;
        }

        // Stragglers after key release: the arm must drop these, not emit.
        chunks.push((false, vec![0.9; 341]));
        chunks.push((false, vec![0.2; 342]));
        chunks
    }

    /// The neutrality proof: same input, same emitted sequence.
    #[test]
    fn streaming_path_emits_an_identical_event_sequence_after_the_extraction() {
        let chunks = fixture_chunks();
        let before = replay(&chunks, legacy_emit_mic_level);
        let after = replay(&chunks, emit_mic_level);

        println!("--- BEFORE ({} events) ---", before.len());
        for e in &before {
            println!("{}", e.0);
        }
        println!("--- AFTER ({} events) ---", after.len());
        for e in &after {
            println!("{}", e.0);
        }

        assert_eq!(
            before.len(),
            after.len(),
            "event COUNT changed: the streaming cadence is not neutral"
        );
        assert_eq!(
            before, after,
            "event PAYLOAD changed: target, level, message, or rms bits differ"
        );
        // Guard the fixture itself: a silently empty replay would pass above.
        assert_eq!(
            after.len(),
            chunks.iter().filter(|(rec, _)| *rec).count(),
            "one event per recorded chunk, and none for post-release stragglers"
        );
    }

    /// Bit-for-bit, not approximately: f32 summation order and the `max(1)`
    /// divisor are both part of the value the waveform renders.
    #[test]
    fn mic_level_is_bit_identical_to_the_legacy_streaming_expression() {
        for (recording, samples) in fixture_chunks() {
            let _ = recording;
            assert_eq!(
                mic_level_rms(&samples).to_bits(),
                legacy_streaming_rms(&samples).to_bits(),
                "rms bits differ for a {}-sample chunk",
                samples.len()
            );
        }
    }

    /// The three contractual properties of the event, asserted directly:
    /// both UI layers depend on all three and none is enforced by the type
    /// system.
    #[test]
    fn the_event_keeps_its_target_level_and_field_name() {
        let events = replay(&[(true, vec![0.25; 341])], emit_mic_level);
        assert_eq!(events.len(), 1);
        let line = &events[0].0;
        // Exact, not a prefix: `rekody_core::something` would also start with
        // `rekody_core` yet be a different filter target.
        assert!(
            line.starts_with("target=rekody_core level="),
            "must stay in the rekody_core target: the default EnvFilter only \
             carries rekody_core=debug, so any other target (rekody_audio, a \
             submodule) is filtered away before either UI layer sees it. \
             got: {line}"
        );
        assert!(
            line.contains("level=DEBUG"),
            "must stay at debug so default log filtering is unchanged: {line}"
        );
        assert!(
            line.contains("msg=mic level"),
            "both UI layers match on the literal message: {line}"
        );
        assert!(
            !line.contains("rms=<none>"),
            "both UI layers read the value out of a field named `rms`: {line}"
        );
    }
}
