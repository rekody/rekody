//! Chamgei CLI — standalone voice dictation pipeline with polished inline UI.
//!
//! Run with: cargo run -p chamgei-core

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use chamgei_core::{ChamgeiConfig, Pipeline, load_config};
use chamgei_core::onboarding;

// ── Session stats (shared across the tracing layer) ────────────────────────

/// Lightweight session-level counters updated from the tracing layer.
struct SessionStats {
    dictation_count: AtomicU64,
    total_audio_secs: Mutex<f64>,
    total_cost_usd: Mutex<f64>,
}

impl SessionStats {
    fn new() -> Self {
        Self {
            dictation_count: AtomicU64::new(0),
            total_audio_secs: Mutex::new(0.0),
            total_cost_usd: Mutex::new(0.0),
        }
    }

    fn record(&self, audio_secs: f64) {
        self.dictation_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut secs) = self.total_audio_secs.lock() {
            *secs += audio_secs;
        }
        // Estimated cost: ~130 input + 80 output tokens at Groq pricing
        // ($0.05/$0.08 per million tokens).
        let cost_per_dictation =
            (130.0 * 0.05 + 80.0 * 0.08) / 1_000_000.0;
        if let Ok(mut cost) = self.total_cost_usd.lock() {
            *cost += cost_per_dictation;
        }
    }

    fn summary_line(&self) -> String {
        let count = self.dictation_count.load(Ordering::Relaxed);
        let secs = self.total_audio_secs.lock().map(|s| *s).unwrap_or(0.0);
        format!(
            "  {} {} dictation{} · {:.1}s audio",
            style("Session:").dim(),
            count,
            if count == 1 { "" } else { "s" },
            secs,
        )
    }
}

// ── Custom tracing layer that drives the indicatif spinner ─────────────────

/// A [`tracing_subscriber::Layer`] that intercepts pipeline log messages and
/// translates them into indicatif spinner updates. This avoids modifying the
/// core pipeline while giving us a polished inline UI.
struct UiLayer {
    spinner: ProgressBar,
    session: Arc<SessionStats>,
    recording_start: Mutex<Option<Instant>>,
}

impl UiLayer {
    fn new(spinner: ProgressBar, session: Arc<SessionStats>) -> Self {
        Self {
            spinner,
            session,
            recording_start: Mutex::new(None),
        }
    }

    fn set_idle(&self) {
        let idle_style = ProgressStyle::with_template(
            "  {msg}",
        )
        .unwrap();
        self.spinner.set_style(idle_style);
        self.spinner.set_message(format!(
            "{} {} — waiting for Fn",
            style("○").cyan(),
            style("Ready").green().bold(),
        ));
        self.spinner.tick();
    }

    fn set_recording(&self) {
        if let Ok(mut start) = self.recording_start.lock() {
            *start = Some(Instant::now());
        }
        let rec_style = ProgressStyle::with_template(
            "  {spinner} {msg}",
        )
        .unwrap()
        .tick_strings(&["●", "◉", "○", "◉", "●"]);
        self.spinner.set_style(rec_style);
        self.spinner.set_message(format!(
            "{}",
            style("Recording...").red().bold(),
        ));
        self.spinner.enable_steady_tick(std::time::Duration::from_millis(250));
    }

    fn update_recording_elapsed(&self) {
        if let Ok(guard) = self.recording_start.lock() {
            if let Some(start) = *guard {
                let elapsed = start.elapsed().as_secs_f64();
                self.spinner.set_message(format!(
                    "{} — {:.1}s",
                    style("Recording").red().bold(),
                    elapsed,
                ));
            }
        }
    }

