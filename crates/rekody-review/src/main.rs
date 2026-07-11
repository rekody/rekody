//! rekody-review: a teacher-model pass plus a localhost review page for
//! cleaning the personal dictation dataset before fine-tuning.
//!
//! On startup the server comes up immediately at http://localhost:7878 and
//! a background thread transcribes every unscored clip with whisper
//! large-v3, appending to review.jsonl (resumable across runs). Decisions
//! made in the page land in decisions.jsonl; accept_teacher/edit also
//! rewrite the manifest entry (corrected + label_source) with the same
//! atomic tmp + rename pattern the rest of rekody uses.
//!
//! Nothing leaves the machine: the server binds 127.0.0.1 only, and the
//! sole outbound request this tool can make is the one-time whisper model
//! download from the Rekody Hugging Face mirror (checksum-pinned).

mod server;
mod store;
mod teacher;
mod wer;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "rekody-review",
    about = "Rekody training-data label review (localhost)",
    version
)]
struct Args {
    /// Port for the localhost review page.
    #[arg(long, default_value_t = 7878)]
    port: u16,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();

    let root = store::dataset_dir();
    let loaded = store::Store::load(root.clone())
        .with_context(|| format!("loading dataset at {}", root.display()))?;
    tracing::info!(
        clips = loaded.clip_count(),
        hours = format!("{:.2}", loaded.total_duration_secs() / 3600.0),
        transcribed = loaded.teacher_count(),
        decided = loaded.decision_count(),
        "dataset loaded"
    );
    let shared: store::SharedStore = std::sync::Arc::new(std::sync::Mutex::new(loaded));

    // Teacher pass in the background: the page is usable right away and
    // rows fill in as transcription progresses.
    let teacher_store = shared.clone();
    let teacher_root = root.clone();
    std::thread::Builder::new()
        .name("teacher-pass".into())
        .spawn(move || teacher::run(&teacher_store, &teacher_root))
        .context("spawning the teacher thread")?;

    server::serve(shared, root.join("audio"), args.port)
}
