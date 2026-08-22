//! rekody — voice dictation CLI
//!
//! Entrypoint for the rekody binary. Handles subcommand dispatch and the
//! polished inline TUI for the live dictation pipeline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use rekody_core::onboarding;
use rekody_core::{Pipeline, RekodyConfig, TRIGGER_LABEL, load_config};

// ── CLI definition ─────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rekody",
    version = env!("CARGO_PKG_VERSION"),
    about = "Voice dictation — speak, it types",
    disable_help_subcommand = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Enable verbose tracing output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Bypass VAD — capture every frame (use for media-playback transcription)
    #[arg(long)]
    record_all_audio: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run first-time setup or reconfigure
    Setup,
    /// Show or edit current configuration (interactive when run in a terminal)
    Config {
        /// Output redacted JSON instead of rendering — for piping into jq/scripts
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<ConfigCmd>,
    },
    /// Browse dictation history
    History {
        /// Number of entries to display (default: 20)
        #[arg(short, long, default_value = "20")]
        count: usize,
        /// Filter by text content (case-insensitive)
        #[arg(short, long)]
        search: Option<String>,
        /// Filter by app name (e.g. "VS Code", "Terminal")
        #[arg(short, long)]
        app: Option<String>,
        /// Show full transcript text (not truncated)
        #[arg(short, long)]
        full: bool,
        /// Show session statistics summary
        #[arg(long)]
        stats: bool,
        /// Output raw JSON (pipe-friendly)
        #[arg(long)]
        json: bool,
        /// Copy the Nth most-recent entry to the clipboard (1 = latest)
        #[arg(long, value_name = "N")]
        copy: Option<usize>,
        /// Open the interactive history browser (search, copy, navigate)
        #[arg(short, long)]
        interactive: bool,
    },
    /// Check STT and LLM provider connectivity
    Doctor,
    /// Manage API keys stored in the system keychain (interactive when run in a terminal)
    Key {
        #[command(subcommand)]
        action: Option<KeyCmd>,
    },
    /// Check for and install the latest version
    Update {
        /// Only check — don't install
        #[arg(long)]
        check: bool,
    },
    /// Show what changed in recent releases
    Changelog {
        /// List recent releases instead of just the latest
        #[arg(long)]
        all: bool,
    },
    /// Benchmark local Whisper transcription latency (A/B Core ML vs Metal)
    Bench {
        /// Number of measured runs per sample (default: 5)
        #[arg(long, default_value = "5")]
        runs: usize,
        /// Warmup runs before measurement — first warm-up can absorb Core ML
        /// MIL compile or cold-cache costs (default: 2)
        #[arg(long, default_value = "2")]
        warmup: usize,
    },
    /// Pick the active skill that reshapes dictation (interactive when run in a terminal)
    Skill {
        #[command(subcommand)]
        action: Option<SkillCmd>,
    },
    /// Manage your personal vocabulary (instant corrections for your terms)
    Dictionary {
        #[command(subcommand)]
        action: Option<DictionaryCmd>,
    },
    /// Correct the transcript of a recent dictation in the training dataset
    /// (fix mishears while you still remember what you said)
    Fix {
        /// The corrected text. Omit to be prompted (you can dictate it).
        text: Option<String>,
        /// Reach back to the Nth most recent dictation (1 = latest)
        #[arg(short, long, default_value = "1")]
        n: usize,
        /// Play the clip's audio before prompting
        #[arg(short, long)]
        play: bool,
    },
    /// Review your dictation dataset in a local page: fix transcripts,
    /// remove bad clips, and export (or import) a fine-tuning-ready cut
    Review {
        /// Dataset directory (default: $REKODY_TRAINING_DIR, then
        /// ~/.local/share/rekody/training-data)
        // global: `rekody review export --dir X` must work, not just
        // `rekody review --dir X export` — scripts and coding agents write
        // the flag after the subcommand (found by scripts/review-e2e.sh).
        #[arg(long, global = true)]
        dir: Option<std::path::PathBuf>,
        /// Port for the local page; the next nine ports are tried when busy
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,
        /// Exit after this many seconds without page activity (0 = stay up)
        #[arg(long, default_value_t = 0, value_name = "N")]
        auto_exit_secs: u64,
        /// Also listen on the local network so a phone on the same Wi-Fi
        /// can open the page (anyone on the network can edit this dataset)
        #[arg(long)]
        lan: bool,
        #[command(subcommand)]
        action: Option<ReviewCmd>,
    },
    /// Drive the live status region through fake states (UI development aid)
    #[command(hide = true)]
    UiDemo,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the path of the config file
    Path,
}

#[derive(Subcommand)]
enum DictionaryCmd {
    /// Add a term (jargon, a name, a product) — multi-word is fine, no quotes needed
    Add {
        #[arg(required = true, num_args = 1..)]
        term: Vec<String>,
    },
    /// Remove a term
    Remove {
        #[arg(required = true, num_args = 1..)]
        term: Vec<String>,
    },
    /// List all terms (pipe-friendly)
    List,
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// Export reviewed clips as a training-ready cut, without the server.
    /// Prints exactly one stdout line on success: EXPORT_PATH=<absolute path>
    Export {
        /// Include clips dated this day or earlier (default: today)
        #[arg(long, value_name = "YYYY-MM-DD")]
        until: Option<String>,
        /// Copy the audio files into the cut so the folder is self-contained
        #[arg(long)]
        copy_audio: bool,
        /// Write the cut folder into DIR instead of <dataset>/exports
        #[arg(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
        /// Produce one cut-<date>-<hash>.zip instead of a folder, audio
        /// included, ready to carry to another machine
        #[arg(long)]
        zip: bool,
    },
    /// Merge a cut from another machine into this dataset (folder or .zip).
    /// Prints exactly one stdout line on success: IMPORT_SUMMARY=<json>
    Import {
        /// The cut to merge in
        #[arg(value_name = "PATH")]
        source: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum SkillCmd {
    /// Set the active skill by name
    Use {
        /// Skill name (see `rekody skill list`)
        name: String,
    },
    /// Clear the active skill (back to built-in app detection)
    None,
    /// List available skills (pipe-friendly)
    List,
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Save an API key for a provider (prompts securely)
    Set {
        /// Provider: groq, deepgram, anthropic, openai, gemini, cerebras
        provider: String,
    },
    /// Remove a stored API key
    Delete {
        /// Provider name
        provider: String,
    },
}

// ── ASCII banner ─────────────────────────────────────────────────────────────

fn print_ascii_banner() {
    // "rekody" rendered in figlet `nancyj`, gradient teal→blue.
    const ART: &[&str] = &[
        r#"                  dP                      dP          "#,
        r#"                  88                      88          "#,
        r#"88d888b. .d8888b. 88  .dP  .d8888b. .d888b88 dP    dP "#,
        r#"88'  `88 88ooood8 88888"   88'  `88 88'  `88 88    88 "#,
        r#"88       88.  ... 88  `8b. 88.  .88 88.  .88 88.  .88 "#,
        r#"dP       `88888P' dP   `YP `88888P' `88888P8 `8888P88 "#,
        r#"                                                  .88 "#,
        r#"                                              d8888P  "#,
    ];
    // Gradient stays inside the rekody teal family:
    //   top:    #4FB8C5  (lightened brand teal — luminous on dark terminals)
    //   bottom: #20808D  (brand teal, exact)
    // Ends on-brand; anchors the eye on the canonical color.
    const TOP: (u8, u8, u8) = (0x4F, 0xB8, 0xC5);
    const BOT: (u8, u8, u8) = (0x20, 0x80, 0x8D);
    let n = ART.len();
    for (i, line) in ART.iter().enumerate() {
        let t = i as f32 / (n - 1) as f32;
        let r = (TOP.0 as f32 + (BOT.0 as f32 - TOP.0 as f32) * t) as u8;
        let g = (TOP.1 as f32 + (BOT.1 as f32 - TOP.1 as f32) * t) as u8;
        let b = (TOP.2 as f32 + (BOT.2 as f32 - TOP.2 as f32) * t) as u8;
        eprintln!("\x1b[38;2;{r};{g};{b}m{line}\x1b[0m");
    }
    // Tagline in brand teal, exact.
    eprintln!(
        "\x1b[38;2;{};{};{}mvoice dictation for everyone\x1b[0m\n",
        BOT.0, BOT.1, BOT.2
    );
}

// ── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => run_dictation(cli.verbose, cli.record_all_audio).await,
        Some(Cmd::Setup) => cmd_setup(),
        Some(Cmd::Config { json, action }) => cmd_config(json, action),
        Some(Cmd::History {
            count,
            search,
            app,
            full,
            stats,
            json,
            copy,
            interactive,
        }) => cmd_history(count, search, app, full, stats, json, copy, interactive),
        Some(Cmd::Doctor) => cmd_doctor().await,
        Some(Cmd::Key { action }) => cmd_key(action),
        Some(Cmd::Update { check }) => cmd_update(check).await,
        Some(Cmd::Changelog { all }) => cmd_changelog(all).await,
        Some(Cmd::Bench { runs, warmup }) => {
            let cfg = load_config_or_default(&find_config_path());
            rekody_core::bench::run(&cfg, runs, warmup).await
        }
        Some(Cmd::Skill { action }) => cmd_skill(action),
        Some(Cmd::Dictionary { action }) => cmd_dictionary(action),
        Some(Cmd::Fix { text, n, play }) => cmd_fix(text, n, play),
        Some(Cmd::Review {
            dir,
            port,
            no_open,
            auto_exit_secs,
            lan,
            action,
        }) => cmd_review(dir, port, no_open, auto_exit_secs, lan, action),
        Some(Cmd::UiDemo) => cmd_ui_demo(),
    }
}

// ── Subcommand: review ───────────────────────────────────────────────────────

/// Serve the dataset review page (the rekody-review crate) on localhost,
/// or run the headless `export` subcommand.
///
/// stdout is a machine surface here: the library prints exactly one line
/// per mode. Serving prints `REVIEW_URL=http://127.0.0.1:<port>`, which the
/// Mac app parses to find the page; `rekody review export` prints
/// `EXPORT_PATH=<absolute path>` and `rekody review import` prints
/// `IMPORT_SUMMARY=<json>`, so a script or coding agent can run either and
/// read the result. Everything else logs to stderr, so those contracts
/// stay clean.
fn cmd_review(
    dir: Option<std::path::PathBuf>,
    port: u16,
    no_open: bool,
    auto_exit_secs: u64,
    lan: bool,
    action: Option<ReviewCmd>,
) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // Precedence: --dir beats $REKODY_TRAINING_DIR beats the default
    // training-data location (the latter two live in the library).
    let dir = dir.unwrap_or_else(rekody_review::default_dataset_dir);
    match action {
        Some(ReviewCmd::Export {
            until,
            copy_audio,
            out,
            zip,
        }) => rekody_review::export(rekody_review::ExportOptions {
            dir,
            until,
            copy_audio,
            out,
            zip,
        }),
        Some(ReviewCmd::Import { source }) => {
            rekody_review::import(rekody_review::ImportOptions { dir, source })
        }
        None => rekody_review::serve(rekody_review::ReviewOptions {
            dir,
            port,
            open_browser: !no_open,
            auto_exit_secs,
            lan,
        }),
    }
}

/// Correct the transcript label of a recent training pair, while the context
/// is still fresh. With no text argument, prompts interactively — the
/// correction can be dictated.
fn cmd_fix(text: Option<String>, n: usize, play: bool) -> Result<()> {
    use rekody_core::training_data;

    let pair = training_data::nth_recent(n.saturating_sub(1))?;
    let mins = (pair.duration / 60.0) as u64;
    let secs = pair.duration % 60.0;
    println!();
    println!(
        "  {BRAND}╭─{RESET}  {BRAND_LIGHT}{BOLD}rekody fix{RESET}  {DIM}{} · {}m{:.0}s{}{RESET}",
        pair.timestamp,
        mins,
        secs,
        if pair.corrected {
            " · already corrected"
        } else {
            ""
        },
    );
    println!("  {BRAND}│{RESET}");
    println!("  {BRAND}│{RESET}   {DIM}heard:{RESET}");
    for chunk in wrap_plain(&pair.text, 76) {
        println!("  {BRAND}│{RESET}   {CREAM}{chunk}{RESET}");
    }
    println!("  {BRAND}│{RESET}");

    if play {
        #[cfg(target_os = "macos")]
        {
            println!("  {BRAND}│{RESET}   {DIM}playing clip…{RESET}");
            let status = std::process::Command::new("afplay")
                .arg(&pair.audio_path)
                .status();
            if !status.map(|s| s.success()).unwrap_or(false) {
                println!(
                    "  {BRAND}│{RESET}   {WARN}couldn't play {}{RESET}",
                    pair.audio_path.display()
                );
            }
        }
        // Clip playback isn't wired up off macOS yet — say so plainly rather
        // than shelling out to a player that isn't there. The transcript is
        // already shown above, which is what `fix` actually needs.
        #[cfg(not(target_os = "macos"))]
        println!(
            "  {BRAND}│{RESET}   {DIM}clip playback isn't supported on this platform yet — \
             the clip is at {}{RESET}",
            pair.audio_path.display()
        );
    }

    let new_text = match text {
        Some(t) => t,
        None => {
            println!(
                "  {BRAND}╰─{RESET}  {DIM}type or dictate the corrected text (empty = abort){RESET}"
            );
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            buf
        }
    };
    if new_text.trim().is_empty() {
        println!("  {DIM}unchanged.{RESET}");
        return Ok(());
    }
    training_data::correct_text(&pair, &new_text)?;
    println!(
        "  {OK}{BOLD}✓{RESET}  {CREAM}label corrected{RESET}  {DIM}→ {}{RESET}",
        new_text.trim()
    );
    Ok(())
}

/// Plain word-wrap for terminal output (no ANSI awareness needed).
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let cur = lines.last_mut().unwrap();
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            lines.push(word.to_string());
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    lines
}

