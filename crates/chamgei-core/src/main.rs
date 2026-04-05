//! chamgei — voice dictation CLI
//!
//! Entrypoint for the chamgei binary. Handles subcommand dispatch and the
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

use chamgei_core::onboarding;
use chamgei_core::{ChamgeiConfig, Pipeline, load_config};

// ── CLI definition ─────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "chamgei",
    version = env!("CARGO_PKG_VERSION"),
    about = "Voice dictation — speak, it types",
    long_about = "chamgei listens for your voice while you hold ⌥Space, \
transcribes it, optionally cleans it up with an LLM, \
and types the result into the focused window.",
    disable_help_subcommand = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Enable verbose tracing output
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run first-time setup or reconfigure
    Setup,
    /// Show or edit current configuration
    Config {
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
    },
    /// Check STT and LLM provider connectivity
    Doctor,
    /// Manage API keys stored in the system keychain
    Key {
        #[command(subcommand)]
        action: KeyCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print current configuration (default)
    Show,
    /// Open config file in $EDITOR
    Edit,
    /// Print the path of the config file
    Path,
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
    /// List which providers have keys stored
    List,
}

// ── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => run_dictation(cli.verbose).await,
        Some(Cmd::Setup) => cmd_setup(),
        Some(Cmd::Config { action }) => cmd_config(action),
        Some(Cmd::History { count, search, app, full, stats, json, copy }) => {
            cmd_history(count, search, app, full, stats, json, copy)
        }
        Some(Cmd::Doctor) => cmd_doctor().await,
        Some(Cmd::Key { action }) => cmd_key(action),
    }
}

// ── Subcommand: setup ────────────────────────────────────────────────────────

fn cmd_setup() -> Result<()> {
    onboarding::run_onboarding()
}

// ── Subcommand: config ───────────────────────────────────────────────────────

fn cmd_config(action: Option<ConfigCmd>) -> Result<()> {
    let config_path = find_config_path();
    let config = load_config_or_default(&config_path);

    match action.unwrap_or(ConfigCmd::Show) {
        ConfigCmd::Show => print_config(&config, &config_path),
        ConfigCmd::Path => {
            match &config_path {
                Some(p) => println!("{}", p),
                None => println!("{}", style("no config file found").yellow()),
            }
        }
        ConfigCmd::Edit => {
            let path = match &config_path {
                Some(p) => p.clone(),
                None => {
                    let default = default_config_path();
                    println!(
                        "{} {}",
                        style("Creating config at").dim(),
                        style(&default).cyan()
                    );
                    default
                }
            };
            let editor = std::env::var("EDITOR")
                .or_else(|_| std::env::var("VISUAL"))
                .unwrap_or_else(|_| "nano".to_string());
            std::process::Command::new(&editor).arg(&path).status()?;
        }
    }
    Ok(())
}

