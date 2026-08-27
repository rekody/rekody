//! The speech-to-text provider catalog: one table every surface renders from.
//!
//! Before this module the engine list was hand-maintained in seven places
//! that drifted against each other: the pipeline's construction match, the
//! LLM-cleanup default, the setup wizard's picker, `rekody config`'s picker
//! and its conditional fields, `rekody doctor`, the run-loop header chip,
//! and the Mac app's Settings page. Cohere shipped in the daemon and never
//! appeared in the app; the streaming engine shipped and did not appear in
//! `rekody config`. Both were the same bug twice.
//!
//! Now every surface reads [`catalog`]. Each entry carries enough for a UI
//! to render itself with no per-provider code: a stable id, a display name,
//! where the audio goes, whether a key is needed and where to get one, any
//! extra configuration fields, one plain line of description, and whether
//! the provider formats its own text.
//!
//! ## Adding a provider
//!
//! One entry here, one `SttEngine` impl in `rekody-stt`, one arm in
//! `Pipeline::new`. Nothing else: the wizard, both pickers, the doctor, the
//! header chip, and the Mac app's Settings all follow the table.
//!
//! ## Order is meaning
//!
//! The list is in recommendation order, best first, so [`recommended`] is
//! simply the head. The streaming entry is `#[cfg(feature = "nemotron")]`,
//! which is what makes the Intel build correct for free: that slice is
//! compiled `--no-default-features`, the entry is absent, and the head of
//! the list becomes local Whisper without a single architecture check in
//! any consumer.

use serde::Serialize;

/// Where a provider runs, and therefore where the recorded audio goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Locality {
    /// Runs on this Mac. Audio never leaves the machine.
    OnDevice,
    /// A server the user runs themselves, reached over loopback.
    LocalServer,
    /// A third party reached over the network. Audio leaves the machine.
    Cloud,
}

impl Locality {
    /// True when picking this provider sends recorded audio to someone else.
    ///
    /// Every surface that offers a provider must label this. Rekody's public
    /// promise is that cloud engines are optional and labeled, and this is
    /// the flag that keeps it true.
    pub fn sends_audio_off_device(self) -> bool {
        matches!(self, Locality::Cloud)
    }

    /// Short label for a UI badge.
    pub fn label(self) -> &'static str {
        match self {
            Locality::OnDevice => "On this Mac",
            Locality::LocalServer => "Local server",
            Locality::Cloud => "Cloud",
        }
    }
}

/// What kind of value an extra field holds, and therefore how to render and
/// validate it. Deliberately a small closed set: a UI can switch on four
/// cases, and a new provider that needs a fifth is a real design decision
/// rather than an accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
    /// One of the values in [`Field::options`].
    Choice,
    /// An API base URL. Must be https, or http on loopback.
    BaseUrl,
    /// A model identifier passed straight through to the provider.
    ModelName,
    /// A TCP port on this machine.
    Port,
}

/// One configurable value a provider needs beyond its API key.
#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub kind: FieldKind,
    /// Top-level key in config.toml this field reads and writes.
    pub config_key: &'static str,
    /// Field label.
    pub label: &'static str,
    /// Placeholder shown in an empty input.
    pub placeholder: &'static str,
    /// One line of help under the field.
    pub help: &'static str,
    /// Allowed values for [`FieldKind::Choice`]. Empty for every other kind.
    pub options: &'static [&'static str],
    /// The provider cannot start without this value.
    ///
    /// False when the daemon supplies a working default, in which case the
    /// placeholder shows what that default is. A UI must not nag about an
    /// empty field the daemon already has an answer for.
    pub required: bool,
}

/// How a provider's API key is named, stored, and obtained.
///
/// Keys live in two places by design, and both are load-bearing:
///
/// * the keychain, service `com.rekody.voice`, account [`Self::keyring_account`],
///   which is what `rekody key` manages;
/// * [`Self::config_key`] in config.toml, which is what the daemon actually
///   reads at start.
///
/// The wizard writes both. Nothing here ever logs or prints a key.
#[derive(Debug, Clone, Serialize)]
pub struct KeySpec {
    /// Account under keychain service `com.rekody.voice`.
    pub keyring_account: &'static str,
    /// Top-level key in config.toml the daemon reads at start.
    pub config_key: &'static str,
    /// Placeholder showing the key's shape, never a real key.
    pub placeholder: &'static str,
    /// Page where a user gets a key.
    pub obtain_url: &'static str,
    /// Plain label for that page.
    pub obtain_label: &'static str,
    /// The provider refuses to transcribe without a key.
    pub required: bool,
}