/// Hidden dev aid: loop the live status region through idle → recording
/// (waveform + growing partials) → done with synthetic data, so UI changes
/// can be eyeballed and screenshotted without a mic or hotkey.
fn cmd_ui_demo() -> Result<()> {
    let config = load_config_or_default(&find_config_path());
    let ui = Arc::new(Ui::new(&config));
    // Long enough that the wrapped preview overflows PREVIEW_LINES and the
    // auto-scroll (older lines sliding off the top) is visible in the demo.
    let words: Vec<&str> = "Fixed the race in the audio tap and pushed the branch for review, \
        then started drafting the release notes for the streaming engine. The new preview \
        wraps across multiple lines as you talk, and once it grows past five lines the \
        oldest words quietly scroll away off the top, just like dictating on your phone. \
        The current line stays bright with a cursor while everything above it dims, so \
        your eye always lands on the words that just arrived, and the final text still \
        lands at your cursor the instant you release the key."
        .split_whitespace()
        .collect();
    let sleep = |ms: u64| std::thread::sleep(std::time::Duration::from_millis(ms));
    loop {
        ui.idle();
        sleep(4000);
        ui.start_recording();
        let mut partial = String::new();
        for (i, word) in words.iter().enumerate() {
            // Synthetic speech-shaped mic levels.
            let rms = 0.018 + 0.16 * ((i as f32 * 0.9).sin().abs());
            ui.on_mic_level(rms);
            ui.on_mic_level(rms * 0.6);
            partial.push_str(word);
            partial.push(' ');
            ui.on_partial(partial.trim_end());
            sleep(160);
        }
        sleep(2500);
        ui.stop_recording("inserting…");
        sleep(900);
        ui.done_line(format!(
            "  {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}  {OK}●{RESET} {DIM}52ms stt{RESET}",
            words.join(" ")
        ));
        ui.idle();
        sleep(5000);
    }
}

// ── Subcommand: setup ────────────────────────────────────────────────────────

fn cmd_setup() -> Result<()> {
    print_ascii_banner();
    onboarding::run_onboarding()
}

// ── Subcommand: config ───────────────────────────────────────────────────────

fn cmd_config(json: bool, action: Option<ConfigCmd>) -> Result<()> {
    use std::io::IsTerminal;

    let config_path = find_config_path();
    let config = load_config_or_default(&config_path);

    if let Some(ConfigCmd::Path) = action {
        match &config_path {
            Some(p) => println!("{}", p),
            None => println!("{}", style("no config file found").yellow()),
        }
        return Ok(());
    }

    if json {
        let redacted = redact_for_output(config.clone());
        println!("{}", serde_json::to_string_pretty(&redacted)?);
        return Ok(());
    }

    // Bare `rekody config`: TUI when stdout is a terminal, text print when piped.
    if std::io::stdout().is_terminal() {
        let path = config_path.unwrap_or_else(default_config_path);
        rekody_core::config_tui::run(config, path)?;
    } else {
        print_config(&config, &config_path);
    }
    Ok(())
}

/// Strip API keys before serializing for `--json` output.
fn redact_for_output(mut cfg: RekodyConfig) -> RekodyConfig {
    for p in &mut cfg.providers {
        if !p.api_key.is_empty() {
            p.api_key = "[REDACTED]".into();
        }
    }
    if let Some(k) = &cfg.deepgram_api_key
        && !k.is_empty()
    {
        cfg.deepgram_api_key = Some("[REDACTED]".into());
    }
    if let Some(k) = &cfg.groq_api_key
        && !k.is_empty()
    {
        cfg.groq_api_key = Some("[REDACTED]".into());
    }
    if let Some(k) = &cfg.cerebras_api_key
        && !k.is_empty()
    {
        cfg.cerebras_api_key = Some("[REDACTED]".into());
    }
    cfg
}

fn print_config(config: &RekodyConfig, path: &Option<String>) {
    let rule = "─".repeat(48);
    let subtitle = match path {
        Some(p) => p.clone(),
        None => "(no config file — using defaults)".to_string(),
    };

    println!();
    // Top: corner + integrated title.
    println!(
        "  {BRAND}╭─{RESET}  {BRAND_LIGHT}{BOLD}rekody config{RESET}  {DIM}{}{RESET}",
        subtitle
    );
    println!("  {BRAND}│{RESET}");

    // STT section
    let stt_display = stt_display_name(config);
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}STT{RESET}");
    println!(
        "  {BRAND}│{RESET}     {DIM}Engine{RESET}  {CREAM}{BOLD}{}{RESET}",
        stt_display
    );
    if let Some(key) = &config.deepgram_api_key {
        println!(
            "  {BRAND}│{RESET}     {DIM}Key   {RESET}  {DIM}{}{RESET}",
            mask_key(key)
        );
    }
    println!("  {BRAND}│{RESET}");

    // LLM Providers section
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}LLM Providers{RESET}");
    if config.providers.is_empty() {
        // Legacy
        let has_groq = config.groq_api_key.as_ref().is_some_and(|k| !k.is_empty());
        let has_cerebras = config
            .cerebras_api_key
            .as_ref()
            .is_some_and(|k| !k.is_empty());
        if has_groq {
            println!(
                "  {BRAND}│{RESET}     {DIM}1{RESET}  {CREAM}{BOLD}groq{RESET}  {DIM}(legacy key){RESET}"
            );
        }
        if has_cerebras {
            println!(
                "  {BRAND}│{RESET}     {DIM}2{RESET}  {CREAM}{BOLD}cerebras{RESET}  {DIM}(legacy key){RESET}"
            );
        }
        if !has_groq && !has_cerebras {
            println!("  {BRAND}│{RESET}     {WARN}none configured  — run: rekody setup{RESET}");
        }
    } else {
        for (i, p) in config.providers.iter().enumerate() {
            let key_hint = if p.api_key.is_empty() {
                format!("{WARN}(no key){RESET}")
            } else {
                format!("{DIM}{}{RESET}", mask_key(&p.api_key))
            };
            println!(
                "  {BRAND}│{RESET}     {DIM}{}{RESET}  {CREAM}{BOLD}{}/{}{RESET}  {}",
                i + 1,
                p.name,
                p.model,
                key_hint,
            );
        }
    }
    println!("  {BRAND}│{RESET}");

    // Options section
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}Options{RESET}");
    println!(
        "  {BRAND}│{RESET}     {DIM}Mode  {RESET}  {CREAM}{BOLD}{}{RESET}",
        format_activation_mode(&config.activation_mode)
    );
    println!(
        "  {BRAND}│{RESET}     {DIM}Inject{RESET}  {CREAM}{BOLD}{}{RESET}",
        config.injection_method
    );
    println!(
        "  {BRAND}│{RESET}     {DIM}VAD   {RESET}  {CREAM}{BOLD}{}{RESET}",
        config.vad_threshold
    );
    let vad_mode = if config.record_all_audio {
        "off (record_all_audio = true — every frame captured, no gating)"
    } else {
        "on (RMS gating; pass --record-all-audio to bypass for one session)"
    };
    println!(
        "  {BRAND}│{RESET}     {DIM}Gate  {RESET}  {CREAM}{}{RESET}",
        vad_mode
    );
    println!("  {BRAND}│{RESET}");
    // Bottom: corner + rule, closing the card.
    println!("  {BRAND}╰{}{RESET}", rule);
    println!();
}

/// Mask an API key, showing only the last 4 characters.
fn mask_key(key: &str) -> String {
    if key.len() <= 4 {
        return "████".to_string();
    }
    let visible = &key[key.len() - 4..];
    format!("████████████{}", visible)
}

// ── Subcommand: history ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn cmd_history(
    count: usize,
    search: Option<String>,
    app_filter: Option<String>,
    full: bool,
    stats: bool,
    json_out: bool,
    copy_nth: Option<usize>,
    interactive: bool,
) -> Result<()> {
    let history = rekody_core::history::History::load();
    let all = history.entries();

    if interactive {
        return rekody_core::history_tui::run(&history);
    }

    // --copy N: copy the Nth most-recent entry to clipboard and exit.
    if let Some(n) = copy_nth {
        let n = n.max(1);
        let entry = all.iter().rev().nth(n - 1);
        match entry {
            Some(e) => {
                let mut clipboard = arboard::Clipboard::new()?;
                clipboard.set_text(&e.text)?;
                println!(
                    "  {}  Copied entry #{} to clipboard",
                    style("✓").green().bold(),
                    n
                );
                println!("     {}", style(&e.text).dim());
            }
            None => {
                println!(
                    "  {}  No entry #{} in history ({} total)",
                    style("✗").red().bold(),
                    n,
                    all.len()
                );
            }
        }
        return Ok(());
    }

    // Apply filters
    let filtered: Vec<_> = all
        .iter()
        .filter(|e| {
            if let Some(ref q) = search {
                let q = q.to_lowercase();
                if !e.text.to_lowercase().contains(&q)
                    && !e.raw_transcript.to_lowercase().contains(&q)
                {
                    return false;
                }
            }
            if let Some(ref app) = app_filter
                && !e.app_context.to_lowercase().contains(&app.to_lowercase())
            {
                return false;
            }
            true
        })
        .collect();

    let shown: Vec<_> = filtered.iter().rev().take(count).collect();

    // JSON output (pipe-friendly)
    if json_out {
        println!("{}", serde_json::to_string_pretty(&shown)?);
        return Ok(());
    }

    println!();

    // Stats view
    if stats || shown.is_empty() {
        let total = all.len();
        let total_filtered = filtered.len();
        let avg_stt = if total > 0 {
            all.iter().map(|e| e.stt_latency_ms).sum::<u64>() / total as u64
        } else {
            0
        };
        let avg_llm = {
            let llm_entries: Vec<_> = all.iter().filter_map(|e| e.llm_latency_ms).collect();
            if llm_entries.is_empty() {
                None
            } else {
                Some(llm_entries.iter().sum::<u64>() / llm_entries.len() as u64)
            }
        };

        // App breakdown
        let mut app_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for e in all {
            *app_counts.entry(e.app_context.as_str()).or_insert(0) += 1;
        }
        let mut app_sorted: Vec<_> = app_counts.iter().collect();
        app_sorted.sort_by(|a, b| b.1.cmp(a.1));

        println!(
            "  {}  {}",
            style("History Stats").bold(),
            style("─".repeat(38)).dim()
        );
        println!();
        println!(
            "  {}  {}",
            style("Total dictations  ").dim(),
            style(total).white().bold()
        );
        if total_filtered != total {
            println!(
                "  {}  {}",
                style("Matching filter   ").dim(),
                style(total_filtered).white()
            );
        }
        println!(
            "  {}  {}",
            style("Avg STT latency   ").dim(),
            style(format!("{}ms", avg_stt)).white()
        );
        if let Some(llm) = avg_llm {
            println!(
                "  {}  {}",
                style("Avg LLM latency   ").dim(),
                style(format!("{}ms", llm)).white()
            );
        }
        println!();
        if !app_sorted.is_empty() {
            println!("  {}", style("Top apps").bold());
            for (app, count) in app_sorted.iter().take(5) {
                println!(
                    "    {}  {}",
                    style(format!("{:<28}", app)).white(),
                    style(format!("{} dictations", count)).dim()
                );
            }
            println!();
        }

        if shown.is_empty() {
            if search.is_some() || app_filter.is_some() {
                println!("  {}", style("No entries match the filter.").yellow());
            } else {
                println!("  {}", style("No history yet — start dictating!").dim());
            }
            println!();
            return Ok(());
        }
        println!(
            "  {}  {}",
            style("Recent").bold(),
            style("─".repeat(45)).dim()
        );
        println!();
    } else {
        // Header
        let mut header = format!(
            "  {}  {}",
            style("History").bold(),
            style("─".repeat(40)).dim()
        );
        if let Some(ref q) = search {
            header = format!(
                "  {}  {}  {}",
                style("History").bold(),
                style("─".repeat(30)).dim(),
                style(format!("search: \"{}\"", q)).cyan()
            );
        } else if let Some(ref app) = app_filter {
            header = format!(
                "  {}  {}  {}",
                style("History").bold(),
                style("─".repeat(30)).dim(),
                style(format!("app: \"{}\"", app)).cyan()
            );
        }
        println!("{}", header);
        println!();
    }

    // Entry listing grouped by date
    let mut last_date = String::new();
    for entry in &shown {
        let date = entry.timestamp.get(..10).unwrap_or("").to_string();
        if date != last_date {
            if !last_date.is_empty() {
                println!();
            }
            println!("  {}", style(&date).bold().underlined());
            last_date = date;
        }

        let time = entry.timestamp.get(11..16).unwrap_or("--:--");
        let latency = match entry.llm_latency_ms {
            Some(llm) => format!("STT {}ms  LLM {}ms", entry.stt_latency_ms, llm),
            None => format!("STT {}ms", entry.stt_latency_ms),
        };
        let app_col = if entry.app_context.len() > 18 {
            format!("{}…", &entry.app_context[..17])
        } else {
            entry.app_context.clone()
        };

        if full {
            // Full transcript — show raw + formatted on separate lines
            println!(
                "  {}  {}  {}",
                style(time).dim(),
                style(format!("{:<20}", app_col)).dim(),
                style(&latency).dim()
            );
            println!("     {}", style(&entry.text).white());
            if entry.raw_transcript != entry.text {
                println!(
                    "     {}  {}",
                    style("raw:").dim(),
                    style(&entry.raw_transcript).dim()
                );
            }
        } else {
            let max_text = 58;
            let text = if entry.text.len() > max_text {
                format!("{}…", &entry.text[..max_text - 1])
            } else {
                entry.text.clone()
            };
            println!(
                "  {}  {}  {}  {}",
                style(time).dim(),
                style(format!("{:<58}", text)).white(),
                style(format!("{:<20}", app_col)).dim(),
                style(&latency).dim(),
            );
        }
    }
    println!();
    println!(
        "  {}",
        style(format!(
            "Showing {} of {} entries{}",
            shown.len(),
            filtered.len(),
            if shown.len() < filtered.len() {
                format!("  —  use -c {} for more", filtered.len())
            } else {
                String::new()
            }
        ))
        .dim()
    );
    println!();
    Ok(())
}

// ── Subcommand: doctor ───────────────────────────────────────────────────────