fn print_config(config: &ChamgeiConfig, path: &Option<String>) {
    println!();
    println!(
        "  {}  {}",
        style("Configuration").bold(),
        style("─".repeat(42)).dim()
    );
    println!();

    match path {
        Some(p) => println!("  {}  {}", style("File  ").dim(), style(p).cyan()),
        None => println!(
            "  {}  {}",
            style("File  ").dim(),
            style("(no config file — using defaults)").yellow()
        ),
    }
    println!();

    // STT
    let stt_display = stt_display_name(config);
    println!("  {}", style("STT").bold());
    println!("    {}  {}", style("Engine").dim(), style(&stt_display).white());
    if let Some(key) = &config.deepgram_api_key {
        println!(
            "    {}  {}",
            style("Key   ").dim(),
            style(mask_key(key)).dim()
        );
    }
    println!();

    // LLM
    println!("  {}", style("LLM Providers").bold());
    if config.providers.is_empty() {
        // Legacy
        let has_groq = config.groq_api_key.as_ref().is_some_and(|k| !k.is_empty());
        let has_cerebras = config
            .cerebras_api_key
            .as_ref()
            .is_some_and(|k| !k.is_empty());
        if has_groq {
            println!("    {}  {}", style("1").dim(), style("groq  (legacy key)").white());
        }
        if has_cerebras {
            println!("    {}  {}", style("2").dim(), style("cerebras  (legacy key)").white());
        }
        if !has_groq && !has_cerebras {
            println!("    {}", style("none configured  — run: chamgei setup").yellow());
        }
    } else {
        for (i, p) in config.providers.iter().enumerate() {
            let key_hint = if p.api_key.is_empty() {
                style("(no key)").yellow().to_string()
            } else {
                style(mask_key(&p.api_key)).dim().to_string()
            };
            println!(
                "    {}  {}/{} {}",
                style(format!("{}", i + 1)).dim(),
                style(&p.name).white(),
                style(&p.model).white(),
                key_hint,
            );
        }
    }
    println!();

    // Options
    println!("  {}", style("Options").bold());
    println!(
        "    {}  {}",
        style("Mode  ").dim(),
        style(format_activation_mode(&config.activation_mode)).white()
    );
    println!(
        "    {}  {}",
        style("Inject").dim(),
        style(&config.injection_method).white()
    );
    println!(
        "    {}  {}",
        style("VAD   ").dim(),
        style(format!("{}", config.vad_threshold)).white()
    );
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

fn cmd_history(
    count: usize,
    search: Option<String>,
    app_filter: Option<String>,
    full: bool,
    stats: bool,
    json_out: bool,
    copy_nth: Option<usize>,
) -> Result<()> {
    let history = chamgei_core::history::History::load();
    let all = history.entries();

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
            if let Some(ref app) = app_filter {
                if !e.app_context.to_lowercase().contains(&app.to_lowercase()) {
                    return false;
                }
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
                println!(
                    "  {}",
                    style("No history yet — start dictating!").dim()
                );
            }
            println!();
            return Ok(());
        }
        println!("  {}  {}", style("Recent").bold(), style("─".repeat(45)).dim());
        println!();
    } else {
        // Header
        let mut header = format!("  {}  {}", style("History").bold(), style("─".repeat(40)).dim());
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

    println!();
    println!(
        "  {}  {}",
        style("Provider Health Check").bold(),
        style("─".repeat(31)).dim()
    );
    println!();

    // STT check
    println!("  {}", style("STT").bold());
    let stt_name = stt_display_name(&config);
    match config.stt_engine.to_lowercase().as_str() {
        "deepgram" => {
            let key = config.deepgram_api_key.as_deref().unwrap_or("");
            if key.is_empty() {
                println!(
                    "    {}  {}  {}",
                    style("✗").red().bold(),
                    style(&stt_name).white(),
                    style("no API key — run: chamgei key set deepgram").yellow()
                );
            } else {
                let t = Instant::now();
                let ok = reqwest::Client::new()
                    .get("https://api.deepgram.com/v1/projects")
                    .header("Authorization", format!("Token {}", key))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                let ms = t.elapsed().as_millis();
                if ok {
                    println!(
                        "    {}  {}  {}",
                        style("✓").green().bold(),
                        style(&stt_name).white(),
                        style(format!("{}ms", ms)).dim()
                    );
                } else {
                    println!(
                        "    {}  {}  {}",
                        style("✗").red().bold(),
                        style(&stt_name).white(),
                        style("auth failed — run: chamgei key set deepgram").yellow()
                    );
                }
            }
        }
        "groq" => {
            let key = config.groq_api_key.as_deref().unwrap_or("");
            check_openai_compat_provider("Groq Whisper", "https://api.groq.com/openai/v1/models", key).await;
        }
        _ => {
            println!(
                "    {}  {}",
                style("○").cyan(),
                style("Local Whisper (no network check needed)").dim()
            );
        }
    }
    println!();

    // LLM providers
    println!("  {}", style("LLM").bold());
    if config.providers.is_empty() && config.groq_api_key.is_none() && config.cerebras_api_key.is_none() {
        println!("    {}", style("none configured — run: chamgei setup").yellow());
    } else if !config.providers.is_empty() {
        for p in &config.providers {
            match p.name.as_str() {
                "ollama" | "lm-studio" | "vllm" => {
                    let url = p.base_url.as_deref().unwrap_or("http://localhost:11434");
                    let t = Instant::now();
                    let ok = reqwest::Client::new()
                        .get(url)
                        .send()
                        .await
                        .is_ok();
                    let ms = t.elapsed().as_millis();
                    let status = if ok {
                        format!("{}  {}", style("✓").green().bold(), style(format!("{}ms", ms)).dim())
                    } else {
                        format!("{}  {}", style("✗").red().bold(), style("not running").yellow())
                    };
                    println!("    {}  {}/{}", status, style(&p.name).white(), style(&p.model).dim());
                }
                "gemini" => {
                    let url = "https://generativelanguage.googleapis.com/v1beta/openai/models";
                    check_openai_compat_provider_keyed(&format!("{}/{}", p.name, p.model), url, &p.api_key, "x-goog-api-key").await;
                }
                _ => {
                    let url = p.base_url.clone().unwrap_or_else(|| provider_models_url(&p.name));
                    check_openai_compat_provider(&format!("{}/{}", p.name, p.model), &url, &p.api_key).await;
                }
            }
        }
    } else {
        // Legacy
        if let Some(key) = &config.groq_api_key {
            check_openai_compat_provider("groq", "https://api.groq.com/openai/v1/models", key).await;
        }
        if let Some(key) = &config.cerebras_api_key {
            check_openai_compat_provider("cerebras", "https://api.cerebras.ai/v1/models", key).await;
        }
    }
    println!();

    // System
    println!("  {}", style("System").bold());
    #[cfg(target_os = "macos")]
    {
        let mic = check_macos_permission("kTCCServiceMicrophone");
        let acc = check_macos_permission("kTCCServiceAccessibility");
        print_permission("Microphone", mic);
        print_permission("Accessibility", acc);
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("    {}  {}", style("○").cyan(), style("System checks not available on this platform").dim());
    }
    println!();

    Ok(())
}

