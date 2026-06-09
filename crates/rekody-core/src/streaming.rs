//! Bridge between the async pipeline and the Nemotron streaming engine
//! (feature `nemotron`).
//!
//! The engine decodes on big 560ms chunks (~60–80ms compute each on Apple
//! silicon) and is not `Sync`, so it lives on its own dedicated OS thread.
//! The pipeline feeds it raw 16kHz mono samples from the live audio tap via
//! a std mpsc channel and receives partial/final transcripts back on a tokio
//! channel it can `select!` on.
//!
//! Decode happens DURING the recording: by the time the user releases the
//! key, only the sub-chunk tail remains to flush (~55ms measured), so the
//! final transcript lands near-instantly — unlike batch Whisper, which only
//! starts transcribing at key release.

use std::time::Instant;

use rekody_stt::nemotron::NemotronStreamingEngine;

/// Messages from the pipeline to the engine thread.
pub enum StreamMsg {
    /// Raw 16kHz mono samples from the live audio tap.
    Samples(Vec<f32>),
    /// Key released: flush the tail, emit `StreamEvent::Final`, reset state.
    Flush,
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
pub fn spawn(
    model_dir: std::path::PathBuf,
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
            let mut engine = match NemotronStreamingEngine::new(&dir) {
                Ok(e) => e,
                Err(e) => {
                    let _ = event_tx.send(StreamEvent::Error(format!(
                        "failed to load Nemotron model from {dir}: {e:#}"
                    )));
                    return;
                }
            };
            tracing::info!(
                load_secs = format!("{:.1}", t_load.elapsed().as_secs_f64()),
                "Nemotron streaming model ready"
            );

            while let Ok(msg) = msg_rx.recv() {
                match msg {
                    StreamMsg::Samples(samples) => match engine.feed(&samples) {
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
                            let _ = event_tx
                                .send(StreamEvent::Error(format!("nemotron decode failed: {e:#}")));
                        }
                    },
                    StreamMsg::Flush => {
                        let t = Instant::now();
                        match engine.finish() {
                            Ok(text) => {
                                let latency_ms = t.elapsed().as_millis() as u64;
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
                }
            }
            tracing::debug!("nemotron stream thread exiting (pipeline closed)");
        })
        .expect("spawn nemotron stream thread");

    (msg_tx, event_rx)
}
