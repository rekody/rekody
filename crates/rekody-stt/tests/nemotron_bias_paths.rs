//! Real-model checks for decode-time term biasing (issue #91, spec section
//! 6 test 5): the disabled path must be bit-identical to the plain engine,
//! and the enabled path must decode without disturbing the utterance
//! lifecycle (including a mid-utterance term reload, which defers).
//!
//! Skip-if-missing like `short_audio_smoke.rs`: the tests need the shipped
//! Nemotron int8 model under `~/.local/share/rekody/models/nemotron-en-int8`
//! (or `$REKODY_MODEL_DIR/nemotron-en-int8`); without it they print a skip
//! message and pass, keeping CI green.

#![cfg(feature = "nemotron")]

use std::path::PathBuf;

use rekody_stt::biasing::BiasSettings;
use rekody_stt::nemotron::NemotronStreamingEngine;

/// The Nemotron model directory, only when all three files are present.
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

/// Deterministic speech-band test signal: two modulated tones plus a slow
/// chirp, ~1.6s at 16kHz (long enough for several chunks at any streaming
/// profile, plus a ragged tail that exercises the pad-flush path).
/// Closed-form, so every engine hears identical bytes.
fn test_audio() -> Vec<f32> {
    let n = 8960 * 2 + 8960 / 2;
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

/// Feed the audio in uneven slices (mimicking live-tap chunk sizes), then
/// finish. Returns (per-feed emissions, final transcript).
fn run_utterance(engine: &mut NemotronStreamingEngine, audio: &[f32]) -> (Vec<String>, String) {
    let mut emitted = Vec::new();
    for slice in audio.chunks(1600) {
        emitted.push(engine.feed(slice).expect("feed"));
    }
    let final_text = engine.finish().expect("finish");
    (emitted, final_text)
}

/// Spec section 6, test 5: `new_with_bias(dir, None)` installs no processor
/// and produces bit-identical output to `new` on identical audio. Guards
/// the "zero cost when off" claim.
#[test]
fn disabled_bias_is_bit_identical_to_plain_new() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let dir = dir.to_string_lossy();
    let audio = test_audio();

    let mut plain = NemotronStreamingEngine::new(&dir).expect("plain engine loads");
    let mut disabled =
        NemotronStreamingEngine::new_with_bias(&dir, None).expect("disabled engine loads");

    // Off means off: no biasing state at all, so no bias_hits and no logs.
    assert_eq!(plain.bias_hits(), None);
    assert_eq!(disabled.bias_hits(), None);

    // Two utterances back to back so the reset path is covered too.
    for pass in 0..2 {
        let (plain_parts, plain_final) = run_utterance(&mut plain, &audio);
        let (disabled_parts, disabled_final) = run_utterance(&mut disabled, &audio);
        assert_eq!(
            plain_parts, disabled_parts,
            "per-chunk emissions must match bit for bit (pass {pass})"
        );
        assert_eq!(
            plain_final, disabled_final,
            "final transcript must match bit for bit (pass {pass})"
        );
    }

    // set_terms on a no-bias engine is a documented no-op.
    disabled.set_terms(vec!["Rekody".into()]);
    assert_eq!(disabled.bias_hits(), None);
}

/// Enabled path: real dictionary terms load, the utterance lifecycle stays
/// intact, `bias_hits` is observable, and a mid-utterance `set_terms` defers
/// to `finish()` without disturbing the current utterance.
#[test]
fn enabled_bias_decodes_and_defers_midutterance_reload() {
    let Some(dir) = find_model_dir() else {
        eprintln!("skipping: no Nemotron model under ~/.local/share/rekody/models");
        return;
    };
    let dir = dir.to_string_lossy();
    let audio = test_audio();

    let settings = BiasSettings {
        terms: vec!["Rekody".into(), "Chamgei".into(), "Core ML".into()],
        ..BiasSettings::default()
    };
    let mut engine =
        NemotronStreamingEngine::new_with_bias(&dir, Some(settings)).expect("biased engine loads");
    assert_eq!(
        engine.bias_hits(),
        Some(0),
        "hits observable from the start"
    );

    // Utterance 1: a reload arrives mid-utterance (buffer non-empty) and
    // must defer to `finish()` instead of swapping the live trie.
    let mut reloaded = false;
    for slice in audio.chunks(1600) {
        engine.feed(slice).expect("feed");
        if !reloaded {
            engine.set_terms(vec!["Ollama".into()]);
            reloaded = true;
        }
    }
    let _ = engine.finish().expect("finish applies the deferred reload");
    assert_eq!(
        engine.bias_hits(),
        Some(0),
        "synthetic tones complete no term"
    );

    // Utterances 2 and 3 both run on the reloaded trie from a reset state:
    // identical audio through an identical trie must decode identically.
    let (_, second) = run_utterance(&mut engine, &audio);
    let (_, third) = run_utterance(&mut engine, &audio);
    assert_eq!(
        second, third,
        "same trie, same audio, same reset state must transcribe identically"
    );

    // Between utterances a reload applies immediately; an empty list
    // uninstalls the processor entirely and decoding still works.
    engine.set_terms(Vec::new());
    let (_, _fourth) = run_utterance(&mut engine, &audio);
    assert_eq!(engine.bias_hits(), Some(0));
}