async fn cmd_doctor() -> Result<()> {
    let config_path = find_config_path();
    let config = load_config_or_default(&config_path);
    let rule = "─".repeat(48);

    println!();
    // Top: corner + integrated title.
    println!(
        "  {BRAND}╭─{RESET}  {BRAND_LIGHT}{BOLD}rekody doctor{RESET}  {DIM}provider health check{RESET}"
    );
    println!("  {BRAND}│{RESET}");

    // STT check
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}STT{RESET}");
    let stt_name = stt_display_name(&config);
    match config.stt_engine.to_lowercase().as_str() {
        "deepgram" => {
            let key = config.deepgram_api_key.as_deref().unwrap_or("");
            if key.is_empty() {
                println!(
                    "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}no API key — run: rekody key set deepgram{RESET}",
                    stt_name
                );
            } else {
                println!(
                    "  {BRAND}│{RESET}     {WARN}{BOLD}…{RESET}  {CREAM}{}{RESET}  {DIM}checking…{RESET}",
                    stt_name
                );
                let t = Instant::now();
                let ok = reqwest::Client::new()
                    .get("https://api.deepgram.com/v1/projects")
                    .header("Authorization", format!("Token {}", key))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                let ms = t.elapsed().as_millis();
                // Overwrite the previous line with the final result.
                print!("\x1b[1A\x1b[2K");
                if ok {
                    println!(
                        "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}  {DIM}{}ms{RESET}",
                        stt_name, ms
                    );
                } else {
                    println!(
                        "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}auth failed — run: rekody key set deepgram{RESET}",
                        stt_name
                    );
                }
            }
        }
        "groq" => {
            let key = config.groq_api_key.as_deref().unwrap_or("");
            check_openai_compat_provider(
                "Groq Whisper",
                "https://api.groq.com/openai/v1/models",
                key,
            )
            .await;
        }
        "nemotron" => {
            let model_dir = rekody_models_dir().join("nemotron-en-int8");
            let missing: Vec<&str> = ["encoder.onnx", "decoder_joint.onnx", "tokenizer.model"]
                .into_iter()
                .filter(|f| !model_dir.join(f).exists())
                .collect();
            if missing.is_empty() {
                println!(
                    "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}  {DIM}model files present{RESET}",
                    stt_name
                );
            } else {
                println!(
                    "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}missing {} — re-run: rekody setup{RESET}",
                    stt_name,
                    missing.join(", ")
                );
            }
        }
        _ => {
            // Local Whisper needs no network, but it does need the GGML file
            // on disk. Without this probe a missing model (e.g. a hand edit
            // to stt_engine = "local") only surfaces on the first dictation.
            let ggml_file = whisper_ggml_file(&config.whisper_model);
            if rekody_models_dir().join(ggml_file).exists() {
                println!(
                    "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}  {DIM}model present ({}){RESET}",
                    stt_name, ggml_file
                );
            } else {
                println!(
                    "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}missing {} · re-run: rekody setup to download it{RESET}",
                    stt_name, ggml_file
                );
            }
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                let (label, ok) = coreml_status(&config.whisper_model);
                if ok {
                    println!(
                        "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {DIM}Core ML encoder:{RESET}  {CREAM}{}{RESET}",
                        label
                    );
                } else {
                    println!(
                        "  {BRAND}│{RESET}     {WARN}{BOLD}!{RESET}  {DIM}Core ML encoder:{RESET}  {WARN}{}{RESET}",
                        label
                    );
                }
            }
        }
    }
    println!("  {BRAND}│{RESET}");

    // LLM providers
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}LLM{RESET}");
    if config.providers.is_empty()
        && config.groq_api_key.is_none()
        && config.cerebras_api_key.is_none()
    {
        println!("  {BRAND}│{RESET}     {WARN}none configured — run: rekody setup{RESET}");
    } else if !config.providers.is_empty() {
        for p in &config.providers {
            match p.name.as_str() {
                "ollama" | "lm-studio" | "vllm" => {
                    let url = p.base_url.as_deref().unwrap_or("http://localhost:11434");
                    let t = Instant::now();
                    let ok = reqwest::Client::new().get(url).send().await.is_ok();
                    let ms = t.elapsed().as_millis();
                    if ok {
                        println!(
                            "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}/{RESET}{DIM}{}{RESET}  {DIM}{}ms{RESET}",
                            p.name, p.model, ms
                        );
                    } else {
                        println!(
                            "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}/{RESET}{DIM}{}{RESET}  {WARN}not running{RESET}",
                            p.name, p.model
                        );
                    }
                }
                "apple" | "apple-foundation" | "foundation" => {
                    let provider = rekody_llm::AppleFoundationProvider::new();
                    match provider.check().await {
                        Ok(()) => println!(
                            "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}Apple on-device{RESET}  {DIM}Foundation Models ready{RESET}"
                        ),
                        Err(e) => println!(
                            "  {BRAND}│{RESET}     {WARN}{BOLD}!{RESET}  {CREAM}Apple on-device{RESET}  {WARN}{}{RESET}",
                            e
                        ),
                    }
                }
                "gemini" => {
                    let url = "https://generativelanguage.googleapis.com/v1beta/openai/models";
                    check_openai_compat_provider_keyed(
                        &format!("{}/{}", p.name, p.model),
                        url,
                        &p.api_key,
                        "x-goog-api-key",
                    )
                    .await;
                }
                _ => {
                    let url = p
                        .base_url
                        .clone()
                        .unwrap_or_else(|| provider_models_url(&p.name));
                    check_openai_compat_provider(
                        &format!("{}/{}", p.name, p.model),
                        &url,
                        &p.api_key,
                    )
                    .await;
                }
            }
        }
    } else {
        // Legacy
        if let Some(key) = &config.groq_api_key {
            check_openai_compat_provider("groq", "https://api.groq.com/openai/v1/models", key)
                .await;
        }
        if let Some(key) = &config.cerebras_api_key {
            check_openai_compat_provider("cerebras", "https://api.cerebras.ai/v1/models", key)
                .await;
        }
    }
    println!("  {BRAND}│{RESET}");

    // System
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}System{RESET}");
    #[cfg(target_os = "macos")]
    {
        let mic = check_macos_permission("kTCCServiceMicrophone");
        let acc = check_macos_permission("kTCCServiceAccessibility");
        print_permission("Microphone", mic);
        print_permission("Accessibility", acc);
    }
    print_input_device(&config);
    #[cfg(not(target_os = "macos"))]
    {
        println!(
            "  {BRAND}│{RESET}     {BRAND_LIGHT}○{RESET}  {DIM}System checks not available on this platform{RESET}"
        );
    }
    println!("  {BRAND}│{RESET}");

    // Model integrity: artifacts installed before the move to Rekody's own
    // model repo carry no checksum guarantee, and setup used to skip
    // verification whenever a file already existed. Surface that here so a
    // stale third-party artifact cannot sit unnoticed forever.
    let unverified = rekody_core::onboarding::unverified_installed_models();
    if unverified.is_empty() {
        println!(
            "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}Model files{RESET}  {DIM}match their published checksums{RESET}"
        );
    } else {
        println!(
            "  {BRAND}│{RESET}     {WARN}{BOLD}…{RESET}  {CREAM}Model files{RESET}  {WARN}{} does not match its published checksum{RESET}",
            unverified.join(", ")
        );
        println!(
            "  {BRAND}│{RESET}        {DIM}run `rekody setup` to replace it with the published build{RESET}"
        );
    }
    println!("  {BRAND}│{RESET}");

    // Training data (local fine-tuning dataset; default-on capture)
    println!("  {BRAND}│{RESET}   {BRAND_LIGHT}{BOLD}Training data{RESET}");
    if config.save_training_data {
        match rekody_core::training_data::stats() {
            Some((clips, bytes)) => println!(
                "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{} clips · {:.1} MB{RESET}  {DIM}{} · save_training_data = false to disable{RESET}",
                clips,
                bytes as f64 / 1e6,
                rekody_core::training_data::dataset_dir().display()
            ),
            None => println!(
                "  {BRAND}│{RESET}     {BRAND_LIGHT}○{RESET}  {DIM}capture on — pairs save to {} after your next dictation{RESET}",
                rekody_core::training_data::dataset_dir().display()
            ),
        }
    } else {
        println!("  {BRAND}│{RESET}     {DIM}off (save_training_data = false){RESET}");
    }
    println!("  {BRAND}│{RESET}");
    // Bottom: corner + rule, closing the card.
    println!("  {BRAND}╰{}{RESET}", rule);
    println!();

    Ok(())
}

async fn check_openai_compat_provider(label: &str, url: &str, key: &str) {
    if key.is_empty() {
        println!(
            "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}no API key — run: rekody key set <provider>{RESET}",
            label
        );
        return;
    }
    let t = Instant::now();
    let ok = reqwest::Client::new()
        .get(url)
        .bearer_auth(key)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let ms = t.elapsed().as_millis();
    if ok {
        println!(
            "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}  {DIM}{}ms{RESET}",
            label, ms
        );
    } else {
        println!(
            "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}auth failed — check your API key{RESET}",
            label
        );
    }
}

async fn check_openai_compat_provider_keyed(label: &str, url: &str, key: &str, header: &str) {
    if key.is_empty() {
        println!(
            "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}no API key{RESET}",
            label
        );
        return;
    }
    let t = Instant::now();
    let ok = reqwest::Client::new()
        .get(url)
        .header(header, key)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let ms = t.elapsed().as_millis();
    if ok {
        println!(
            "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}  {DIM}{}ms{RESET}",
            label, ms
        );
    } else {
        println!(
            "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}auth failed — check your API key{RESET}",
            label
        );
    }
}

fn provider_models_url(name: &str) -> String {
    match name {
        "groq" => "https://api.groq.com/openai/v1/models",
        "cerebras" => "https://api.cerebras.ai/v1/models",
        "openai" => "https://api.openai.com/v1/models",
        "together" => "https://api.together.xyz/v1/models",
        "openrouter" => "https://openrouter.ai/api/v1/models",
        "fireworks" => "https://api.fireworks.ai/inference/v1/models",
        "anthropic" => "https://api.anthropic.com/v1/models",
        _ => "http://localhost:11434/v1/models",
    }
    .to_string()
}

#[cfg(target_os = "macos")]
fn check_macos_permission(service: &str) -> MicCheck {
    if service == "kTCCServiceAccessibility" {
        return if rekody_hotkey::is_accessibility_trusted() {
            MicCheck::Granted
        } else {
            MicCheck::Denied
        };
    }
    // Microphone: probe the default input device via cpal. This fires the
    // macOS TCC prompt on first access and returns Denied synchronously if
    // the user has already blocked access. See rekody_audio::probe_microphone.
    match rekody_audio::probe_microphone() {
        rekody_audio::MicStatus::Granted => MicCheck::Granted,
        rekody_audio::MicStatus::Denied => MicCheck::Denied,
        rekody_audio::MicStatus::NoDevice => MicCheck::NoDevice,
        rekody_audio::MicStatus::Unknown => MicCheck::Unknown,
    }
}

/// Tri-state result for macOS permission checks in `doctor`.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MicCheck {
    Granted,
    Denied,
    NoDevice,
    Unknown,
}

#[cfg(target_os = "macos")]
fn print_permission(name: &str, status: MicCheck) {
    match status {
        MicCheck::Granted => {
            println!(
                "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}{}{RESET}",
                name
            );
        }
        MicCheck::Denied => {
            println!(
                "  {BRAND}│{RESET}     {SLOW}{BOLD}✗{RESET}  {CREAM}{}{RESET}  {WARN}open System Settings → Privacy{RESET}",
                name
            );
        }
        MicCheck::NoDevice => {
            println!(
                "  {BRAND}│{RESET}     {WARN}{BOLD}…{RESET}  {CREAM}{}{RESET}  {DIM}no input device detected{RESET}",
                name
            );
        }
        MicCheck::Unknown => {
            println!(
                "  {BRAND}│{RESET}     {WARN}{BOLD}…{RESET}  {CREAM}{}{RESET}  {DIM}could not probe — try recording to test{RESET}",
                name
            );
        }
    }
}

/// Show which input device rekody will capture from, given the configured
/// `input_device` preference (a single pin or an ordered fallback chain),
/// and warn when nothing configured is connected.
fn print_input_device(config: &RekodyConfig) {
    use rekody_core::InputDevicePref;

    let devices = rekody_audio::list_input_devices();
    let system_default_line = || {
        println!(
            "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}Input device{RESET}  {DIM}system default{RESET}"
        );
    };
    match &config.input_device {
        Some(InputDevicePref::Name(want))
            if !want.trim().is_empty() && !want.eq_ignore_ascii_case("system") =>
        {
            if let Some(idx) = rekody_audio::match_device_name(&devices, want) {
                let found = &devices[idx];
                println!(
                    "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}Input device{RESET}  {DIM}{found} (pinned){RESET}"
                );
            } else {
                println!(
                    "  {BRAND}│{RESET}     {WARN}{BOLD}…{RESET}  {CREAM}Input device{RESET}  {WARN}\"{want}\" not connected, using system default{RESET}"
                );
            }
        }
        Some(InputDevicePref::Chain(chain)) => {
            // Entries that actually name a device; blanks and the "system"
            // sentinel are skipped, matching capture-time resolution.
            let entries: Vec<&str> = chain
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("system"))
                .collect();
            if entries.is_empty() {
                system_default_line();
                return;
            }
            let active = entries
                .iter()
                .position(|want| rekody_audio::match_device_name(&devices, want).is_some());
            let rendered: Vec<String> = entries
                .iter()
                .enumerate()
                .map(|(i, want)| {
                    let n = i + 1;
                    match (
                        active == Some(i),
                        rekody_audio::match_device_name(&devices, want),
                    ) {
                        (true, _) => format!("{CREAM}{n}. {want}{RESET} {OK}(active){RESET}"),
                        (false, Some(_)) => format!("{DIM}{n}. {want} (connected){RESET}"),
                        (false, None) => format!("{DIM}{n}. {want} (not connected){RESET}"),
                    }
                })
                .collect();
            let rendered = rendered.join("  ");
            if active.is_some() {
                println!(
                    "  {BRAND}│{RESET}     {OK}{BOLD}✓{RESET}  {CREAM}Input device{RESET}  {rendered}"
                );
            } else {
                println!(
                    "  {BRAND}│{RESET}     {WARN}{BOLD}…{RESET}  {CREAM}Input device{RESET}  {rendered}  {WARN}none connected, using system default{RESET}"
                );
            }
        }
        _ => system_default_line(),
    }
}

// ── Subcommand: update ───────────────────────────────────────────────────────

fn is_homebrew_install(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/Cellar/") || s.contains("/homebrew/")
}

