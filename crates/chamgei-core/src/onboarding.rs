//! First-run onboarding flow for Chamgei.
//!
//! Guides new users through provider selection, API key entry,
//! Whisper model download, and macOS permission checks.
//! Uses `cliclack` for a beautiful interactive CLI experience.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use cliclack::{confirm, input, intro, outro, select, spinner};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` if the user has not yet completed onboarding.
///
/// Checks three conditions:
/// 1. Config file exists at `~/.config/chamgei/config.toml`
/// 2. At least one LLM provider has a non-empty API key (or is a local provider)
/// 3. At least one Whisper model file is present in the model directory
pub fn needs_onboarding() -> bool {
    // Onboarding is needed only if there's no valid config file.
    // The minimal requirement: a parseable config.toml exists.
    // Missing models or API keys are handled at runtime with
    // clear error messages — not by re-triggering the full wizard.
    let config_path = match config_path() {
        Some(p) => p,
        None => return true,
    };

    if !config_path.exists() {
        return true;
    }

    // Config must be parseable TOML.
    let config_contents = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return true,
    };
    toml::from_str::<crate::ChamgeiConfig>(&config_contents).is_err()
}

/// Run the interactive first-run onboarding wizard.
///
/// Walks the user through provider selection, API key entry, Whisper model
/// download, macOS permission guidance, and config file creation.
pub fn run_onboarding() -> Result<()> {
    // --- Header -----------------------------------------------------------
    intro(format!("chamgei v{}", env!("CARGO_PKG_VERSION"))).map_err(|e| anyhow::anyhow!(e))?;

    // --- Step 1: LLM provider --------------------------------------------
    let provider: &str = select("Choose your LLM provider (cleans up transcriptions)")
        .item(
            "none",
            "None — skip LLM cleanup",
            "use raw STT output (Deepgram/Parakeet already include punctuation)",
        )
        .item("groq", "Groq", "recommended — free tier, ultra-fast")
        .item("cerebras", "Cerebras", "wafer-scale inference")
        .item("together", "Together AI", "wide model selection")
        .item("openrouter", "OpenRouter", "multi-provider routing")
        .item("openai", "OpenAI", "GPT models")
        .item("anthropic", "Anthropic", "Claude models")
        .item("gemini", "Google Gemini", "Gemini Flash")
        .item("ollama", "Ollama", "local, no API key needed")
        .item("custom", "Custom endpoint", "any OpenAI-compatible API")
        .interact()
        .map_err(|e| anyhow::anyhow!(e))?;

    let provider_name = provider;

    let default_model = match provider_name {
        "groq" => "openai/gpt-oss-20b",
        "cerebras" => "llama3.1-8b",
        "together" => "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
        "openrouter" => "meta-llama/llama-3.1-8b-instruct:free",
        "openai" => "gpt-4o-mini",
        "anthropic" => "claude-sonnet-4-20250514",
        "gemini" => "gemini-2.0-flash",
        "ollama" => "llama3.2:3b",
        "custom" => "my-model",
        _ => "my-model",
    };

    let needs_key = provider_name != "ollama" && provider_name != "none";

    // --- API key — check Keychain first ----------------------------------
    let api_key: String = if needs_key {
        if let Some(masked) = get_keychain_masked(provider_name) {
            println!("  Found in Keychain: {masked}");
            let use_existing: bool = confirm("Use existing key? (No = enter a new one)")
                .initial_value(true)
                .interact()
                .map_err(|e| anyhow::anyhow!(e))?;
            if use_existing {
                // Retrieve the actual key from Keychain
                get_keychain_full(provider_name).unwrap_or_default()
            } else {
                let key: String = input("Enter your new API key")
                    .placeholder("sk-...")
                    .interact()
                    .map_err(|e| anyhow::anyhow!(e))?;
                if !key.is_empty() {
                    set_keychain(provider_name, &key);
                }
                key
            }
        } else {
            let key: String = input("Enter your API key")
                .placeholder("sk-...")
                .interact()
                .map_err(|e| anyhow::anyhow!(e))?;
            if !key.is_empty() {
                set_keychain(provider_name, &key);
            }
            key
        }
    } else {
        String::new()
    };

    // --- Validate LLM API key --------------------------------------------
    let api_key: String = if needs_key && !api_key.is_empty() {
        let mut current_key = api_key;
        loop {
            let sp = spinner();
            sp.start("Validating API key...");
            if validate_api_key(provider_name, &current_key) {
                sp.stop("API key valid \u{2713}");
                break current_key;
            } else {
                sp.stop("API key validation failed \u{2014} check your key");
                let proceed: bool = confirm("Continue anyway?")
                    .initial_value(false)
                    .interact()
                    .map_err(|e| anyhow::anyhow!(e))?;
                if proceed {
                    break current_key;
                }
                // Re-prompt for key
                let key: String = input("Enter your API key")
                    .placeholder("sk-...")
                    .interact()
                    .map_err(|e| anyhow::anyhow!(e))?;
                if !key.is_empty() {
                    set_keychain(provider_name, &key);
                }
                current_key = key;
            }
        }
    } else {
        api_key
    };

    // --- Model name ------------------------------------------------------
    let model: String = if provider_name == "none" {
        String::new()
    } else if provider_name == "ollama" {
        pick_ollama_model(default_model)?
    } else {
        input("Model name")
            .default_input(default_model)
            .placeholder(default_model)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?
    };

    // --- Custom base URL -------------------------------------------------
    let custom_base_url: Option<String> = if provider_name == "custom" {
        let url: String = input("API base URL")
            .placeholder("https://...")
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        if url.is_empty() { None } else { Some(url) }
    } else {
        None
    };

    // --- Step 2: Speech-to-text engine ------------------------------------
    let stt_engine: &str = select("Choose your speech-to-text engine")
        .item(
            "local",
            "Local Whisper",
            "private — audio stays on your Mac (needs model download)",
        )
        .item(
            "groq",
            "Groq Cloud Whisper",
            "fastest + most accurate — audio sent to Groq (uses your Groq API key)",
        )
        .item(
            "deepgram",
            "Deepgram Nova-3",
            "most accurate — audio sent to Deepgram (needs separate API key)",
        )
        .interact()
        .map_err(|e| anyhow::anyhow!(e))?;

    // Deepgram needs its own API key — check Keychain first
    let deepgram_api_key: Option<String> = if stt_engine == "deepgram" {
        if let Some(masked) = get_keychain_masked("deepgram") {
            println!("  Found in Keychain: {masked}");
            let use_existing: bool = confirm("Use existing key? (No = enter a new one)")
                .initial_value(true)
                .interact()
                .map_err(|e| anyhow::anyhow!(e))?;
            if use_existing {
                // Retrieve the actual key from Keychain and write it to config
                get_keychain_full("deepgram")
            } else {
                let key: String = input("Enter your new Deepgram API key")
                    .placeholder("dg_...")
                    .interact()
                    .map_err(|e| anyhow::anyhow!(e))?;
                if key.is_empty() {
                    None
                } else {
                    set_keychain("deepgram", &key);
                    Some(key)
                }
            }
        } else {
            let key: String = input("Enter your Deepgram API key")
                .placeholder("dg_...")
                .interact()
                .map_err(|e| anyhow::anyhow!(e))?;
            if key.is_empty() {
                None
            } else {
                set_keychain("deepgram", &key);
                Some(key)
            }
        }
    } else {
        None
    };

    // --- Validate Deepgram API key ---------------------------------------
    let deepgram_api_key: Option<String> = if let Some(ref dg_key) = deepgram_api_key {
        if !dg_key.is_empty() {
            let mut current_key = dg_key.clone();
            loop {
                let sp = spinner();
                sp.start("Validating Deepgram API key...");
                if validate_api_key("deepgram", &current_key) {
                    sp.stop("Deepgram API key valid \u{2713}");
                    break Some(current_key);
                } else {
                    sp.stop("Deepgram API key validation failed \u{2014} check your key");
                    let proceed: bool = confirm("Continue anyway?")
                        .initial_value(false)
                        .interact()
                        .map_err(|e| anyhow::anyhow!(e))?;
                    if proceed {
                        break Some(current_key);
                    }
                    let key: String = input("Enter your Deepgram API key")
                        .placeholder("dg_...")
                        .interact()
                        .map_err(|e| anyhow::anyhow!(e))?;
                    if !key.is_empty() {
                        set_keychain("deepgram", &key);
                    }
                    current_key = key;
                }
            }
        } else {
            deepgram_api_key
        }
    } else {
        deepgram_api_key
    };

    // For local Whisper, ask model size and download
    let whisper_size: &str = if stt_engine == "local" {
        select("Choose local Whisper model size")
            .item("tiny", "Tiny (75 MB)", "fastest — good for most use")
            .item("small", "Small (250 MB)", "balanced")
            .item("medium", "Medium (750 MB)", "better accuracy")
            .item("large", "Large (1.5 GB)", "best accuracy")
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?
    } else {
        // Cloud STT doesn't need a local model, but keep tiny as a fallback
        "tiny"
    };

    let (whisper_file, whisper_url) = match whisper_size {
        "tiny" => (
            "ggml-tiny.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        ),
        "small" => (
            "ggml-small.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        ),
        "medium" => (
            "ggml-medium.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        ),
        "large" => (
            "ggml-large.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large.bin",
        ),
        _ => (
            "ggml-tiny.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        ),
    };

    // --- Download model (only for local STT) -----------------------------
    if stt_engine == "local" {
        let model_dir = resolve_model_dir();
        let model_path = model_dir.join(whisper_file);

        if model_path.exists() {
            let sp = spinner();
            sp.start("Checking Whisper model...");
            sp.stop(format!(
                "Model already downloaded at {}",
                model_path.display()
            ));
        } else {
            std::fs::create_dir_all(&model_dir).context("failed to create model directory")?;

            download_model(whisper_url, &model_path).context("failed to download Whisper model")?;

            // Verify checksum (warning only — does not block).
            let expected = expected_checksum_for(whisper_file);
            verify_model_checksum(model_path.to_str().unwrap_or(""), expected);
        }
    } // end if stt_engine == "local"

    // --- Step 3: macOS permissions ---------------------------------------
    #[cfg(target_os = "macos")]
    {
        let open_prefs: bool = confirm("Open System Settings to grant Accessibility permissions?")
            .initial_value(true)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;

        if open_prefs {
            let _ = Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status();
        }

        let _: bool = confirm("Open System Settings to grant Microphone permissions?")
            .initial_value(true)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))
            .inspect(|&open_mic| {
                if open_mic {
                    let _ = Command::new("open")
                        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
                        .status();
                }
            })?;

        println!(
            "  \u{1f4a1} Tip: Set System Settings > Keyboard > \"Press \u{1f310} key to\" \u{2192} \"Do Nothing\""
        );
        println!("     This ensures the Fn key triggers Chamgei instead of Emoji picker.");
    }

    // --- Step 4: Write config --------------------------------------------
    let sp = spinner();
    sp.start("Writing configuration...");

    let config_dir = config_dir().context("could not determine config directory")?;
    let config_path = config_dir.join("config.toml");
    std::fs::create_dir_all(&config_dir).context("failed to create config directory")?;

    let base_url_line = match &custom_base_url {
        Some(url) => format!("base_url = \"{}\"", url),
        None => String::new(),
    };

    // Build optional config lines
    let stt_line = if stt_engine != "local" {
        format!("stt_engine = \"{stt_engine}\"")
    } else {
        String::new()
    };

    let deepgram_line = match &deepgram_api_key {
        Some(key) if !key.is_empty() => format!("deepgram_api_key = \"{key}\""),
        _ => String::new(),
    };

    // If STT is groq, we need groq_api_key for STT (separate from the provider key)
    let groq_stt_line = if stt_engine == "groq" {
        format!("groq_api_key = \"{api_key}\"")
    } else {
        String::new()
    };

    let provider_block = if provider_name == "none" {
        String::new()
    } else {
        format!(
            r#"[[providers]]
name = "{provider_name}"
api_key = "{api_key}"
model = "{model}"
{base_url_line}
"#,
            provider_name = provider_name,
            api_key = api_key,
            model = model,
            base_url_line = base_url_line,
        )
    };

    let config_contents = format!(
        r#"# Chamgei Configuration
# Generated by the first-run setup wizard.
# Edit freely — see https://github.com/tonykipkemboi/chamgei for docs.

activation_mode = "push_to_talk"
whisper_model = "{whisper_size}"
vad_threshold = 0.01
injection_method = "clipboard"
{stt_line}
{deepgram_line}
{groq_stt_line}

{provider_block}
"#,
        whisper_size = whisper_size,
        stt_line = stt_line,
        deepgram_line = deepgram_line,
        groq_stt_line = groq_stt_line,
        provider_block = provider_block,
    );

    std::fs::write(&config_path, &config_contents).context("failed to write config file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&config_path, perms);
    }

    sp.stop(format!("Config written to {}", config_path.display()));

    // --- Summary & Done --------------------------------------------------
    let stt_display = match stt_engine {
        "groq" => "Groq Cloud Whisper Large v3".to_string(),
        "deepgram" => "Deepgram Nova-3".to_string(),
        _ => format!("Local Whisper ({whisper_size})"),
    };

    let summary = format!(
        "Setup complete! Run 'chamgei' to start dictating.\n\
         \n  LLM:     {provider}/{model}\
         \n  STT:     {stt}\
         \n  Config:  {config}",
        provider = provider_name,
        model = model,
        stt = stt_display,
        config = config_path.display(),
    );

    outro(summary).map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// API key validation
// ---------------------------------------------------------------------------

/// Make a lightweight test call to verify an API key works.
///
/// Returns `true` if the key appears valid (HTTP 2xx or 400),
/// `false` on auth errors or network failures.
fn validate_api_key(provider: &str, key: &str) -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let (url, auth) = match provider {
        "groq" => (
            "https://api.groq.com/openai/v1/models",
            format!("Bearer {}", key),
        ),
        "deepgram" => (
            "https://api.deepgram.com/v1/projects",
            format!("Token {}", key),
        ),
        "openai" => (
            "https://api.openai.com/v1/models",
            format!("Bearer {}", key),
        ),
        "cerebras" => (
            "https://api.cerebras.ai/v1/models",
            format!("Bearer {}", key),
        ),
        "anthropic" => return true, // Anthropic has no lightweight endpoint
        "gemini" => return true,    // Gemini validation is complex
        _ => return true,           // Local providers don't need validation
    };

    match client.get(url).header("Authorization", &auth).send() {
        Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 400,
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Model download with progress bar
// ---------------------------------------------------------------------------

/// Download a file from `url` to `dest` using reqwest with an indicatif progress bar.
///
/// Downloads to a `.partial` temp file first, then atomically renames on success.
/// This prevents partial/corrupt model files from being left behind on interruption.
fn download_model(url: &str, dest: &std::path::Path) -> Result<()> {
    let partial_path = dest.with_extension(
        dest.extension()
            .map(|e| format!("{}.partial", e.to_string_lossy()))
            .unwrap_or_else(|| "partial".to_string()),
    );

    let response = reqwest::blocking::get(url).context("failed to start model download")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Model download failed with HTTP status {}. \
             You can download it manually:\n  curl -fSL -o {} {}",
            response.status(),
            dest.display(),
            url
        );
    }

    let total = response.content_length().unwrap_or(0);
    let pb = indicatif::ProgressBar::new(total);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "  {bar:40.cyan/blue} {percent}% \u{b7} {bytes}/{total_bytes} \u{b7} {bytes_per_sec} \u{b7} ETA {eta}",
        )
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
        .progress_chars("\u{2588}\u{2593}\u{2591}"),
    );

    let mut file =
        std::fs::File::create(&partial_path).context("failed to create partial model file")?;

    let mut reader = std::io::BufReader::new(response);
    let mut buf = [0u8; 8192];
    loop {
        use std::io::Read;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        pb.inc(n as u64);
    }

    pb.finish_and_clear();

    // Atomic rename: only move to final path after successful download.
    std::fs::rename(&partial_path, dest)
        .context("failed to rename partial download to final model path")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Checksum verification
// ---------------------------------------------------------------------------

// Known SHA-256 checksums for whisper.cpp GGML models from HuggingFace.
// These may change when models are updated — verify at:
//   https://huggingface.co/ggerganov/whisper.cpp/tree/main
//
// To obtain a fresh checksum:
//   shasum -a 256 ggml-tiny.en.bin    (macOS)
//   sha256sum ggml-tiny.en.bin        (Linux)
//
// Last verified: 2026-03-16
const EXPECTED_CHECKSUMS: &[(&str, &str)] = &[
    (
        "ggml-tiny.en.bin",
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    ),
    (
        "ggml-small.en.bin",
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    ),
    ("ggml-medium.en.bin", ""), // TODO: fill in after downloading and hashing
    ("ggml-large.bin", ""),     // TODO: fill in after downloading and hashing
];

/// Verify the SHA-256 checksum of a downloaded file.
///
/// Uses `shasum -a 256` on macOS or `sha256sum` on Linux.
/// Returns `true` if the computed hash matches `expected_sha256`,
/// or if `expected_sha256` is empty (skipped).
fn verify_model_checksum(path: &str, expected_sha256: &str) -> bool {
    if expected_sha256.is_empty() {
        println!("  Checksum verification skipped (no expected hash configured).");
        return true;
    }

    // Try shasum first (macOS), then sha256sum (Linux).
    let output = Command::new("shasum")
        .args(["-a", "256", path])
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output());

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            // Output format: "<hash>  <filename>\n" — grab the first token.
            let actual_hash = stdout.split_whitespace().next().unwrap_or("");
            if actual_hash.eq_ignore_ascii_case(expected_sha256) {
                println!("  Checksum verified (SHA-256 matches).");
                true
            } else {
                println!("  WARNING: SHA-256 checksum mismatch!");
                println!("    Expected: {}", expected_sha256);
                println!("    Actual:   {}", actual_hash);
                println!("    The model file may have been updated upstream.");
                println!("    If you trust the source, you can ignore this warning.");
                false
            }
        }
        _ => {
            println!("  WARNING: Could not compute SHA-256 checksum (shasum/sha256sum not found).");
            println!("  You can verify manually with: shasum -a 256 {}", path);
            false
        }
    }
}