/// One speech-to-text provider, described completely enough to render.
#[derive(Debug, Clone, Serialize)]
pub struct SttProvider {
    /// The `stt_engine` value in config.toml. Stable forever: existing
    /// configs are matched on it.
    pub id: &'static str,
    /// Name shown to people.
    pub display_name: &'static str,
    /// Where it runs, and so where the audio goes.
    pub locality: Locality,
    /// One plain line: what picking this gets you.
    pub description: &'static str,
    /// Key requirements, or `None` for providers that need no key.
    pub key: Option<KeySpec>,
    /// Extra configuration beyond the key.
    pub fields: &'static [Field],
    /// The provider punctuates and capitalizes its own output, so AI cleanup
    /// defaults off. Drives [`crate::has_llm_providers`].
    pub formats_own_text: bool,
    /// Accepts a BCP-47 `stt_language` hint.
    pub supports_language_hint: bool,
    /// Needs a model download on disk before it can run.
    pub needs_download: bool,
    /// The user supplies the endpoint, so the destination is only known once
    /// they have configured it. A UI must show the resolved host before use,
    /// and this provider is never a default or a fallback.
    pub user_supplied_endpoint: bool,
}

impl SttProvider {
    /// Convenience mirror of [`Locality::sends_audio_off_device`].
    pub fn sends_audio_off_device(&self) -> bool {
        self.locality.sends_audio_off_device()
    }

    /// The field with this config key, if the provider has one.
    pub fn field(&self, config_key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.config_key == config_key)
    }
}

// ── The catalog ─────────────────────────────────────────────────────────────

#[cfg(feature = "nemotron")]
const STREAMING: SttProvider = SttProvider {
    id: "nemotron",
    display_name: "Rekody Streaming",
    locality: Locality::OnDevice,
    description: "Transcribes while you talk. English, on this Mac, nothing leaves it.",
    key: None,
    fields: &[],
    formats_own_text: false,
    supports_language_hint: false,
    needs_download: true,
    user_supplied_endpoint: false,
};

const WHISPER_SIZES: &[&str] = &["tiny", "small", "medium", "large", "turbo"];

const LOCAL: SttProvider = SttProvider {
    id: "local",
    display_name: "Whisper",
    locality: Locality::OnDevice,
    description: "Runs on this Mac in 100+ languages. Text lands a second or two after you stop.",
    key: None,
    fields: &[Field {
        kind: FieldKind::Choice,
        config_key: "whisper_model",
        label: "Model size",
        placeholder: "turbo",
        help: "Bigger is more accurate and slower. Turbo suits Apple silicon, small suits Intel.",
        options: WHISPER_SIZES,
        required: true,
    }],
    formats_own_text: false,
    supports_language_hint: true,
    needs_download: true,
    user_supplied_endpoint: false,
};

const GROQ: SttProvider = SttProvider {
    id: "groq",
    display_name: "Groq Cloud Whisper",
    locality: Locality::Cloud,
    description: "Whisper Large v3 on Groq. Fast, and your audio is sent to Groq.",
    key: Some(KeySpec {
        keyring_account: "groq",
        config_key: "groq_api_key",
        placeholder: "gsk_...",
        obtain_url: "https://console.groq.com/keys",
        obtain_label: "console.groq.com/keys",
        required: true,
    }),
    fields: &[],
    formats_own_text: false,
    supports_language_hint: true,
    needs_download: false,
    user_supplied_endpoint: false,
};

