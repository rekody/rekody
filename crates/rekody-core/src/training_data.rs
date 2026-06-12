//! Opt-in local capture of (audio, transcript) pairs for personal ASR
//! fine-tuning (`save_training_data = true` in config.toml).
//!
//! Every dictation saves its 16kHz mono audio as FLAC (lossless, ~50-60% of
//! WAV for speech) plus a line in a NeMo-style JSONL manifest:
//!
//!   ~/.local/share/rekody/training-data/
//!     audio/2026-06-12T08-31-02-x4f2.flac
//!     manifest.jsonl   {"audio_filepath": "audio/…", "text": …, "duration": …}
//!
//! The manifest `text` is the RAW engine transcript (the acoustic label),
//! not the LLM-cleaned text — cleaned text diverges from what was actually
//! said (retractions removed, numbers reformatted) and would mistrain the
//! acoustic model. Style pairs (raw → cleaned) already live in history.json.
//!
//! FLAC encoding shells out to macOS's bundled `afconvert`; if that fails
//! the WAV is kept so no audio is ever lost. Nothing leaves the machine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Warn (once per process) when the dataset grows past this many bytes.
const SIZE_WARN_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GB
static SIZE_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Dataset root: `$REKODY_TRAINING_DIR` or `~/.local/share/rekody/training-data`.
pub fn dataset_dir() -> PathBuf {
    std::env::var("REKODY_TRAINING_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| {
                    h.join(".local")
                        .join("share")
                        .join("rekody")
                        .join("training-data")
                })
                .unwrap_or_else(|| PathBuf::from("training-data"))
        })
}

/// Save one utterance as a FLAC + manifest pair. Returns the audio path.
///
/// `samples` must be 16kHz mono f32 (what both the VAD segment path and the
/// streaming live tap produce). Skips silently-short clips (<0.4s) — they
/// are key-taps, not speech.
pub fn save_pair(samples: &[f32], raw_text: &str, engine: &str) -> Result<PathBuf> {
    const SAMPLE_RATE: u32 = 16_000;
    let duration = samples.len() as f64 / SAMPLE_RATE as f64;
    if duration < 0.4 || raw_text.trim().is_empty() {
        anyhow::bail!("clip too short or transcript empty — not saved");
    }

    let root = dataset_dir();
    let audio_dir = root.join("audio");
    std::fs::create_dir_all(&audio_dir).context("creating training-data/audio")?;

    // Filename: timestamp + short random suffix (two dictations can share a
    // second). Colons are unfriendly in filenames — use dashes.
    let stamp = timestamp_for_filename();
    let suffix = std::process::id() % 0x10000;
    let base = format!("{stamp}-{suffix:04x}-{}", samples.len() % 0x1000);
    let wav_path = audio_dir.join(format!("{base}.wav"));
    let flac_path = audio_dir.join(format!("{base}.flac"));

    // 1. Write WAV (i16 PCM).
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).context("creating wav")?;
    for &s in samples {
        writer.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    writer.finalize().context("finalizing wav")?;

    // 2. Compress to FLAC via macOS afconvert; keep the WAV on any failure.
    let audio_path = match std::process::Command::new("afconvert")
        .arg("-f")
        .arg("flac")
        .arg("-d")
        .arg("flac")
        .arg(&wav_path)
        .arg(&flac_path)
        .output()
    {
        Ok(out) if out.status.success() && flac_path.exists() => {
            let _ = std::fs::remove_file(&wav_path);
            flac_path
        }
        _ => {
            tracing::warn!("afconvert FLAC failed — keeping uncompressed WAV");
            wav_path
        }
    };

    // 3. Append the manifest line (NeMo fine-tuning format).
    let rel = format!(
        "audio/{}",
        audio_path.file_name().unwrap_or_default().to_string_lossy()
    );
    let line = serde_json::json!({
        "audio_filepath": rel,
        "text": raw_text.trim(),
        "duration": (duration * 1000.0).round() / 1000.0,
        "engine": engine,
        "timestamp": stamp,
    });
    let manifest = root.join("manifest.jsonl");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&manifest)
        .context("opening manifest.jsonl")?;
    writeln!(f, "{line}")?;

    warn_if_large(&root);
    tracing::debug!(
        path = %audio_path.display(),
        secs = format!("{duration:.1}"),
        "training pair saved"
    );
    Ok(audio_path)
}

/// Dataset stats for `rekody doctor`: (clip count, total audio bytes).
/// `None` when no dataset exists yet.
pub fn stats() -> Option<(usize, u64)> {
    let entries: Vec<_> = std::fs::read_dir(dataset_dir().join("audio"))
        .ok()?
        .flatten()
        .collect();
    if entries.is_empty() {
        return None;
    }
    let bytes = entries
        .iter()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    Some((entries.len(), bytes))
}

/// One-time-per-run size warning so the dataset can't silently eat the disk.
fn warn_if_large(root: &Path) {
    if SIZE_WARNED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let total: u64 = std::fs::read_dir(root.join("audio"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    if total > SIZE_WARN_BYTES {
        SIZE_WARNED.store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!(
            gb = format!("{:.1}", total as f64 / 1e9),
            "training-data folder is large — prune old clips or archive it"
        );
    }
}

/// `YYYY-MM-DDTHH-MM-SS` (filesystem-safe ISO-ish), local-agnostic UTC.
fn timestamp_for_filename() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // civil_from_days (Howard Hinnant) — same algorithm history.rs uses.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}-{m:02}-{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_flac_and_manifest_line() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test-local env mutation; tests in this module run serially
        // on the same var.
        unsafe { std::env::set_var("REKODY_TRAINING_DIR", dir.path()) };

        // 1s of 220Hz sine at 16kHz.
        let samples: Vec<f32> = (0..16_000)
            .map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin() * 0.4)
            .collect();
        let path = save_pair(&samples, "hello training world", "nemotron").unwrap();
        assert!(path.exists());

        let manifest = std::fs::read_to_string(dir.path().join("manifest.jsonl")).unwrap();
        let entry: serde_json::Value =
            serde_json::from_str(manifest.lines().last().unwrap()).unwrap();
        assert_eq!(entry["text"], "hello training world");
        assert!((entry["duration"].as_f64().unwrap() - 1.0).abs() < 0.01);

        unsafe { std::env::remove_var("REKODY_TRAINING_DIR") };
    }

    #[test]
    fn rejects_too_short_clips() {
        let samples = vec![0.1f32; 1000]; // 62ms
        assert!(save_pair(&samples, "tap", "nemotron").is_err());
    }
}