/// Render a GitHub release-notes body for the terminal: section headers
/// bold, bullets kept, `by @user in <pr-url>` collapsed to `(#N)`.
fn render_release_body(body: &str) {
    // The body comes from the GitHub API — strip control characters so a
    // crafted release can't inject terminal escape sequences.
    let body: String = body
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    for raw in body.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            println!();
            continue;
        }
        // "* feat: x by @user in https://github.com/o/r/pull/41" → "• feat: x (#41)"
        let mut text = line.trim_start().to_string();
        let bullet = text.starts_with("* ") || text.starts_with("- ");
        if bullet {
            text = text[2..].to_string();
        }
        if let Some(idx) = text.find(" by @") {
            let pr = text
                .rsplit("/pull/")
                .next()
                .filter(|n| n.chars().all(|c| c.is_ascii_digit()))
                .map(|n| format!(" (#{n})"))
                .unwrap_or_default();
            text.truncate(idx);
            text.push_str(&pr);
        }
        // **bold** → ANSI bold
        while let Some(start) = text.find("**") {
            match text[start + 2..].find("**") {
                Some(end) => {
                    let inner = text[start + 2..start + 2 + end].to_string();
                    text.replace_range(start..start + 4 + end, &format!("{}", style(inner).bold()));
                }
                None => break,
            }
        }
        if let Some(h) = text.strip_prefix("## ") {
            println!("  {}", style(h).bold());
        } else if let Some(h) = text.strip_prefix("# ") {
            println!("  {}", style(h).bold());
        } else if text.starts_with("**Full Changelog**") {
            // skip the compare-link footer; `rekody changelog --all` covers it
        } else if bullet {
            println!("    {} {}", style("•").cyan(), text);
        } else {
            println!("  {text}");
        }
    }
}

/// Show release notes from GitHub: the latest release by default, or a
/// one-line-per-release list with `--all`.
async fn cmd_changelog(all: bool) -> Result<()> {
    const CURRENT: &str = env!("CARGO_PKG_VERSION");
    const REPO: &str = "rekody/rekody";

    let client = reqwest::Client::builder()
        .user_agent("rekody-changelog")
        .build()?;

    println!();
    println!(
        "  {}  {}",
        style("Changelog").bold(),
        style("─".repeat(42)).dim()
    );

    if all {
        let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=10");
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            println!("  Could not reach GitHub. Check your internet connection.");
            return Ok(());
        }
        let releases: serde_json::Value = resp.json().await?;
        for rel in releases.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let tag = rel["tag_name"].as_str().unwrap_or("?");
            let date = rel["published_at"]
                .as_str()
                .map(|d| &d[..10.min(d.len())])
                .unwrap_or("");
            let name = rel["name"].as_str().unwrap_or("");
            let name = if name == tag || name.is_empty() {
                String::new()
            } else {
                format!("  {name}")
            };
            let marker = if tag.trim_start_matches('v') == CURRENT {
                style(" ← installed").green().to_string()
            } else {
                String::new()
            };
            println!(
                "  {} {}{}{}",
                style(tag).cyan(),
                style(date).dim(),
                name,
                marker
            );
        }
        println!();
        println!(
            "  {}",
            style("rekody changelog — details of the latest release").dim()
        );
        println!();
        return Ok(());
    }

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        println!("  Could not reach GitHub. Check your internet connection.");
        return Ok(());
    }
    let release: serde_json::Value = resp.json().await?;
    let tag = release["tag_name"].as_str().unwrap_or("?");
    let date = release["published_at"]
        .as_str()
        .map(|d| &d[..10.min(d.len())])
        .unwrap_or("");

    let installed = tag.trim_start_matches('v') == CURRENT;
    let status = if installed {
        style("you're on this version").green().to_string()
    } else {
        format!(
            "{} {}",
            style("newer than your").yellow(),
            style(format!("v{CURRENT} — run `rekody update`")).yellow()
        )
    };
    println!(
        "  {} {}  {}",
        style(tag).cyan().bold(),
        style(date).dim(),
        status
    );
    println!();
    render_release_body(release["body"].as_str().unwrap_or("(no notes)"));
    println!();
    println!(
        "  {}",
        style("rekody changelog --all — recent releases").dim()
    );
    println!();
    Ok(())
}