const DEEPGRAM: SttProvider = SttProvider {
    id: "deepgram",
    display_name: "Deepgram Nova-3",
    locality: Locality::Cloud,
    description: "Accurate, already punctuated, and your audio is sent to Deepgram.",
    key: Some(KeySpec {
        keyring_account: "deepgram",
        config_key: "deepgram_api_key",
        placeholder: "dg_...",
        obtain_url: "https://console.deepgram.com",
        obtain_label: "console.deepgram.com",
        required: true,
    }),
    fields: &[],
    // Nova-3's smart_format already punctuates and capitalizes, so a second
    // cleanup pass only adds latency. This is the flag behind the default.
    formats_own_text: true,
    supports_language_hint: true,
    needs_download: false,
    user_supplied_endpoint: false,
};

const COHERE: SttProvider = SttProvider {
    id: "cohere",
    display_name: "Cohere local server",
    locality: Locality::LocalServer,
    description: "Talks to a Cohere transcription server you run on this machine.",
    key: None,
    fields: &[Field {
        kind: FieldKind::Port,
        config_key: "cohere_stt_port",
        label: "Port",
        placeholder: "8099",
        help: "The port your local Cohere transcription server listens on. Blank uses 8099.",
        options: &[],
        // The daemon defaults this to 8099, so an empty field still works.
        required: false,
    }],
    formats_own_text: false,
    supports_language_hint: false,
    needs_download: false,
    user_supplied_endpoint: false,
};

/// Any endpoint that speaks OpenAI's `/v1/audio/transcriptions`: OpenAI
/// itself, Together, Fireworks, a self-hosted vLLM, LM Studio, anything.
///
/// Opt-in only. Never a default, never a fallback: `Pipeline::new` refuses
/// to start rather than quietly sending a user's voice to an endpoint that
/// was not deliberately configured.
const CUSTOM: SttProvider = SttProvider {
    id: "custom",
    display_name: "Other (OpenAI compatible)",
    locality: Locality::Cloud,
    description: "Any endpoint that speaks OpenAI's audio transcriptions API. You give the URL.",
    key: Some(KeySpec {
        keyring_account: "custom-stt",
        config_key: "custom_stt_api_key",
        placeholder: "sk_...",
        obtain_url: "",
        obtain_label: "",
        // A self-hosted vLLM or LM Studio usually wants no key at all.
        required: false,
    }),
    fields: &[
        Field {
            kind: FieldKind::BaseUrl,
            config_key: "custom_stt_base_url",
            label: "API base URL",
            placeholder: "https://api.openai.com/v1",
            help: "Must be https, or http on localhost. Rekody appends /audio/transcriptions.",
            options: &[],
            required: true,
        },
        Field {
            kind: FieldKind::ModelName,
            config_key: "custom_stt_model",
            label: "Model",
            placeholder: "whisper-1",
            help: "The model id this endpoint expects.",
            options: &[],
            required: true,
        },
    ],
    formats_own_text: false,
    supports_language_hint: true,
    needs_download: false,
    user_supplied_endpoint: true,
};

/// Recommendation order, best first. "Other" stays last: it is the escape
/// hatch, not a suggestion.
#[cfg(feature = "nemotron")]
static CATALOG: &[SttProvider] = &[STREAMING, LOCAL, GROQ, DEEPGRAM, COHERE, CUSTOM];

/// Same list without the streaming engine, which this build cannot run.
/// The head becomes local Whisper, so every "recommended" surface is correct
/// on Intel with no extra branch.
#[cfg(not(feature = "nemotron"))]
static CATALOG: &[SttProvider] = &[LOCAL, GROQ, DEEPGRAM, COHERE, CUSTOM];

/// Every provider this build supports, in recommendation order.
pub fn catalog() -> &'static [SttProvider] {
    CATALOG
}

/// The provider a fresh install should land on: the head of the list.
///
/// Apple silicon gets the streaming engine. Intel, where the `nemotron`
/// feature is compiled out, gets local Whisper. Neither is spelled out
/// anywhere; both fall out of the table.
pub fn recommended() -> &'static SttProvider {
    &CATALOG[0]
}