async fn check_openai_compat_provider(label: &str, url: &str, key: &str) {
    if key.is_empty() {
        println!(
            "    {}  {}  {}",
            style("✗").red().bold(),
            style(label).white(),
            style("no API key — run: chamgei key set <provider>").yellow()
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
            "    {}  {}  {}",
            style("✓").green().bold(),
            style(label).white(),
            style(format!("{}ms", ms)).dim()
        );
    } else {
        println!(
            "    {}  {}  {}",
            style("✗").red().bold(),
            style(label).white(),
            style("auth failed — check your API key").yellow()
        );
    }
}

async fn check_openai_compat_provider_keyed(label: &str, url: &str, key: &str, header: &str) {
    if key.is_empty() {
        println!(
            "    {}  {}  {}",
            style("✗").red().bold(),
            style(label).white(),
            style("no API key").yellow()
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
            "    {}  {}  {}",
            style("✓").green().bold(),
            style(label).white(),
            style(format!("{}ms", ms)).dim()
        );
    } else {
        println!(
            "    {}  {}  {}",
            style("✗").red().bold(),
            style(label).white(),
            style("auth failed — check your API key").yellow()
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
fn check_macos_permission(service: &str) -> bool {
    // Best-effort heuristic: try to read TCC database
    let db = std::path::Path::new("/Library/Application Support/com.apple.TCC/TCC.db");
    if !db.exists() {
        return true; // can't tell, assume ok
    }
    // Fall back to just returning true — macOS will show a system dialog if missing.
    let _ = service;
    true
}

#[cfg(target_os = "macos")]
fn print_permission(name: &str, ok: bool) {
    if ok {
        println!("    {}  {}", style("✓").green().bold(), style(name).white());
    } else {
        println!(
            "    {}  {}  {}",
            style("✗").red().bold(),
            style(name).white(),
            style("open System Settings → Privacy").yellow()
        );
    }
}

// ── Subcommand: key ──────────────────────────────────────────────────────────

fn cmd_key(action: KeyCmd) -> Result<()> {
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
            println!("\n  {}  {} key saved.", style("✓").green().bold(), style(&provider).white());
        }
        KeyCmd::Delete { provider } => {
            match delete_keychain_key(&provider) {
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
            }
        }
        KeyCmd::List => {
            println!();
            println!(
                "  {}  {}",
                style("Stored Keys").bold(),
                style("─".repeat(40)).dim()
            );
            println!();
            let providers = &[
                "groq", "deepgram", "anthropic", "openai", "gemini", "cerebras", "together",
                "openrouter", "fireworks",
            ];
            let mut any = false;
            for p in providers {
                if let Ok(key) = get_keychain_key(p) {
                    println!("    {}  {}  {}", style("✓").green(), style(*p).white(), style(mask_key(&key)).dim());
                    any = true;
                }
            }
            if !any {
                println!("    {}", style("No keys stored. Run: chamgei key set <provider>").dim());
            }
            println!();
        }
    }
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
    let entry = keyring::Entry::new("com.chamgei.voice", provider)?;
    entry.set_password(key)?;
    Ok(())
}

fn delete_keychain_key(provider: &str) -> Result<()> {
    let entry = keyring::Entry::new("com.chamgei.voice", provider)?;
    entry.delete_credential()?;
    Ok(())
}

fn get_keychain_key(provider: &str) -> Result<String> {
    let entry = keyring::Entry::new("com.chamgei.voice", provider)?;
    Ok(entry.get_password()?)
}

// ── Config helpers ───────────────────────────────────────────────────────────

fn find_config_path() -> Option<String> {
    let candidates = [
        dirs::home_dir().map(|h| h.join(".config").join("chamgei").join("config.toml")),
        dirs::config_dir().map(|c| c.join("chamgei").join("config.toml")),
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
                .join("chamgei")
                .join("config.toml")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| "~/.config/chamgei/config.toml".to_string())
}

fn load_config_or_default(path: &Option<String>) -> ChamgeiConfig {
    path.as_deref()
        .and_then(|p| load_config(p).ok())
        .unwrap_or_default()
}

fn stt_display_name(config: &ChamgeiConfig) -> String {
    match config.stt_engine.to_lowercase().as_str() {
        "groq" => "Groq Cloud Whisper Large v3".to_string(),
        "deepgram" => "Deepgram Nova-3".to_string(),
        "cohere" => format!("Cohere local (port {})", config.cohere_stt_port),
        _ => format!("Local Whisper ({})", config.whisper_model),
    }
}

fn format_activation_mode(mode: &str) -> &str {
    match mode.to_lowercase().as_str() {
        "toggle" => "toggle — tap ⌥Space to start/stop",
        _ => "push-to-talk — hold ⌥Space",
    }
}

// ── Live dictation pipeline ──────────────────────────────────────────────────

async fn run_dictation(verbose: bool) -> Result<()> {
    // If no config exists, run onboarding first.
    if onboarding::needs_onboarding() {
        onboarding::run_onboarding()?;
    }

    let config_path = find_config_path();
    let mut config = load_config_or_default(&config_path);

    // Pull missing API keys from the keychain into config at runtime.
    inject_keychain_keys(&mut config);

    // Print the startup banner.
    print_banner(&config);

    // Create the status spinner.
    let spinner = ProgressBar::new_spinner();
    set_idle_style(&spinner);

    // Session stats tracker.
    let session = Arc::new(SessionStats::new());

    // Set up tracing with our custom UI layer.
    let ui_layer = UiLayer::new(spinner.clone(), Arc::clone(&session));

    let level = if verbose { "debug" } else { "info" };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| format!("{},chamgei=debug", level).parse().unwrap());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(ui_layer)
        .init();

    let pipeline = Pipeline::new(config)?;
    pipeline.run().await?;

    spinner.finish_and_clear();
    Ok(())
}

