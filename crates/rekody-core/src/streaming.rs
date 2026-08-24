//! Bridge between the async pipeline and the Nemotron streaming engine
//! (feature `nemotron`).
//!
//! The engine decodes on 160ms chunks (~48ms compute each on an M2, measured
//! on the owner's certified clips) and is not `Sync`, so it lives on its own
//! dedicated OS thread. The chunk length comes from the loaded artifact, not
//! from a constant here: an older 560ms export is driven at 560ms.
//! The pipeline feeds it raw 16kHz mono samples from the live audio tap via
//! a std mpsc channel and receives partial/final transcripts back on a tokio
//! channel it can `select!` on.
//!
//! Decode happens DURING the recording, so by the time the user releases the
//! key only the tail remains. Flushing that tail costs ~170 to 210ms measured:
//! `finish()` pushes a fixed 560ms of trailing silence through the encoder so
//! the last word is committed, which is more than the old pad-to-one-chunk
//! flush cost (~55ms) and is what stopped final words being dropped. Batch
//! Whisper, by contrast, only starts transcribing at key release.

use std::time::Instant;

use rekody_stt::biasing::BiasSettings;
use rekody_stt::nemotron::NemotronStreamingEngine;

/// Digital silence handed to the engine before the first real sample of an
/// utterance (issue #133).
///
/// The microphone is opened on key-down, so the first callback lands tens of
/// milliseconds after the user has begun speaking and the encoder's very
/// first chunk starts mid-word with no left context. Two separate things are
/// wrong there and only one of them is recoverable: the audio the device
/// never captured is gone, but the missing *run-up* is not. Silence is free,
/// opens no microphone, and gives the encoder the same warm cache it gets on
/// an utterance that began after a pause.
///
/// 80ms, measured over 77 of the owner's certified clips with the onset
/// clipped to simulate the microphone opening late (first-word survival,
/// paired McNemar):
///
/// ```text
/// onset lost |  lead 0     40      80     160
///       0ms  |  80.5%   85.7%   87.0%   88.3%
///      50ms  |  68.8%   76.6%   83.1%   84.4%
///      90ms  |  61.0%   68.8%   72.7%   74.0%
/// ```
///
/// 160ms is not measurably better than 80ms at any clip level (p=1.0 in all
/// three), while 40ms gives back a chunk of the gain where it matters most
/// (83.1% -> 76.6% at 50ms of lost onset). 80ms is also strictly shorter than
/// one 160ms chunk, which matters: `feed()` emits only whole chunks, so the
/// run-up stays in the buffer, costs no encoder call of its own, and the
/// first partial then lands after 80ms of speech instead of 160ms. A pre-roll
/// of a full chunk or longer would forfeit both of those. See
/// `rekody-stt/tests/nemotron_preroll.rs`.
///
/// This is emitted into the engine, not into the capture tap, so captured
/// sample counts, saved training clips and the duration stats built on them
/// are untouched.
const PREROLL_SILENCE_MS: usize = 80;
const PREROLL_SILENCE_SAMPLES: usize = PREROLL_SILENCE_MS * 16_000 / 1000;

/// At 40ms the curve above has not levelled off, so a shorter run-up gives
/// back a measurable part of the gain it exists to capture.
const _: () = assert!(PREROLL_SILENCE_MS >= 40);
/// One 160ms chunk or longer buys nothing measurable and costs an encoder
/// call on pure silence plus the sooner first partial. `nemotron_preroll.rs`
/// pins this against the engine's real chunk size; this is the cheap guard.
const _: () = assert!(PREROLL_SILENCE_SAMPLES < 160 * 16_000 / 1000);

/// Messages from the pipeline to the engine thread.
pub enum StreamMsg {
    /// Raw 16kHz mono samples from the live audio tap.
    Samples(Vec<f32>),
    /// Key released: flush the tail, emit `StreamEvent::Final`, reset state.
    Flush,
    /// The personal dictionary changed on disk: rebuild the term-biasing
    /// trie so `rekody dictionary add` applies without a daemon restart.
    /// Sent between utterances; the engine defers mid-utterance calls to
    /// its next `finish()` anyway. Ignored when biasing is off.
    ReloadTerms(Vec<String>),
}

/// Events from the engine thread back to the pipeline.
pub enum StreamEvent {
    /// Updated in-progress transcript for the current utterance (full text so
    /// far, not a delta). Display-only — never injected.
    Partial(String),
    /// Utterance complete. `latency_ms` is flush time (release → text ready).
    Final { text: String, latency_ms: u64 },
    /// Engine failed to load or decode; the utterance (or engine) is lost.
    Error(String),
}