/// Look up the expected SHA-256 for `filename` in a `SHA256SUMS` body.
///
/// Lines follow the `shasum`/`sha256sum` format: `<hex>␠␠<filename>` (two
/// spaces for binary mode, but we tolerate any whitespace and a leading `*`).
/// SHA256SUMS lists every release asset, so we match only the file we
/// downloaded. Returns the lowercased hex digest, or `None` if not listed.
fn sha256sums_lookup(sums: &str, filename: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        // The name field may carry a leading `*` (binary-mode marker).
        let name = parts.next()?.trim_start_matches('*');
        if name == filename {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Compute the SHA-256 of a file by shelling out to `shasum -a 256`
/// (falling back to `sha256sum`), mirroring `install.sh`. Returns the
/// lowercased hex digest, or `None` if neither tool is available or the
/// command fails. We shell out rather than pull in a new crate so the
/// updater matches the install script byte-for-byte and adds no deps.
fn sha256_of_file(path: &std::path::Path) -> Option<String> {
    fn run(cmd: &str, args: &[&str], path: &std::path::Path) -> Option<String> {
        let out = std::process::Command::new(cmd)
            .args(args)
            .arg(path)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let stdout = String::from_utf8(out.stdout).ok()?;
        stdout
            .split_whitespace()
            .next()
            .map(|s| s.to_ascii_lowercase())
    }
    run("shasum", &["-a", "256"], path).or_else(|| run("sha256sum", &[], path))
}

async fn cmd_update(check_only: bool) -> Result<()> {
    const CURRENT: &str = env!("CARGO_PKG_VERSION");
    const REPO: &str = "rekody/rekody";

    println!();
    println!(
        "  {}  {}",
        style("Update").bold(),
        style("─".repeat(42)).dim()
    );
    println!("  Current version: {}", style(format!("v{CURRENT}")).cyan());
    print!("  Checking latest release… ");

    let client = reqwest::Client::builder()
        .user_agent("rekody-updater")
        .build()?;

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client.get(&url).send().await?;

    if !resp.status().is_success() {
        println!("{}", style("failed").red());
        println!("  Could not reach GitHub. Check your internet connection.");
        return Ok(());
    }

    let release: serde_json::Value = resp.json().await?;
    let latest_tag = release["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v');

    println!("{}", style(format!("v{latest_tag}")).cyan());

    if latest_tag == CURRENT {
        println!();
        println!(
            "  {} You're already on the latest version.",
            style("✓").green()
        );
        println!();
        return Ok(());
    }

    // Simple semver comparison (major.minor.patch)
    fn parse_ver(s: &str) -> (u64, u64, u64) {
        let mut parts = s.splitn(3, '.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    }

    if parse_ver(latest_tag) <= parse_ver(CURRENT) {
        println!();
        println!(
            "  {} You're already on the latest version.",
            style("✓").green()
        );
        println!();
        return Ok(());
    }

    println!(
        "  {} v{} → v{}",
        style("Update available:").yellow().bold(),
        CURRENT,
        latest_tag
    );

    // Resolve where the running binary actually lives, following symlinks
    // (e.g. /opt/homebrew/bin/rekody → Cellar/...). Hardcoding
    // /usr/local/bin/rekody silently desynced any install outside that path.
    let install_path = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(e) => {
            println!();
            println!(
                "  {} Could not resolve current binary path: {}",
                style("✗").red(),
                e
            );
            return Ok(());
        }
    };

    let owned_by_brew = is_homebrew_install(&install_path);

    // Homebrew clones the tap locally and only refreshes it on `brew update`
    // (auto-update is rate-limited), so `brew upgrade` alone can see a stale
    // formula and claim the old version is current. Always hint both steps.
    const BREW_UPGRADE: &str = "brew update && brew upgrade rekody";

    if check_only {
        println!();
        let cmd = if owned_by_brew {
            BREW_UPGRADE
        } else {
            "rekody update"
        };
        println!("  Run {} to install it.", style(cmd).cyan());
        println!();
        return Ok(());
    }

    // Defer to the package manager when one owns this install — overwriting
    // its files breaks `brew upgrade` / `brew uninstall` bookkeeping.
    if owned_by_brew {
        println!();
        println!("  {} rekody was installed via Homebrew.", style("ℹ").cyan());
        println!("  Run {} to upgrade.", style(BREW_UPGRADE).cyan());
        println!();
        return Ok(());
    }

    println!();

    // Detect platform/arch
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let platform = match os {
        "macos" => "macos",
        "linux" => "linux",
        other => {
            println!("  {} Unsupported OS: {}", style("✗").red(), other);
            println!("  Download manually from: https://github.com/{REPO}/releases");
            return Ok(());
        }
    };
    let arch_name = match arch {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => {
            println!("  {} Unsupported arch: {}", style("✗").red(), other);
            return Ok(());
        }
    };

    let tarball = format!("rekody-{latest_tag}-{platform}-{arch_name}.tar.gz");
    let download_url =
        format!("https://github.com/{REPO}/releases/download/v{latest_tag}/{tarball}");

    println!("  Downloading {}…", style(&tarball).dim());

    let resp = client.get(&download_url).send().await?;
    if !resp.status().is_success() {
        println!(
            "  {} Download failed (HTTP {}) — try: curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | bash",
            style("✗").red(),
            resp.status()
        );
        return Ok(());
    }
    let bytes = resp.bytes().await?;
    // gzip magic bytes: 1f 8b
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        println!(
            "  {} Download did not return a valid gzip archive — try: curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | bash",
            style("✗").red()
        );
        return Ok(());
    }

    // Stage the download into a temp dir before touching anything on disk.
    let tmp = std::env::temp_dir().join(format!("rekody-update-{latest_tag}"));
    std::fs::create_dir_all(&tmp)?;
    let tarball_path = tmp.join(&tarball);
    std::fs::write(&tarball_path, &bytes)?;

    // Verify the tarball against the published SHA256SUMS BEFORE extracting.
    // A MITM or a compromised mirror could otherwise serve a tampered binary;
    // SECURITY.md scopes "update mechanism integrity" and "tampered tarballs,
    // checksums" as in-scope, and install.sh already verifies this way. We fail
    // closed: every published release ships a SHA256SUMS, so a missing or
    // unmatched checksum means something is wrong and we must not install.
    let sums_url = format!("https://github.com/{REPO}/releases/download/v{latest_tag}/SHA256SUMS");
    print!("  Verifying checksum… ");
    let sums_resp = client.get(&sums_url).send().await?;
    if !sums_resp.status().is_success() {
        println!("{}", style("failed").red());
        println!(
            "  {} Could not fetch SHA256SUMS (HTTP {}) — refusing to install an unverified binary.",
            style("✗").red(),
            sums_resp.status()
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(());
    }
    let sums_text = sums_resp.text().await?;
    let expected = match sha256sums_lookup(&sums_text, &tarball) {
        Some(h) => h,
        None => {
            println!("{}", style("failed").red());
            println!(
                "  {} {} is not listed in SHA256SUMS — refusing to install an unverified binary.",
                style("✗").red(),
                tarball
            );
            let _ = std::fs::remove_dir_all(&tmp);
            return Ok(());
        }
    };

    let actual = match sha256_of_file(&tarball_path) {
        Some(h) => h,
        None => {
            println!("{}", style("failed").red());
            println!(
                "  {} Could not compute SHA-256 (need `shasum` or `sha256sum` on PATH) — refusing to install an unverified binary.",
                style("✗").red()
            );
            let _ = std::fs::remove_dir_all(&tmp);
            return Ok(());
        }
    };

    if !actual.eq_ignore_ascii_case(&expected) {
        println!("{}", style("failed").red());
        println!(
            "  {} Checksum mismatch — aborting for safety. The download may be corrupt or tampered with.",
            style("✗").red()
        );
        println!("    Expected: {expected}");
        println!("    Got:      {actual}");
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(());
    }
    println!("{}", style("ok").green());

    let status = std::process::Command::new("tar")
        .args([
            "-xzf",
            tarball_path.to_str().unwrap(),
            "-C",
            tmp.to_str().unwrap(),
        ])
        .status()?;

    if !status.success() {
        println!("  {} Failed to extract tarball.", style("✗").red());
        return Ok(());
    }

    let new_bin = tmp.join("rekody");

    // Atomic replace: stage next to the target, then rename over it. On POSIX
    // rename() swaps inodes atomically and works even though we're replacing
    // the running binary (the live process keeps the old inode). Direct
    // fs::copy onto a running executable fails with ETXTBSY on Linux.
    let staged = install_path.with_file_name(format!(
        ".rekody.update-{}.{}",
        latest_tag,
        std::process::id()
    ));

    let install_ok = std::fs::copy(&new_bin, &staged)
        .and_then(|_| std::fs::rename(&staged, &install_path))
        .is_ok();

    if !install_ok {
        // Clean up the staged file before falling back so we don't leave litter.
        let _ = std::fs::remove_file(&staged);
        let sudo = std::process::Command::new("sudo")
            .args([
                "install",
                "-m",
                "0755",
                new_bin.to_str().unwrap(),
                install_path.to_str().unwrap(),
            ])
            .status()?;
        if !sudo.success() {
            println!(
                "  {} Could not write to {}. Try running with sudo.",
                style("✗").red(),
                install_path.display()
            );
            return Ok(());
        }
    } else {
        let _ = std::process::Command::new("chmod")
            .args(["+x", install_path.to_str().unwrap()])
            .status();
    }

    let _ = std::fs::remove_dir_all(&tmp);

    println!();
    println!(
        "  {} Updated to {}  (was {})",
        style("✓").green().bold(),
        style(format!("v{latest_tag}")).cyan().bold(),
        style(format!("v{CURRENT}")).dim()
    );
    println!();
    Ok(())
}

// ── Subcommand: key ──────────────────────────────────────────────────────────

fn cmd_key(action: Option<KeyCmd>) -> Result<()> {
    use std::io::IsTerminal;

    let Some(action) = action else {
        // Bare `rekody key`: TUI when TTY, otherwise list to stdout.
        if std::io::stdout().is_terminal() {
            return rekody_core::key_tui::run();
        }
        return print_key_list();
    };
    match action {
        KeyCmd::Set { provider } => {
            use std::io::{self, Write};
            print!(
                "  {} API key for {}: ",
                style("Enter").bold(),
                style(&provider).cyan().bold()
            );
            io::stdout().flush()?;
            // Read without echo
            let key = rpassword_read_password(&provider)?;
            if key.trim().is_empty() {
                println!("\n  {}", style("No key entered — aborted.").yellow());
                return Ok(());
            }
            save_keychain_key(&provider, key.trim())?;
            println!(
                "\n  {}  {} key saved.",
                style("✓").green().bold(),
                style(&provider).white()
            );
        }
        KeyCmd::Delete { provider } => match delete_keychain_key(&provider) {
            Ok(_) => println!(
                "  {}  {} key deleted.",
                style("✓").green().bold(),
                style(&provider).white()
            ),
            Err(_) => println!(
                "  {}  No key found for {}.",
                style("○").dim(),
                style(&provider).white()
            ),
        },
    }
    Ok(())
}

/// Pipe-friendly fallback: same content as the old `rekody key list`.
fn print_key_list() -> Result<()> {
    let rule = "─".repeat(48);
    println!();
    println!(
        "  {BRAND}╭─{RESET}  {BRAND_LIGHT}{BOLD}rekody keys{RESET}  {DIM}keychain status{RESET}"
    );
    println!("  {BRAND}│{RESET}");
    let providers = &[
        "groq",
        "deepgram",
        "anthropic",
        "openai",
        "gemini",
        "cerebras",
        "together",
        "openrouter",
        "fireworks",
    ];
    let mut any = false;
    for p in providers {
        match get_keychain_key(p) {
            Ok(key) if !key.is_empty() => {
                println!(
                    "  {BRAND}│{RESET}   {CREAM}{BOLD}{:<11}{RESET}  {OK}✓ stored{RESET}  {DIM}{}{RESET}",
                    p,
                    mask_key(&key)
                );
                any = true;
            }
            _ => {
                println!("  {BRAND}│{RESET}   {CREAM}{:<11}{RESET}  {DIM}—{RESET}", p);
            }
        }
    }
    if !any {
        println!("  {BRAND}│{RESET}");
        println!("  {BRAND}│{RESET}   {DIM}No keys stored. Run: rekody key set <provider>{RESET}");
    }
    println!("  {BRAND}│{RESET}");
    println!("  {BRAND}╰{}{RESET}", rule);
    println!();
    Ok(())
}

// ── Subcommand: skill ──────────────────────────────────────────────────────

fn cmd_skill(action: Option<SkillCmd>) -> Result<()> {
    use std::io::IsTerminal;

    // Make sure the starter pack exists for any skill interaction.
    rekody_core::skill::ensure_starter_pack().ok();

    // Whether LLM post-processing is actually on — skills are a no-op without
    // it, so we warn rather than silently affirm "Active".
    let llm_active = rekody_core::has_llm_providers(&load_config_or_default(&find_config_path()));

    let Some(action) = action else {
        // Bare `rekody skill`: TUI when TTY, otherwise print the list.
        if std::io::stdout().is_terminal() {
            return rekody_core::skill_tui::run(llm_active);
        }
        return print_skill_list();
    };

    match action {
        SkillCmd::Use { name } => {
            // Validate the skill exists before activating it.
            match rekody_core::skill::load_skill(&name) {
                Some(skill) => {
                    rekody_core::skill::set_active(Some(&skill.name))?;
                    println!(
                        "  {}  Active skill set to {}",
                        style("✓").green().bold(),
                        style(&skill.name).cyan().bold()
                    );
                    if !skill.description.is_empty() {
                        println!("     {}", style(&skill.description).dim());
                    }
                    if !llm_active {
                        println!(
                            "     {}",
                            style(
                                "Note: LLM post-processing is off, so this skill won't take \
                                 effect until you enable a provider — see `rekody config`."
                            )
                            .yellow()
                        );
                    }
                }
                None => {
                    println!(
                        "  {}  No skill named {}.",
                        style("✗").red().bold(),
                        style(&name).white()
                    );
                    println!("     {}", style("Run: rekody skill list").dim());
                }
            }
        }
        SkillCmd::None => {
            rekody_core::skill::set_active(None)?;
            println!(
                "  {}  Active skill cleared — using built-in app detection.",
                style("✓").green().bold()
            );
        }
        SkillCmd::List => print_skill_list()?,
    }
    Ok(())
}

/// Pipe-friendly skill list (also the bare-command fallback when not a TTY).
fn print_skill_list() -> Result<()> {
    let rule = "─".repeat(48);
    let active = rekody_core::skill::active_name();
    let skills = rekody_core::skill::list_skills();

    println!();
    println!(
        "  {BRAND}╭─{RESET}  {BRAND_LIGHT}{BOLD}rekody skills{RESET}  {DIM}dictation presets{RESET}"
    );
    println!("  {BRAND}│{RESET}");
    // Cross-check the active name against the loaded skills so a deleted skill
    // reads as "missing" rather than falsely active.
    let active_row = match &active {
        None => format!("{OK}{BOLD}Auto (built-in app detection){RESET}"),
        Some(name) if skills.iter().any(|s| &s.name == name) => {
            format!("{OK}{BOLD}{name}{RESET}")
        }
        Some(name) => format!("{WARN}{BOLD}{name} (missing — using Auto){RESET}"),
    };
    println!("  {BRAND}│{RESET}   {DIM}active{RESET}  {}", active_row);
    println!("  {BRAND}│{RESET}");

    if skills.is_empty() {
        println!("  {BRAND}│{RESET}   {DIM}No skills found in ~/.config/rekody/skills/{RESET}");
    } else {
        for s in &skills {
            let is_active = active.as_deref() == Some(s.name.as_str());
            let marker = if is_active {
                format!("{OK}●{RESET}")
            } else {
                format!("{DIM}○{RESET}")
            };
            println!(
                "  {BRAND}│{RESET}   {marker}  {CREAM}{BOLD}{:<12}{RESET}  {DIM}{}{RESET}",
                s.name, s.description
            );
        }
    }
    println!("  {BRAND}│{RESET}");
    println!("  {BRAND}│{RESET}   {DIM}rekody skill use <name>  ·  rekody skill none{RESET}");
    println!("  {BRAND}│{RESET}");
    println!("  {BRAND}╰{}{RESET}", rule);
    println!();
    Ok(())
}

// ── Subcommand: dictionary ─────────────────────────────────────────────────

fn cmd_dictionary(action: Option<DictionaryCmd>) -> Result<()> {
    use rekody_core::dictionary::Dictionary;
    let path = Dictionary::default_path()?;

    match action {
        None | Some(DictionaryCmd::List) => print_dictionary()?,
        Some(DictionaryCmd::Add { term }) => {
            let term = term.join(" ");
            let mut dict = Dictionary::load(&path)?;
            dict.add_term(term.clone());
            dict.save(&path)?;
            println!(
                "  {}  Added {} to your dictionary",
                style("✓").green().bold(),
                style(&term).cyan().bold()
            );
        }
        Some(DictionaryCmd::Remove { term }) => {
            let term = term.join(" ");
            let mut dict = Dictionary::load(&path)?;
            if dict.remove_term(&term) {
                dict.save(&path)?;
                println!(
                    "  {}  Removed {}",
                    style("✓").green().bold(),
                    style(&term).white()
                );
            } else {
                println!(
                    "  {}  {} is not in your dictionary",
                    style("○").dim(),
                    style(&term).white()
                );
            }
        }
    }
    Ok(())
}

/// Pipe-friendly dictionary listing (also the bare-command view).
fn print_dictionary() -> Result<()> {
    use rekody_core::dictionary::Dictionary;
    let rule = "─".repeat(48);
    let terms = Dictionary::load_or_empty();
    let terms = terms.terms();

    println!();
    println!(
        "  {BRAND}╭─{RESET}  {BRAND_LIGHT}{BOLD}rekody dictionary{RESET}  {DIM}custom vocabulary{RESET}"
    );
    println!("  {BRAND}│{RESET}");
    if terms.is_empty() {
        println!(
            "  {BRAND}│{RESET}   {DIM}No terms yet — add jargon, names, or products and{RESET}"
        );
        println!("  {BRAND}│{RESET}   {DIM}rekody will keep them spelled your way.{RESET}");
    } else {
        for t in terms {
            println!("  {BRAND}│{RESET}   {CREAM}{BOLD}{}{RESET}", t);
        }
    }
    println!("  {BRAND}│{RESET}");
    println!("  {BRAND}│{RESET}   {DIM}rekody dictionary add <term>  ·  remove <term>{RESET}");
    println!("  {BRAND}│{RESET}");
    println!("  {BRAND}╰{}{RESET}", rule);
    println!();
    Ok(())
}

fn rpassword_read_password(_provider: &str) -> Result<String> {
    // Simple stdin read (terminal should handle echo=off via stty if needed)
    // Use rpassword-style approach: disable echo
    #[cfg(unix)]
    {
        // Disable echo via termios
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&std::io::stdin());
        let mut term: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { libc::tcgetattr(fd, &mut term) };
        let mut noecho = term;
        noecho.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &noecho) };

        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;

        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
        Ok(buf.trim_end_matches('\n').to_string())
    }
    #[cfg(not(unix))]
    {
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        Ok(buf.trim_end_matches('\n').to_string())
    }
}

fn save_keychain_key(provider: &str, key: &str) -> Result<()> {
    let entry = keyring::Entry::new("com.rekody.voice", provider)?;
    entry.set_password(key)?;
    Ok(())
}

fn delete_keychain_key(provider: &str) -> Result<()> {
    let entry = keyring::Entry::new("com.rekody.voice", provider)?;
    entry.delete_credential()?;
    Ok(())
}

fn get_keychain_key(provider: &str) -> Result<String> {
    let entry = keyring::Entry::new("com.rekody.voice", provider)?;
    Ok(entry.get_password()?)
}

// ── Config helpers ───────────────────────────────────────────────────────────

fn find_config_path() -> Option<String> {
    let candidates = [
        dirs::home_dir().map(|h| h.join(".config").join("rekody").join("config.toml")),
        dirs::config_dir().map(|c| c.join("rekody").join("config.toml")),
        Some(std::path::PathBuf::from("config/default.toml")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
}

fn default_config_path() -> String {
    dirs::home_dir()
        .map(|h| {
            h.join(".config")
                .join("rekody")
                .join("config.toml")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| "~/.config/rekody/config.toml".to_string())
}

fn load_config_or_default(path: &Option<String>) -> RekodyConfig {
    path.as_deref()
        .and_then(|p| load_config(p).ok())
        .unwrap_or_default()
}

/// Model directory: `$REKODY_MODEL_DIR` or `~/.local/share/rekody/models`.
fn rekody_models_dir() -> std::path::PathBuf {
    std::env::var("REKODY_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".local").join("share").join("rekody").join("models"))
                .unwrap_or_else(|| std::path::PathBuf::from("models"))
        })
}

fn stt_display_name(config: &RekodyConfig) -> String {
    match config.stt_engine.to_lowercase().as_str() {
        "groq" => "Groq Cloud Whisper Large v3".to_string(),
        "deepgram" => "Deepgram Nova-3".to_string(),
        "cohere" => format!("Cohere local (port {})", config.cohere_stt_port),
        "nemotron" => "Rekody Streaming (English)".to_string(),
        _ => format!("Local Whisper ({})", config.whisper_model),
    }
}

fn format_activation_mode(mode: &str) -> String {
    match mode.to_lowercase().as_str() {
        "toggle" => format!("toggle — tap {TRIGGER_LABEL} to start/stop"),
        "push_to_talk" | "ptt" | "hold" => format!("push-to-talk — hold {TRIGGER_LABEL}"),
        _ => format!("both — hold {TRIGGER_LABEL} to talk, quick-tap for hands-free"),
    }
}

/// GGML file the local Whisper engine loads for a configured model size.
/// Mirrors the Pipeline mapping in rekody-core: rekody-stt's multilingual
/// filenames, with unknown sizes falling back to turbo.
fn whisper_ggml_file(whisper_model: &str) -> &'static str {
    use rekody_stt::WhisperModel;
    let model = match whisper_model.to_lowercase().as_str() {
        "tiny" => WhisperModel::Tiny,
        "small" => WhisperModel::Small,
        "medium" => WhisperModel::Medium,
        "large" => WhisperModel::Large,
        _ => WhisperModel::Turbo,
    };
    model.file_name()
}

/// Detect whether the Core ML encoder is installed for the configured local
/// model. Returns a human label + an ok flag. Only compiled on Apple Silicon
/// macOS, where Core ML offers a ~2× speedup over Metal-only inference.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn coreml_status(whisper_model: &str) -> (String, bool) {
    let mlmodelc_name = match whisper_model.to_lowercase().as_str() {
        "tiny" => "ggml-tiny-encoder.mlmodelc",
        "small" => "ggml-small-encoder.mlmodelc",
        "medium" => "ggml-medium-encoder.mlmodelc",
        "turbo" => "ggml-large-v3-turbo-encoder.mlmodelc",
        "large" => "ggml-large-encoder.mlmodelc",
        other => return (format!("unknown model: {other}"), false),
    };
    let model_dir = std::env::var("REKODY_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".local").join("share").join("rekody").join("models"))
                .unwrap_or_else(|| std::path::PathBuf::from("models"))
        });
    let path = model_dir.join(mlmodelc_name);
    if path.exists() {
        (format!("loaded ({mlmodelc_name})"), true)
    } else {
        (
            "missing — re-run `rekody setup` to download (~2× speedup)".to_string(),
            false,
        )
    }
}

// ── Live dictation pipeline ──────────────────────────────────────────────────

