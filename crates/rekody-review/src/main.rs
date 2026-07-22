//! Standalone `rekody-review` binary: a thin wrapper over the library that
//! `rekody review` also uses. Kept for direct invocation and scripts; the
//! env/flag surface is unchanged ($REKODY_TRAINING_DIR plus --port).
//!
//! Logs go to stderr so stdout stays reserved for the single
//! `REVIEW_URL=...` contract line the library prints before serving.

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "rekody-review",
    about = "Rekody training-data label review (localhost)",
    version
)]
struct Args {
    /// Port for the localhost review page (the next nine are tried if busy).
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Also listen on the local network so a phone on the same Wi-Fi can
    /// open the page. Anyone on the network can edit and delete clips
    /// while the server runs.
    #[arg(long)]
    lan: bool,
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

    rekody_review::serve(rekody_review::ReviewOptions {
        dir: rekody_review::default_dataset_dir(),
        port: args.port,
        open_browser: false,
        auto_exit_secs: 0,
        lan: args.lan,
    })
}