/// Pull API keys from the keychain into the config struct if they are missing.
fn inject_keychain_keys(config: &mut ChamgeiConfig) {
    // Deepgram STT key
    if config.deepgram_api_key.is_none() || config.deepgram_api_key.as_deref() == Some("") {
        if let Ok(key) = get_keychain_key("deepgram") {
            if !key.is_empty() {
                config.deepgram_api_key = Some(key);
            }
        }
        // Also try the legacy account name
        if config.deepgram_api_key.is_none() {
            if let Ok(key) = get_keychain_key("deepgram_api_key") {
                if !key.is_empty() {
                    config.deepgram_api_key = Some(key);
                }
            }
        }
    }
    // Groq key for STT or LLM
    if config.groq_api_key.is_none() || config.groq_api_key.as_deref() == Some("") {
        if let Ok(key) = get_keychain_key("groq") {
            if !key.is_empty() {
                config.groq_api_key = Some(key.clone());
                // Update existing groq provider entry, or create one if absent.
                let existing = config.providers.iter_mut().find(|p| p.name == "groq");
                if let Some(p) = existing {
                    if p.api_key.is_empty() {
                        p.api_key = key;
                    }
                } else {
                    config.providers.push(chamgei_core::ProviderConfig {
                        name: "groq".into(),
                        api_key: key,
                        model: "openai/gpt-oss-20b".into(),
                        base_url: None,
                    });
                }
            }
        }
    }
    // Inject keychain keys into any providers array entries that lack a key.
    for p in config.providers.iter_mut() {
        if p.api_key.is_empty() {
            if let Ok(key) = get_keychain_key(&p.name) {
                if !key.is_empty() {
                    p.api_key = key;
                }
            }
        }
    }
}