/// Look up the expected checksum for a given whisper model filename.
fn expected_checksum_for(filename: &str) -> &'static str {
    EXPECTED_CHECKSUMS
        .iter()
        .find(|(f, _)| *f == filename)
        .map(|(_, h)| *h)
        .unwrap_or("")
}

// ---------------------------------------------------------------------------
// Keychain helpers
// ---------------------------------------------------------------------------

/// Check if a provider has an API key stored in macOS Keychain.
/// Returns a masked version (e.g., "gsk_...a8T2") or None.
fn get_keychain_masked(provider: &str) -> Option<String> {
    let entry = keyring::Entry::new("com.chamgei.voice", provider).ok()?;
    let key = entry.get_password().ok()?;
    if key.is_empty() {
        return None;
    }
    if key.len() > 8 {
        Some(format!("{}...{}", &key[..4], &key[key.len() - 4..]))
    } else {
        Some("****".to_string())
    }
}

/// Retrieve the full (unmasked) API key from macOS Keychain, or None.
fn get_keychain_full(provider: &str) -> Option<String> {
    let entry = keyring::Entry::new("com.chamgei.voice", provider).ok()?;
    let key = entry.get_password().ok()?;
    if key.is_empty() { None } else { Some(key) }
}

/// Save an API key to macOS Keychain.
fn set_keychain(provider: &str, key: &str) {
    if let Ok(entry) = keyring::Entry::new("com.chamgei.voice", provider)
        && let Err(e) = entry.set_password(key)
    {
        tracing::warn!("failed to save {provider} key to Keychain: {e}");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Canonical config directory: `~/.config/chamgei`
fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("chamgei"))
}