async fn run_dictation(verbose: bool, record_all_audio_flag: bool) -> Result<()> {
    // If no config exists, run onboarding first.
    if onboarding::needs_onboarding() {
        onboarding::run_onboarding()?;
    }

    let config_path = find_config_path();
    let mut config = load_config_or_default(&config_path);

    // Seed the starter skill pack so app-triggered skills work out of the box,
    // even for users who never open `rekody skill`. Idempotent + infallible.
    rekody_core::skill::ensure_starter_pack().ok();

    // Pull missing API keys from the keychain into config at runtime.
    inject_keychain_keys(&mut config);

    // CLI flag wins over config: --record-all-audio always forces it on.
    if record_all_audio_flag {
        config.record_all_audio = true;
    }

    // Live status region: wordmark + config chips, transcript hero line, and
    // a status/footer line (replaces the old one-line spinner + boxed banner).
    let ui = Arc::new(Ui::new(&config));
    ui.idle();

    // Session stats tracker.
    let session = Arc::new(SessionStats::new());

    // Set up tracing with our custom UI layer.
    let ui_layer = UiLayer::new(Arc::clone(&ui), Arc::clone(&session));

    // Floating HUD pill (rekody-hud helper). Entirely best-effort: any
    // failure here logs and dictation runs without the HUD. The server always
    // starts when not explicitly disabled (an externally launched helper can
    // still connect); the helper is only spawned when its binary is found.
    let mut hud_notes: Vec<(bool, String)> = Vec::new(); // (is_warning, message)
    let hud_server = if config.hud == Some(false) {
        None
    } else {
        let sock = rekody_core::hud::socket_path();
        match rekody_core::hud::HudServer::new(&sock, "bottom") {
            Ok(server) => {
                match rekody_core::hud::spawn_helper(&sock) {
                    Ok(Some(handle)) => server.attach_helper(handle),
                    Ok(None) => {
                        hud_notes.push((false, "rekody-hud helper not found — HUD off".into()));
                    }
                    Err(e) => {
                        hud_notes.push((true, format!("failed to supervise rekody-hud: {e}")));
                    }
                }
                Some(Arc::new(server))
            }
            Err(e) => {
                hud_notes.push((
                    true,
                    format!("HUD server unavailable ({e}) — continuing without HUD"),
                ));
                None
            }
        }
    };
    let hud_layer = hud_server
        .as_ref()
        .map(|server| HudLayer::new(Arc::clone(server), &config));

    // rekody=debug covers the BINARY target only; the pipeline (and its
    // "mic level" waveform events) lives in the rekody_core LIB target,
    // which EnvFilter treats as a different name. Both need debug or the
    // live waveform starves.
    let level = if verbose { "debug" } else { "info" };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        format!("{level},rekody=debug,rekody_core=debug")
            .parse()
            .unwrap()
    });

    // DEBUG: tee tracing events to file when REKODY_DEBUG_LOG=<path> is set.
    let debug_layer = std::env::var("REKODY_DEBUG_LOG")
        .ok()
        .and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        })
        .map(|f| {
            tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(f))
                .with_target(true)
                .with_thread_ids(true)
                .with_ansi(false)
        });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(ui_layer)
        .with(hud_layer)
        .with(debug_layer)
        .init();

    // HUD setup notes gathered before tracing was initialized.
    for (is_warning, note) in hud_notes {
        if is_warning {
            tracing::warn!("{note}");
        } else {
            tracing::info!("{note}");
        }
    }

    let pipeline = Pipeline::new(config)?;
    pipeline.run().await?;

    ui.finish();
    Ok(())
}

/// Pull API keys from the keychain into the config struct if they are missing.
fn inject_keychain_keys(config: &mut RekodyConfig) {
    // Deepgram STT key
    if config.deepgram_api_key.is_none() || config.deepgram_api_key.as_deref() == Some("") {
        if let Ok(key) = get_keychain_key("deepgram")
            && !key.is_empty()
        {
            config.deepgram_api_key = Some(key);
        }
        // Also try the legacy account name
        if config.deepgram_api_key.is_none()
            && let Ok(key) = get_keychain_key("deepgram_api_key")
            && !key.is_empty()
        {
            config.deepgram_api_key = Some(key);
        }
    }
    // Groq key for STT or LLM
    if (config.groq_api_key.is_none() || config.groq_api_key.as_deref() == Some(""))
        && let Ok(key) = get_keychain_key("groq")
        && !key.is_empty()
    {
        config.groq_api_key = Some(key.clone());
        // Update existing groq provider entry, or create one if absent.
        let existing = config.providers.iter_mut().find(|p| p.name == "groq");
        if let Some(p) = existing {
            if p.api_key.is_empty() {
                p.api_key = key;
            }
        } else {
            config.providers.push(rekody_core::ProviderConfig {
                name: "groq".into(),
                api_key: key,
                model: "openai/gpt-oss-20b".into(),
                base_url: None,
            });
        }
    }
    // Inject keychain keys into any providers array entries that lack a key.
    for p in config.providers.iter_mut() {
        if p.api_key.is_empty()
            && let Ok(key) = get_keychain_key(&p.name)
            && !key.is_empty()
        {
            p.api_key = key;
        }
    }
}

// ── Live status region (Studio UI) ──────────────────────────────────────────
//
// A persistent multi-line region at the bottom of the terminal, drawn with
// indicatif MultiProgress (scrollback above it stays intact — completed
// dictations are printed into it):
//
//   rekody. v0.5.9   [● streaming · en] [groq cleanup] [⌥space]
//
//   <transcript hero — live partials, dim head / bright tail, block cursor>
//
//   ● 0:06  ▁▃▆▇▅▂▄▇▆▃  release ⌥space to insert
//
// The transcript line is the visual hero: the only bright text on screen
// while recording. Config collapses into chips on the header line.

/// Number of recent mic levels shown in the waveform.
const WAVE_BARS: usize = 14;

/// Safety net for the busy ("transcribing…/formatting…") state. The state is
/// normally cleared by a terminal pipeline event (injection succeeded, empty
/// transcript, or a processing error). If none of those ever arrive (a bug, a
/// panicked worker, a dropped event), the console must still return to idle
/// rather than hang the box forever. Generous so it never clips a genuinely
/// slow cloud-LLM cleanup; it only exists to bound an otherwise-infinite wait.
const BUSY_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(90);
const WAVE_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

// Chip backgrounds (solid — terminals don't alpha-blend).
const CHIP_TEAL_BG: &str = "\x1b[48;2;21;51;58m"; // dark teal fill
const CHIP_TEAL_FG: &str = "\x1b[38;2;127;201;211m"; // #7FC9D3
const CHIP_BG: &str = "\x1b[48;2;32;40;40m"; // neutral dark fill
const CHIP_FG: &str = "\x1b[38;2;150;162;162m";

#[derive(Default)]
struct UiState {
    recording: bool,
    /// Streaming engine active — mic levels drive a live waveform.
    streaming: bool,
    recording_start: Option<Instant>,
    wave: std::collections::VecDeque<f32>,
    partial: String,
    /// Status line override while transcribing/formatting (batch path).
    busy: Option<&'static str>,
    /// When the current busy state began. Drives a watchdog so a missing
    /// terminal event can never leave the "formatting…" box on screen forever.
    busy_since: Option<Instant>,
    /// Both mode: recording latched hands-free (quick tap) — show stop hint.
    handsfree: bool,
    /// Shimmer sweep position, advanced by the ticker (~8fps).
    shimmer_phase: f32,
}

struct Ui {
    /// One spinner whose multi-line message IS the live region (transcript +
    /// status) — indicatif redraws embedded newlines correctly, and `println`
    /// inserts completed dictations above it into scrollback. The wordmark +
    /// chips header is printed ONCE at startup so dictations always flow
    /// BELOW it, never on top.
    bar: ProgressBar,
    state: Mutex<UiState>,
    ticker_running: std::sync::atomic::AtomicBool,
}

impl Ui {
    fn new(config: &RekodyConfig) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(ProgressStyle::with_template("{msg}").unwrap());

        // Skills only apply during AI cleanup, so the whole skill surface
        // (footer hint here, Tab cycling in the run loops) keys off this.
        let llm_enabled = rekody_core::has_llm_providers(config);

        // Header chips: (accented, label).
        let engine = match config.stt_engine.to_lowercase().as_str() {
            "nemotron" => "● streaming · en".to_string(),
            "groq" => "● groq whisper".to_string(),
            "deepgram" => "● deepgram nova-3".to_string(),
            "cohere" => "● cohere local".to_string(),
            _ => format!("● whisper {}", config.whisper_model),
        };
        let cleanup = if llm_enabled {
            config
                .providers
                .first()
                .map(|p| format!("{} cleanup", p.name))
                .unwrap_or_else(|| "cleanup".into())
        } else {
            "no cleanup".into()
        };
        let trigger = match config.trigger_key.to_lowercase().as_str() {
            "fn_key" | "fn" => "🌐 fn".to_string(),
            _ => TRIGGER_LABEL.to_string(),
        };
        let chip_list = [(true, engine), (false, cleanup), (false, trigger)];

        // Print the mast once into scrollback — dictations stack below it.
        let mut chips = String::new();
        for (accent, label) in &chip_list {
            let (bg, fg) = if *accent {
                (CHIP_TEAL_BG, CHIP_TEAL_FG)
            } else {
                (CHIP_BG, CHIP_FG)
            };
            chips.push_str(&format!("{bg}{fg} {label} {RESET}  "));
        }
        // The whole top block is static, printed once — completed dictations
        // stack BELOW all of it; only transient activity draws underneath.
        println!();
        println!(
            "  {CREAM}{BOLD}rekody{RESET}{BRAND}{BOLD}.{RESET} {DIM}v{}{RESET}   {chips}",
            env!("CARGO_PKG_VERSION"),
        );
        println!();
        println!("  {DIM}ready — hold {TRIGGER_LABEL} to talk · quick-tap for hands-free{RESET}");
        println!();
        // The ⇥ skill hint only shows when cleanup can actually run a skill;
        // without a provider the run loops ignore Tab, so advertising it
        // would be a dead control.
        let skill_hint = if llm_enabled {
            format!("⇥ skill   {}   ", sep())
        } else {
            String::new()
        };
        println!(
            "  {SUBTLE}{TRIGGER_LABEL} dictate   {sep}   {skill_hint}^c quit{RESET}",
            sep = sep()
        );
        println!();

        let streaming = config.stt_engine.to_lowercase() == "nemotron";
        Self {
            bar,
            state: Mutex::new(UiState {
                streaming,
                ..UiState::default()
            }),
            ticker_running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Live preview: full transcript word-wrapped to the terminal width,
    /// showing the last `PREVIEW_LINES` lines — older lines auto-scroll off
    /// the top as new words arrive (like a phone dictation sheet). Earlier
    /// visible lines render dim; the current line is bright with a cursor.
    /// Word-wrap the partial transcript to `avail` columns and return the
    /// last `PREVIEW_LINES` (older lines auto-scroll off the top). Each entry
    /// is (styled, plain_width): earlier lines dim, current line bright with
    /// a cursor while recording.
    fn preview_lines(&self, s: &UiState, avail: usize) -> Vec<(String, usize)> {
        const PREVIEW_LINES: usize = 5;
        if s.partial.is_empty() {
            return vec![(format!("{BRAND_LIGHT}▌{RESET}"), 1)];
        }
        let mut lines: Vec<String> = vec![String::new()];
        for word in s.partial.split_whitespace() {
            let wlen = word.chars().count();
            if wlen > avail {
                // Hard-break pathological tokens so terminal soft-wrap can
                // never corrupt the live region redraw.
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(avail) {
                    lines.push(chunk.iter().collect());
                }
                continue;
            }
            let cur = lines.last_mut().expect("never empty");
            let need = if cur.is_empty() {
                wlen
            } else {
                cur.chars().count() + 1 + wlen
            };
            if need <= avail {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            } else {
                lines.push(word.to_string());
            }
        }
        let start = lines.len().saturating_sub(PREVIEW_LINES);
        let visible = &lines[start..];
        let mut out = Vec::new();
        for (i, line) in visible.iter().enumerate() {
            let scroll_mark = if i == 0 && start > 0 { "…" } else { "" };
            let plain = line.chars().count() + scroll_mark.chars().count();
            if i + 1 == visible.len() {
                let (cursor, cw) = if s.recording {
                    (format!("{BRAND_LIGHT}▌{RESET}"), 1)
                } else {
                    (String::new(), 0)
                };
                out.push((
                    format!("{DIM}{scroll_mark}{RESET}{CREAM}{line}{RESET}{cursor}"),
                    plain + cw,
                ));
            } else {
                out.push((format!("{DIM}{scroll_mark}{line}{RESET}"), plain));
            }
        }
        out
    }

    /// Mic sparkline: eighth-block time series colored by amplitude along a
    /// teal ramp (the cava trick — 8 sub-steps per cell reads as analog).
    fn wave_sparkline(s: &UiState) -> (String, usize) {
        if !s.streaming || s.wave.is_empty() {
            return (String::new(), 0);
        }
        let mut out = String::new();
        for rms in &s.wave {
            let level = (rms.sqrt() * 14.0).min(7.0) as usize;
            let color = match level {
                0..=2 => "\x1b[38;2;32;128;141m", // #20808D
                3..=4 => "\x1b[38;2;47;163;179m", // #2FA3B3
                5..=6 => "\x1b[38;2;79;184;197m", // #4FB8C5
                _ => "\x1b[38;2;127;212;222m",    // #7FD4DE
            };
            out.push_str(color);
            out.push(WAVE_GLYPHS[level]);
        }
        out.push_str(RESET);
        (out, s.wave.len())
    }

    /// Claude-Code-style shimmer: a bright window sweeps across dim text.
    /// Pure per-char truecolor — the highlight position advances with the
    /// ticker phase.
    fn shimmer(text: &str, phase: f32) -> String {
        const BASE: (f32, f32, f32) = (94.0, 110.0, 110.0); // dim slate
        const HI: (f32, f32, f32) = (168.0, 228.0, 234.0); // bright teal
        let n = text.chars().count() as f32;
        let sweep = phase % (n + 8.0) - 4.0;
        let mut out = String::new();
        for (i, ch) in text.chars().enumerate() {
            let d = (i as f32 - sweep).abs();
            let w = (-d * d / 4.5).exp(); // gaussian window, sigma ≈ 1.5
            let r = (BASE.0 + (HI.0 - BASE.0) * w) as u8;
            let g = (BASE.1 + (HI.1 - BASE.1) * w) as u8;
            let b = (BASE.2 + (HI.2 - BASE.2) * w) as u8;
            out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
            out.push(ch);
        }
        out.push_str(RESET);
        out
    }

    /// Repaint the live region. Idle = empty (the static top block already
    /// says everything). Active = one rounded Console Card: state pill in
    /// the border, auto-scrolling preview, sparkline + shimmer verb footer.
    fn render(&self) {
        let Ok(s) = self.state.lock() else { return };
        if !s.recording && s.busy.is_none() {
            drop(s);
            self.bar.set_message(String::new());
            self.bar.tick();
            return;
        }

        let cols = (console::Term::stdout().size().1 as usize).max(40);
        // Card geometry: 2-col margin, "│ " + content + " │".
        let inner = cols.saturating_sub(8).min(110);
        let border = if s.recording {
            BRAND
        } else {
            "\x1b[38;2;79;184;197m" // lighter while finishing
        };

        // Title pill — inverted block, state-colored (the atuin badge move).
        let (pill, pill_w) = if s.recording {
            let elapsed = s
                .recording_start
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            let label = if s.handsfree {
                format!(" ● REC {}:{:02} · tap to stop ", elapsed / 60, elapsed % 60)
            } else {
                format!(" ● REC {}:{:02} ", elapsed / 60, elapsed % 60)
            };
            let w = label.chars().count();
            (
                format!("\x1b[48;2;217;100;89m\x1b[38;2;11;17;17m{BOLD}{label}{RESET}"),
                w,
            )
        } else {
            let label = " ◐ WORKING ";
            (
                format!("\x1b[48;2;21;51;58m\x1b[38;2;127;201;211m{BOLD}{label}{RESET}"),
                label.chars().count(),
            )
        };

        // Footer: sparkline + shimmer verb left, hint + word count right.
        let (wave, wave_w) = Self::wave_sparkline(&s);
        let verb = if s.recording {
            "listening…"
        } else {
            s.busy.unwrap_or("working…")
        };
        let verb_styled = Self::shimmer(verb, s.shimmer_phase);
        let words = s.partial.split_whitespace().count();
        // Active skill stays visible while ⌥Space+Tab cycles mid-recording.
        let skill_seg = rekody_core::skill::active_name()
            .map(|n| format!("◆ {n} · "))
            .unwrap_or_default();
        let right_plain = format!("{skill_seg}release {TRIGGER_LABEL} · {words:>3} words");
        let left_w = wave_w + if wave_w > 0 { 2 } else { 0 } + verb.chars().count();
        let mut footer = String::new();
        let mut footer_w = left_w;
        if wave_w > 0 {
            footer.push_str(&wave);
            footer.push_str("  ");
        }
        footer.push_str(&verb_styled);
        if inner > left_w + right_plain.chars().count() + 2 {
            let gap = inner - left_w - right_plain.chars().count();
            footer.push_str(&" ".repeat(gap));
            footer.push_str(&format!("{SUBTLE}{right_plain}{RESET}"));
            footer_w = inner;
        }

        // Assemble the card.
        let mut rows: Vec<(String, usize)> = self.preview_lines(&s, inner);
        rows.push((String::new(), 0)); // breathing room above the footer
        rows.push((footer, footer_w));
        let elapsed_phase = s.shimmer_phase; // keep borrow short
        let _ = elapsed_phase;
        drop(s);

        let mut msg = String::new();
        let top_dashes = (inner + 2).saturating_sub(1 + pill_w);
        msg.push_str(&format!(
            "  {border}╭─{RESET}{pill}{border}{}╮{RESET}\n",
            "─".repeat(top_dashes)
        ));
        for (styled, plain_w) in rows {
            let pad = inner.saturating_sub(plain_w);
            msg.push_str(&format!(
                "  {border}│{RESET} {styled}{} {border}│{RESET}\n",
                " ".repeat(pad)
            ));
        }
        msg.push_str(&format!("  {border}╰{}╯{RESET}", "─".repeat(inner + 2)));

        self.bar.set_message(msg);
        self.bar.tick();
    }
    fn idle(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.recording = false;
            s.busy = None;
            s.busy_since = None;
            s.partial.clear();
            s.wave.clear();
        }
        self.render();
    }