/// Look up a provider by its `stt_engine` value, case-insensitively.
///
/// `None` means this build does not offer that id, which happens two ways:
/// an unknown value in a hand-edited config, or the streaming engine on a
/// build compiled without it. Callers treat both the same way.
pub fn find(id: &str) -> Option<&'static SttProvider> {
    let id = id.trim().to_lowercase();
    CATALOG.iter().find(|p| p.id == id)
}

/// Every provider id this build supports, in catalog order.
pub fn ids() -> Vec<&'static str> {
    CATALOG.iter().map(|p| p.id).collect()
}

/// Config keys the provider with this id owns beyond `stt_engine`: its API
/// key, if any, plus every extra field. Used by surfaces that need to know
/// which keys to show or clear for a given engine.
pub fn config_keys(id: &str) -> Vec<&'static str> {
    let Some(p) = find(id) else {
        return Vec::new();
    };
    let mut keys: Vec<&'static str> = p.key.iter().map(|k| k.config_key).collect();
    keys.extend(p.fields.iter().map(|f| f.config_key));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids are the config contract. Anyone on `stt_engine = "deepgram"`
    /// today must still resolve tomorrow, so this list only ever grows.
    #[test]
    fn every_shipped_engine_id_still_resolves() {
        for id in ["local", "groq", "deepgram", "cohere"] {
            assert!(find(id).is_some(), "{id} must stay in the catalog");
        }
        #[cfg(feature = "nemotron")]
        assert!(find("nemotron").is_some());
    }

    #[test]
    fn lookup_is_case_and_whitespace_insensitive() {
        assert_eq!(find("  DeepGram ").map(|p| p.id), Some("deepgram"));
        assert!(find("nope").is_none());
    }

    /// The Intel build must recommend Whisper. It is the same assertion as
    /// "the streaming entry is feature gated", stated the way a user sees it.
    #[test]
    fn recommended_is_streaming_on_full_builds_and_whisper_without_it() {
        #[cfg(feature = "nemotron")]
        assert_eq!(recommended().id, "nemotron");
        #[cfg(not(feature = "nemotron"))]
        assert_eq!(recommended().id, "local");
    }

    /// A build that cannot run the streaming engine must not list it, or the
    /// UI would offer an engine the daemon refuses to start.
    #[cfg(not(feature = "nemotron"))]
    #[test]
    fn streaming_is_absent_without_the_feature() {
        assert!(find("nemotron").is_none());
        assert!(!ids().contains(&"nemotron"));
    }

    /// Deepgram is the only provider that formats its own text, and that is
    /// exactly the set for which AI cleanup defaults off.
    #[test]
    fn deepgram_is_the_only_self_formatting_provider() {
        let formatters: Vec<&str> = catalog()
            .iter()
            .filter(|p| p.formats_own_text)
            .map(|p| p.id)
            .collect();
        assert_eq!(formatters, vec!["deepgram"]);
    }

    /// "Other" is opt-in only. It must never be what a fresh install lands
    /// on, and it must never be the head of the list.
    #[test]
    fn custom_is_never_recommended() {
        assert_ne!(recommended().id, "custom");
        assert_eq!(catalog().last().map(|p| p.id), Some("custom"));
        assert!(find("custom").unwrap().user_supplied_endpoint);
    }

    /// Only the custom provider has an endpoint the user supplies. Every
    /// other one points at an address Rekody chose and can vouch for.
    #[test]
    fn only_custom_has_a_user_supplied_endpoint() {
        let user_supplied: Vec<&str> = catalog()
            .iter()
            .filter(|p| p.user_supplied_endpoint)
            .map(|p| p.id)
            .collect();
        assert_eq!(user_supplied, vec!["custom"]);
    }

    /// Cloud providers are the ones that send audio away, and every one of
    /// them must carry a key spec so a UI can ask for the key.
    #[test]
    fn cloud_providers_send_audio_away_and_take_a_key() {
        for p in catalog() {
            assert_eq!(
                p.sends_audio_off_device(),
                p.locality == Locality::Cloud,
                "{} labels its destination wrongly",
                p.id
            );
            if p.locality == Locality::Cloud {
                assert!(p.key.is_some(), "{} is cloud but takes no key", p.id);
            }
        }
    }

    /// On-device providers must never ask for a key or an endpoint. This is
    /// the privacy claim expressed as a test.
    #[test]
    fn on_device_providers_need_no_key_and_no_endpoint() {
        for p in catalog()
            .iter()
            .filter(|p| p.locality == Locality::OnDevice)
        {
            assert!(p.key.is_none(), "{} is on-device but wants a key", p.id);
            assert!(!p.user_supplied_endpoint, "{} points off device", p.id);
            assert!(
                !p.sends_audio_off_device(),
                "{} claims to send audio away",
                p.id
            );
        }
    }

    /// Ids and config keys are what everything else joins on, so duplicates
    /// would make lookups ambiguous.
    #[test]
    fn ids_and_config_keys_are_unique() {
        let ids = ids();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate provider id");

        let mut keys: Vec<&str> = catalog()
            .iter()
            .flat_map(|p| config_keys(p.id))
            .filter(|k| *k != "whisper_model")
            .collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(before, keys.len(), "two providers claim one config key");
    }

    /// A `Choice` field is the only kind with options, and its placeholder
    /// has to be one of them or the UI would suggest an invalid value.
    #[test]
    fn choice_fields_are_internally_consistent() {
        for p in catalog() {
            for f in p.fields {
                if f.kind == FieldKind::Choice {
                    assert!(!f.options.is_empty(), "{}/{} has no options", p.id, f.label);
                    assert!(
                        f.options.contains(&f.placeholder),
                        "{}/{} suggests a value it does not offer",
                        p.id,
                        f.label
                    );
                } else {
                    assert!(
                        f.options.is_empty(),
                        "{}/{} has stray options",
                        p.id,
                        f.label
                    );
                }
            }
        }
    }

    /// Copy shown to people: no em dashes anywhere, and every provider says
    /// something. Empty description would render a blank row.
    #[test]
    fn copy_is_present_and_free_of_em_dashes() {
        for p in catalog() {
            assert!(!p.description.is_empty(), "{} has no description", p.id);
            let mut copy = vec![p.display_name, p.description, p.locality.label()];
            if let Some(k) = &p.key {
                copy.push(k.placeholder);
                copy.push(k.obtain_label);
            }
            for f in p.fields {
                copy.extend([f.label, f.placeholder, f.help]);
            }
            for line in copy {
                assert!(!line.contains('\u{2014}'), "em dash in {}: {line}", p.id);
            }
        }
    }

    /// Every key-taking provider a user must sign up for needs somewhere to
    /// go. The custom endpoint is the exception: only the user knows it.
    #[test]
    fn required_keys_come_with_somewhere_to_get_one() {
        for p in catalog() {
            let Some(k) = &p.key else { continue };
            if k.required {
                assert!(
                    k.obtain_url.starts_with("https://"),
                    "{} asks for a key with no https page to get one",
                    p.id
                );
            }
        }
    }

    /// `required` means the daemon has no default to fall back on. A field
    /// the daemon defaults must not be marked required, or every UI nags
    /// about a configuration that already works.
    #[test]
    fn only_fields_without_a_daemon_default_are_required() {
        let required: Vec<&str> = catalog()
            .iter()
            .flat_map(|p| p.fields.iter())
            .filter(|f| f.required)
            .map(|f| f.config_key)
            .collect();
        // whisper_model defaults in RekodyConfig but is always written by
        // every surface, so it is genuinely always present; the custom
        // endpoint's two fields have no default at all.
        assert_eq!(
            required,
            vec!["whisper_model", "custom_stt_base_url", "custom_stt_model"]
        );
        assert!(
            !find("cohere")
                .unwrap()
                .field("cohere_stt_port")
                .unwrap()
                .required
        );
    }

    #[test]
    fn config_keys_cover_key_and_fields() {
        assert_eq!(config_keys("deepgram"), vec!["deepgram_api_key"]);
        assert_eq!(config_keys("cohere"), vec!["cohere_stt_port"]);
        assert_eq!(
            config_keys("custom"),
            vec![
                "custom_stt_api_key",
                "custom_stt_base_url",
                "custom_stt_model"
            ]
        );
        assert!(config_keys("nope").is_empty());
    }
}