    fn set_processing(&self, detail: &str) {
        self.spinner.disable_steady_tick();
        let proc_style = ProgressStyle::with_template(
            "  {spinner:.cyan} {msg}",
        )
        .unwrap()
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "⠋"]);
        self.spinner.set_style(proc_style);
        self.spinner.set_message(format!(
            "{} — {}",
            style("Processing").cyan().bold(),
            style(detail).dim(),
        ));
        self.spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    }

    fn set_done(&self, text: &str, stt_ms: &str, llm_ms: Option<&str>) {
        self.spinner.disable_steady_tick();
        let done_style = ProgressStyle::with_template(
            "  {msg}",
        )
        .unwrap();
        self.spinner.set_style(done_style);

        let latency = if let Some(llm) = llm_ms {
            format!("STT {}ms + LLM {}ms", stt_ms, llm)
        } else {
            format!("STT {}ms", stt_ms)
        };

        // Truncate text for display if it's very long.
        let display_text = if text.len() > 60 {
            format!("{}…", &text[..59])
        } else {
            text.to_string()
        };

        self.spinner.set_message(format!(
            "{} \"{}\" ({})",
            style("✓").green().bold(),
            style(&display_text).white(),
            style(&latency).dim(),
        ));

        // Record session stats.
        let audio_secs = self
            .recording_start
            .lock()
            .ok()
            .and_then(|s| s.map(|start| start.elapsed().as_secs_f64()))
            .unwrap_or(0.0);
        self.session.record(audio_secs);

        // Print session stats below.
        self.spinner.println(self.session.summary_line());
    }

    fn set_error(&self, msg: &str) {
        self.spinner.disable_steady_tick();
        let err_style = ProgressStyle::with_template(
            "  {msg}",
        )
        .unwrap();
        self.spinner.set_style(err_style);
        self.spinner.set_message(format!(
            "{} {} — {}",
            style("✗").red().bold(),
            style("Error").red().bold(),
            style(msg).red(),
        ));
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
        // Extract the message from the tracing event.
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let msg = visitor.message;

        // Match against known pipeline log messages.
        if msg.contains("recording started") {
            self.set_recording();
        } else if msg.contains("recording stopped") {
            self.update_recording_elapsed();
            // Will transition to Processing when audio segment arrives.
        } else if msg.contains("received audio segment") {
            self.set_processing("transcribing...");
        } else if msg.contains("transcription complete") {
            // Store STT latency for the done message.
            let stt_ms = visitor.fields.get("latency_ms").cloned().unwrap_or_default();
            let text = visitor.fields.get("text").cloned().unwrap_or_default();
            if let Ok(mut guard) = STT_RESULT.lock() {
                *guard = Some(SttResult {
                    text,
                    latency_ms: stt_ms,
                    done_shown: false,
                });
            }
            self.set_processing("formatting with LLM...");
        } else if msg.contains("LLM formatting complete") {
            let llm_ms = visitor.fields.get("latency_ms").cloned().unwrap_or_default();
            let stt = STT_RESULT.lock().ok().and_then(|mut g| {
                if let Some(ref mut r) = *g {
                    r.done_shown = true;
                }
                g.clone()
            });
            if let Some(stt) = stt {
                self.set_done(&stt.text, &stt.latency_ms, Some(&llm_ms));
            }
        } else if msg.contains("text injected successfully") {
            // If LLM was not used, show done with just STT latency.
            let stt = STT_RESULT.lock().ok().and_then(|g| g.clone());
            if let Some(stt) = &stt {
                if !stt.done_shown {
                    self.set_done(&stt.text, &stt.latency_ms, None);
                }
            }
            // After injection, schedule a return to idle.
            let spinner = self.spinner.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                let idle_style = ProgressStyle::with_template("  {msg}").unwrap();
                spinner.set_style(idle_style);
                spinner.set_message(format!(
                    "{} {} — waiting for Fn",
                    style("○").cyan(),
                    style("Ready").green().bold(),
                ));
            });
        } else if msg.contains("LLM formatting failed") || msg.contains("failed to process audio") {
            let error_detail = visitor.fields.get("error").cloned().unwrap_or_else(|| msg.clone());
            self.set_error(&error_detail);
            // Return to idle after a delay.
            let spinner = self.spinner.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                let idle_style = ProgressStyle::with_template("  {msg}").unwrap();
                spinner.set_style(idle_style);
                spinner.set_message(format!(
                    "{} {} — waiting for Fn",
                    style("○").cyan(),
                    style("Ready").green().bold(),
                ));
            });
        } else if msg.contains("no LLM API keys") {
            // When no LLM is configured, transcription complete -> injection
            // happens without the LLM step. We handle the done state on injection.
        } else if msg.contains("empty transcript") {
            self.set_idle();
        }
    }
}