    fn start_recording(self: &Arc<Self>) {
        if let Ok(mut s) = self.state.lock() {
            s.recording = true;
            s.busy = None;
            s.busy_since = None;
            s.handsfree = false;
            s.recording_start = Some(Instant::now());
            s.partial.clear();
            s.wave.clear();
        }
        self.render();

        // Animation ticker (~8fps): advances the timer, the shimmer sweep,
        // and the sparkline through recording AND the busy tail, stopping
        // only once the region returns to idle.
        if !self
            .ticker_running
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            let ui = Arc::clone(self);
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                    let (active, watchdog_fired) = ui
                        .state
                        .lock()
                        .map(|mut s| {
                            s.shimmer_phase += 0.9;
                            // Watchdog: a busy state that outlives BUSY_WATCHDOG
                            // with no clearing event is treated as stuck and
                            // force-cleared so the box can never hang forever.
                            let stuck = busy_state_is_stuck(
                                s.recording,
                                s.busy,
                                s.busy_since,
                                Instant::now(),
                            );
                            if stuck {
                                s.busy = None;
                                s.busy_since = None;
                                s.partial.clear();
                                s.wave.clear();
                            }
                            (s.recording || s.busy.is_some(), stuck)
                        })
                        .unwrap_or((false, false));
                    if watchdog_fired {
                        tracing::warn!(
                            "dictation busy state cleared by watchdog (no terminal event)"
                        );
                    }
                    // Always repaint once more so the final (idle) frame lands
                    // even on the iteration that decides to stop.
                    ui.render();
                    if !active {
                        break;
                    }
                }
                ui.ticker_running
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            });
        }
    }

    fn stop_recording(&self, busy: &'static str) {
        if let Ok(mut s) = self.state.lock() {
            s.recording = false;
            s.handsfree = false;
            s.busy = Some(busy);
            // Start (or keep) the watchdog clock the moment we go busy. Only
            // the first transition arms it; later label changes
            // (transcribing…→formatting…) keep the original deadline so a
            // long-but-progressing dictation isn't penalized.
            s.busy_since.get_or_insert_with(Instant::now);
        }
        self.render();
    }

    /// Both mode: quick tap latched the recording hands-free.
    fn on_latched(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.handsfree = true;
        }
        self.render();
    }

    /// New mic RMS level from the live tap (streaming engines).
    fn on_mic_level(&self, rms: f32) {
        if let Ok(mut s) = self.state.lock() {
            if !s.recording {
                return;
            }
            s.wave.push_back(rms);
            while s.wave.len() > WAVE_BARS {
                s.wave.pop_front();
            }
        }
        self.render();
    }

    /// Streaming partial: dim head, bright tail, block cursor.
    fn on_partial(&self, text: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.partial = text.to_string();
        }
        self.render();
    }

    /// Repaint the header (e.g. after skill cycling).
    fn render_header(&self) {
        self.render();
    }

    /// Print a completed dictation into scrollback and reset to idle.
    fn done_line(&self, line: String) {
        self.bar.println(line);
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}
// ── Session statistics ───────────────────────────────────────────────────────

struct SessionStats {
    dictation_count: AtomicU64,
    total_audio_secs: Mutex<f64>,
}

impl SessionStats {
    fn new() -> Self {
        Self {
            dictation_count: AtomicU64::new(0),
            total_audio_secs: Mutex::new(0.0),
        }
    }

    fn record(&self, audio_secs: f64) {
        self.dictation_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut secs) = self.total_audio_secs.lock() {
            *secs += audio_secs;
        }
    }

    fn summary_line(&self) -> String {
        let count = self.dictation_count.load(Ordering::Relaxed);
        let secs = self.total_audio_secs.lock().map(|s| *s).unwrap_or(0.0);
        let label = if count == 1 {
            "dictation"
        } else {
            "dictations"
        };
        format!(
            "     {SUBTLE}session{RESET}  {DIM}{} {} {sep} {:.1}s audio{RESET}",
            count,
            label,
            secs,
            sep = sep()
        )
    }
}

// ── Spinner style helpers ────────────────────────────────────────────────────
//
// Inline status panel for the live dictation pipeline. Stays one line tall so
// the terminal remains usable around it. Visual language matches the rekody
// brand palette: brand teal #20808D (BRAND), lighter teal #4FB8C5 (BRAND_LIGHT),
// cream #FBFAF4 (CREAM) for emphasis text, dim gray for secondary labels.
// Latency dot semantics match the history TUI: <5s green, <15s amber, else red.

const BRAND: &str = "\x1b[38;2;32;128;141m"; // #20808D
const BRAND_LIGHT: &str = "\x1b[38;2;79;184;197m"; // #4FB8C5
const CREAM: &str = "\x1b[38;2;251;250;244m"; // #FBFAF4
const DIM: &str = "\x1b[38;2;119;119;119m";
const SUBTLE: &str = "\x1b[38;2;85;85;85m";
const OK: &str = "\x1b[38;2;107;203;119m";
const WARN: &str = "\x1b[38;2;230;180;80m";
const SLOW: &str = "\x1b[38;2;217;107;107m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn latency_color(total_ms: u64) -> &'static str {
    match total_ms {
        0..=4_999 => OK,
        5_000..=14_999 => WARN,
        _ => SLOW,
    }
}

fn sep() -> String {
    format!("{DIM}·{RESET}")
}

/// True when a tracing message marks a dictation that ended in a terminal
/// pipeline error, so the live UI (console card / HUD pill) must clear its
/// busy "formatting…/working…" state and return to idle.
///
/// Both the batch and streaming paths run through `process_transcript`, but
/// they log DIFFERENT strings when it fails: the batch path emits "failed to
/// process audio segment" (lib.rs) and the streaming path emits "failed to
/// process transcript" (lib.rs). Matching only one of them left the other
/// path with the busy box stuck on screen forever. Both layers route their
/// terminal-error handling through here so the two strings can never drift
/// apart again; see the matcher test below.
fn is_dictation_terminal_error(msg: &str) -> bool {
    msg.contains("failed to process audio") || msg.contains("failed to process transcript")
}

/// Watchdog predicate: should the busy ("formatting…") state be force-cleared?
/// True only when we are NOT recording, a busy label is set, and it has been
/// busy for at least [`BUSY_WATCHDOG`] with no clearing event. Pure so the
/// ticker's safety net is unit-testable without spinning a real UI.
fn busy_state_is_stuck(
    recording: bool,
    busy: Option<&str>,
    busy_since: Option<Instant>,
    now: Instant,
) -> bool {
    !recording
        && busy.is_some()
        && busy_since.is_some_and(|t| now.saturating_duration_since(t) >= BUSY_WATCHDOG)
}

struct UiLayer {
    ui: Arc<Ui>,
    session: Arc<SessionStats>,
    recording_start: Mutex<Option<Instant>>,
    stt_result: Mutex<Option<SttResult>>,
}

#[derive(Clone)]
struct SttResult {
    text: String,
    latency_ms: String,
    done_shown: bool,
}

impl UiLayer {
    fn new(ui: Arc<Ui>, session: Arc<SessionStats>) -> Self {
        Self {
            ui,
            session,
            recording_start: Mutex::new(None),
            stt_result: Mutex::new(None),
        }
    }

    fn on_recording_started(&self) {
        if let Ok(mut start) = self.recording_start.lock() {
            *start = Some(Instant::now());
        }
        self.ui.start_recording();
    }

    fn on_transcription_complete(&self, text: &str, latency_ms: &str) {
        if let Ok(mut guard) = self.stt_result.lock() {
            *guard = Some(SttResult {
                text: text.to_string(),
                latency_ms: latency_ms.to_string(),
                done_shown: false,
            });
        }
        self.ui.stop_recording("formatting…");
        self.ui.on_partial(text);
    }

    fn on_llm_complete(&self, llm_ms: &str, skill: Option<&str>) {
        let stt = self.stt_result.lock().ok().and_then(|mut g| {
            if let Some(ref mut r) = *g {
                r.done_shown = true;
            }
            g.clone()
        });
        if let Some(stt) = stt {
            self.print_done(&stt, Some(llm_ms), skill);
        }
    }

    fn on_injected(&self) {
        let stt = self.stt_result.lock().ok().and_then(|g| g.clone());
        if let Some(ref stt) = stt
            && !stt.done_shown
        {
            // No-LLM path: no skill applies here.
            self.print_done(stt, None, None);
        }
        self.ui.idle();
    }

    /// Completed dictation → one line in scrollback, then back to idle.
    fn print_done(&self, stt: &SttResult, llm_ms: Option<&str>, skill: Option<&str>) {
        let display: String = {
            let chars: Vec<char> = stt.text.chars().collect();
            if chars.len() > 64 {
                format!("{}…", chars[..63].iter().collect::<String>())
            } else {
                stt.text.clone()
            }
        };
        let stt_num: u64 = stt.latency_ms.parse().unwrap_or(0);
        let llm_num: u64 = llm_ms.and_then(|s| s.parse().ok()).unwrap_or(0);
        let dot = latency_color(stt_num + llm_num);
        let lat = match llm_ms {
            Some(l) => format!("{}ms stt {sep} {l}ms llm", stt.latency_ms, sep = sep()),
            None => format!("{}ms stt", stt.latency_ms),
        };
        let skill_seg = match skill {
            Some(s) if !s.is_empty() => {
                format!("  {sep}  {BRAND_LIGHT}◆ {s}{RESET}", sep = sep())
            }
            _ => String::new(),
        };
        let audio_secs = self
            .recording_start
            .lock()
            .ok()
            .and_then(|s| s.map(|start| start.elapsed().as_secs_f64()))
            .unwrap_or(0.0);
        self.session.record(audio_secs);
        self.ui.done_line(format!(
            "  {OK}{BOLD}✓{RESET}  {CREAM}{display}{RESET}  {dot}●{RESET} {DIM}{lat}{RESET}{skill_seg}"
        ));
        self.ui.done_line(self.session.summary_line());
    }

    fn on_error(&self, msg: &str) {
        let short: String = msg.chars().take(90).collect();
        self.ui
            .done_line(format!("  {SLOW}{BOLD}✗{RESET}  {SLOW}{short}{RESET}"));
        self.ui.idle();
    }
}

impl<S> tracing_subscriber::Layer<S> for UiLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        let msg = &visitor.message;

        // Highest-frequency event first (every ~50ms while recording).
        if msg.contains("mic level") {
            if let Some(rms) = visitor.fields.get("rms").and_then(|v| v.parse().ok()) {
                self.ui.on_mic_level(rms);
            }
        } else if msg.contains("recording started") {
            self.on_recording_started();
        } else if msg.contains("hands-free latched") {
            self.ui.on_latched();
        } else if msg.contains("no speech detected") {
            self.on_error("no speech detected — speak louder or lower vad_threshold in config");
        } else if msg.contains("recording stopped") {
            self.ui.stop_recording("working…");
        } else if msg.contains("received audio segment") {
            self.ui.stop_recording("transcribing…");
        } else if msg.contains("partial transcript") {
            let text = visitor.fields.get("text").cloned().unwrap_or_default();
            self.ui.on_partial(&text);
        } else if msg.contains("transcription complete") {
            let text = visitor.fields.get("text").cloned().unwrap_or_default();
            let latency = visitor
                .fields
                .get("latency_ms")
                .cloned()
                .unwrap_or_default();
            self.on_transcription_complete(&text, &latency);
        } else if msg.contains("LLM formatting complete") {
            let latency = visitor
                .fields
                .get("latency_ms")
                .cloned()
                .unwrap_or_default();
            let skill = visitor
                .fields
                .get("skill")
                .map(|s| s.trim_matches('"').to_string())
                .filter(|s| !s.is_empty());
            self.on_llm_complete(&latency, skill.as_deref());
        } else if msg.contains("text injected successfully") {
            self.on_injected();
        } else if msg.contains("LLM formatting failed") || is_dictation_terminal_error(msg) {
            let err = visitor
                .fields
                .get("error")
                .cloned()
                .unwrap_or_else(|| msg.clone());
            self.on_error(&err);
        } else if msg.contains("empty transcript") {
            self.ui.idle();
        } else if msg.contains("skill cycled") {
            // Chip in the header repaints live; ⌥Space may still be held.
            self.ui.render_header();
        } else if msg.contains("no LLM API keys") {
            // Will show done on injection without LLM step.
        }
    }
}

