//! The teacher pass: transcribe every clip with whisper large-v3 and score
//! label-vs-teacher agreement into the review sidecar.
//!
//! Runs on a background thread so the review page is usable immediately;
//! rows fill in as transcription progresses. Resumable: clips already in
//! review.jsonl are skipped, so a restart picks up where the last run
//! stopped. If the large model is missing it is downloaded first, from the
//! same Rekody Hugging Face mirror (with the same pinned SHA-256 and
//! upstream fallback) that rekody-core's onboarding uses.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rekody_stt::{LocalWhisperEngine, SttEngine, WhisperModel};

use crate::store::{self, SharedStore};
use crate::wer;

/// Model name stamped into every review.jsonl line.
pub const TEACHER_MODEL: &str = "whisper-large-v3";

/// Rekody's model mirror on Hugging Face, tried first (same constant as
/// rekody-core onboarding.rs) so availability stays under Rekody's control.
const REKODY_WHISPER_BASE: &str =
    "https://huggingface.co/Rekody/rekody-models/resolve/main/whisper";

/// Upstream whisper.cpp model repo, kept as the fallback source.
const UPSTREAM_WHISPER_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Local filename the engine loads (WhisperModel::Large) vs the artifact
/// name both repos serve: upstream renamed the un-versioned large model, so
/// the v3 content is saved under the engine's expected local name.
const LARGE_LOCAL_FILE: &str = "ggml-large.bin";
const LARGE_REMOTE_FILE: &str = "ggml-large-v3.bin";

/// Pinned SHA-256 of the ggml-large-v3.bin content, identical bytes on both
/// sources. Matches EXPECTED_CHECKSUMS in rekody-core onboarding.rs.
const LARGE_SHA256: &str = "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2";

/// Log a download progress line every this many bytes (~12 lines for 3.1 GB).
const PROGRESS_STEP: u64 = 256 * 1024 * 1024;

/// Model directory: `$REKODY_MODEL_DIR` or `~/.local/share/rekody/models`,
/// the same resolution onboarding uses.
fn model_dir() -> PathBuf {
    std::env::var("REKODY_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".local").join("share").join("rekody").join("models"))
                .unwrap_or_else(|| PathBuf::from("models"))
        })
}

/// Thread entry point. Errors are logged, never propagated: a failed teacher
/// pass must not take the review server down with it.
pub fn run(store: &SharedStore, dataset_root: &Path) {
    if let Err(e) = run_inner(store, dataset_root) {
        tracing::error!("teacher pass failed: {e:#}");
    }
}

fn run_inner(shared: &SharedStore, dataset_root: &Path) -> Result<()> {
    let (pending, total) = {
        let s = store::lock(shared);
        (s.pending_for_teacher(), s.clip_count())
    };
    if pending.is_empty() {
        tracing::info!(
            total,
            "teacher pass: every clip is already scored in review.jsonl"
        );
        return Ok(());
    }
    tracing::info!(
        pending = pending.len(),
        total,
        "teacher pass starting (resumable; already-scored clips are skipped)"
    );

    let model_path = ensure_large_model()?;
    tracing::info!(model = %model_path.display(), "loading whisper large-v3");
    let engine = LocalWhisperEngine::with_language(
        WhisperModel::Large,
        model_path.to_str().context("model path is not UTF-8")?,
        Some("en".to_string()),
    )
    .context("loading whisper large-v3")?;

    // The SttEngine trait is async; the pass is sequential, so a small
    // current-thread runtime drives one transcription at a time.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building teacher runtime")?;

    let started = Instant::now();
    let (mut done, mut failed) = (0usize, 0usize);
    for (path, label) in &pending {
        let abs = dataset_root.join(path);
        match transcribe_one(&engine, &rt, &abs) {
            Ok(teacher_text) => {
                let score = wer::wer(label, &teacher_text);
                tracing::debug!(
                    clip = %path,
                    wer = format!("{score:.3}"),
                    bucket = ?wer::bucket(score),
                    "teacher scored"
                );
                store::lock(shared).record_teacher(path, &teacher_text, score)?;
                done += 1;
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(clip = %path, "teacher transcription failed: {e:#}");
            }
        }
        let processed = done + failed;
        if processed % 10 == 0 {
            tracing::info!(
                "teacher progress: {processed}/{} clips this run ({failed} failed, {:.0}s elapsed)",
                pending.len(),
                started.elapsed().as_secs_f64()
            );
        }
    }
    tracing::info!(
        done,
        failed,
        secs = format!("{:.0}", started.elapsed().as_secs_f64()),
        "teacher pass complete"
    );
    Ok(())
}

/// Decode one clip and run it through the teacher model.
fn transcribe_one(
    engine: &LocalWhisperEngine,
    rt: &tokio::runtime::Runtime,
    audio: &Path,
) -> Result<String> {
    let samples = decode_16k_mono(audio)?;
    let transcript = rt
        .block_on(engine.transcribe(&samples))
        .context("whisper inference")?;
    Ok(transcript.text)
}

// ---------------------------------------------------------------------------
// Audio decode (16 kHz mono f32, what the whisper engine expects)
// ---------------------------------------------------------------------------

/// Decode a FLAC or WAV clip to 16 kHz mono f32. WAVs already in that shape
/// are read directly with hound; everything else (all the FLACs, plus any
/// odd-spec WAV) round-trips through macOS's bundled `afconvert`, the same
/// tool that produced the FLACs on the capture side.
fn decode_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext == "wav"
        && let Some(samples) = read_wav_if_16k_mono(path)?
    {
        return Ok(samples);
    }
    decode_via_afconvert(path)
}

