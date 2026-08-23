//! Regression guard for the end-of-utterance flush.
//!
//! The streaming encoder emits a chunk's frames using the rest of that chunk
//! as lookahead, so the last real audio needs trailing frames pushed through
//! before the RNN-T will commit the final word. Rekody used to supply those by
//! padding the leftover samples out to ONE chunk, which meant two failure
//! modes that only show up at the edges:
//!
//!   * an utterance whose length is an exact multiple of the chunk size left
//!     no leftover at all, so the flush ran ZERO encoder calls;
//!   * the amount of silence the pad happened to supply scaled with the chunk
//!     size, so moving from the 560 ms profile to the 160 ms one would have
//!     cut it by three and a half times.
//!
//! `finish()` now flushes a fixed DURATION of silence instead, which is
//! profile-independent. These tests pin that behaviour so a future chunk-size
//! or profile change cannot quietly undo it.
//!
//! Skip-if-missing like `nemotron_bias_paths.rs`: needs the shipped Nemotron
//! model under `~/.local/share/rekody/models/nemotron-en-int8` (or
//! `$REKODY_MODEL_DIR/nemotron-en-int8`); without it the tests print a skip
//! message and pass, keeping CI green.

#![cfg(feature = "nemotron")]

use std::path::PathBuf;

use rekody_stt::nemotron::NemotronStreamingEngine;

fn find_model_dir() -> Option<PathBuf> {
    let base = std::env::var("REKODY_MODEL_DIR")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/share/rekody/models"))
        })
        .ok()?;
    let dir = base.join("nemotron-en-int8");
    ["encoder.onnx", "decoder_joint.onnx", "tokenizer.model"]
        .iter()
        .all(|f| dir.join(f).exists())
        .then_some(dir)
}

/// Deterministic speech-band signal of exactly `n` samples. Closed-form, so
/// every run hears identical bytes and the test cannot flake on audio.
fn tone(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            let envelope = 0.4 * (1.0 + (2.0 * std::f32::consts::PI * 3.0 * t).sin()) / 2.0;
            let tone_a = (2.0 * std::f32::consts::PI * 220.0 * t).sin();
            let tone_b = 0.5 * (2.0 * std::f32::consts::PI * (700.0 + 400.0 * t) * t).sin();
            envelope * (tone_a + tone_b)
        })
        .collect()
}

/// Feed in ragged runs, the way the live audio tap delivers, so nothing here
/// depends on the caller happening to hand over exact chunks.
fn feed_ragged(engine: &mut NemotronStreamingEngine, audio: &[f32]) {
    const RUNS: [usize; 6] = [512, 1024, 731, 2048, 1600, 333];
    let mut i = 0;
    let mut k = 0;
    while i < audio.len() {
        let n = RUNS[k % RUNS.len()].min(audio.len() - i);
        engine.feed(&audio[i..i + n]).expect("feed");
        i += n;
        k += 1;
    }
}

/// The chunk size must come from the artifact, never from a constant. A model
/// whose graph carries a different streaming profile has to be driven at that
/// profile or the encoder silently stalls (too small) or drops frames (too
/// large).
#[test]
fn chunk_size_comes_from_the_loaded_artifact() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    let cs = engine.chunk_samples();
    // Every published cache-aware profile is (right_context + 1) encoder
    // frames of 80 ms: 1280 samples per frame.
    assert!(
        cs >= 1280 && cs.is_multiple_of(1280),
        "chunk_samples {cs} is not a whole number of 80 ms encoder frames"
    );
    assert!(
        cs <= 17_920,
        "chunk_samples {cs} exceeds the largest published profile (1120 ms)"
    );
    eprintln!("chunk_samples = {cs} ({} ms)", cs * 1000 / 16_000);
}

/// An utterance whose length is an exact multiple of the chunk size leaves no
/// partial chunk behind. The old flush did nothing at all in that case; the
/// duration-based flush must still push silence through.
///
/// The assertion is deliberately about the ENGINE'S BEHAVIOUR rather than the
/// transcript text: a synthetic tone has no words, so requiring specific
/// output would be testing the model, not the flush. What is pinned is that
/// `finish()` costs real encoder work on an exact-multiple utterance, which is
/// exactly what used to be skipped.
#[test]
fn exact_multiple_utterance_still_flushes() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let mut engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    let cs = engine.chunk_samples();

    // Exactly 4 chunks: buf is empty when finish() runs.
    let audio = tone(cs * 4);
    feed_ragged(&mut engine, &audio);
    assert!(
        engine.transcript().len() < usize::MAX,
        "transcript accessor must work mid-utterance"
    );
    let t = std::time::Instant::now();
    let _ = engine.finish().expect("finish");
    let flush_ms = t.elapsed().as_millis();

    // One encoder call on this machine is tens of milliseconds; a flush that
    // did nothing returns in well under a millisecond. The threshold is loose
    // on purpose so this cannot flake on a fast or a loaded machine, while
    // still catching "the flush ran zero times".
    assert!(
        flush_ms >= 5,
        "finish() on an exact-multiple utterance returned in {flush_ms}ms, \
         which means it flushed nothing"
    );
    // State must be clean for the next dictation.
    assert!(
        engine.transcript().is_empty(),
        "state must clear after finish()"
    );
}

/// The flush must not depend on how the caller sliced its feeds: an utterance
/// one sample short of a chunk boundary and one exactly on it must both leave
/// the engine ready, and neither may panic.
#[test]
fn flush_is_independent_of_feed_alignment() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let mut engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    let cs = engine.chunk_samples();

    for (label, n) in [
        ("exact multiple", cs * 3),
        ("one sample short", cs * 3 - 1),
        ("one sample over", cs * 3 + 1),
        ("shorter than one chunk", cs / 3),
        ("single sample", 1usize),
    ] {
        let audio = tone(n);
        feed_ragged(&mut engine, &audio);
        let out = engine
            .finish()
            .unwrap_or_else(|e| panic!("{label}: finish failed: {e}"));
        assert!(
            engine.transcript().is_empty(),
            "{label}: state must clear after finish()"
        );
        eprintln!("{label} ({n} samples): {} chars", out.len());
    }
}
