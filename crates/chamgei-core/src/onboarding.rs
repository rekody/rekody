//! First-run onboarding flow for Chamgei.
//!
//! Guides new users through provider selection, API key entry,
//! Whisper model download, and macOS permission checks.

use std::io::{self, BufRead, Write as _};
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

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
    let config_path = match config_path() {
        Some(p) => p,
        None => return true,
    };

    // 1. Config file must exist.
    if !config_path.exists() {
        return true;
    }

    // 2. Config must contain at least one usable provider.
    let config_contents = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return true,
    };
    let config: crate::ChamgeiConfig = match toml::from_str(&config_contents) {
        Ok(c) => c,
        Err(_) => return true,
    };
    if !has_any_provider(&config) {
        return true;
    }

    // 3. A Whisper model file must be present.
    let model_dir = resolve_model_dir();
    let model_file = model_dir.join(whisper_file_name(&config.whisper_model));
    if !model_file.exists() {
        return true;
    }

    false
}

/// Run the interactive first-run onboarding wizard.
///
/// Walks the user through provider selection, API key entry, Whisper model
/// download, macOS permission guidance, and config file creation.
pub fn run_onboarding() -> Result<()> {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    println!();
    println!("╔══════════════════════════════════════════════╗");
    println!("║   Welcome to Chamgei - Voice Dictation       ║");
    println!("║   First-time setup                           ║");
    println!("╚══════════════════════════════════════════════╝");
    println!();
    println!("This wizard will help you configure Chamgei.");
    println!("It only takes a minute.");
    println!();

    // --- Step 1: LLM provider ------------------------------------------------
    println!("─── Step 1: Choose your LLM provider ───");
    println!();
    println!("Chamgei uses an LLM to clean up and format transcriptions.");
    println!("Pick a provider (you can change this later in the config).");
    println!();
    println!("  1) Groq        (recommended - free tier, fast)");
    println!("  2) Cerebras");
    println!("  3) Together");
    println!("  4) OpenRouter");
    println!("  5) OpenAI");
    println!("  6) Anthropic");
    println!("  7) Gemini");
    println!("  8) Ollama      (local, no API key needed)");
    println!("  9) Custom endpoint");
    println!();

    let choice = prompt_choice(&mut lines, "Enter a number [1-9]", 1, 9, 1)?;

    let (provider_name, default_model, needs_key) = match choice {
        1 => ("groq", "openai/gpt-oss-20b", true),
        2 => ("cerebras", "llama3.1-8b", true),
        3 => ("together", "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo", true),
        4 => ("openrouter", "meta-llama/llama-3.1-8b-instruct:free", true),
        5 => ("openai", "gpt-4o-mini", true),
        6 => ("anthropic", "claude-sonnet-4-20250514", true),
        7 => ("gemini", "gemini-2.0-flash", true),
        8 => ("ollama", "llama3.2:3b", false),
        9 => ("custom", "my-model", true),
        _ => unreachable!(),
    };

    // --- Custom base URL (only for option 9) ---------------------------------
    let mut custom_base_url: Option<String> = None;
    if choice == 9 {
        println!();
        print!("Enter the base URL for your custom endpoint: ");
        io::stdout().flush()?;
        let url = read_line(&mut lines)?;
        if !url.is_empty() {
            custom_base_url = Some(url);
        }
    }

    // --- API key -------------------------------------------------------------
    let api_key = if needs_key {
        println!();
        print!("Enter your {} API key: ", provider_name);
        io::stdout().flush()?;
        let key = read_line(&mut lines)?;
        if key.is_empty() {
            println!("  (No key entered - you can add it later in ~/.config/chamgei/config.toml)");
        }
        key
    } else {
        println!();
        println!("  No API key needed for {}.", provider_name);
        String::new()
    };

    // --- Model ---------------------------------------------------------------
    println!();
    print!(
        "Which model? [default: {}]: ",
        default_model
    );
    io::stdout().flush()?;
    let model_input = read_line(&mut lines)?;
    let model = if model_input.is_empty() {
        default_model.to_string()
    } else {
        model_input
    };

    // --- Step 2: Whisper model -----------------------------------------------
    println!();
    println!("─── Step 2: Choose Whisper model size ───");
    println!();
    println!("This model runs locally for speech-to-text.");
    println!();
    println!("  1) tiny    (~75 MB,  fastest,  good enough for most use)");
    println!("  2) small   (~250 MB, balanced)");
    println!("  3) medium  (~750 MB, better accuracy)");
    println!("  4) large   (~1.5 GB, best accuracy)");
    println!();

    let whisper_choice = prompt_choice(&mut lines, "Enter a number [1-4]", 1, 4, 1)?;

    let (whisper_size, whisper_file, whisper_url) = match whisper_choice {
        1 => (
            "tiny",
            "ggml-tiny.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        ),
        2 => (
            "small",
            "ggml-small.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        ),
        3 => (
            "medium",
            "ggml-medium.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        ),
        4 => (
            "large",
            "ggml-large.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large.bin",
        ),
        _ => unreachable!(),
    };

    // Download model if not present.
    let model_dir = resolve_model_dir();
    let model_path = model_dir.join(whisper_file);

    if model_path.exists() {
        println!();
        println!("  Model already downloaded at {}", model_path.display());
    } else {
        println!();
        println!("  Downloading {} model...", whisper_size);
        println!("  URL: {}", whisper_url);
        println!("  Destination: {}", model_path.display());
        println!();

        std::fs::create_dir_all(&model_dir)
            .context("failed to create model directory")?;

        let status = Command::new("curl")
            .args([
                "-fSL",
                "--progress-bar",
                "-o",
                model_path.to_str().unwrap_or("model.bin"),
                whisper_url,
            ])
            .status()
            .context("failed to run curl — is it installed?")?;

        if !status.success() {
            anyhow::bail!(
                "Model download failed (exit code {:?}). \
                 You can download it manually:\n  curl -fSL -o {} {}",
                status.code(),
                model_path.display(),
                whisper_url
            );
        }

        println!("  Download complete.");

        // Verify checksum (warning only — does not block).
        let expected = expected_checksum_for(whisper_file);
        verify_model_checksum(model_path.to_str().unwrap_or(""), expected);
    }

    // --- Step 3: macOS permissions -------------------------------------------
    println!();
    println!("─── Step 3: macOS permissions ───");
    println!();
    println!("Chamgei needs two macOS permissions to work:");
    println!();
    println!("  1. Microphone access — so it can hear you.");
    println!("     Go to: System Settings > Privacy & Security > Microphone");
    println!("     Make sure your terminal app (or Chamgei.app) is enabled.");
    println!();
    println!("  2. Accessibility access — so it can type text into other apps.");
    println!("     Go to: System Settings > Privacy & Security > Accessibility");
    println!("     Add your terminal app (or Chamgei.app) to the list.");
    println!();

    // Quick check: try to detect if accessibility is enabled.
    // This uses the macOS `tccutil` indirectly — we just remind the user.
    #[cfg(target_os = "macos")]
    {
        // Try to open System Settings to the right pane.
        print!("Open System Settings to Accessibility now? [Y/n]: ");
        io::stdout().flush()?;
        let open_prefs = read_line(&mut lines)?;
        if open_prefs.is_empty() || open_prefs.to_lowercase().starts_with('y') {
            let _ = Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                .status();
        }
    }

    println!();
    print!("Press Enter once you have granted both permissions...");
    io::stdout().flush()?;
    let _ = read_line(&mut lines);

    // --- Step 4: Write config ------------------------------------------------
    println!();
    println!("─── Step 4: Writing configuration ───");
    println!();

    let config_dir = config_dir().context("could not determine config directory")?;
    let config_path = config_dir.join("config.toml");
    std::fs::create_dir_all(&config_dir)
        .context("failed to create config directory")?;

    let base_url_line = match &custom_base_url {
        Some(url) => format!("base_url = \"{}\"", url),
        None => String::new(),
    };

    let config_contents = format!(
        r#"# Chamgei Configuration
# Generated by the first-run setup wizard.
# Edit freely — see https://github.com/tonykipkemboi/chamgei for docs.

activation_mode = "push_to_talk"
whisper_model = "{whisper_size}"
vad_threshold = 0.01
injection_method = "clipboard"

[[providers]]
name = "{provider_name}"
api_key = "{api_key}"
model = "{model}"
{base_url_line}
"#,
        whisper_size = whisper_size,
        provider_name = provider_name,
        api_key = api_key,
        model = model,
        base_url_line = base_url_line,
    );

    std::fs::write(&config_path, &config_contents)
        .context("failed to write config file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&config_path, perms);
    }

    println!("  Config written to {}", config_path.display());

    // --- Done ----------------------------------------------------------------
    println!();
    println!("╔══════════════════════════════════════════════╗");
    println!("║   Setup complete!                            ║");
    println!("╚══════════════════════════════════════════════╝");
    println!();
    println!("  Hotkeys:");
    println!("    Fn (hold)        push-to-talk dictation");
    println!("    Fn + Space       hands-free toggle (start/stop)");
    println!("    Fn + Enter       command mode (transform selected text)");
    println!();
    println!("  Config location:   {}", config_path.display());
    println!("  Model location:    {}", model_path.display());
    println!();
    println!("  To reconfigure, edit the config file or delete it and relaunch.");
    println!();

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
    ("ggml-tiny.en.bin",   "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"),
    ("ggml-small.en.bin",  "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"),
    ("ggml-medium.en.bin", ""),  // TODO: fill in after downloading and hashing
    ("ggml-large.bin",     ""),  // TODO: fill in after downloading and hashing
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
                .map(|h| h.join(".local").join("share").join("chamgei").join("models"))
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
    if config
        .groq_api_key
        .as_ref()
        .is_some_and(|k| !k.is_empty())
    {
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

/// Read a single line from the iterator, trimming whitespace.
fn read_line(lines: &mut impl Iterator<Item = io::Result<String>>) -> Result<String> {
    match lines.next() {
        Some(Ok(line)) => Ok(line.trim().to_string()),
        Some(Err(e)) => Err(e.into()),
        None => Ok(String::new()),
    }
}

/// Prompt the user for a numeric choice in `[min, max]`, with a default.
fn prompt_choice(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    prompt: &str,
    min: u32,
    max: u32,
    default: u32,
) -> Result<u32> {
    loop {
        print!("{} (default {}): ", prompt, default);
        io::stdout().flush()?;
        let input = read_line(lines)?;
        if input.is_empty() {
            return Ok(default);
        }
        match input.parse::<u32>() {
            Ok(n) if n >= min && n <= max => return Ok(n),
            _ => println!("  Please enter a number between {} and {}.", min, max),
        }
    }
}