// ── Startup banner ───────────────────────────────────────────────────────────

fn print_banner(config: &ChamgeiConfig) {
    println!();

    // Title line
    println!(
        "  {}  {}",
        style(format!("chamgei  v{}", env!("CARGO_PKG_VERSION")))
            .cyan()
            .bold(),
        style("─".repeat(40)).dim(),
    );
    println!();

    // STT
    let stt = stt_display_name(config);
    println!("  {}  {}", style("STT   ").dim(), style(&stt).white().bold());

    // LLM — show effective state, not just what's configured.
    let llm_active = chamgei_core::has_llm_providers(config);
    if llm_active {
        let names: Vec<_> = config
            .providers
            .iter()
            .map(|p| format!("{}/{}", p.name, p.model))
            .collect();
        println!(
            "  {}  {}",
            style("LLM   ").dim(),
            style(names.join("  ›  ")).white()
        );
    } else {
        let reason = if config.providers.is_empty() {
            style("none").dim().to_string()
        } else if config.llm_enabled == Some(false) {
            style("off").dim().to_string()
        } else {
            // Auto-disabled because Deepgram smart_format handles formatting.
            format!(
                "{}  {}",
                style("none").dim(),
                style("(Deepgram smart_format handles formatting)").dim()
            )
        };
        println!("  {}  {}", style("LLM   ").dim(), reason);
    }

    // Mode
    println!(
        "  {}  {}",
        style("Mode  ").dim(),
        style(format_activation_mode(&config.activation_mode)).white()
    );

    println!();
    println!("  {}", style("─".repeat(52)).dim());
    println!(
        "  {}  {}   {}  {}",
        style("⌥Space").white().bold(),
        style("hold to dictate").dim(),
        style("Ctrl+C").white().bold(),
        style("quit").dim(),
    );
    println!("  {}", style("─".repeat(52)).dim());
    println!();
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
        format!(
            "     {} {} {} · {:.1}s audio",
            style("Session:").dim(),
            style(count).dim(),
            style(if count == 1 { "dictation" } else { "dictations" }).dim(),
            secs,
        )
    }
}

// ── Spinner style helpers ────────────────────────────────────────────────────

/// Single shared style used for every state — avoids the new-line glitch
/// caused by swapping styles while `enable_steady_tick` is running.
fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("  {msg}").unwrap()
}

fn set_spinner_msg(spinner: &ProgressBar, msg: impl Into<String>) {
    spinner.set_style(spinner_style());
    spinner.set_message(msg.into());
    spinner.tick();
}

fn set_idle_style(spinner: &ProgressBar) {
    set_spinner_msg(
        spinner,
        format!(
            "{}  {}",
            style("○").cyan(),
            style("Ready — hold ⌥Space to dictate").dim(),
        ),
    );
}

fn set_recording_style(spinner: &ProgressBar, elapsed_secs: Option<f64>) {
    let msg = match elapsed_secs {
        Some(s) => format!(
            "{}  {}  {}",
            style("●").red().bold(),
            style("Recording").red().bold(),
            style(format!("{:.1}s", s)).red().dim(),
        ),
        None => format!(
            "{}  {}",
            style("●").red().bold(),
            style("Recording").red().bold(),
        ),
    };
    set_spinner_msg(spinner, msg);
}

fn set_processing_style(spinner: &ProgressBar, detail: &str) {
    set_spinner_msg(
        spinner,
        format!(
            "{}  {}  {}",
            style("◌").cyan(),
            style("Processing").cyan().bold(),
            style(detail).dim(),
        ),
    );
}

fn set_done_style(spinner: &ProgressBar, text: &str, stt_ms: &str, llm_ms: Option<&str>) {
    let latency = match llm_ms {
        Some(l) => format!("{}ms STT  ·  {}ms LLM", stt_ms, l),
        None => format!("{}ms STT", stt_ms),
    };
    let display = if text.len() > 60 {
        format!("{}…", &text[..59])
    } else {
        text.to_string()
    };
    set_spinner_msg(
        spinner,
        format!(
            "{}  \"{}\"  {}",
            style("✓").green().bold(),
            style(&display).white(),
            style(format!("({})", latency)).dim(),
        ),
    );
}

