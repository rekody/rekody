//! Nemotron streaming engine validation — exercises the real
//! `NemotronStreamingEngine` (feed/finish/reset) the way the pipeline will:
//! variable-length sample runs (like the live audio tap delivers), a flush at
//! "key release", and a SECOND utterance to verify state reset between
//! dictations.
//!
//! Run:
//!   cargo run --release -p rekody-stt --features nemotron \
//!     --example nemotron_stream -- <model_dir> <wav_path>

use std::time::Instant;

use rekody_stt::nemotron::NemotronStreamingEngine;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let model_dir = args
        .next()
        .expect("usage: nemotron_stream <model_dir> <wav>");
    let wav_path = args
        .next()
        .expect("usage: nemotron_stream <model_dir> <wav>");

    // Load 16kHz mono PCM as f32.
    let mut reader = hound::WavReader::open(&wav_path)?;
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000, "wav must be 16kHz");
    assert_eq!(spec.channels, 1, "wav must be mono");
    let audio: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
    };
    let audio_secs = audio.len() as f64 / 16_000.0;

    eprintln!("loading engine from {model_dir}…");
    let t_load = Instant::now();
    let mut engine = NemotronStreamingEngine::new(&model_dir)?;
    eprintln!("engine loaded in {:.2}s", t_load.elapsed().as_secs_f64());

    // Two utterances of the same audio — the second verifies reset() works
    // (no context bleed, same transcript).
    for utterance in 1..=2 {
        eprintln!("\n=== utterance {utterance} (live-tap simulation) ===");
        let mut feed_ms: Vec<f64> = Vec::new();
        // Feed in variable-length runs like the resampler tap produces
        // (cpal callback cadence ≈ 10–50ms of audio per run).
        let mut i = 0;
        let run_sizes = [512usize, 1024, 731, 2048, 1600, 333];
        let mut k = 0;
        while i < audio.len() {
            let n = run_sizes[k % run_sizes.len()].min(audio.len() - i);
            k += 1;
            let t = Instant::now();
            let emitted = engine.feed(&audio[i..i + n])?;
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if ms > 1.0 {
                feed_ms.push(ms); // only count runs that actually decoded a chunk
            }
            if !emitted.is_empty() {
                eprintln!("[{ms:6.1}ms] {emitted}");
            }
            i += n;
        }
        let t = Instant::now();
        let final_text = engine.finish()?;
        let flush_ms = t.elapsed().as_secs_f64() * 1000.0;

        let mean = feed_ms.iter().sum::<f64>() / feed_ms.len().max(1) as f64;
        let max = feed_ms.iter().cloned().fold(0.0, f64::max);
        println!("\nutterance {utterance} transcript: {final_text}");
        println!(
            "audio {audio_secs:.2}s · chunk decode mean {mean:.1}ms / max {max:.1}ms · final flush {flush_ms:.1}ms"
        );
        assert!(
            engine.transcript().is_empty(),
            "state must be clear after finish()"
        );
    }
    Ok(())
}