/// Holds the STT result between the transcription and LLM formatting steps.
#[derive(Clone)]
struct SttResult {
    text: String,
    latency_ms: String,
    /// Whether the done state was already shown (by LLM complete handler).
    done_shown: bool,
}

static STT_RESULT: std::sync::LazyLock<Mutex<Option<SttResult>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

// ── Tracing visitor to extract message and fields ──────────────────────────

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: std::collections::HashMap<String, String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let val = format!("{:?}", value);
        if field.name() == "message" {
            self.message = val.trim_matches('"').to_string();
        } else {
            self.fields.insert(field.name().to_string(), val.trim_matches('"').to_string());
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

// ── Startup banner ─────────────────────────────────────────────────────────

fn print_banner(config: &ChamgeiConfig) {
    println!();
    println!(
        "  {} {} — voice dictation",
        style("chamgei").cyan().bold(),
        style("v0.1.0").dim(),
    );
    println!();

    // Provider info.
    let provider_str = if !config.providers.is_empty() {
        config
            .providers
            .iter()
            .map(|p| format!("{}/{}", p.name, p.model))
            .collect::<Vec<_>>()
            .join(", ")
    } else if config.groq_api_key.is_some() || config.cerebras_api_key.is_some() {
        config.llm_provider.clone()
    } else {
        "none".to_string()
    };

    println!(
        "  {}   {}",
        style("Provider").dim(),
        style(&provider_str).bold(),
    );
    let stt_display = match config.stt_engine.to_lowercase().as_str() {
        "groq" => "Groq Cloud Whisper Large v3".to_string(),
        "deepgram" => "Deepgram Nova-2".to_string(),
        _ => format!("{} (local, Metal GPU)", config.whisper_model),
    };
    println!(
        "  {}        {}",
        style("STT").dim(),
        style(&stt_display).bold(),
    );
    println!(
        "  {}       {}",
        style("Mode").dim(),
        style(format_activation_mode(&config.activation_mode)).bold(),
    );
    println!();
    println!(
        "  {} {}  {}  {}  {}",
        style("Hotkeys:").dim(),
        style("Fn=dictate").white(),
        style("").white(),
        style("Fn+Enter=command").white(),
        style("Ctrl+C=quit").white(),
    );
    println!();
}

fn format_activation_mode(mode: &str) -> String {
    match mode.to_lowercase().as_str() {
        "toggle" => "push-to-talk (Fn hold)".to_string(),
        _ => "push-to-talk (Fn hold)".to_string(),
    }
}

// ── Main ───────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Run first-time setup if needed.
    if onboarding::needs_onboarding() {
        onboarding::run_onboarding()?;
    }

    // Load config — check multiple paths in order:
    // 1. ~/.config/chamgei/config.toml (XDG-style)
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
        load_config(path.to_str().unwrap_or("config.toml"))?
    } else {
        ChamgeiConfig::default()
    };

    // Print the styled startup banner.
    print_banner(&config);

    // Create the indicatif spinner for the status line.
    let spinner = ProgressBar::new_spinner();
    let idle_style = ProgressStyle::with_template("  {msg}").unwrap();
    spinner.set_style(idle_style);
    spinner.set_message(format!(
        "{} {} — waiting for Fn",
        style("○").cyan(),
        style("Ready").green().bold(),
    ));

    // Session stats tracker.
    let session = Arc::new(SessionStats::new());

    // Set up tracing with our custom UI layer.
    let ui_layer = UiLayer::new(spinner.clone(), Arc::clone(&session));

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,chamgei=debug".parse().unwrap());

    // Use the UI layer for spinner updates. Suppress the default fmt output
    // so tracing logs don't interfere with the inline UI.
    tracing_subscriber::registry()
        .with(env_filter)
        .with(ui_layer)
        .init();

    // Run the pipeline.
    let pipeline = Pipeline::new(config)?;
    pipeline.run().await?;

    spinner.finish_and_clear();
    Ok(())
}
