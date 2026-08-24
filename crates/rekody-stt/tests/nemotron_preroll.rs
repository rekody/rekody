//! Regression guard for the utterance pre-roll (issue #133).
//!
//! `crates/rekody-core/src/streaming.rs` hands the engine a fixed run of
//! digital silence before the first real sample of every utterance, because
//! the microphone opens on key-down and the encoder's first chunk would
//! otherwise start mid-word with no left context.
//!
//! The evidence for that change was measured offline by PREPENDING silence to
//! recorded clips and running them through the engine. That only transfers to
//! the shipped code if the two are the same thing: a separate `feed()` of
//! silence followed by the audio must produce exactly what one `feed()` of
//! [silence ++ audio] produces. `feed()` accumulates into an internal chunk
//! buffer, so it does — and this test pins that, because a future change that
//! reset any state per `feed()` call would silently invalidate every number
//! in the pre-roll measurement without failing anything else.
//!
//! Skip-if-missing like `nemotron_boundary_flush.rs`: needs the shipped
//! Nemotron model under `~/.local/share/rekody/models/nemotron-en-int8` (or
//! `$REKODY_MODEL_DIR/nemotron-en-int8`); without it the tests print a skip
//! message and pass, keeping CI green.

#![cfg(feature = "nemotron")]

use std::path::PathBuf;

use rekody_stt::nemotron::NemotronStreamingEngine;

/// Kept byte-identical to `streaming.rs`'s `PREROLL_SILENCE_SAMPLES`. If that
/// constant moves, this test should be updated deliberately rather than
/// tracking it, so the change shows up in review.
const PREROLL_SAMPLES: usize = 80 * 16_000 / 1000;

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

/// The claim the offline measurement rests on.
#[test]
fn a_separate_preroll_feed_matches_prepended_silence() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let speech = tone(16_000 * 2);

    // (a) what the offline harness measured: silence baked into the audio.
    let mut baked: Vec<f32> = vec![0.0; PREROLL_SAMPLES];
    baked.extend_from_slice(&speech);
    let mut engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    feed_ragged(&mut engine, &baked);
    let from_baked = engine.finish().expect("finish");

    // (b) what streaming.rs does: one feed of silence, then the audio.
    let mut engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    engine
        .feed(&vec![0.0f32; PREROLL_SAMPLES])
        .expect("preroll feed");
    feed_ragged(&mut engine, &speech);
    let from_separate = engine.finish().expect("finish");

    assert_eq!(
        from_baked, from_separate,
        "the pre-roll measured offline is not what the code feeds"
    );
}

/// The pre-roll must not survive into the next utterance, and must be applied
/// again to it. `finish()` resets the engine, so a second utterance on the
/// same engine has to behave exactly like the first on a fresh one.
#[test]
fn preroll_applies_per_utterance_not_per_engine() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let speech = tone(16_000 * 2);
    let preroll = vec![0.0f32; PREROLL_SAMPLES];

    let mut engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    engine.feed(&preroll).expect("feed");
    feed_ragged(&mut engine, &speech);
    let first = engine.finish().expect("finish");

    // Same engine, second utterance, pre-rolled again.
    engine.feed(&preroll).expect("feed");
    feed_ragged(&mut engine, &speech);
    let second = engine.finish().expect("finish");

    assert_eq!(
        first, second,
        "a pre-rolled utterance decoded differently the second time on one engine"
    );
}

/// Why the pre-roll is shorter than one chunk.
///
/// `feed()` emits only whole `chunk_samples` chunks (`nemotron.rs`), so a
/// pre-roll shorter than a chunk stays in the buffer and the first encoder
/// call then needs only `chunk_samples - PREROLL_SAMPLES` real samples
/// instead of a full chunk. The run-up is therefore free twice over: the
/// encoder gets its left context AND the first partial lands sooner than it
/// does today.
///
/// A pre-roll of a full chunk or longer would lose both halves of that: it
/// would fire an encoder call on pure silence, and real audio would still
/// have to wait a whole chunk. This test fails if anyone grows the constant
/// past that line.
#[test]
fn preroll_is_shorter_than_one_chunk_so_the_first_partial_comes_sooner() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let mut engine = NemotronStreamingEngine::new(dir.to_str().unwrap()).expect("load");
    let chunk = engine.chunk_samples();

    assert!(
        PREROLL_SAMPLES < chunk,
        "pre-roll ({PREROLL_SAMPLES}) must be shorter than one chunk ({chunk}): \
         at or above it the pre-roll burns an encoder call on silence and buys no latency"
    );

    // The pre-roll alone must not complete a chunk, so it triggers no encoder
    // call and can contribute no tokens of its own to the transcript.
    let emitted = engine
        .feed(&vec![0.0f32; PREROLL_SAMPLES])
        .expect("preroll feed");
    assert!(
        emitted.is_empty(),
        "pre-roll alone emitted {emitted:?}; it should not complete a chunk"
    );
    assert!(
        engine.transcript().is_empty(),
        "pre-roll put text into the transcript before any audio arrived"
    );
}
