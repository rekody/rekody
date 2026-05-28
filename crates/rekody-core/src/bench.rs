//! `rekody bench` — measure local Whisper transcription latency.
//!
//! Runs the bundled audio sample(s) through the local Whisper engine N times
//! and reports mean / p50 / p95 latency plus real-time factor (RTF). The big
//! intended use is A/B: rename the `.mlmodelc` directory out of the way to
//! compare Metal-only vs Core ML / ANE.
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
pub async fn run(config: &RekodyConfig, runs: usize, warmup: usize) -> Result<()> {
    if !matches!(config.stt_engine.to_lowercase().as_str(), "local") {
        anyhow::bail!(
            "`rekody bench` benchmarks the local Whisper engine; your config uses `{}`.\n\
             Switch with `rekody config` → STT → Engine → local, or set REKODY_BENCH_MODEL.",
            config.stt_engine
        );
    }
    let model = model_from_str(&config.whisper_model)?;
    let model_dir = resolve_model_dir();
    let model_path = model_dir.join(WhisperModel::multilingual_file_name(model));
    if !model_path.exists() {
        anyhow::bail!(
            "Whisper model not found at {}. Run `rekody setup` first.",
            model_path.display()
        );
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
        "  {brand}│{reset}   {dim}Model :{reset}  {cream}{bold}{:?}{reset}  {dim}({}){reset}",
        model,
        model_path.display()
    );
    let acceleration = if coreml_present_for(&model_dir, model) {
        format!("{ok}{bold}Core ML / ANE{reset}  {dim}(encoder on Neural Engine){reset}")
    } else {
        format!("{warn}{bold}Metal{reset}  {dim}(no Core ML encoder — rename rules apply){reset}")
    };
    println!("  {brand}│{reset}   {dim}Accel :{reset}  {acceleration}");
    println!(
        "  {brand}│{reset}   {dim}Runs  :{reset}  {cream}{bold}{}{reset}  {dim}(+ {} warmup, discarded){reset}",
        runs, warmup
    );
    println!("  {brand}│{reset}");

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
    }

    // ── A/B hint ──
    println!("  {brand}│{reset}   {dim}A/B vs Metal:{reset}");
    println!(
        "  {brand}│{reset}     {dim}1. mv {model_dir}/<model>-encoder.mlmodelc /tmp/{reset}",
        model_dir = model_dir.display()
    );
    println!(
        "  {brand}│{reset}     {dim}2. rekody bench   {dim}# Metal-only numbers{reset}{reset}"
    );
    println!(
        "  {brand}│{reset}     {dim}3. mv /tmp/<model>-encoder.mlmodelc {model_dir}/{reset}",
        model_dir = model_dir.display()
    );
    println!("  {brand}│{reset}     {dim}4. rekody bench   {dim}# Core ML numbers{reset}{reset}");

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