/// Spawn the engine thread. Returns the sample/flush sender and the event
/// receiver. The model (~3.4s load) is loaded on the spawned thread so the
/// pipeline starts immediately; samples sent before loading completes queue
/// in the channel and are processed once ready. If loading fails, a single
/// `StreamEvent::Error` is emitted and the thread exits.
///
/// `bias` arms decode-time dictionary term biasing (config `[term_biasing]`,
/// default off). `None` keeps the engine byte-identical to a build without
/// the feature: no processor is installed and no biasing log lines fire.
pub fn spawn(
    model_dir: std::path::PathBuf,
    bias: Option<BiasSettings>,
) -> (
    std::sync::mpsc::Sender<StreamMsg>,
    tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
) {
    let (msg_tx, msg_rx) = std::sync::mpsc::channel::<StreamMsg>();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

    std::thread::Builder::new()
        .name("nemotron-stream".into())
        .spawn(move || {
            let dir = model_dir.to_string_lossy();
            tracing::info!(dir = %dir, "loading Nemotron streaming model");
            let t_load = Instant::now();
            let mut engine = match NemotronStreamingEngine::new_with_bias(&dir, bias) {
                Ok(e) => e,
                Err(e) => {
                    let _ = event_tx.send(StreamEvent::Error(format!(
                        "failed to load Nemotron model from {dir}: {e:#}"
                    )));
                    return;
                }
            };
            let chunk_ms = engine.chunk_samples() * 1000 / 16_000;
            tracing::info!(
                load_secs = format!("{:.1}", t_load.elapsed().as_secs_f64()),
                chunk_ms,
                "Nemotron streaming model ready"
            );
            // An artifact from before the 160 ms export still loads and still
            // works, since the runtime reads its geometry and drives it correctly at
            // the old profile. But the user upgraded the binary expecting the
            // faster one, so say plainly that a re-run of setup is what fetches
            // it. Silence here is the failure mode this guards against.
            if chunk_ms != crate::onboarding::NEMOTRON_SHIPPED_CHUNK_MS {
                tracing::warn!(
                    chunk_ms,
                    expected_ms = crate::onboarding::NEMOTRON_SHIPPED_CHUNK_MS,
                    "installed streaming model is an older latency profile; \
                     run `rekody setup` to replace it with the {}ms build",
                    crate::onboarding::NEMOTRON_SHIPPED_CHUNK_MS
                );
            }

            // Whether the current utterance has already been given its
            // pre-roll. Cleared by `Flush`, which is what ends an utterance.
            let mut utterance_open = false;

            while let Ok(msg) = msg_rx.recv() {
                match msg {
                    StreamMsg::Samples(samples) => {
                        if !utterance_open {
                            utterance_open = true;
                            let preroll = vec![0.0f32; PREROLL_SILENCE_SAMPLES];
                            if let Err(e) = engine.feed(&preroll) {
                                // Non-fatal: the utterance is still decodable
                                // without its run-up, just likelier to lose
                                // the first word. Don't fail the dictation.
                                tracing::warn!(error = %format!("{e:#}"), "pre-roll feed failed");
                            }
                        }
                        match engine.feed(&samples) {
                            Ok(emitted) => {
                                if !emitted.is_empty()
                                    && event_tx
                                        .send(StreamEvent::Partial(engine.transcript().to_string()))
                                        .is_err()
                                {
                                    return; // pipeline gone
                                }
                            }
                            Err(e) => {
                                let _ = event_tx.send(StreamEvent::Error(format!(
                                    "nemotron decode failed: {e:#}"
                                )));
                            }
                        }
                    }
                    StreamMsg::Flush => {
                        utterance_open = false;
                        let t = Instant::now();
                        match engine.finish() {
                            Ok(text) => {
                                let latency_ms = t.elapsed().as_millis() as u64;
                                // Observability for dogfooding (spec section
                                // 5, step 5.4): how often biasing completed a
                                // term this utterance. `None` when the
                                // feature is off, keeping the off path free
                                // of even this log line.
                                if let Some(bias_hits) = engine.bias_hits() {
                                    tracing::info!(bias_hits, "term biasing utterance summary");
                                }
                                if event_tx
                                    .send(StreamEvent::Final { text, latency_ms })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = event_tx.send(StreamEvent::Error(format!(
                                    "nemotron flush failed: {e:#}"
                                )));
                            }
                        }
                    }
                    StreamMsg::ReloadTerms(terms) => {
                        // No-op (with a debug log) when biasing is off.
                        engine.set_terms(terms);
                    }
                }
            }
            tracing::debug!("nemotron stream thread exiting (pipeline closed)");
        })
        .expect("spawn nemotron stream thread");

    (msg_tx, event_rx)
}
