//! One real call to Google's transcribe API, ignored by default.
//!
//! `cargo test` never runs this: it needs a key and the network, and it costs
//! money. It exists so the wire contract can be re-verified by hand when
//! Google changes something, without anyone having to reconstruct the request
//! shape from scratch.
//!
//! ```text
//! GEMINI_API_KEY=... GEMINI_TEST_WAV=/path/to/16k-mono.wav \
//!   cargo test -p rekody-stt --test gemini_live -- --ignored --nocapture
//! ```
//!
//! Generate the WAV from synthetic speech, never from a real recording:
//!
//! ```text
//! say -o /tmp/clip.aiff "your sentence here"
//! afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/clip.aiff /tmp/clip.wav
//! ```
//!
//! The key is read from the environment and never written to a config file,
//! a log line, or this repository.

use rekody_stt::{GeminiMode, GeminiTranscribeEngine, SttEngine};

/// Read a 16-bit PCM WAV into the 16 kHz mono f32 samples the engine takes.
fn read_wav(path: &str) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("test WAV should open");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "the engine takes mono audio");
    assert_eq!(spec.sample_rate, 16_000, "the engine takes 16 kHz audio");
    reader
        .samples::<i16>()
        .map(|s| s.expect("sample should decode") as f32 / 32768.0)
        .collect()
}

#[tokio::test]
#[ignore = "needs GEMINI_API_KEY, the network, and real money"]
async fn gemini_returns_a_real_transcript() {
    let key = std::env::var("GEMINI_API_KEY").expect("set GEMINI_API_KEY");
    let wav = std::env::var("GEMINI_TEST_WAV").expect("set GEMINI_TEST_WAV");
    let samples = read_wav(&wav);
    let mode = match std::env::var("GEMINI_TEST_MODE").as_deref() {
        Ok("smart") => GeminiMode::Smart,
        _ => GeminiMode::Verbatim,
    };

    let engine = GeminiTranscribeEngine::new(key, mode);
    engine.set_bias_terms(&["Rekody".to_string()]);
    let transcript = engine
        .transcribe(&samples)
        .await
        .expect("Gemini should return a transcript");

    println!(
        "mode={} latency={}ms\ntranscript: {}",
        mode.label(),
        transcript.latency_ms,
        transcript.text
    );
    assert!(
        !transcript.text.is_empty(),
        "a real clip must not come back empty (the :generateContent path does that)"
    );
}