/// Full path to `~/.config/chamgei/config.toml`.
fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Model directory: `$CHAMGEI_MODEL_DIR` or `~/.local/share/chamgei/models`.
fn resolve_model_dir() -> PathBuf {
    std::env::var("CHAMGEI_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| {
                    h.join(".local")
                        .join("share")
                        .join("chamgei")
                        .join("models")
                })
                .unwrap_or_else(|| PathBuf::from("models"))
        })
}

/// Map a whisper model size string to its GGML filename.
fn whisper_file_name(size: &str) -> &str {
    match size.to_lowercase().as_str() {
        "tiny" => "ggml-tiny.en.bin",
        "small" => "ggml-small.en.bin",
        "medium" => "ggml-medium.en.bin",
        "large" => "ggml-large.bin",
        _ => "ggml-small.en.bin",
    }
}

/// Auto-detect Ollama models and let the user pick one.
/// Falls back to manual text input if Ollama is not running.
fn pick_ollama_model(default: &str) -> Result<String> {
    let sp = spinner();
    sp.start("Checking for Ollama models...");

    let models = chamgei_llm::list_ollama_models();

    if models.is_empty() {
        sp.stop("Ollama not running or no models found");
        // Fall back to manual input
        let model: String = input("Model name (run 'ollama pull llama3.2:3b' first)")
            .default_input(default)
            .placeholder(default)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(model);
    }

    sp.stop(format!(
        "Found {} Ollama model{}",
        models.len(),
        if models.len() == 1 { "" } else { "s" }
    ));

    // Build a selection menu from available models
    let mut sel = select("Choose an Ollama model");
    for m in &models {
        let size_str = chamgei_llm::format_model_size(m.size);
        let hint = if size_str.is_empty() {
            "local".to_string()
        } else {
            format!("local · {}", size_str)
        };
        sel = sel.item(m.name.as_str(), &m.name, hint);
    }
    sel = sel.item("_custom", "Other (type manually)", "enter a model name");

    let chosen: &str = sel.interact().map_err(|e| anyhow::anyhow!(e))?;

    if chosen == "_custom" {
        let model: String = input("Model name")
            .default_input(default)
            .placeholder(default)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(model)
    } else {
        Ok(chosen.to_string())
    }
}

/// Check whether the config has at least one usable LLM provider.
fn has_any_provider(config: &crate::ChamgeiConfig) -> bool {
    // New-style providers list.
    for p in &config.providers {
        // Local providers (ollama, lm-studio, vllm) need no key.
        let local = matches!(
            p.name.to_lowercase().as_str(),
            "ollama" | "lm-studio" | "vllm"
        );
        if local || !p.api_key.is_empty() {
            return true;
        }
    }
    // Legacy keys.
    if config.groq_api_key.as_ref().is_some_and(|k| !k.is_empty()) {
        return true;
    }
    if config
        .cerebras_api_key
        .as_ref()
        .is_some_and(|k| !k.is_empty())
    {
        return true;
    }
    false
}
