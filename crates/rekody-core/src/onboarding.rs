//! First-run onboarding flow for rekody.
//!
//! Guides new users through provider selection, API key entry,
//! Whisper model download, and macOS permission checks.
//! Uses `cliclack` for a beautiful interactive CLI experience.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::DEFAULT_WHISPER_MODEL;
use crate::stt_catalog;
use cliclack::{Theme, ThemeState, confirm, input, intro, outro, select, spinner};
use console::Style;

// ---------------------------------------------------------------------------
// Brand theme (Studio CLI)
// ---------------------------------------------------------------------------
//
// cliclack's default theme is cyan/green; this maps it onto the rekody
// palette using the closest xterm-256 colors (console::Style has no
// truecolor): 30 ≈ brand teal #20808D, 80 ≈ light teal #4FB8C5,
// 230 ≈ cream #FBFAF4.

struct RekodyTheme;

impl Theme for RekodyTheme {
    fn bar_color(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Active => Style::new().color256(30),
            ThemeState::Cancel => Style::new().red(),
            ThemeState::Submit => Style::new().color256(238),
            ThemeState::Error(_) => Style::new().yellow(),
        }
    }

    fn state_symbol_color(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Submit => Style::new().color256(80),
            _ => self.bar_color(state),
        }
    }

    fn radio_symbol(&self, state: &ThemeState, selected: bool) -> String {
        match state {
            ThemeState::Active if selected => {
                Style::new().color256(80).bold().apply_to("❯").to_string()
            }
            ThemeState::Active => " ".to_string(),
            _ => String::new(),
        }
    }

    fn input_style(&self, _state: &ThemeState) -> Style {
        Style::new().color256(230).bold()
    }

    fn placeholder_style(&self, _state: &ThemeState) -> Style {
        Style::new().color256(243)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` if the user has not yet completed onboarding.
///
/// Checks three conditions:
/// 1. Config file exists at `~/.config/rekody/config.toml`
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
    toml::from_str::<crate::RekodyConfig>(&config_contents).is_err()
}

/// Run the interactive first-run onboarding wizard.
///
/// Walks the user through provider selection, API key entry, Whisper model
/// download, macOS permission guidance, and config file creation.
pub fn run_onboarding() -> Result<()> {
    // --- Header -----------------------------------------------------------
    cliclack::set_theme(RekodyTheme);
    intro(format!("rekody v{}", env!("CARGO_PKG_VERSION"))).map_err(|e| anyhow::anyhow!(e))?;

    // --- Step 1: LLM provider --------------------------------------------
    let provider: &str = select("Choose your LLM provider (cleans up transcriptions)")
        .item(
            "none",
            "None — skip LLM cleanup",
            "use raw STT output (Rekody Streaming and Deepgram already punctuate)",
        )
        .item(
            "apple",
            "Apple on-device",
            "private + free, no API key — macOS 26+ (Apple Intelligence)",
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
        // The cursor starts on the recommended provider, not on "none":
        // Enter-through picks Groq instead of silently skipping cleanup.
        .initial_value("groq")
        .interact()
        .map_err(|e| anyhow::anyhow!(e))?;

    let provider_name = provider;

    let default_model = match provider_name {
        "apple" => "on-device",
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

    let needs_key =
        provider_name != "ollama" && provider_name != "none" && provider_name != "apple";

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
    // Only for providers with a real endpoint check. Where none exists
    // (custom base URLs), no spinner and no "valid": say what is true.
    let api_key: String = if needs_key && !api_key.is_empty() {
        if provider_has_key_check(provider_name) {
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
            println!("  Key saved. It will be verified on first use.");
            api_key
        }
    } else {
        api_key
    };

    // --- Model name ------------------------------------------------------
    let model: String = if provider_name == "none" {
        String::new()
    } else if provider_name == "apple" {
        // On-device Apple model is fixed; nothing to choose.
        default_model.to_string()
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
    // Every option, its order, and its one-line description come from
    // `stt_catalog`. Nothing here names an engine, so a build compiled
    // without the streaming engine simply does not offer it and local
    // Whisper leads instead.
    let mut stt_select = select("Choose your speech-to-text engine");
    for provider in stt_catalog::catalog() {
        stt_select = stt_select.item(provider.id, provider.display_name, provider.description);
    }
    let stt_engine: &str = stt_select.interact().map_err(|e| anyhow::anyhow!(e))?;
    let stt_provider =
        stt_catalog::find(stt_engine).expect("the picker only offers ids from the catalog");

    // Where the audio goes, said before anything is configured. Rekody's
    // promise is that cloud engines are optional and labeled, and the
    // wizard is one of the places that has to keep it.
    if stt_provider.sends_audio_off_device() {
        println!(
            "  {}  {}",
            console::style("!").yellow().bold(),
            console::style(format!(
                "{} is a cloud engine: your recorded audio leaves this Mac.",
                stt_provider.display_name
            ))
            .dim()
        );
    }

    // --- Step 2b: that provider's own fields ------------------------------
    // The URL comes before the key, because on a custom endpoint the key is
    // meaningless until Rekody knows where it is going.
    let mut custom_stt_base_url: Option<String> = None;
    let mut custom_stt_model: Option<String> = None;
    for field in stt_provider.fields {
        match field.config_key {
            "custom_stt_base_url" => {
                custom_stt_base_url = Some(prompt_endpoint_url(field)?);
            }
            "custom_stt_model" => {
                let model: String = input(field.label)
                    .placeholder(field.placeholder)
                    .interact()
                    .map_err(|e| anyhow::anyhow!(e))?;
                custom_stt_model = Some(model.trim().to_string());
            }
            // `whisper_model` has its own architecture-aware step below, and
            // `cohere_stt_port` keeps its config-file default. Both are
            // still declared in the catalog so every other surface renders
            // them; the wizard just owns their prompts.
            _ => {}
        }
    }

    if let Some(url) = &custom_stt_base_url {
        // Say the destination host plainly, once, before a key is typed.
        if let Ok(endpoint) = rekody_stt::resolve_transcription_endpoint(url) {
            println!(
                "  {}  {}",
                console::style("→").cyan().bold(),
                console::style(format!(
                    "Audio will be sent to {}",
                    endpoint.host_str().unwrap_or("this endpoint")
                ))
                .dim()
            );
        }
    }

    // --- Step 2c: that provider's API key ---------------------------------
    // Keychain first, then validation where there is a real endpoint to
    // validate against. Entirely driven by the catalog's KeySpec, so a new
    // provider inherits the whole flow.
    let reuse_llm_key_for_groq_stt =
        can_reuse_llm_key_for_groq_stt(stt_engine, provider_name, &api_key);
    let stt_api_key: Option<String> = match &stt_provider.key {
        // The LLM step's key is only a Groq key when that provider is itself
        // Groq; any other provider's key must never be written to
        // groq_api_key.
        Some(_) if reuse_llm_key_for_groq_stt => Some(api_key.clone()),
        Some(spec) => prompt_stt_key(stt_provider.display_name, spec)?,
        None => None,
    };

    // For local Whisper, ask model size and download.
    //
    // The offer is architecture-aware (#141). Turbo carries the full 32-layer
    // large-v3 encoder, and whisper.cpp pays that encode once per dictation
    // against a zero-padded 30 s window, so "fast" is true only where the
    // encoder runs on the Neural Engine. Recommending it to a machine with no
    // ANE promises Apple Silicon speed the hardware cannot deliver, which is
    // what the first Intel user hit. See `DEFAULT_WHISPER_MODEL`.
    let whisper_size: &str = if stt_engine == "local" {
        whisper_size_prompt()
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?
    } else {
        // Non-local STT downloads nothing here, but whisper_model still gets
        // written to config. Write the recommended default so a later hand
        // edit to stt_engine = "local" points at the model Setup and the app
        // actually fetch, not at a never-downloaded tiny.
        DEFAULT_WHISPER_MODEL
    };

    let (whisper_file, whisper_remote) = whisper_download_spec(whisper_size);

    // --- Download model (only for local STT) -----------------------------
    if stt_engine == "local" {
        let model_dir = resolve_model_dir();
        let model_path = model_dir.join(whisper_file);

        // An existing file is not a trusted file. Artifacts installed before
        // the move to Rekody's own model repo predate the pinned checksums,
        // so presence alone would let an unverified model live forever on a
        // machine that has since upgraded. Re-hash it and re-fetch on a
        // mismatch. Costs a few seconds, and only during setup.
        let whisper_ok = model_path.exists() && {
            let sp = spinner();
            sp.start("Verifying Whisper model...");
            let ok = verify_model_checksum(
                model_path.to_str().unwrap_or(""),
                expected_checksum_for(whisper_file),
            );
            if ok {
                sp.stop(format!("Model verified at {}", model_path.display()));
            } else {
                sp.stop("Installed Whisper model does not match its checksum, re-downloading");
                let _ = std::fs::remove_file(&model_path);
            }
            ok
        };

        if !whisper_ok {
            std::fs::create_dir_all(&model_dir).context("failed to create model directory")?;

            // Rekody model mirror first, upstream whisper.cpp as fallback;
            // both repos serve identical bytes, so the same pinned checksum
            // is enforced on whichever source serves the file.
            let (primary_url, fallback_url) = whisper_asset_urls(whisper_remote);
            let expected_sha256 = expected_checksum_for(whisper_file);
            download_with_fallback(
                &primary_url,
                &fallback_url,
                &model_path,
                expected_sha256,
                expected_sha256,
            )
            .context("failed to download Whisper model")?;
        }

        // Apple Silicon: fetch the matching Core ML encoder so whisper.cpp can
        // run the encoder pass on the Neural Engine. No-op on other platforms.
        if let Err(e) = ensure_coreml_encoder(whisper_size, &model_dir) {
            println!(
                "  {}  Core ML encoder not installed: {}",
                console::style("⚠").yellow().bold(),
                e
            );
            println!(
                "     {}",
                console::style("(rekody will still work; the encoder just runs on the CPU)").dim()
            );
        }
    } // end if stt_engine == "local"

    // --- Download the Rekody streaming model (3 files, ~892 MB total) -----
    #[cfg(feature = "nemotron")]
    if stt_engine == "nemotron" {
        let model_dir = resolve_model_dir().join("nemotron-en-int8");
        std::fs::create_dir_all(&model_dir).context("failed to create model directory")?;
        for &file in NEMOTRON_FILES {
            let dest = model_dir.join(file);
            // Same rule as Whisper above: verify what is already on disk
            // rather than trusting that it arrived from us.
            let verified = dest.exists() && {
                let sp = spinner();
                sp.start(format!("Verifying {file}..."));
                let ok =
                    verify_model_checksum(dest.to_str().unwrap_or(""), expected_checksum_for(file));
                if ok {
                    sp.stop(format!("{file} verified"));
                } else {
                    sp.stop(format!(
                        "{file} does not match its checksum, re-downloading"
                    ));
                    let _ = std::fs::remove_file(&dest);
                }
                ok
            };

            if !verified {
                // Rekody's published conversion is the sole source; every
                // artifact verifies against its pinned checksum.
                let url = format!("{REKODY_NEMOTRON_BASE}/{file}");
                download_verified(&url, &dest, expected_checksum_for(file)).with_context(|| {
                    format!("failed to download the Rekody streaming model {file}")
                })?;
            }
        }
    }

    // --- Step 3: macOS permissions ---------------------------------------
    #[cfg(target_os = "macos")]
    {
        // Detect Accessibility permission. If missing, trigger the macOS
        // dialog via AXIsProcessTrustedWithOptions(prompt=true), which also
        // adds /usr/local/bin/rekody to the Accessibility list.
        if rekody_hotkey::is_accessibility_trusted() {
            println!(
                "  {} Accessibility permission already granted.",
                console::style("✓").green().bold()
            );
        } else {
            println!(
                "  {} Accessibility permission not granted.",
                console::style("✗").red().bold()
            );
            println!("  rekody needs Accessibility to capture ⌥ + space system-wide.");
            println!();
            println!("  A macOS dialog will appear asking you to open System Settings.");
            println!("  After granting permission, restart rekody.");
            println!();

            // Trigger the system prompt (adds rekody to the Accessibility list).
            let _ = rekody_hotkey::request_accessibility_permission();

            // Also open the Accessibility settings pane directly.
            let _ = Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .status();
        }

        // Eagerly probe the microphone. On first access this fires the
        // macOS TCC prompt while the user is still in the wizard, so they
        // don't hit a silent failure the first time they try to record.
        //
        // Note: on CLI apps, macOS attributes microphone permission to the
        // "responsible process" — usually the parent terminal (Terminal.app,
        // iTerm2, Warp, Ghostty). The prompt and the entry in
        // System Settings → Privacy → Microphone will be under the terminal's
        // name, not rekody. That's expected and correct under TCC rules.
        match rekody_audio::probe_microphone() {
            rekody_audio::MicStatus::Granted => {
                println!(
                    "  {} Microphone permission granted.",
                    console::style("✓").green().bold()
                );
            }
            rekody_audio::MicStatus::Denied => {
                println!(
                    "  {} Microphone permission denied.",
                    console::style("✗").red().bold()
                );
                println!(
                    "  Grant access to your terminal in System Settings → Privacy & Security → Microphone,"
                );
                println!("  then restart rekody.");
                let _ = Command::new("open")
                    .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
                    .status();
            }
            rekody_audio::MicStatus::NoDevice => {
                println!(
                    "  {} No microphone detected. Connect one and re-run `rekody setup`.",
                    console::style("!").yellow().bold()
                );
            }
            rekody_audio::MicStatus::Unknown => {
                println!(
                    "  {} Microphone probe inconclusive — will prompt on first recording.",
                    console::style("?").yellow().bold()
                );
            }
        }
    }

    // --- Step 3b: Hotkey -------------------------------------------------
    // One key for everyone: ⌥ + space. Hold to talk, quick-tap for
    // hands-free. (fn_key remains honored in config.toml for existing
    // setups, but isn't offered here — two key choices confused users.)
    println!();
    println!("  {}", console::style("Hotkey").bold());
    println!(
        "    hold {} to talk · quick-tap for hands-free",
        console::style(crate::TRIGGER_LABEL).cyan()
    );
    let trigger_key: &str = "option_space";

    // --- Step 4: Training-data consent -------------------------------------
    // One-time explicit consent for the default-on local dataset capture, so
    // a voice app never writes recordings to disk without the user saying yes.
    let save_training_data: bool = confirm(
        "Build a local fine-tuning dataset as you dictate? \
         (saves audio + transcripts locally — never leaves this device)",
    )
    .initial_value(true)
    .interact()
    .map_err(|e| anyhow::anyhow!(e))?;

    // --- Step 5: Write config --------------------------------------------
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

    // The selected provider's key goes to the config key its catalog entry
    // names. That is how `groq_api_key` still carries a Groq STT key (a real
    // Groq key, either reused from a Groq LLM step or prompted for above,
    // never another provider's) without this code knowing about Groq.
    let stt_key_line = match (&stt_provider.key, &stt_api_key) {
        (Some(spec), Some(key)) if !key.is_empty() => {
            format!("{} = {}", spec.config_key, toml_string(key))
        }
        _ => String::new(),
    };

    // Extra fields the provider declared, in catalog order.
    let mut stt_field_lines: Vec<String> = Vec::new();
    if let Some(url) = &custom_stt_base_url {
        stt_field_lines.push(format!("custom_stt_base_url = {}", toml_string(url)));
    }
    if let Some(model) = &custom_stt_model {
        stt_field_lines.push(format!("custom_stt_model = {}", toml_string(model)));
    }
    let stt_field_block = stt_field_lines.join("\n");

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
        r#"# rekody Configuration
# Generated by the first-run setup wizard.
# Edit freely — see https://github.com/rekody/rekody for docs.

# How the trigger key starts/stops dictation:
#   "both"          hold to talk (release stops) OR quick-tap to go hands-free
#                   until you tap again — best of both, the default
#   "push_to_talk"  hold only
#   "toggle"        tap to start, tap to stop
activation_mode = "both"
# Dictation trigger key: "option_space" (⌥Space) or "fn_key" (Fn/Globe).
# fn_key requires System Settings → Keyboard → "Press 🌐 key to" → "Do Nothing".
trigger_key = "{trigger_key}"
# Longest single recording in seconds (safety stop for forgotten sessions).
# Raise it if you ramble hands-free for a long time. 0 = no limit.
max_recording_secs = 1800
whisper_model = "{whisper_size}"
vad_threshold = 0.01
# Microphone to capture from, matched by name (case-insensitive). "system"
# follows the macOS default input — which jumps to whatever you connected
# last (e.g. AirPods). Pin a device to keep it: input_device = "MacBook Air Microphone"
# (run `rekody doctor` to see the resolved device). Unknown names fall back
# to the system default. A list is an ordered fallback chain, first connected
# wins: input_device = ["Shure MVX2U", "MacBook Air Microphone"]
input_device = "system"
injection_method = "clipboard"
# Save each dictation's audio (FLAC) + transcript locally so a personal
# fine-tuning dataset is ready when you want one. Local-only, never uploaded.
# Folder: ~/.local/share/rekody/training-data — set false to disable.
save_training_data = {save_training_data}
{stt_line}
{stt_key_line}
{stt_field_block}

{provider_block}
"#,
        trigger_key = trigger_key,
        whisper_size = whisper_size,
        stt_line = stt_line,
        stt_key_line = stt_key_line,
        stt_field_block = stt_field_block,
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

    // On-device Apple cleanup needs the rekody-fm helper. If it isn't installed
    // yet, tell the user how (it degrades to raw transcript until then).
    if provider_name == "apple" && !rekody_llm::AppleFoundationProvider::helper_installed() {
        println!(
            "\n  {}  Apple on-device cleanup needs a one-time helper build:\n     {}\n     (Until then, dictation still works — it just skips cleanup.)",
            console::style("→").cyan().bold(),
            console::style("make fm-helper").cyan()
        );
    }

    // --- Summary & Done --------------------------------------------------
    // One naming table, the catalog's. Whisper is the only entry whose
    // summary carries more than its name, because the size was just chosen.
    let stt_display = if stt_engine == "local" {
        format!("{} ({whisper_size})", stt_provider.display_name)
    } else {
        stt_provider.display_name.to_string()
    };

    let summary = format!(
        "Setup complete! Run 'rekody' to start dictating.\n\
         \n  LLM:     {provider}/{model}\
         \n  STT:     {stt}\
         \n  Config:  {config}\
         \n\nTip: run 'rekody skill' to reshape dictation into email, notes, commit messages, and more.",
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

/// Whether the Groq STT step may reuse the key collected in the LLM step.
///
/// Only a Groq LLM provider's key is a Groq key. Reusing any other
/// provider's key (Anthropic, OpenAI, ...) as `groq_api_key` stores a key
/// Groq will reject; the wizard prompts for a real one instead.
fn can_reuse_llm_key_for_groq_stt(stt_engine: &str, llm_provider: &str, llm_key: &str) -> bool {
    stt_engine == "groq" && llm_provider == "groq" && !llm_key.is_empty()
}

/// Providers whose keys [`validate_api_key`] can actually check against a
/// stable, documented endpoint. For anything else (notably "custom", whose
/// base URL is unknown) the wizard must not fake it: no spinner, and never
/// the word "valid" without a real network check behind it.
fn provider_has_key_check(provider: &str) -> bool {
    matches!(
        provider,
        "groq"
            | "deepgram"
            | "openai"
            | "cerebras"
            | "anthropic"
            | "gemini"
            | "together"
            | "openrouter"
    )
}

/// Make a lightweight test call to verify an API key works.
///
/// Returns `true` if the key appears valid, `false` on auth errors or
/// network failures. Only meaningful for providers where
/// [`provider_has_key_check`] is true; anything else returns `true` without
/// checking, and callers must not present that as validation.
/// A TOML basic string, quotes and all.
///
/// The wizard writes config lines by hand, and a value typed by a user can
/// contain a quote or a backslash. A model name like `whis"per` would
/// otherwise produce a config file the daemon cannot parse, which looks
/// like setup silently breaking Rekody. `toml` does the escaping.
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

/// Ask for a URL the user's voice will be sent to, and refuse to accept one
/// Rekody would not send audio to.
///
/// The same guard the engine enforces, run while the URL is being typed, so
/// nobody finishes setup with an endpoint that cannot work.
fn prompt_endpoint_url(field: &stt_catalog::Field) -> anyhow::Result<String> {
    loop {
        let raw: String = input(field.label)
            .placeholder(field.placeholder)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        match rekody_stt::resolve_transcription_endpoint(&raw) {
            Ok(_) => return Ok(raw.trim().to_string()),
            Err(e) => println!("  {}  {}", console::style("✗").red().bold(), e),
        }
    }
}

/// Prompt for a speech-to-text provider's API key: keychain first, then
/// validation where the provider has an endpoint worth validating against.
///
/// Driven entirely by [`stt_catalog::KeySpec`], so a provider added to the
/// catalog inherits existing-key detection, the masked preview, rotation,
/// validation, and the "continue anyway" escape without a line of new code.
/// The key is written to the keychain and returned; it is never printed.
fn prompt_stt_key(
    display_name: &str,
    spec: &stt_catalog::KeySpec,
) -> anyhow::Result<Option<String>> {
    let account = spec.keyring_account;

    // Ask for a key, remembering it in the keychain when one is given.
    let ask = |prompt: &str| -> anyhow::Result<Option<String>> {
        if !spec.obtain_label.is_empty() {
            println!(
                "  {}  {}",
                console::style("→").cyan().bold(),
                console::style(format!("Get one at {}", spec.obtain_label)).dim()
            );
        }
        let key: String = input(prompt)
            .placeholder(spec.placeholder)
            // An optional key is genuinely optional: a transcription server
            // on localhost usually wants none.
            .required(spec.required)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        let key = key.trim().to_string();
        if key.is_empty() {
            return Ok(None);
        }
        set_keychain(account, &key);
        Ok(Some(key))
    };

    let mut key = if let Some(masked) = get_keychain_masked(account) {
        println!("  Found in Keychain: {masked}");
        let use_existing: bool = confirm("Use existing key? (No = enter a new one)")
            .initial_value(true)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        if use_existing {
            get_keychain_full(account)
        } else {
            ask(&format!("Enter your new {display_name} API key"))?
        }
    } else {
        ask(&format!("Enter your {display_name} API key"))?
    };

    // Validate only where a real endpoint answers. Claiming "valid" for a
    // provider with no check would be a lie the user acts on.
    if !provider_has_key_check(account) {
        return Ok(key);
    }
    loop {
        let Some(current) = key.clone().filter(|k| !k.is_empty()) else {
            return Ok(key);
        };
        let sp = spinner();
        sp.start(format!("Validating {display_name} API key..."));
        if validate_api_key(account, &current) {
            sp.stop(format!("{display_name} API key valid \u{2713}"));
            return Ok(Some(current));
        }
        sp.stop(format!(
            "{display_name} API key validation failed: check your key"
        ));
        let proceed: bool = confirm("Continue anyway?")
            .initial_value(false)
            .interact()
            .map_err(|e| anyhow::anyhow!(e))?;
        if proceed {
            return Ok(Some(current));
        }
        key = ask(&format!("Enter your {display_name} API key"))?;
    }
}

fn validate_api_key(provider: &str, key: &str) -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let request = match provider {
        "groq" => client
            .get("https://api.groq.com/openai/v1/models")
            .header("Authorization", format!("Bearer {key}")),
        "deepgram" => client
            .get("https://api.deepgram.com/v1/projects")
            .header("Authorization", format!("Token {key}")),
        "openai" => client
            .get("https://api.openai.com/v1/models")
            .header("Authorization", format!("Bearer {key}")),
        "cerebras" => client
            .get("https://api.cerebras.ai/v1/models")
            .header("Authorization", format!("Bearer {key}")),
        "anthropic" => client
            .get("https://api.anthropic.com/v1/models")
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        "gemini" => client
            .get("https://generativelanguage.googleapis.com/v1beta/models")
            .query(&[("key", key)]),
        "together" => client
            .get("https://api.together.xyz/v1/models")
            .header("Authorization", format!("Bearer {key}")),
        "openrouter" => client
            .get("https://openrouter.ai/api/v1/key")
            .header("Authorization", format!("Bearer {key}")),
        // No stable endpoint to check against (custom base URLs, local
        // providers). Callers gate on provider_has_key_check.
        _ => return true,
    };

    // A 400 from an OpenAI-style endpoint still means the key authenticated
    // (the request itself was malformed). Gemini is the exception: it
    // signals an invalid key WITH a 400, so only 2xx counts there.
    let accept_bad_request = provider != "gemini";
    match request.send() {
        Ok(resp) => {
            resp.status().is_success() || (accept_bad_request && resp.status().as_u16() == 400)
        }
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
// Whisper model sources (Rekody mirror first, upstream fallback)
// ---------------------------------------------------------------------------

/// Rekody's own model mirror on Hugging Face. Tried first for every whisper
/// artifact so availability stays under Rekody's control — an upstream rename
/// or removal (like the ggml-large.bin one) cannot break onboarding.
const REKODY_WHISPER_BASE: &str =
    "https://huggingface.co/Rekody/rekody-models/resolve/main/whisper";

/// Upstream whisper.cpp model repo, kept as the fallback source.
const UPSTREAM_WHISPER_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Build the `(primary, fallback)` download URLs for a whisper artifact.
///
/// Both repos serve the artifact under the same `remote_file` name; only the
/// base path differs.
fn whisper_asset_urls(remote_file: &str) -> (String, String) {
    (
        format!("{REKODY_WHISPER_BASE}/{remote_file}"),
        format!("{UPSTREAM_WHISPER_BASE}/{remote_file}"),
    )
}

/// Download `url` to `dest`, then enforce the expected SHA-256.
///
/// On a checksum mismatch the corrupt file is deleted before returning an
/// error, so nothing unverified is left on disk for the engine (or a retry)
/// to pick up.
fn download_verified(url: &str, dest: &std::path::Path, expected_sha256: &str) -> Result<()> {
    download_model(url, dest)?;
    if !verify_model_checksum(dest.to_str().unwrap_or(""), expected_sha256) {
        let _ = std::fs::remove_file(dest);
        anyhow::bail!("SHA-256 checksum mismatch for {}", dest.display());
    }
    Ok(())
}

/// Download a model artifact from the Rekody-controlled repo, falling back
/// to the secondary source when the primary fails for any reason: non-2xx
/// status, network error, or checksum mismatch. Each attempt verifies
/// against its own source's pinned SHA-256 (see [`download_verified`]);
/// whisper artifacts are byte-identical on both sources (Rekody mirror,
/// upstream whisper.cpp) so their two pins are equal. Nemotron artifacts
/// never come through here: the Rekody org is their sole source, with no
/// fallback. The source that served the file is echoed so it is always
/// clear where the bytes came from.
fn download_with_fallback(
    primary_url: &str,
    fallback_url: &str,
    dest: &std::path::Path,
    expected_sha256_primary: &str,
    expected_sha256_fallback: &str,
) -> Result<()> {
    match download_verified(primary_url, dest, expected_sha256_primary) {
        Ok(()) => {
            println!(
                "  {} Downloaded from Rekody's model repo.",
                console::style("✓").green().bold()
            );
            Ok(())
        }
        Err(primary_err) => {
            tracing::warn!("Rekody model repo failed for {primary_url}: {primary_err:#}");
            println!(
                "  {}  Rekody model repo unavailable; trying the fallback source...",
                console::style("⚠").yellow().bold()
            );
            download_verified(fallback_url, dest, expected_sha256_fallback)?;
            println!(
                "  {} Downloaded from the fallback source.",
                console::style("✓").green().bold()
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Nemotron streaming model sources (Rekody model repo, the sole source)
// ---------------------------------------------------------------------------

/// Rekody's branded streaming model on Hugging Face: an int8 ONNX conversion
/// of the current (March 2026) NVIDIA Nemotron streaming checkpoint, exported
/// at the `[70,1]` cache-aware profile (160 ms chunks). The sole source for
/// every streaming artifact (unlike whisper, which keeps upstream whisper.cpp
/// as a fallback), so availability stays under Rekody's control.
///
/// This is a SEPARATE repo from the 560 ms artifact, deliberately. The two
/// exports are not interchangeable: a 160 ms encoder fed a 560 ms chunk
/// silently emits only the first 2 of 7 frames and drops five sevenths of the
/// audio. Publishing the new profile over the old repo would therefore break
/// every user still on an older binary, whose pinned checksum would stop
/// matching what the URL serves and whose re-download would loop. Keeping the
/// old repo untouched makes both upgrade and rollback self-healing: whichever
/// binary is installed re-verifies the local file, finds the other profile's
/// bytes, and re-fetches from its own pinned source.
#[cfg(feature = "nemotron")]
const REKODY_NEMOTRON_BASE: &str =
    "https://huggingface.co/Rekody/rekody-streaming-en-0.6b-160ms-int8/resolve/main";

/// Files the streaming engine loads from `<models>/nemotron-en-int8/`.
#[cfg(feature = "nemotron")]
const NEMOTRON_FILES: &[&str] = &["encoder.onnx", "decoder_joint.onnx", "tokenizer.model"];

/// Chunk length, in milliseconds, of the streaming artifact the checksums
/// below pin.
///
/// The runtime never uses this to DRIVE anything; it reads the real geometry
/// off whatever artifact loaded, which is the whole point. This exists only so
/// the daemon can notice that an older artifact is still installed and say so.
/// Without it, a user who upgrades the binary but has not re-run setup keeps
/// the slower profile silently, since the old artifact still loads and still
/// works.
#[cfg(feature = "nemotron")]
pub const NEMOTRON_SHIPPED_CHUNK_MS: usize = 160;

// ---------------------------------------------------------------------------
// Checksum verification
// ---------------------------------------------------------------------------

// Known SHA-256 checksums for model artifacts, keyed by the LOCAL filename
// the artifact is saved as. For whisper artifacts, the Rekody model mirror
// (Rekody/rekody-models) and upstream ggerganov/whisper.cpp serve identical
// bytes, so one hash covers both sources. Where the local name differs from
// the upstream one (the "large" family), the hash is of the upstream v3
// content that gets saved under the engine's expected local name. Streaming
// artifacts pin the Rekody-published model
// (Rekody/rekody-streaming-en-0.6b-160ms-int8),
// the sole source for that engine.
//
// To refresh after an upstream model update:
//   shasum -a 256 ggml-tiny.bin    (macOS)
//   sha256sum ggml-tiny.bin        (Linux)
//
// Last verified: 2026-07-10
const EXPECTED_CHECKSUMS: &[(&str, &str)] = &[
    // GGML model weights
    (
        "ggml-tiny.bin",
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    ),
    (
        "ggml-tiny.en.bin",
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    ),
    (
        "ggml-small.bin",
        "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    ),
    (
        "ggml-medium.bin",
        "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    ),
    (
        "ggml-large-v3-turbo-q5_0.bin",
        "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    ),
    // Local name for the "large" model; the file content is upstream
    // ggml-large-v3.bin (saved-as — see whisper_download_spec).
    (
        "ggml-large.bin",
        "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    ),
    // Core ML encoder archives (Apple Silicon), keyed by the local zip name
    // `<mlmodelc_dir>.zip` that ensure_coreml_encoder downloads to.
    (
        "ggml-tiny-encoder.mlmodelc.zip",
        "c88cbd2648e1f5415092bcf5256add463a0f19943e6938f46e8d4ffdebd47739",
    ),
    (
        "ggml-small-encoder.mlmodelc.zip",
        "de43fb9fed471e95c19e60ae67575c2bf09e8fb607016da171b06ddad313988b",
    ),
    (
        "ggml-medium-encoder.mlmodelc.zip",
        "79b0b8d436d47d3f24dd3afc91f19447dd686a4f37521b2f6d9c30a642133fbd",
    ),
    (
        "ggml-large-v3-turbo-encoder.mlmodelc.zip",
        "84bedfe895bd7b5de6e8e89a0803dfc5addf8c0c5bc4c937451716bf7cf7988a",
    ),
    // Local name for the "large" encoder archive; the content is upstream
    // ggml-large-v3-encoder.mlmodelc.zip (saved-as, mirroring ggml-large.bin).
    (
        "ggml-large-encoder.mlmodelc.zip",
        "47837be7594a29429ec08620043390c4d6d467f8bd362df09e9390ace76a55a4",
    ),
    // Nemotron streaming artifacts (Rekody/rekody-streaming-en-0.6b-160ms-int8),
    // keyed by the local filename under models/nemotron-en-int8/. These pin
    // the PRIMARY (Rekody) bytes, hashes published in that repo's SHA256SUMS.
    //
    // These are the [70,1] / 160 ms export. A user upgrading from the 560 ms
    // artifact has the OLD encoder.onnx on disk; it fails this checksum, is
    // deleted, and is replaced from the URL above. That migration only works
    // because setup re-verifies files it finds already installed (18207b6):
    // before that fix an upgrading user would have been left on the old
    // artifact silently, which the new runtime would then drive at the wrong
    // chunk size.
    //
    // tokenizer.model is byte-identical across both profiles (the SentencePiece
    // model does not depend on the latency profile), so its pin is unchanged
    // and an upgrading user does not re-download it.
    (
        "encoder.onnx",
        "5a8f5c01d7804346e3e79d539bde6011907961192c578441d7b2eb725f82220c",
    ),
    (
        "decoder_joint.onnx",
        "6fa6b88cd439b0281b1b9361c5789d51c851fe202c43bb2fdccabb87186a9630",
    ),
    (
        "tokenizer.model",
        "07d4e5a63840a53ab2d4d106d2874768143fb3fbdd47938b3910d2da05bfb0a9",
    ),
];

/// Verify the SHA-256 checksum of a downloaded file.
///
/// Uses `shasum -a 256` on macOS or `sha256sum` on Linux.
/// Returns `false` only on a definite mismatch — callers treat that as
/// "delete the file and try another source". An empty `expected_sha256`
/// skips verification, and a missing hashing tool warns without blocking
/// (there is no evidence of corruption in either case).
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
                false
            }
        }
        _ => {
            println!("  WARNING: Could not compute SHA-256 checksum (shasum/sha256sum not found).");
            println!(
                "  Proceeding unverified. Check manually with: shasum -a 256 {}",
                path
            );
            true
        }
    }
}

/// Look up the expected checksum for a model artifact by its LOCAL filename
/// (whisper `.bin`, Core ML encoder `.zip`, or Nemotron file). Empty string
/// when unpinned.
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
    let entry = keyring::Entry::new("com.rekody.voice", provider).ok()?;
    let key = entry.get_password().ok()?;
    if key.is_empty() {
        return None;
    }
    Some(mask_secret(&key))
}

/// Show only the last four characters of a secret, the way `rekody key` and
/// `rekody config` already do (`key_tui::mask`, `main::mask_key`).
///
/// This used to reveal the first four as well, which is eight characters of
/// a live key on screen and in terminal scrollback for no benefit: the tail
/// alone is enough to tell two keys apart. Counted in characters rather than
/// bytes so a non-ASCII value cannot panic on a slice boundary.
fn mask_secret(key: &str) -> String {
    let tail: String = {
        let chars: Vec<char> = key.chars().collect();
        if chars.len() <= 4 {
            return "████".to_string();
        }
        chars[chars.len() - 4..].iter().collect()
    };
    format!("████████████{tail}")
}

/// Retrieve the full (unmasked) API key from macOS Keychain, or None.
fn get_keychain_full(provider: &str) -> Option<String> {
    let entry = keyring::Entry::new("com.rekody.voice", provider).ok()?;
    let key = entry.get_password().ok()?;
    if key.is_empty() { None } else { Some(key) }
}

/// Save an API key to macOS Keychain.
fn set_keychain(provider: &str, key: &str) {
    if let Ok(entry) = keyring::Entry::new("com.rekody.voice", provider)
        && let Err(e) = entry.set_password(key)
    {
        tracing::warn!("failed to save {provider} key to Keychain: {e}");
    }
}

// ---------------------------------------------------------------------------
// Whisper model picker
// ---------------------------------------------------------------------------

/// One row of Setup's local-Whisper size picker: `(value, label, hint)`.
type SizeItem = (&'static str, &'static str, &'static str);

/// The picker's prompt and rows for the machine this build runs on.
///
/// Split out from the `cliclack` builder so the offer is assertable without
/// driving a TTY: `whisper_size_offer_tests` pins the Apple Silicon rows
/// character for character against what shipped before #141.
///
/// Two separate lists rather than one list with conditional strings, so the
/// Apple Silicon offer can be read straight off the page.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn whisper_size_offer() -> (&'static str, [SizeItem; 5]) {
    (
        "Choose local Whisper model size",
        [
            (
                "turbo",
                "Turbo (574 MB)",
                "fast + near-large accuracy (recommended)",
            ),
            ("tiny", "Tiny (75 MB)", "fastest — basic accuracy"),
            ("small", "Small (488 MB)", "balanced"),
            ("medium", "Medium (1.5 GB)", "better accuracy"),
            ("large", "Large (3.1 GB)", "best accuracy"),
        ],
    )
}

/// No Neural Engine here, so the encoder runs on CPU and the ordering by
/// speed changes completely. Turbo stays on the list (it is the accuracy
/// ceiling, and someone transcribing long-form on a fast desktop may still
/// want it) but it is no longer preselected and no longer described as fast.
/// The hints say which way each choice trades, and the prompt names the
/// reason so the recommendation does not look arbitrary.
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn whisper_size_offer() -> (&'static str, [SizeItem; 5]) {
    (
        "Choose local Whisper model size (this machine has no Neural Engine)",
        [
            (
                "small",
                "Small (488 MB)",
                "recommended here: good accuracy at a workable speed on CPU",
            ),
            ("tiny", "Tiny (75 MB)", "fastest, but visibly less accurate"),
            (
                "turbo",
                "Turbo (574 MB)",
                "near-large accuracy, but a large-model encode on every sentence",
            ),
            ("medium", "Medium (1.5 GB)", "slower again than turbo"),
            ("large", "Large (3.1 GB)", "best accuracy, slowest"),
        ],
    )
}

/// Assemble the picker. The preselection is `DEFAULT_WHISPER_MODEL` on every
/// architecture, which on Apple Silicon is "turbo": the same value, and the
/// same first row, that shipped before #141.
fn whisper_size_prompt() -> cliclack::Select<&'static str> {
    let (prompt, items) = whisper_size_offer();
    let mut sel = select(prompt);
    for (value, label, hint) in items {
        sel = sel.item(value, label, hint);
    }
    sel.initial_value(DEFAULT_WHISPER_MODEL)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Canonical config directory: `~/.config/rekody`
fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("rekody"))
}

/// Full path to `~/.config/rekody/config.toml`.
fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Model directory: `$REKODY_MODEL_DIR` or `~/.local/share/rekody/models`.
fn resolve_model_dir() -> PathBuf {
    std::env::var("REKODY_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".local").join("share").join("rekody").join("models"))
                .unwrap_or_else(|| PathBuf::from("models"))
        })
}

/// Map a whisper model size to `(local_file, remote_file)`.
///
/// `local_file` is the on-disk name the engine loads (and the key into
/// EXPECTED_CHECKSUMS); `remote_file` is the artifact name in both the
/// Rekody model mirror and upstream whisper.cpp. They differ only for
/// "large": upstream renamed the un-versioned large model (ggml-large.bin
/// is gone, the current release is ggml-large-v3.bin), so we download the
/// v3 content but keep the local filename `ggml-large.bin`, which is what
/// the engine (rekody-stt `WhisperModel::Large`) loads from disk.
fn whisper_download_spec(size: &str) -> (&'static str, &'static str) {
    match size {
        "tiny" => ("ggml-tiny.bin", "ggml-tiny.bin"),
        "small" => ("ggml-small.bin", "ggml-small.bin"),
        "medium" => ("ggml-medium.bin", "ggml-medium.bin"),
        "turbo" => (
            "ggml-large-v3-turbo-q5_0.bin",
            "ggml-large-v3-turbo-q5_0.bin",
        ),
        "large" => ("ggml-large.bin", "ggml-large-v3.bin"),
        _ => ("ggml-tiny.bin", "ggml-tiny.bin"),
    }
}

/// Map a whisper model size string to its GGML filename.
#[allow(dead_code)]
fn whisper_file_name(size: &str) -> &str {
    match size.to_lowercase().as_str() {
        "tiny" => "ggml-tiny.bin",
        "small" => "ggml-small.bin",
        "medium" => "ggml-medium.bin",
        "turbo" => "ggml-large-v3-turbo-q5_0.bin",
        "large" => "ggml-large.bin",
        _ => "ggml-small.bin",
    }
}

// ---------------------------------------------------------------------------
// Core ML encoder (Apple Silicon)
// ---------------------------------------------------------------------------
//
// whisper.cpp loads `<model>-encoder.mlmodelc` from the same directory as the
// `.bin` file when the binary is built with WHISPER_COREML=1 (whisper-rs
// `coreml` feature). The encoder then runs on the Neural Engine instead of
// the CPU, which is the whole reason Turbo is a sensible Apple Silicon
// default (Metal is never compiled in: whisper-rs 0.13 sets GGML_METAL=OFF).
// First use compiles the MIL bytecode into
// `~/Library/Caches/com.apple.e5rt.e5bundlecache` (30–60 s, one-time).

/// Return `(mlmodelc_dir_name, remote_archive_name)` for the given whisper
/// size, or `None` if no Core ML encoder is published for it.
///
/// The remote name is the artifact in both the Rekody model mirror and
/// upstream whisper.cpp (see whisper_asset_urls); the local dir name is what
/// whisper.cpp derives from the model filename on disk.
///
/// Only `ensure_coreml_encoder` (and its tests) call this, and that is
/// compiled out where there is no Neural Engine, so the cfg keeps the Intel
/// release build free of a dead-code warning while the tests still run on
/// every host.
#[cfg(any(all(target_os = "macos", target_arch = "aarch64"), test))]
fn coreml_archive_for(size: &str) -> Option<(&'static str, &'static str)> {
    match size.to_lowercase().as_str() {
        "tiny" => Some((
            "ggml-tiny-encoder.mlmodelc",
            "ggml-tiny-encoder.mlmodelc.zip",
        )),
        "small" => Some((
            "ggml-small-encoder.mlmodelc",
            "ggml-small-encoder.mlmodelc.zip",
        )),
        "medium" => Some((
            "ggml-medium-encoder.mlmodelc",
            "ggml-medium-encoder.mlmodelc.zip",
        )),
        "turbo" => Some((
            "ggml-large-v3-turbo-encoder.mlmodelc",
            "ggml-large-v3-turbo-encoder.mlmodelc.zip",
        )),
        // Upstream renamed the un-versioned large encoder: the old
        // ggml-large-encoder.mlmodelc.zip is gone (404); fetch the v3 archive.
        // The local directory name stays `ggml-large-encoder.mlmodelc` because
        // whisper.cpp derives it from the model filename (`ggml-large.bin`) —
        // ensure_coreml_encoder renames the extracted dir to match.
        "large" => Some((
            "ggml-large-encoder.mlmodelc",
            "ggml-large-v3-encoder.mlmodelc.zip",
        )),
        _ => None,
    }
}

/// Download + unzip the Core ML encoder archive into `model_dir`. No-op if the
/// `.mlmodelc` directory already exists. Uses the system `unzip` binary (always
/// present on macOS) to avoid pulling a zip crate into the build.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn ensure_coreml_encoder(whisper_size: &str, model_dir: &std::path::Path) -> Result<()> {
    let Some((mlmodelc_name, remote_zip)) = coreml_archive_for(whisper_size) else {
        return Ok(());
    };
    let mlmodelc_path = model_dir.join(mlmodelc_name);
    if mlmodelc_path.exists() {
        let sp = spinner();
        sp.start("Checking Core ML encoder...");
        sp.stop(format!(
            "Core ML encoder already present at {}",
            mlmodelc_path.display()
        ));
        return Ok(());
    }

    println!(
        "  {}  Apple Silicon detected — fetching Core ML encoder for ~2× faster transcription.",
        console::style("→").cyan().bold()
    );
    println!(
        "     {}",
        console::style("(first transcription afterwards takes 30–60s while macOS compiles the model; subsequent runs are fast)")
            .dim()
    );

    let archive_file = format!("{mlmodelc_name}.zip");
    let archive_path = model_dir.join(&archive_file);
    let (primary_url, fallback_url) = whisper_asset_urls(remote_zip);
    let expected_sha256 = expected_checksum_for(&archive_file);
    download_with_fallback(
        &primary_url,
        &fallback_url,
        &archive_path,
        expected_sha256,
        expected_sha256,
    )
    .context("failed to download Core ML encoder archive")?;

    let sp = spinner();
    sp.start("Unzipping Core ML encoder...");
    let status = std::process::Command::new("unzip")
        .args(["-q", "-o"])
        .arg(&archive_path)
        .arg("-d")
        .arg(model_dir)
        .status()
        .context("failed to invoke unzip")?;
    if !status.success() {
        sp.stop("Unzip failed");
        anyhow::bail!(
            "unzip exited with {}; archive left at {}",
            status,
            archive_path.display()
        );
    }

    // The archive's root directory is named after the remote file (archive
    // name minus ".zip"), which can differ from the local name whisper.cpp
    // expects — e.g. ggml-large-v3-encoder.mlmodelc.zip extracts to
    // ggml-large-v3-encoder.mlmodelc, but next to ggml-large.bin the encoder
    // must be ggml-large-encoder.mlmodelc. Rename when they differ.
    let extracted_name = remote_zip.strip_suffix(".zip").unwrap_or(mlmodelc_name);
    if extracted_name != mlmodelc_name && !mlmodelc_path.exists() {
        let extracted_path = model_dir.join(extracted_name);
        if extracted_path.exists() {
            std::fs::rename(&extracted_path, &mlmodelc_path)
                .context("failed to rename extracted Core ML encoder")?;
        }
    }
    if !mlmodelc_path.exists() {
        sp.stop("Core ML encoder missing after unzip");
        anyhow::bail!(
            "expected {} after extracting {}, but it was not created",
            mlmodelc_path.display(),
            archive_path.display()
        );
    }

    sp.stop(format!(
        "Core ML encoder ready at {}",
        mlmodelc_path.display()
    ));
    let _ = std::fs::remove_file(&archive_path);
    Ok(())
}

/// No-op on non–Apple-Silicon targets so the call site stays clean.
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn ensure_coreml_encoder(_whisper_size: &str, _model_dir: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Auto-detect Ollama models and let the user pick one.
/// Falls back to manual text input if Ollama is not running.
fn pick_ollama_model(default: &str) -> Result<String> {
    let sp = spinner();
    sp.start("Checking for Ollama models...");

    let models = rekody_llm::list_ollama_models();

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
        let size_str = rekody_llm::format_model_size(m.size);
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
#[allow(dead_code)]
fn has_any_provider(config: &crate::RekodyConfig) -> bool {
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

/// Verify every installed model artifact against its pinned checksum.
///
/// Returns the names of files that are present but do NOT match, so callers
/// can tell "not installed" (empty, nothing to check) apart from "installed
/// and wrong". Artifacts that predate the move to Rekody's own model repo
/// carry no pin guarantee, and presence alone was previously enough to skip
/// verification forever; this is what lets `rekody doctor` surface that.
///
/// Hashing the streaming encoder takes a couple of seconds, so this belongs
/// in user-initiated commands (setup, doctor), never on the dictation path.
pub fn unverified_installed_models() -> Vec<String> {
    let mut bad = Vec::new();
    let root = resolve_model_dir();

    // The streaming model only ships where the nemotron feature is built
    // (the Intel release drops it, since ort has no x86_64-darwin prebuilt).
    #[cfg(feature = "nemotron")]
    {
        let nemotron_dir = root.join("nemotron-en-int8");
        for &file in NEMOTRON_FILES {
            let path = nemotron_dir.join(file);
            if path.exists()
                && !verify_model_checksum(path.to_str().unwrap_or(""), expected_checksum_for(file))
            {
                bad.push(file.to_string());
            }
        }
    }

    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("ggml-") || !name.ends_with(".bin") {
            continue;
        }
        let expected = expected_checksum_for(&name);
        // Unknown filenames have no pin to check against; skip rather than
        // reporting a file we never published as a mismatch.
        if expected.is_empty() {
            continue;
        }
        if !verify_model_checksum(entry.path().to_str().unwrap_or(""), expected) {
            bad.push(name);
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Setup's model offer, per architecture (#141).
    ///
    /// The Apple Silicon half is a neutrality guard, not a spec: it pins the
    /// prompt, the five rows, their order, their hints, and the preselection
    /// to exactly what shipped in 0.5.29, so a change to the Intel offer
    /// cannot quietly move what Apple Silicon users see.
    mod whisper_size_offer_tests {
        use super::*;

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        #[test]
        fn apple_silicon_offer_is_unchanged_character_for_character() {
            let (prompt, items) = whisper_size_offer();
            assert_eq!(prompt, "Choose local Whisper model size");
            assert_eq!(
                items,
                [
                    (
                        "turbo",
                        "Turbo (574 MB)",
                        "fast + near-large accuracy (recommended)"
                    ),
                    ("tiny", "Tiny (75 MB)", "fastest — basic accuracy"),
                    ("small", "Small (488 MB)", "balanced"),
                    ("medium", "Medium (1.5 GB)", "better accuracy"),
                    ("large", "Large (3.1 GB)", "best accuracy"),
                ]
            );
            assert_eq!(
                crate::DEFAULT_WHISPER_MODEL,
                "turbo",
                "Apple Silicon still preselects Turbo"
            );
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        #[test]
        fn without_a_neural_engine_the_offer_leads_with_small_and_stops_calling_turbo_fast() {
            let (prompt, items) = whisper_size_offer();
            assert!(
                prompt.contains("no Neural Engine"),
                "the prompt must say why the recommendation differs: {prompt}"
            );
            assert_eq!(items[0].0, "small", "small leads the list");
            assert_eq!(crate::DEFAULT_WHISPER_MODEL, "small");

            let turbo = items
                .iter()
                .find(|(value, _, _)| *value == "turbo")
                .expect("turbo stays available as the accuracy ceiling");
            let hint = turbo.2.to_lowercase();
            assert!(
                !hint.contains("fast") && !hint.contains("recommended"),
                "turbo must not be sold as fast or recommended where the \
                 encoder runs on the CPU: {hint}"
            );
        }

        /// Whatever the architecture, the preselected value has to be a real
        /// row in the list, a size the engine can parse, and a file Setup can
        /// actually fetch and checksum.
        #[test]
        fn the_preselected_size_is_offered_downloadable_and_pinned() {
            let (_, items) = whisper_size_offer();
            assert!(
                items
                    .iter()
                    .any(|(value, _, _)| *value == crate::DEFAULT_WHISPER_MODEL),
                "the preselection must be one of the offered rows"
            );
            let (local, _remote) = whisper_download_spec(crate::DEFAULT_WHISPER_MODEL);
            assert_eq!(
                expected_checksum_for(local).len(),
                64,
                "{local} must carry a pinned checksum"
            );
        }

        /// Every row still resolves, so reordering or relabelling cannot
        /// smuggle in a size with no artifact behind it.
        #[test]
        fn every_offered_row_resolves_to_a_pinned_artifact() {
            let (_, items) = whisper_size_offer();
            let mut seen: Vec<&str> = Vec::new();
            for (value, label, hint) in items {
                assert!(!seen.contains(&value), "{value} listed twice");
                seen.push(value);
                assert!(!label.is_empty() && !hint.is_empty());
                let (local, _remote) = whisper_download_spec(value);
                assert_eq!(expected_checksum_for(local).len(), 64, "{local}");
            }
            assert_eq!(seen.len(), 5, "all five sizes stay on offer");
        }

        /// The predicate that picks the default is the same one that decides
        /// whether a Core ML encoder gets installed. If those ever disagree,
        /// a machine could be steered to Turbo with no Neural Engine to run
        /// it on, which is the bug.
        #[test]
        fn the_default_tracks_the_core_ml_predicate() {
            assert_eq!(
                crate::HAS_NEURAL_ENGINE,
                cfg!(all(target_os = "macos", target_arch = "aarch64"))
            );
            assert_eq!(
                crate::DEFAULT_WHISPER_MODEL == "turbo",
                crate::HAS_NEURAL_ENGINE,
                "turbo is the default exactly where the encoder can run on the ANE"
            );
        }
    }

    /// Every size the wizard offers must resolve to a local file with a
    /// pinned, well-formed SHA-256 — an empty hash would silently skip
    /// verification.
    #[test]
    fn every_whisper_size_has_a_pinned_checksum() {
        for size in ["tiny", "small", "medium", "turbo", "large"] {
            let (local, _remote) = whisper_download_spec(size);
            let sha = expected_checksum_for(local);
            assert_eq!(sha.len(), 64, "{local} checksum must be 64 hex chars");
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit()),
                "{local} checksum must be hex"
            );
        }
    }

    /// The "large" model is a save-as: local ggml-large.bin holds upstream
    /// ggml-large-v3.bin content, so its pinned hash is the v3 hash.
    #[test]
    fn large_maps_local_name_to_v3_content() {
        let (local, remote) = whisper_download_spec("large");
        assert_eq!(local, "ggml-large.bin");
        assert_eq!(remote, "ggml-large-v3.bin");
        assert_eq!(
            expected_checksum_for("ggml-large.bin"),
            "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2"
        );
    }

    #[test]
    fn unknown_size_falls_back_to_tiny() {
        assert_eq!(
            whisper_download_spec("bogus"),
            ("ggml-tiny.bin", "ggml-tiny.bin")
        );
    }

    #[test]
    fn unknown_file_has_no_checksum() {
        assert_eq!(expected_checksum_for("ggml-nonexistent.bin"), "");
    }

    #[test]
    fn asset_urls_prefer_rekody_mirror_then_upstream() {
        let (primary, fallback) = whisper_asset_urls("ggml-tiny.bin");
        assert_eq!(
            primary,
            "https://huggingface.co/Rekody/rekody-models/resolve/main/whisper/ggml-tiny.bin"
        );
        assert_eq!(
            fallback,
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin"
        );
    }

    /// The streaming engine downloads only from the Rekody model repo.
    #[cfg(feature = "nemotron")]
    #[test]
    fn nemotron_urls_use_the_rekody_repo() {
        let url = format!("{REKODY_NEMOTRON_BASE}/encoder.onnx");
        assert_eq!(
            url,
            "https://huggingface.co/Rekody/rekody-streaming-en-0.6b-160ms-int8/resolve/main/encoder.onnx"
        );
    }

    /// Every streaming artifact must carry a real pin for the Rekody-published
    /// model, the engine's sole download source.
    #[cfg(feature = "nemotron")]
    #[test]
    fn every_nemotron_file_has_a_pinned_checksum() {
        for &file in NEMOTRON_FILES {
            let sha = expected_checksum_for(file);
            assert_eq!(sha.len(), 64, "{file} pin must be 64 hex chars");
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit()),
                "{file} pin must be hex"
            );
        }
    }

    /// Every published Core ML encoder archive must have a pinned checksum
    /// keyed by its LOCAL zip name (`<mlmodelc_dir>.zip`).
    #[test]
    fn every_coreml_archive_has_a_pinned_checksum() {
        for size in ["tiny", "small", "medium", "turbo", "large"] {
            let (mlmodelc_name, remote_zip) =
                coreml_archive_for(size).expect("encoder published for size");
            assert!(remote_zip.ends_with(".zip"), "{remote_zip} must be a zip");
            let local_zip = format!("{mlmodelc_name}.zip");
            let sha = expected_checksum_for(&local_zip);
            assert_eq!(sha.len(), 64, "{local_zip} checksum must be 64 hex chars");
        }
    }

    /// The large encoder is also a save-as: the remote v3 archive lands in
    /// the local ggml-large-encoder.mlmodelc.zip slot.
    #[test]
    fn large_coreml_archive_is_v3_saved_as() {
        let (local_dir, remote_zip) = coreml_archive_for("large").unwrap();
        assert_eq!(local_dir, "ggml-large-encoder.mlmodelc");
        assert_eq!(remote_zip, "ggml-large-v3-encoder.mlmodelc.zip");
        assert_eq!(
            expected_checksum_for("ggml-large-encoder.mlmodelc.zip"),
            "47837be7594a29429ec08620043390c4d6d467f8bd362df09e9390ace76a55a4"
        );
    }

    /// Empty expected hash means "skip" (passes); a pinned hash is enforced:
    /// matching passes (case-insensitively), mismatching fails.
    #[test]
    fn checksum_verification_enforces_non_empty_hashes() {
        // SHA-256 of the empty file, a well-known constant.
        const EMPTY_SHA256: &str =
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let path = file.path().to_str().unwrap();

        assert!(verify_model_checksum(path, ""), "empty hash must skip");
        assert!(verify_model_checksum(path, EMPTY_SHA256), "match must pass");
        assert!(
            verify_model_checksum(path, &EMPTY_SHA256.to_uppercase()),
            "hash comparison must be case-insensitive"
        );
        assert!(
            !verify_model_checksum(
                path,
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
            "mismatch must fail"
        );
    }

    /// The Groq STT step reuses the LLM step's key ONLY when that provider is
    /// itself Groq. The regression: Anthropic LLM + Groq STT wrote the
    /// Anthropic key to groq_api_key; keyless providers left it empty.
    #[test]
    fn groq_stt_reuses_llm_key_only_when_llm_provider_is_groq() {
        assert!(can_reuse_llm_key_for_groq_stt("groq", "groq", "gsk_abc"));
        assert!(!can_reuse_llm_key_for_groq_stt(
            "groq",
            "anthropic",
            "sk-ant"
        ));
        assert!(!can_reuse_llm_key_for_groq_stt("groq", "openai", "sk-xyz"));
        assert!(
            !can_reuse_llm_key_for_groq_stt("groq", "none", ""),
            "keyless LLM choice must trigger a Groq prompt, not an empty key"
        );
        assert!(
            !can_reuse_llm_key_for_groq_stt("groq", "groq", ""),
            "an empty Groq LLM key is not reusable"
        );
        assert!(
            !can_reuse_llm_key_for_groq_stt("local", "groq", "gsk_abc"),
            "non-Groq STT never writes groq_api_key from this path"
        );
    }

    /// A masked key must never show the front of the secret. It used to show
    /// the first four characters as well as the last four, which is eight
    /// characters of a live key on screen for no benefit.
    #[test]
    fn masking_reveals_only_the_tail() {
        let masked = mask_secret("gsk_liveSecretValue1234");
        assert!(masked.ends_with("1234"));
        assert!(
            !masked.contains("gsk_"),
            "the prefix must not survive: {masked}"
        );
        assert!(
            !masked.contains("liveSecret"),
            "leaked the middle: {masked}"
        );
        // Short values reveal nothing at all.
        assert_eq!(mask_secret("abcd"), "████");
        assert_eq!(mask_secret(""), "████");
        // Multibyte input must not panic on a slice boundary.
        assert!(mask_secret("kéy-wíth-ünicode").ends_with("code"));
    }

    /// The wizard writes config lines by hand. A value a user typed can
    /// contain a quote, and an unescaped one produces a config file the
    /// daemon refuses to parse: setup appearing to have broken Rekody.
    #[test]
    fn wizard_written_values_survive_a_toml_round_trip() {
        for value in [
            "whisper-1",
            "whis\"per-1",
            "back\\slash",
            "https://api.example.com/v1",
            "tab\there",
        ] {
            let line = format!("custom_stt_model = {}", toml_string(value));
            let parsed: toml::Value =
                toml::from_str(&line).unwrap_or_else(|e| panic!("{line} did not parse: {e}"));
            assert_eq!(
                parsed["custom_stt_model"].as_str(),
                Some(value),
                "value changed on the way through TOML"
            );
        }
    }

    /// The whole wizard-written config has to parse as a RekodyConfig, with
    /// the selected provider's key landing in the config key its catalog
    /// entry names.
    #[test]
    fn a_custom_provider_config_parses_back_into_rekody_config() {
        let spec = stt_catalog::find("custom").unwrap().key.as_ref().unwrap();
        let config = format!(
            "activation_mode = \"both\"\n\
             whisper_model = \"turbo\"\n\
             vad_threshold = 0.01\n\
             injection_method = \"clipboard\"\n\
             stt_engine = \"custom\"\n\
             {}\n\
             custom_stt_base_url = {}\n\
             custom_stt_model = {}\n",
            format_args!("{} = {}", spec.config_key, toml_string("sk-test")),
            toml_string("https://api.openai.com/v1"),
            toml_string("whis\"per-1"),
        );
        let parsed: crate::RekodyConfig =
            toml::from_str(&config).unwrap_or_else(|e| panic!("wizard config did not parse: {e}"));
        assert_eq!(parsed.stt_engine, "custom");
        assert_eq!(parsed.custom_stt_api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            parsed.custom_stt_base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(parsed.custom_stt_model.as_deref(), Some("whis\"per-1"));
    }

    /// The wizard may only show a "Validating..." spinner (and say "valid")
    /// for providers with a real endpoint check. "custom" has an unknown
    /// base URL, so it must fall through to the honest no-check path.
    #[test]
    fn only_providers_with_real_endpoint_checks_claim_validation() {
        for p in [
            "groq",
            "deepgram",
            "openai",
            "cerebras",
            "anthropic",
            "gemini",
            "together",
            "openrouter",
        ] {
            assert!(provider_has_key_check(p), "{p} has a real endpoint check");
        }
        for p in ["custom", "ollama", "none", "apple"] {
            assert!(
                !provider_has_key_check(p),
                "{p} has no stable endpoint and must not fake validation"
            );
        }
    }
}