// ── HUD tracing layer ────────────────────────────────────────────────────────
//
// Maps the same pipeline tracing events UiLayer matches into rekody-hud
// protocol events (docs/design/hud-protocol.md) and broadcasts them through
// the HudServer. Fully decoupled from the terminal UI: the layer only exists
// when the HUD server started, and every send is best-effort/non-blocking —
// a slow or missing helper never affects dictation.

/// Minimum interval between forwarded `level` events (protocol caps at 60Hz).
const HUD_LEVEL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

struct HudLayer {
    hud: Arc<rekody_core::hud::HudServer>,
    /// Toggle activation records hands-free (no key held) — the pill shows it.
    handsfree: bool,
    /// Whether LLM post-processing will run (gates the "formatting…" verb).
    llm_enabled: bool,
    /// Final transcript + stt latency_ms, captured at "transcription
    /// complete" and consumed at "text injected successfully" for `done`.
    stt_result: Mutex<Option<(String, u64)>>,
    /// Last forwarded mic level — coalesces the ~50ms event stream.
    last_level: Mutex<Option<Instant>>,
}

impl HudLayer {
    fn new(hud: Arc<rekody_core::hud::HudServer>, config: &RekodyConfig) -> Self {
        Self {
            hud,
            handsfree: config.activation_mode == "toggle",
            llm_enabled: rekody_core::has_llm_providers(config),
            stt_result: Mutex::new(None),
            last_level: Mutex::new(None),
        }
    }

    /// Forward a mic level, coalesced to at most one event per ~16ms.
    fn on_mic_level(&self, rms: f32) {
        let now = Instant::now();
        if let Ok(mut last) = self.last_level.lock() {
            if last.is_some_and(|t| now.duration_since(t) < HUD_LEVEL_INTERVAL) {
                return;
            }
            *last = Some(now);
        }
        self.hud.send(&rekody_core::hud::HudEvent::Level { rms });
    }

    fn on_injected(&self) {
        use rekody_core::hud::HudEvent;
        let stt = self.stt_result.lock().ok().and_then(|mut g| g.take());
        if let Some((text, ms)) = stt {
            let words = text.split_whitespace().count() as u64;
            self.hud.send(&HudEvent::Done { words, ms });
        }
        self.hud.send(&HudEvent::idle());
    }

    fn on_error(&self, msg: &str) {
        self.hud.send(&rekody_core::hud::HudEvent::Error {
            msg: msg.to_string(),
            recoverable: true,
        });
    }
}

impl<S> tracing_subscriber::Layer<S> for HudLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        use rekody_core::hud::HudEvent;

        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        let msg = &visitor.message;

        // Highest-frequency event first (every ~50ms while recording).
        if msg.contains("mic level") {
            if let Some(rms) = visitor.fields.get("rms").and_then(|v| v.parse().ok()) {
                self.on_mic_level(rms);
            }
        } else if msg.contains("recording started") {
            if let Ok(mut guard) = self.stt_result.lock() {
                *guard = None;
            }
            self.hud.send(&HudEvent::listening(self.handsfree));
        } else if msg.contains("hands-free latched") {
            // Quick-tap latch: re-announce listening with the hands-free flag
            // so the pill swaps its hint to "tap ⌥ + space to stop".
            self.hud.send(&HudEvent::listening(true));
        } else if msg.contains("no speech detected") {
            self.on_error("no speech detected");
        } else if msg.contains("recording stopped") {
            self.hud.send(&HudEvent::working("working…"));
        } else if msg.contains("received audio segment") {
            self.hud.send(&HudEvent::working("transcribing…"));
        } else if msg.contains("partial transcript") {
            let text = visitor.fields.get("text").cloned().unwrap_or_default();
            self.hud.send(&HudEvent::Partial {
                tail: rekody_core::hud::trim_tail(&text, 60),
            });
        } else if msg.contains("transcription complete") {
            let text = visitor.fields.get("text").cloned().unwrap_or_default();
            let ms = visitor
                .fields
                .get("latency_ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if let Ok(mut guard) = self.stt_result.lock() {
                *guard = Some((text, ms));
            }
            // Without LLM post-processing the next step is injection — keep
            // the previous verb rather than flashing "formatting…".
            if self.llm_enabled {
                self.hud.send(&HudEvent::working("formatting…"));
            }
        } else if msg.contains("text injected successfully") {
            self.on_injected();
        } else if msg.contains("LLM formatting failed") || is_dictation_terminal_error(msg) {
            let err = visitor
                .fields
                .get("error")
                .cloned()
                .unwrap_or_else(|| msg.clone());
            self.on_error(&err);
        } else if msg.contains("empty transcript") {
            self.hud.send(&HudEvent::idle());
        }
        // "skill cycled" is intentionally ignored in HUD v1.
    }
}

// ── Tracing field visitor ────────────────────────────────────────────────────

#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: std::collections::HashMap<String, String>,
}

impl tracing::field::Visit for EventVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let val = format!("{:?}", value);
        let val = val.trim_matches('"').to_string();
        if field.name() == "message" {
            self.message = val;
        } else {
            self.fields.insert(field.name().to_string(), val);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        // Full precision, not `{:.1}`. Mic-level RMS values are tiny (typical
        // speech ~0.01 to 0.15); rounding to one decimal collapsed most of them
        // to "0.0"/"0.1", so the waveform sparkline flattened to its lowest
        // glyph and read as "no waveform" except on the loudest syllables.
        // The consumer (`on_mic_level`) parses this string back to f32, so it
        // needs the real value; other float fields are display-only in debug
        // logs and are unharmed by keeping precision.
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
}

#[cfg(test)]
mod doctor_whisper_tests {
    use super::whisper_ggml_file;

    /// Doctor must probe the exact GGML file the engine will load: the
    /// rekody-stt multilingual filenames.
    #[test]
    fn sizes_map_to_the_files_the_engine_loads() {
        assert_eq!(whisper_ggml_file("tiny"), "ggml-tiny.bin");
        assert_eq!(whisper_ggml_file("small"), "ggml-small.bin");
        assert_eq!(whisper_ggml_file("medium"), "ggml-medium.bin");
        assert_eq!(whisper_ggml_file("large"), "ggml-large.bin");
        assert_eq!(whisper_ggml_file("turbo"), "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(whisper_ggml_file("TURBO"), "ggml-large-v3-turbo-q5_0.bin");
    }

    /// Unknown sizes (incl. the retired "base") follow the Pipeline's
    /// fallback to turbo, so doctor and the engine always agree.
    #[test]
    fn unknown_sizes_fall_back_to_turbo_like_the_pipeline() {
        assert_eq!(whisper_ggml_file("base"), "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(whisper_ggml_file(""), "ggml-large-v3-turbo-q5_0.bin");
    }
}

#[cfg(test)]
mod update_checksum_tests {
    use super::sha256sums_lookup;

    // Shape of a real rekody SHA256SUMS asset: two-space separator,
    // one line per release tarball.
    const SUMS: &str = "\
abc123def4567890000000000000000000000000000000000000000000000000  rekody-0.5.13-macos-aarch64.tar.gz
fedc000000000000000000000000000000000000000000000000000000000000  rekody-0.5.13-macos-x86_64.tar.gz
";

    #[test]
    fn matches_the_requested_file_only() {
        assert_eq!(
            sha256sums_lookup(SUMS, "rekody-0.5.13-macos-aarch64.tar.gz").as_deref(),
            Some("abc123def4567890000000000000000000000000000000000000000000000000")
        );
        assert_eq!(
            sha256sums_lookup(SUMS, "rekody-0.5.13-macos-x86_64.tar.gz").as_deref(),
            Some("fedc000000000000000000000000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn missing_file_returns_none() {
        assert_eq!(
            sha256sums_lookup(SUMS, "rekody-0.5.13-linux-aarch64.tar.gz"),
            None
        );
    }

    #[test]
    fn tolerates_binary_marker_and_blank_lines() {
        let sums = "\n# comment\nDEAD00000000000000000000000000000000000000000000000000000000BEEF *rekody-0.5.99-macos-aarch64.tar.gz\n";
        assert_eq!(
            sha256sums_lookup(sums, "rekody-0.5.99-macos-aarch64.tar.gz").as_deref(),
            // lookup lowercases the digest for case-insensitive comparison
            Some("dead00000000000000000000000000000000000000000000000000000000beef")
        );
    }
}

#[cfg(test)]
mod ui_event_tests {
    use super::{BUSY_WATCHDOG, WAVE_GLYPHS, busy_state_is_stuck, is_dictation_terminal_error};
    use std::time::Instant;

    // The two run-loop error emissions the live UI must treat as terminal,
    // copied VERBATIM from crates/rekody-core/src/lib.rs so this test fails
    // if either string is edited without updating the matcher.
    //   - batch path:     tracing::error!(..., "failed to process audio segment")
    //   - streaming path: tracing::error!(..., "failed to process transcript")
    const BATCH_ERROR_MSG: &str = "failed to process audio segment";
    const STREAMING_ERROR_MSG: &str = "failed to process transcript";

    #[test]
    fn terminal_error_matcher_covers_both_batch_and_streaming() {
        // The regression: the streaming string was NOT matched, so the busy
        // "formatting…" box hung forever after streaming dictations. Both must
        // now clear the console.
        assert!(
            is_dictation_terminal_error(BATCH_ERROR_MSG),
            "batch error string must clear the busy box"
        );
        assert!(
            is_dictation_terminal_error(STREAMING_ERROR_MSG),
            "streaming error string must clear the busy box"
        );
    }

    #[test]
    fn busy_watchdog_clears_a_stuck_formatting_state() {
        let now = Instant::now();
        // Busy long enough with no clearing event → force-clear (the defensive
        // guarantee: the box can never hang forever).
        let long_ago = now - (BUSY_WATCHDOG + std::time::Duration::from_secs(1));
        assert!(
            busy_state_is_stuck(false, Some("formatting…"), Some(long_ago), now),
            "a busy state older than the watchdog must be force-cleared"
        );
    }

    #[test]
    fn busy_watchdog_leaves_healthy_states_alone() {
        let now = Instant::now();
        let recent = now - std::time::Duration::from_secs(2);
        // Still recording → never a watchdog target.
        assert!(!busy_state_is_stuck(
            true,
            Some("formatting…"),
            Some(recent),
            now
        ));
        // Idle (no busy label) → nothing to clear.
        assert!(!busy_state_is_stuck(false, None, None, now));
        // Busy but well within the deadline → a slow-but-progressing dictation
        // must not be interrupted.
        assert!(!busy_state_is_stuck(
            false,
            Some("formatting…"),
            Some(recent),
            now
        ));
    }

    #[test]
    fn terminal_error_matcher_ignores_unrelated_messages() {
        // Non-terminal / progress messages must NOT be treated as errors, or
        // a normal dictation would flash an error and drop out of its live
        // states early.
        for msg in [
            "transcription complete",
            "text injected successfully",
            "partial transcript",
            "mic level",
            "recording started",
            "LLM formatting complete",
            "snippets not loaded, expansion skipped",
        ] {
            assert!(
                !is_dictation_terminal_error(msg),
                "'{msg}' must not be treated as a terminal dictation error"
            );
        }
    }

    /// Reproduce the mic-level RMS pipeline exactly as it flows through the
    /// UI: `EventVisitor::record_f64` stringifies the value, then
    /// `on_mic_level` parses it back and `wave_sparkline` maps it to a glyph.
    /// This models both the OLD `{:.1}` formatting and the NEW `to_string()`
    /// so the regression (quiet speech collapsing to the flat glyph) is
    /// locked out.
    fn glyph_for_rms(store: impl Fn(f64) -> String, rms: f32) -> char {
        // `tracing` upcasts the f32 field to f64 before `record_f64` sees it.
        let stored = store(rms as f64);
        let parsed: f32 = stored.parse().expect("rms field must parse back");
        // Same amplitude→level math as `wave_sparkline`.
        let level = (parsed.sqrt() * 14.0).min(7.0) as usize;
        WAVE_GLYPHS[level]
    }

    #[test]
    fn waveform_rms_precision_survives_the_visitor() {
        // Quiet-speech RMS values that all round to "0.0" under the old
        // `{:.1}` formatter (anything below ~0.045). The lowest glyph '▁'
        // reads as "no waveform"; anything above it animates.
        let quiet_speech = [0.0056_f32, 0.0312, 0.041];

        let old_fmt = |v: f64| format!("{v:.1}"); // the buggy formatter
        let new_fmt = |v: f64| v.to_string(); // the fix

        // OLD behavior: every one of these collapsed to the flat glyph, so the
        // waveform looked absent except on the loudest syllables.
        for rms in quiet_speech {
            assert_eq!(
                glyph_for_rms(old_fmt, rms),
                WAVE_GLYPHS[0],
                "sanity: the old {{:.1}} formatting really did flatten rms={rms}"
            );
        }

        // NEW behavior: the same values now produce visible, distinct bars.
        for rms in quiet_speech {
            assert_ne!(
                glyph_for_rms(new_fmt, rms),
                WAVE_GLYPHS[0],
                "rms={rms} must animate the waveform, not collapse to the flat glyph"
            );
        }

        // And the levels stay monotonic with loudness (no clipping surprises).
        let ordered = [0.004_f32, 0.02, 0.05, 0.1, 0.178];
        let levels: Vec<usize> = ordered
            .iter()
            .map(|&rms| {
                let stored = new_fmt(rms as f64);
                let parsed: f32 = stored.parse().unwrap();
                (parsed.sqrt() * 14.0).min(7.0) as usize
            })
            .collect();
        for pair in levels.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "louder rms must not map to a lower bar: {levels:?}"
            );
        }
    }
}
