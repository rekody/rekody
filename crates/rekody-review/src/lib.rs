//! rekody-review: the dataset review surface behind `rekody review`.
//!
//! A localhost page for cleaning the personal dictation dataset before
//! fine-tuning: play each clip, compare the saved transcript against an
//! optional second opinion from Whisper large-v3, and save the label you
//! trust. Decisions land in an append-only log; the manifest is rewritten
//! atomically with a one-time backup.
//!
//! Embedding contract (the Mac app relies on both):
//!   1. `serve` prints exactly one line to stdout before serving:
//!      `REVIEW_URL=http://127.0.0.1:<port>`. All logs go to stderr.
//!   2. `GET /api/ping` answers 200 with the `X-Rekody-Review: 1` response
//!      header, so a launcher can probe a port before spawning a second
//!      server.
//!
//! The headless `export` path mirrors that stdout discipline: `export`
//! prints exactly one line, `EXPORT_PATH=<absolute path>`, so scripts and
//! coding agents can drive an export without the server.
//!
//! Nothing leaves the machine: the server binds 127.0.0.1 by default (the
//! opt-in `lan` flag widens that to the local network so a phone on the
//! same Wi-Fi can open the page), and the sole outbound request this crate
//! can make is the one-time whisper model download from the Rekody Hugging
//! Face mirror (checksum-pinned).

mod server;
mod store;
mod teacher;
mod wer;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

/// How `serve` runs: where the dataset lives and how the server behaves.
pub struct ReviewOptions {
    /// Training-data root (holds manifest.jsonl, audio/, review sidecars).
    pub dir: PathBuf,
    /// Preferred port. When busy, the next nine ports are tried before
    /// giving up (7878 busy means 7879..7887 are candidates).
    pub port: u16,
    /// Launch the default browser at the review URL after binding.
    pub open_browser: bool,
    /// Exit cleanly after this many seconds without a single HTTP request.
    /// 0 disables auto-exit. The page pings every 30 s while a tab is open,
    /// so this fires only once every tab is closed.
    pub auto_exit_secs: u64,
    /// Also listen on the local network (bind 0.0.0.0) so a phone on the
    /// same Wi-Fi can open the page. Off by default: the dataset is
    /// personal audio, and anyone on the network could review, edit, and
    /// delete clips while the server runs.
    pub lan: bool,
}

/// Dataset root resolution shared by the binary and the CLI subcommand:
/// `$REKODY_TRAINING_DIR` or `~/.local/share/rekody/training-data`, the
/// same rule rekody-core's training_data module uses.
pub fn default_dataset_dir() -> PathBuf {
    store::dataset_dir()
}

/// How the headless `export` subcommand runs.
pub struct ExportOptions {
    /// Training-data root, resolved the same way as [`ReviewOptions::dir`].
    pub dir: PathBuf,
    /// Inclusive `YYYY-MM-DD` cutoff; `None` means today, which includes
    /// every reviewed clip recorded so far.
    pub until: Option<String>,
    /// Copy the audio files into the cut so the folder is self-contained.
    pub copy_audio: bool,
    /// Parent directory for the cut folder (default: `<dataset>/exports`).
    pub out: Option<PathBuf>,
}

/// Headless export, no server: load the dataset, write a reviewed-only cut
/// (see `store::Store::export_cut`), and print exactly one line to stdout:
/// `EXPORT_PATH=<absolute path>`, mirroring the `REVIEW_URL=` contract so a
/// script or coding agent can run the export and read the result. The human
/// summary and all logs go to stderr.
pub fn export(opts: ExportOptions) -> Result<()> {
    let store = store::Store::load(opts.dir.clone())
        .with_context(|| format!("loading dataset at {}", opts.dir.display()))?;
    let cut = store.export_cut(&store::CutOptions {
        until: opts.until,
        copy_audio: opts.copy_audio,
        out_dir: opts.out,
    })?;
    eprintln!(
        "{} clip{} · {} → {}",
        cut.clip_count,
        if cut.clip_count == 1 { "" } else { "s" },
        fmt_duration(cut.total_duration_secs),
        cut.dir.display()
    );
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        writeln!(out, "EXPORT_PATH={}", cut.dir.display())
            .context("writing EXPORT_PATH to stdout")?;
        out.flush().context("flushing stdout")?;
    }
    Ok(())
}