fn set_error_style(spinner: &ProgressBar, msg: &str) {
    let short = if msg.len() > 70 { &msg[..70] } else { msg };
    set_spinner_msg(
        spinner,
        format!(
            "{}  {}  {}",
            style("✗").red().bold(),
            style("Error").red().bold(),
            style(short).red().dim(),
        ),
    );
}

// ── Tracing → UI layer ───────────────────────────────────────────────────────

struct UiLayer {
    spinner: ProgressBar,
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
    fn new(spinner: ProgressBar, session: Arc<SessionStats>) -> Self {
        Self {
            spinner,
            session,
            recording_start: Mutex::new(None),
            stt_result: Mutex::new(None),
        }
    }

    fn on_recording_started(&self) {
        if let Ok(mut start) = self.recording_start.lock() {
            *start = Some(Instant::now());
        }
        set_recording_style(&self.spinner, None);
    }

    fn on_recording_stopped(&self) {
        // Show elapsed time as we transition to processing.
        let elapsed = self
            .recording_start
            .lock()
            .ok()
            .and_then(|g| g.map(|s| s.elapsed().as_secs_f64()));
        set_recording_style(&self.spinner, elapsed);
    }

    fn on_transcription_complete(&self, text: &str, latency_ms: &str) {
        if let Ok(mut guard) = self.stt_result.lock() {
            *guard = Some(SttResult {
                text: text.to_string(),
                latency_ms: latency_ms.to_string(),
                done_shown: false,
            });
        }
        set_processing_style(&self.spinner, "formatting with LLM…");
    }

    fn on_llm_complete(&self, llm_ms: &str) {
        let stt = self.stt_result.lock().ok().and_then(|mut g| {
            if let Some(ref mut r) = *g {
                r.done_shown = true;
            }
            g.clone()
        });
        if let Some(stt) = stt {
            set_done_style(&self.spinner, &stt.text, &stt.latency_ms, Some(llm_ms));
            self.record_and_show_stats();
        }
    }

    fn on_injected(&self) {
        let stt = self.stt_result.lock().ok().and_then(|g| g.clone());
        if let Some(ref stt) = stt
            && !stt.done_shown
        {
            set_done_style(&self.spinner, &stt.text, &stt.latency_ms, None);
            self.record_and_show_stats();
        }
        self.schedule_idle_reset();
    }

    fn on_error(&self, msg: &str) {
        set_error_style(&self.spinner, msg);
        self.schedule_idle_reset();
    }

    fn record_and_show_stats(&self) {
        let audio_secs = self
            .recording_start
            .lock()
            .ok()
            .and_then(|s| s.map(|start| start.elapsed().as_secs_f64()))
            .unwrap_or(0.0);
        self.session.record(audio_secs);
        self.spinner.println(self.session.summary_line());
    }

    fn schedule_idle_reset(&self) {
        let spinner = self.spinner.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            set_idle_style(&spinner);
        });
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

        if msg.contains("recording started") {
            self.on_recording_started();
        } else if msg.contains("recording stopped") {
            self.on_recording_stopped();
        } else if msg.contains("received audio segment") {
            set_processing_style(&self.spinner, "transcribing…");
        } else if msg.contains("transcription complete") {
            let text = visitor.fields.get("text").cloned().unwrap_or_default();
            let latency = visitor.fields.get("latency_ms").cloned().unwrap_or_default();
            self.on_transcription_complete(&text, &latency);
        } else if msg.contains("LLM formatting complete") {
            let latency = visitor.fields.get("latency_ms").cloned().unwrap_or_default();
            self.on_llm_complete(&latency);
        } else if msg.contains("text injected successfully") {
            self.on_injected();
        } else if msg.contains("LLM formatting failed") || msg.contains("failed to process audio") {
            let err = visitor
                .fields
                .get("error")
                .cloned()
                .unwrap_or_else(|| msg.clone());
            self.on_error(&err);
        } else if msg.contains("empty transcript") {
            set_idle_style(&self.spinner);
        } else if msg.contains("no LLM API keys") {
            // Will show done on injection without LLM step.
        }
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
            self.fields.insert(field.name().to_string(), value.to_string());
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_string(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.insert(field.name().to_string(), format!("{:.1}", value));
    }
}
