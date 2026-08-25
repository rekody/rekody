//! `rekody bench` — measure local Whisper transcription latency.
//!
//! Runs the bundled audio sample(s) through the local Whisper engine N times
//! and reports mean / p50 / p95 latency plus real-time factor (RTF).
//!
//! Two intended uses:
//!
//! * A/B one model against itself: rename the `.mlmodelc` directory out of
//!   the way to compare CPU against Core ML / ANE (Apple Silicon only).
//! * A/B model sizes against each other: `--model tiny --model small --model
//!   turbo` runs each in turn and prints a comparison table. This is how the
//!   right default for a machine gets PICKED rather than guessed, which is
//!   what issue #141 needed on Intel.
//!
//! Samples are embedded in the binary (`include_bytes!`) so the command works
//! anywhere without a download step. Currently shipping with `jfk.wav` —
//! 11 s, 16 kHz mono PCM, public domain (JFK inaugural address excerpt,
//! the canonical Whisper test sample from `ggml-org/whisper.cpp`).

use std::io::Cursor;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use rekody_stt::{LocalWhisperEngine, SttEngine, WhisperModel};

use crate::RekodyConfig;

/// One bundled sample (name, raw WAV bytes, BCP-47 language hint).
struct Sample {
    name: &'static str,
    wav: &'static [u8],
    language: &'static str,
}

const SAMPLES: &[Sample] = &[Sample {
    name: "jfk",
    wav: include_bytes!("../assets/bench/jfk.wav"),
    language: "en",
}];

/// Decode a 16 kHz mono 16-bit PCM WAV into f32 samples normalised to [-1, 1].
fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, f64)> {
    let cursor = Cursor::new(bytes);
    let mut reader = hound::WavReader::new(cursor).context("invalid WAV")?;
    let spec = reader.spec();
    if spec.channels != 1 {
        anyhow::bail!("bench sample must be mono, got {} channels", spec.channels);
    }
    if spec.sample_rate != 16_000 {
        anyhow::bail!("bench sample must be 16 kHz, got {} Hz", spec.sample_rate);
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<Vec<_>, _>>()
            .context("decoding PCM samples")?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .context("decoding float samples")?,
    };
    let duration_secs = samples.len() as f64 / spec.sample_rate as f64;
    Ok((samples, duration_secs))
}

/// Parse a model size string into the `WhisperModel` enum.
fn model_from_str(s: &str) -> Result<WhisperModel> {
    match s.to_lowercase().as_str() {
        "tiny" => Ok(WhisperModel::Tiny),
        "small" => Ok(WhisperModel::Small),
        "medium" => Ok(WhisperModel::Medium),
        "turbo" => Ok(WhisperModel::Turbo),
        "large" => Ok(WhisperModel::Large),
        other => Err(anyhow!("unknown whisper model: {other}")),
    }
}

fn resolve_model_dir() -> PathBuf {
    std::env::var("REKODY_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".local").join("share").join("rekody").join("models"))
                .unwrap_or_else(|| PathBuf::from("models"))
        })
}

