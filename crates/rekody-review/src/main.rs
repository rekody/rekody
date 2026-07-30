//! Standalone `rekody-review` binary: a thin wrapper over the library that
//! `rekody review` also uses. Kept for direct invocation and scripts; the
//! env/flag surface is unchanged ($REKODY_TRAINING_DIR plus --port).
//!
//! Logs go to stderr so stdout stays reserved for the single contract line
//! each mode prints: `REVIEW_URL=...` before serving, `EXPORT_PATH=...`
//! after a headless export.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rekody-review",
    about = "Rekody training-data label review (localhost)",
    version
)]
struct Args {
    /// Dataset directory (default: $REKODY_TRAINING_DIR, then
    /// ~/.local/share/rekody/training-data). Global so it also works
    /// after `export`, where scripts and coding agents put it.
    #[arg(long, global = true)]
    dir: Option<std::path::PathBuf>,
    /// Port for the localhost review page (the next nine are tried if busy).
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Also listen on the local network so a phone on the same Wi-Fi can
    /// open the page. Anyone on the network can edit and delete clips
    /// while the server runs.
    #[arg(long)]
    lan: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
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
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();

    let dir = args.dir.unwrap_or_else(rekody_review::default_dataset_dir);
    match args.command {
        Some(Cmd::Export {
            until,
            copy_audio,
            out,
        }) => rekody_review::export(rekody_review::ExportOptions {
            dir,
            until,
            copy_audio,
            out,
        }),
        None => rekody_review::serve(rekody_review::ReviewOptions {
            dir,
            port: args.port,
            open_browser: false,
            auto_exit_secs: 0,
            lan: args.lan,
        }),
    }
}
