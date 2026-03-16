//! Chamgei CLI — standalone voice dictation pipeline.
//!
//! Run with: cargo run -p chamgei-core

use anyhow::Result;
use chamgei_core::{ChamgeiConfig, Pipeline, load_config};
use chamgei_core::onboarding;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chamgei=debug".parse().unwrap()),
        )
        .init();

    // Run first-time setup if needed.
    if onboarding::needs_onboarding() {
        onboarding::run_onboarding()?;
    }

    println!("╔══════════════════════════════════════╗");
    println!("║   Chamgei — Voice Dictation System   ║");
    println!("╚══════════════════════════════════════╝");
    println!();

    // Load config — check multiple paths in order:
    // 1. ~/.config/chamgei/config.toml (XDG-style, where we told the user to put it)
    // 2. ~/Library/Application Support/chamgei/config.toml (macOS native)
    // 3. ./config/default.toml (repo fallback)
    let config_candidates = [
        dirs::home_dir().map(|h| h.join(".config").join("chamgei").join("config.toml")),
        dirs::config_dir().map(|c| c.join("chamgei").join("config.toml")),
        Some(std::path::PathBuf::from("config/default.toml")),
    ];
    let config_path = config_candidates
        .iter()
        .filter_map(|p| p.as_ref())
        .find(|p| p.exists());

    let config = if let Some(path) = config_path {
        tracing::info!(path = %path.display(), "loading config");
        load_config(path.to_str().unwrap_or("config.toml"))?
    } else {
        tracing::info!("using default config (no config file found)");
        ChamgeiConfig::default()
    };

    println!("  Mode:     {}", config.activation_mode);
    println!("  Whisper:  {}", config.whisper_model);
    println!("  Inject:   {}", config.injection_method);

    // Show provider info
    if !config.providers.is_empty() {
        for p in &config.providers {
            println!("  LLM:      {}/{}", p.name, p.model);
        }
        println!();
        println!("  Cloud LLM formatting: ENABLED");
    } else if config.groq_api_key.is_some() || config.cerebras_api_key.is_some() {
        println!("  LLM:      {}", config.llm_provider);
        println!();
        println!("  Cloud LLM formatting: ENABLED");
    } else {
        println!();
        println!("  Cloud LLM formatting: DISABLED (no API keys)");
        println!("  Run 'chamgei' with no config to start the setup wizard.");
    }

    println!();
    println!("  Hotkeys:");
    println!("    Fn (hold)        — push-to-talk dictation");
    println!("    Fn + Space       — hands-free toggle (start/stop)");
    println!("    Fn + Enter       — command mode (transform selected text)");
    println!();
    println!("  Listening... (press Ctrl+C to quit)");
    println!();

    // Run the pipeline
    let pipeline = Pipeline::new(config)?;
    pipeline.run().await?;

    Ok(())
}