/// How the encoder is actually running, in plain terms.
///
/// Rekody compiles whisper.cpp with `GGML_METAL=OFF` on every target
/// (whisper-rs 0.13 ships `default = []`), and enables `coreml` only on
/// macOS/aarch64. So there are exactly two states: encoder on the Neural
/// Engine, or encoder on the CPU. Naming the fallback "Metal" was wrong and
/// hid the reason Intel is slow.
fn acceleration_label(model_dir: &std::path::Path, model: WhisperModel) -> (&'static str, bool) {
    if !crate::HAS_NEURAL_ENGINE {
        return ("CPU  (no Neural Engine on this machine)", false);
    }
    if coreml_present_for(model_dir, model) {
        ("Core ML / ANE  (encoder on Neural Engine)", true)
    } else {
        ("CPU  (Core ML encoder not installed for this size)", false)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn coreml_present_for(model_dir: &std::path::Path, model: WhisperModel) -> bool {
    let name = match model {
        WhisperModel::Tiny => "ggml-tiny-encoder.mlmodelc",
        WhisperModel::Small => "ggml-small-encoder.mlmodelc",
        WhisperModel::Medium => "ggml-medium-encoder.mlmodelc",
        WhisperModel::Turbo => "ggml-large-v3-turbo-encoder.mlmodelc",
        WhisperModel::Large => "ggml-large-encoder.mlmodelc",
    };
    model_dir.join(name).exists()
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn coreml_present_for(_: &std::path::Path, _: WhisperModel) -> bool {
    false
}

/// Stats for a single sample's run.
struct Stats {
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    min_ms: f64,
    max_ms: f64,
}

fn stats(times: &[Duration]) -> Stats {
    let mut ms: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = ms.len();
    let mean = ms.iter().sum::<f64>() / n as f64;
    let p50 = ms[n / 2];
    let p95_idx = ((n as f64) * 0.95).ceil() as usize - 1;
    let p95 = ms[p95_idx.min(n - 1)];
    Stats {
        mean_ms: mean,
        p50_ms: p50,
        p95_ms: p95,
        min_ms: ms[0],
        max_ms: ms[n - 1],
    }
}

/// Public entrypoint for `rekody bench`.
pub async fn run(
    config: &RekodyConfig,
    runs: usize,
    warmup: usize,
    models: &[String],
) -> Result<()> {
    if !matches!(config.stt_engine.to_lowercase().as_str(), "local") {
        anyhow::bail!(
            "`rekody bench` benchmarks the local Whisper engine; your config uses `{}`.\n\
             Switch with `rekody config` → STT → Engine → local, or set REKODY_BENCH_MODEL.",
            config.stt_engine
        );
    }

    // No --model means "whatever the config runs", which is the historical
    // behaviour. Sizes are de-duplicated but keep the order they were typed,
    // so the comparison table reads the way the tester asked for it.
    let requested: Vec<String> = if models.is_empty() {
        vec![config.whisper_model.clone()]
    } else {
        let mut seen: Vec<String> = Vec::new();
        for m in models {
            let key = m.to_lowercase();
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        seen
    };

    let model_dir = resolve_model_dir();
    // Resolve and check EVERY model up front: a missing 3 GB download should
    // fail before the first benchmark runs, not twenty minutes into a pass.
    let mut plan: Vec<(WhisperModel, PathBuf)> = Vec::with_capacity(requested.len());
    for name in &requested {
        let model = model_from_str(name)?;
        let model_path = model_dir.join(WhisperModel::multilingual_file_name(model));
        if !model_path.exists() {
            anyhow::bail!(
                "Whisper model `{name}` not found at {}.\n\
                 Run `rekody setup` and pick it, so the file is downloaded and \
                 checksum-verified before it is benchmarked.",
                model_path.display()
            );
        }
        plan.push((model, model_path));
    }

    // ── Header ──
    let rule = "─".repeat(60);
    let brand = "\x1b[38;2;32;128;141m";
    let brand_light = "\x1b[38;2;79;184;197m";
    let cream = "\x1b[38;2;251;250;244m";
    let dim = "\x1b[38;2;119;119;119m";
    let ok = "\x1b[38;2;107;203;119m";
    let warn = "\x1b[38;2;230;180;80m";
    let bold = "\x1b[1m";
    let reset = "\x1b[0m";

    println!();
    println!(
        "  {brand}╭─{reset}  {brand_light}{bold}rekody bench{reset}  {dim}local Whisper latency{reset}"
    );
    println!("  {brand}│{reset}");
    println!(
        "  {brand}│{reset}   {dim}Runs  :{reset}  {cream}{bold}{}{reset}  {dim}(+ {} warmup, discarded){reset}",
        runs, warmup
    );
    println!("  {brand}│{reset}");

    // (model, sample, mean_ms, rtf) for the comparison table.
    let mut results: Vec<(WhisperModel, &'static str, f64, f64)> = Vec::new();

    for (model, model_path) in &plan {
        let (model, model_path) = (*model, model_path.as_path());
        println!(
            "  {brand}│{reset}   {dim}Model :{reset}  {cream}{bold}{:?}{reset}  {dim}({}){reset}",
            model,
            model_path.display()
        );
        let (accel_text, accelerated) = acceleration_label(&model_dir, model);
        let accel_color = if accelerated { ok } else { warn };
        println!("  {brand}│{reset}   {dim}Accel :{reset}  {accel_color}{bold}{accel_text}{reset}");

        for sample in SAMPLES {
            // Load engine per sample so we can use the right language hint —
            // saves an auto-detect pass and prevents misclassification of short
            // mono audio. The .bin and .mlmodelc are cached after first load,
            // so the extra construction cost is negligible.
            let engine = LocalWhisperEngine::with_language(
                model,
                model_path.to_str().unwrap_or(""),
                Some(sample.language.to_string()),
            )
            .context("loading whisper model")?;
            let (pcm, duration_secs) = decode_wav(sample.wav).context("decoding sample WAV")?;

            println!(
                "  {brand}│{reset}   {brand_light}{bold}{}.wav{reset}  {dim}{:.2}s audio, {} samples{reset}",
                sample.name,
                duration_secs,
                pcm.len()
            );

            // Warmup runs (discarded; absorb Core ML compile + model state warm-up).
            for w in 0..warmup {
                print!(
                    "\r  {brand}│{reset}     {dim}warmup {}/{}…{reset}    ",
                    w + 1,
                    warmup
                );
                std::io::Write::flush(&mut std::io::stdout()).ok();
                let _ = engine.transcribe(&pcm).await?;
            }

            // Measured runs.
            let mut times = Vec::with_capacity(runs);
            let mut last_text = String::new();
            for r in 0..runs {
                print!(
                    "\r  {brand}│{reset}     {dim}run {}/{}…{reset}    ",
                    r + 1,
                    runs
                );
                std::io::Write::flush(&mut std::io::stdout()).ok();
                let t = Instant::now();
                let tx = engine.transcribe(&pcm).await?;
                times.push(t.elapsed());
                last_text = tx.text;
            }
            // Clear the progress line.
            print!("\r  {brand}│{reset}                                          \r");

            let s = stats(&times);
            let rtf = s.mean_ms / (duration_secs * 1000.0);
            println!(
                "  {brand}│{reset}     {dim}mean  :{reset}  {cream}{bold}{:7.1} ms{reset}    {dim}p50 {:.1} · p95 {:.1} · min {:.1} · max {:.1}{reset}",
                s.mean_ms, s.p50_ms, s.p95_ms, s.min_ms, s.max_ms
            );
            let rtf_color = if rtf < 0.3 {
                ok
            } else if rtf < 0.6 {
                brand_light
            } else {
                warn
            };
            println!(
                "  {brand}│{reset}     {dim}RTF   :{reset}  {rtf_color}{bold}{:.3}×{reset}  {dim}(transcribe-time ÷ audio-time; lower = faster){reset}",
                rtf
            );
            println!(
                "  {brand}│{reset}     {dim}out   :{reset}  {dim}\u{201c}{}\u{201d}{reset}",
                truncate(&last_text, 70)
            );
            println!("  {brand}│{reset}");
            results.push((model, sample.name, s.mean_ms, rtf));
        }
    }

    // ── Comparison table (only earns its space with something to compare) ──
    if plan.len() > 1 {
        println!("  {brand}│{reset}   {brand_light}{bold}size comparison{reset}");
        println!(
            "  {brand}│{reset}     {dim}{:<8} {:>10}  {:>8}   verdict{reset}",
            "model", "mean", "RTF"
        );
        for (model, sample, mean_ms, rtf) in &results {
            // Faster than real time is the bar that matters for dictation:
            // above 1.0x the wait after every sentence is longer than the
            // sentence took to say.
            let (verdict, color) = if *rtf < 0.3 {
                ("comfortably faster than real time", ok)
            } else if *rtf < 1.0 {
                ("faster than real time", brand_light)
            } else {
                ("SLOWER than real time", warn)
            };
            println!(
                "  {brand}│{reset}     {cream}{:<8}{reset} {:>8.0} ms  {color}{:>7.3}×{reset}   {dim}{} ({}.wav){reset}",
                format!("{model:?}").to_lowercase(),
                mean_ms,
                rtf,
                verdict,
                sample
            );
        }
        println!("  {brand}│{reset}");
        println!(
            "  {brand}│{reset}     {dim}The right default is the LARGEST size that stays{reset}"
        );
        println!(
            "  {brand}│{reset}     {dim}comfortably faster than real time on this machine.{reset}"
        );
        println!("  {brand}│{reset}");
    }

    // ── A/B hint: only where there is a Neural Engine to A/B against ──
    if crate::HAS_NEURAL_ENGINE {
        println!("  {brand}│{reset}   {dim}A/B the Neural Engine against the CPU:{reset}");
        println!(
            "  {brand}│{reset}     {dim}1. mv {model_dir}/<model>-encoder.mlmodelc /tmp/{reset}",
            model_dir = model_dir.display()
        );
        println!("  {brand}│{reset}     {dim}2. rekody bench   # CPU-only numbers{reset}");
        println!(
            "  {brand}│{reset}     {dim}3. mv /tmp/<model>-encoder.mlmodelc {model_dir}/{reset}",
            model_dir = model_dir.display()
        );
        println!("  {brand}│{reset}     {dim}4. rekody bench   # Core ML numbers{reset}");
    } else {
        println!(
            "  {brand}│{reset}   {dim}No Neural Engine here, so the encoder runs on CPU and{reset}"
        );
        println!(
            "  {brand}│{reset}   {dim}pays a full 30 s window per dictation whatever you say.{reset}"
        );
        println!(
            "  {brand}│{reset}   {dim}Compare sizes with: rekody bench --model tiny --model small --model turbo{reset}"
        );
    }

    println!("  {brand}│{reset}");
    println!("  {brand}╰{}{reset}", rule);
    println!();

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