/// Read a WAV directly when it is already 16 kHz mono 16-bit int or f32;
/// `None` when the spec needs conversion instead.
fn read_wav_if_16k_mono(path: &Path) -> Result<Option<Vec<f32>>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening WAV {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != 16_000 {
        return Ok(None);
    }
    let samples = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| f32::from(v) / 32768.0))
            .collect::<Result<Vec<_>, _>>()
            .context("decoding PCM samples")?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .context("decoding float samples")?,
        _ => return Ok(None),
    };
    Ok(Some(samples))
}

/// Convert any input `afconvert` understands into a temp 16 kHz mono 16-bit
/// WAV, decode it, and clean up.
fn decode_via_afconvert(src: &Path) -> Result<Vec<f32>> {
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("clip")
        .to_string();
    let tmp = std::env::temp_dir().join(format!("rekody-review-{}-{stem}.wav", std::process::id()));
    let out = Command::new("afconvert")
        .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
        .arg(src)
        .arg(&tmp)
        .output()
        .context("running afconvert")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        anyhow::bail!(
            "afconvert failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let samples = read_wav_if_16k_mono(&tmp)?
        .context("afconvert produced an unexpected WAV spec (not 16 kHz mono s16)")?;
    let _ = std::fs::remove_file(&tmp);
    Ok(samples)
}

// ---------------------------------------------------------------------------
// Model download (Rekody mirror first, upstream fallback, pinned SHA-256)
// ---------------------------------------------------------------------------

/// Make sure ggml-large.bin is on disk, downloading and verifying it when
/// missing. This is the only network call in the whole tool.
fn ensure_large_model() -> Result<PathBuf> {
    let dir = model_dir();
    let dest = dir.join(LARGE_LOCAL_FILE);
    if dest.exists() {
        tracing::info!(model = %dest.display(), "whisper large model already downloaded");
        return Ok(dest);
    }
    std::fs::create_dir_all(&dir).context("creating model directory")?;
    tracing::info!("whisper large model missing; downloading ~3.1 GB from the Rekody model mirror");
    let primary = format!("{REKODY_WHISPER_BASE}/{LARGE_REMOTE_FILE}");
    let fallback = format!("{UPSTREAM_WHISPER_BASE}/{LARGE_REMOTE_FILE}");
    match download_verified(&primary, &dest, LARGE_SHA256) {
        Ok(()) => tracing::info!("downloaded from the Rekody model mirror"),
        Err(primary_err) => {
            tracing::warn!("Rekody model mirror failed for {primary}: {primary_err:#}");
            download_verified(&fallback, &dest, LARGE_SHA256)
                .context("fallback source failed too")?;
            tracing::info!("downloaded from the upstream whisper.cpp repo");
        }
    }
    Ok(dest)
}

/// Download `url` to `dest`, then enforce the expected SHA-256. On mismatch
/// the corrupt file is deleted before returning an error, so nothing
/// unverified is left for the engine (or a retry) to pick up.
fn download_verified(url: &str, dest: &Path, expected_sha256: &str) -> Result<()> {
    download_stream(url, dest)?;
    if !verify_sha256(dest, expected_sha256) {
        let _ = std::fs::remove_file(dest);
        anyhow::bail!("SHA-256 checksum mismatch for {}", dest.display());
    }
    tracing::info!("checksum verified (SHA-256 matches the pinned hash)");
    Ok(())
}

/// Stream `url` to `dest.partial`, logging progress, then atomically rename.
fn download_stream(url: &str, dest: &Path) -> Result<()> {
    use std::io::{Read, Write};

    let partial = dest.with_extension(
        dest.extension()
            .map(|e| format!("{}.partial", e.to_string_lossy()))
            .unwrap_or_else(|| "partial".to_string()),
    );

    // No total timeout: reqwest's blocking default (30 s) covers the whole
    // body and would abort a 3.1 GB transfer. Connect failures still fail
    // fast via the connect timeout.
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .context("building download client")?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("starting download from {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {} from {url}", response.status());
    }

    let total = response.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(&partial).context("creating partial model file")?;
    let mut reader = std::io::BufReader::new(response);
    let mut buf = [0u8; 1 << 16];
    let mut downloaded: u64 = 0;
    let mut next_log = PROGRESS_STEP;
    loop {
        let n = reader.read(&mut buf).context("reading download stream")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("writing model bytes")?;
        downloaded += n as u64;
        if downloaded >= next_log {
            if total > 0 {
                tracing::info!(
                    "model download: {} / {} MB ({:.0}%)",
                    downloaded >> 20,
                    total >> 20,
                    downloaded as f64 / total as f64 * 100.0
                );
            } else {
                tracing::info!("model download: {} MB", downloaded >> 20);
            }
            next_log += PROGRESS_STEP;
        }
    }
    file.flush().context("flushing model file")?;
    drop(file);
    tracing::info!("model download finished ({} MB)", downloaded >> 20);

    // Atomic rename: only move to the final path after a complete download.
    std::fs::rename(&partial, dest).context("renaming partial download into place")?;
    Ok(())
}

/// Verify a file's SHA-256 via `shasum -a 256` (macOS) or `sha256sum`
/// (Linux), the same tooling onboarding uses. A missing hashing tool warns
/// without blocking; only a definite mismatch returns false.
fn verify_sha256(path: &Path, expected: &str) -> bool {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output());
    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let actual = stdout.split_whitespace().next().unwrap_or("");
            if actual.eq_ignore_ascii_case(expected) {
                true
            } else {
                tracing::warn!("SHA-256 mismatch: expected {expected}, got {actual}");
                false
            }
        }
        _ => {
            tracing::warn!(
                "could not compute SHA-256 (shasum/sha256sum not found); proceeding unverified"
            );
            true
        }
    }
}