/// `2 h 10 m` / `10 m` / `45 s`, matching the page's export result line.
fn fmt_duration(secs: f64) -> String {
    let secs = secs.round() as u64;
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h} h {m} m")
    } else if m > 0 {
        format!("{m} m")
    } else {
        format!("{secs} s")
    }
}

/// Cross-thread server state that is not part of the dataset itself:
/// model-download progress and the teacher-thread guard.
#[derive(Default)]
pub(crate) struct RunState {
    /// `(received, total)` bytes while the model download is running.
    pub model_download: Option<(u64, u64)>,
    /// True while a teacher thread is alive (guards double-spawn).
    pub teacher_running: bool,
}

pub(crate) type SharedRun = Arc<Mutex<RunState>>;

/// Load the dataset, bind the server (with port fallback), print the
/// `REVIEW_URL=` contract line, optionally open the browser, and serve
/// until the process is killed or the idle auto-exit fires.
pub fn serve(opts: ReviewOptions) -> Result<()> {
    let root = opts.dir.clone();
    let loaded = store::Store::load(root.clone())
        .with_context(|| format!("loading dataset at {}", root.display()))?;
    tracing::info!(
        found = loaded.manifest_found(),
        clips = loaded.clip_count(),
        hours = format!("{:.2}", loaded.total_duration_secs() / 3600.0),
        scored = loaded.teacher_count(),
        decided = loaded.decision_count(),
        "dataset loaded"
    );
    let shared: store::SharedStore = Arc::new(Mutex::new(loaded));
    let run: SharedRun = Arc::new(Mutex::new(RunState::default()));

    // Sessions with the second opinion active (including datasets from
    // before review-state.json existed, which have review.jsonl only)
    // resume the teacher pass right away; it is resumable and skips
    // everything already scored.
    if matches!(
        store::lock(&shared).effective_mode(),
        store::Mode::Session {
            second_opinion: true
        }
    ) {
        spawn_teacher_once(&shared, &root, &run);
    }

    let (http, port) = server::bind(opts.port, opts.lan)?;
    let url = format!("http://127.0.0.1:{port}");

    // Locked contract: exactly one stdout line, before serving. The Mac app
    // parses it to find the page; everything else this process says goes to
    // stderr via tracing.
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        writeln!(out, "REVIEW_URL={url}").context("writing REVIEW_URL to stdout")?;
        out.flush().context("flushing stdout")?;
    }

    if opts.lan {
        match lan_ip() {
            Some(ip) => {
                eprintln!("On your phone (same Wi-Fi): http://{ip}:{port}");
                eprintln!(
                    "Heads up: while this server runs, anyone on your network can open, edit, and delete this dataset."
                );
            }
            None => eprintln!(
                "Listening on the whole network (0.0.0.0:{port}), but no LAN address could be detected."
            ),
        }
    }

    if opts.open_browser {
        open_in_browser(&url);
    }

    server::run(http, shared, run, root, opts.auto_exit_secs)
}

/// The machine's outbound-facing local address, for printing the phone URL
/// in `--lan` mode. A UDP connect does a route lookup without sending a
/// packet; the target is TEST-NET-1, never actually contacted.
fn lan_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

/// Start the teacher thread unless one is already running. Used at startup
/// (resumed sessions) and from POST /api/start (first run, or upgrading an
/// originals-only session). The thread clears its own guard on exit so a
/// failed pass (say, a download error) can be retried with another start.
pub(crate) fn spawn_teacher_once(shared: &store::SharedStore, root: &Path, run: &SharedRun) {
    {
        let mut r = run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if r.teacher_running {
            return;
        }
        r.teacher_running = true;
    }
    let store = shared.clone();
    let root = root.to_path_buf();
    let run_for_thread = run.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("teacher-pass".into())
        .spawn(move || {
            teacher::run(&store, &root, &run_for_thread);
            let mut r = run_for_thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            r.teacher_running = false;
            r.model_download = None;
        })
    {
        tracing::error!("spawning the teacher thread: {e}");
        let mut r = run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        r.teacher_running = false;
    }
}

/// Best-effort browser launch; a failure only logs, the server still runs.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
            tracing::warn!("could not open the browser: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    tracing::info!("open {url} in your browser");
}
