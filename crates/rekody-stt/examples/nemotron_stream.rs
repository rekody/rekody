//! Nemotron streaming spike — measures real cache-aware streaming performance
//! on this machine before any pipeline integration.
//!
//! Feeds a 16kHz mono WAV through `parakeet_rs::Nemotron` in 560ms chunks
//! (8,960 samples), printing the incremental transcript and per-chunk compute
//! time. The key number is mean chunk compute vs the 560ms budget: anything
//! comfortably under means real-time streaming works on this hardware.
//!
//! Run:
//!   cargo run --release -p rekody-stt --features nemotron \
//!     --example nemotron_stream -- <model_dir> <wav_path>

use std::time::Instant;

use parakeet_rs::Nemotron;

const CHUNK_SIZE: usize = 8960; // 560ms at 16kHz

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

    eprintln!("loading model from {model_dir}…");
    let t_load = Instant::now();
    let mut model = Nemotron::from_pretrained(&model_dir, None)?;
    eprintln!(
        "model loaded in {:.2}s (mode: {:?})",
        t_load.elapsed().as_secs_f64(),
        model.mode()
    );

    let mut chunk_ms: Vec<f64> = Vec::new();
    let mut transcript = String::new();

    for chunk in audio.chunks(CHUNK_SIZE) {
        // Pad the final short chunk to full size with silence.
        let owned;
        let chunk = if chunk.len() == CHUNK_SIZE {
            chunk
        } else {
            let mut v = chunk.to_vec();
            v.resize(CHUNK_SIZE, 0.0);
            owned = v;
            &owned
        };

        let t = Instant::now();
        let text = model.transcribe_chunk(chunk)?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        chunk_ms.push(ms);
        if !text.is_empty() {
            transcript.push_str(&text);
            eprintln!("[{:6.1}ms] {}", ms, text);
        } else {
            eprintln!("[{:6.1}ms]", ms);
        }
    }

    let total_compute_ms: f64 = chunk_ms.iter().sum();
    let mean = total_compute_ms / chunk_ms.len() as f64;
    let max = chunk_ms.iter().cloned().fold(0.0, f64::max);

    println!("\n--- transcript ---\n{}", transcript.trim());
    println!("\n--- stats ---");
    println!(
        "audio: {audio_secs:.2}s in {} chunks of 560ms",
        chunk_ms.len()
    );
    println!("chunk compute: mean {mean:.1}ms / max {max:.1}ms (budget 560ms)");
    println!(
        "realtime factor: {:.1}x (total compute {:.2}s)",
        audio_secs / (total_compute_ms / 1000.0),
        total_compute_ms / 1000.0
    );
    Ok(())
}
