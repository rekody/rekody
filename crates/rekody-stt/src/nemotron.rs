//! Nemotron cache-aware streaming STT engine (feature `nemotron`).
//!
//! Wraps `parakeet_rs::Nemotron` (ONNX Runtime, CPU EP — CoreML EP is
//! unstable for this model per upstream) for push-to-talk streaming: feed raw
//! 16kHz mono samples as they arrive from the live audio tap, get incremental
//! text back, and collect the final transcript at key release.
//!
//! Measured on an M2 with the int8 English export at the 160ms profile:
//! ~48ms compute per 160ms chunk on short dictation and ~43ms on streams over
//! a minute, so the pipeline keeps better than 3x ahead of a speaker. Model
//! load ~1.2s (do it once, off the hot path). The decode happens DURING the
//! recording, so at release only the flush remains, unlike batch Whisper,
//! which only starts when the key is released.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use parakeet_rs::Nemotron;

use crate::biasing::sp::SpModel;
use crate::biasing::{BiasSettings, LogitsProcessor, TermBias};

/// Trailing silence pushed through the encoder when an utterance ends.
///
/// Cache-aware streaming emits a chunk's frames using the rest of that chunk
/// as lookahead, so the final real frames need trailing audio before the
/// RNN-T will commit the last word. The old code padded the residual out to
/// ONE chunk, which at 560 ms handed the model 280 ms of silence on average
/// but at 160 ms would hand it only 80 ms, three and a half times less
/// flush exactly where trailing words get dropped.
///
/// Making the flush a fixed DURATION instead of a fixed chunk count keeps
/// end-of-utterance behaviour identical across latency profiles, and never
/// below what the 560 ms profile did at its worst. 560 ms at 16 kHz.
const FLUSH_SILENCE_SAMPLES: usize = 8960;

/// Decode-time term biasing state (spec step 4). Present only when the
/// engine was constructed with [`BiasSettings`]; `None` means the fork's
/// logits-processor hook is never installed and decoding is byte-identical
/// to a build without the feature.
struct BiasState {
    /// Tokenizer parsed once at engine construction; term reloads rebuild
    /// only the trie (microseconds), never re-read the model file.
    sp: SpModel,
    /// Settings from the `[term_biasing]` config table. `terms` is the only
    /// field that changes after construction (via `set_terms`).
    cfg: BiasSettings,
    /// Completed term matches of the most recently finished utterance,
    /// written by the installed processor at `on_reset` time. An atomic is
    /// the channel back out: the processor itself is boxed away inside the
    /// model once installed.
    hits: Arc<AtomicU64>,
    /// Term reload requested mid-utterance; applied inside `finish()` after
    /// `model.reset()` so the trie never swaps under a live partial match.
    pending_terms: Option<Vec<String>>,
}

/// Wraps [`TermBias`] to publish its per-utterance hit count through a
/// shared atomic before the count is cleared. `Nemotron::reset` runs inside
/// `finish()`, so by the time the engine reads the atomic the value is the
/// finished utterance's total.
struct HitsReporter {
    inner: TermBias,
    hits: Arc<AtomicU64>,
}

impl LogitsProcessor for HitsReporter {
    fn process(&mut self, logits: &mut [f32], blank_id: usize) {
        self.inner.process(logits, blank_id);
    }

    fn on_emit(&mut self, token: usize) {
        self.inner.on_emit(token);
    }

    fn on_reset(&mut self) {
        self.hits.store(self.inner.hits(), Ordering::Relaxed);
        self.inner.on_reset();
    }
}

/// Streaming engine state for one or more push-to-talk utterances.
///
/// Not `Sync`; intended to be owned by a single dedicated thread that
/// receives live audio chunks and feeds them here.
pub struct NemotronStreamingEngine {
    model: Nemotron,
    /// Samples per encoder call, read from the loaded artifact's streaming
    /// profile. 8960 (560 ms) for a `[70,6]` export, 2560 (160 ms) for
    /// `[70,1]`.
    chunk_samples: usize,
    /// Pending samples not yet forming a full chunk.
    buf: Vec<f32>,
    /// Accumulated transcript for the current utterance.
    transcript: String,
    /// Term biasing state; `None` when the feature is off.
    bias: Option<BiasState>,
}

impl NemotronStreamingEngine {
    /// Load the model from a directory containing `encoder.onnx`,
    /// `decoder_joint.onnx`, and `tokenizer.model`. Takes a few seconds;
    /// call once at daemon startup, not per utterance.
    ///
    /// Equivalent to [`Self::new_with_bias`] with `None`: no term biasing,
    /// no logits processor installed.
    pub fn new(model_dir: &str) -> Result<Self> {
        Self::new_with_bias(model_dir, None)
    }

    /// Load the model and, when `bias` is `Some`, arm decode-time dictionary
    /// term biasing (spec: docs/design/nemotron-term-biasing-spec.md): parse
    /// `tokenizer.model` from the same `model_dir`, encode the terms, build
    /// the trie, and install the processor via the fork's
    /// `set_logits_processor` hook. With `None`, or when no term survives
    /// encoding, no processor is installed at all, so the decode loop pays
    /// nothing beyond the fork's `Option` check.
    pub fn new_with_bias(model_dir: &str, bias: Option<BiasSettings>) -> Result<Self> {
        let model = Nemotron::from_pretrained(model_dir, None)
            .with_context(|| format!("loading Nemotron streaming model from {model_dir}"))?;
        // The tokenizer is parsed even when the term list is currently
        // empty: a later `set_terms` (user ran `rekody dictionary add`)
        // must be able to build a trie without re-reading the file.
        let bias = bias.and_then(|cfg| {
            let path = std::path::Path::new(model_dir).join("tokenizer.model");
            match SpModel::from_file(&path) {
                Ok(sp) => Some(BiasState {
                    sp,
                    cfg,
                    hits: Arc::new(AtomicU64::new(0)),
                    pending_terms: None,
                }),
                Err(e) => {
                    // Biasing must never take dictation down with it: the
                    // model itself loaded fine (parakeet parses this same
                    // file), so log loudly and run unbiased.
                    tracing::warn!(
                        error = format!("{e:#}"),
                        path = %path.display(),
                        "term biasing disabled: tokenizer.model failed to parse"
                    );
                    None
                }
            }
        });
        // Read the chunk geometry off the artifact that actually loaded.
        let chunk_samples = model.chunk_samples();
        tracing::info!(
            chunk_samples,
            chunk_ms = chunk_samples * 1000 / 16_000,
            "nemotron streaming profile"
        );
        let mut engine = Self {
            model,
            chunk_samples,
            buf: Vec::with_capacity(chunk_samples * 2),
            transcript: String::new(),
            bias,
        };
        engine.install_processor();
        Ok(engine)
    }

    /// Replace the biased term list, e.g. after `rekody dictionary add`.
    /// No-op when the engine was built without biasing. Mid-utterance calls
    /// are deferred and applied inside [`Self::finish`] right after
    /// `model.reset()`, so the trie never changes while a partial match is
    /// alive.
    pub fn set_terms(&mut self, terms: Vec<String>) {
        let Some(state) = self.bias.as_mut() else {
            tracing::debug!("set_terms ignored: term biasing is not active");
            return;
        };
        if !self.buf.is_empty() || !self.transcript.is_empty() {
            state.pending_terms = Some(terms);
            tracing::debug!("term reload deferred until the current utterance finishes");
            return;
        }
        self.apply_terms(terms);
    }

    /// Completed term matches in the most recently finished utterance
    /// (`bias_hits`). `None` when term biasing is not active, so callers can
    /// keep the off path free of even a log line.
    pub fn bias_hits(&self) -> Option<u64> {
        self.bias
            .as_ref()
            .map(|state| state.hits.load(Ordering::Relaxed))
    }

    /// Store the new term list and rebuild/install the processor.
    fn apply_terms(&mut self, terms: Vec<String>) {
        if let Some(state) = self.bias.as_mut() {
            state.cfg.terms = terms;
        }
        self.install_processor();
    }

    /// (Re)build the trie from the current settings and install it through
    /// the fork hook. An empty or fully-unencodable term list installs
    /// `None`, removing any previous processor.
    fn install_processor(&mut self) {
        let Some(state) = self.bias.as_ref() else {
            return;
        };
        let processor = TermBias::build(&state.sp, state.cfg.clone());
        match processor {
            Some(term_bias) => {
                tracing::info!(
                    terms = state.cfg.terms.len(),
                    "term biasing processor installed"
                );
                self.model.set_logits_processor(Some(Box::new(HitsReporter {
                    inner: term_bias,
                    hits: Arc::clone(&state.hits),
                })));
            }
            None => {
                tracing::debug!(
                    terms = state.cfg.terms.len(),
                    "term biasing idle: no encodable terms, no processor installed"
                );
                state.hits.store(0, Ordering::Relaxed);
                self.model.set_logits_processor(None);
            }
        }
    }

    /// Feed raw 16kHz mono samples. Processes any complete chunks and
    /// returns newly emitted text (empty string if no chunk completed or the
    /// chunk produced no tokens).
    pub fn feed(&mut self, samples: &[f32]) -> Result<String> {
        self.buf.extend_from_slice(samples);
        let mut emitted = String::new();
        while self.buf.len() >= self.chunk_samples {
            let chunk: Vec<f32> = self.buf.drain(..self.chunk_samples).collect();
            let text = self
                .model
                .transcribe_chunk(&chunk)
                .context("nemotron transcribe_chunk")?;
            emitted.push_str(&text);
        }
        self.transcript.push_str(&emitted);
        Ok(emitted)
    }

    /// End of utterance: pad-flush the remaining samples, return the full
    /// transcript, and reset all state (decoder cache included) so the next
    /// utterance starts fresh.
    pub fn finish(&mut self) -> Result<String> {
        // Drain the tail, then keep pushing whole chunks of silence until at
        // least FLUSH_SILENCE_SAMPLES of zeros have gone through the encoder.
        // Zeros used to pad the partial chunk count toward that total, so the
        // 560 ms profile does what it always did plus the guaranteed minimum,
        // and the 160 ms profile gets the same trailing silence rather than a
        // third of it.
        let mut zeros_fed = 0usize;
        if !self.buf.is_empty() {
            let mut chunk = std::mem::take(&mut self.buf);
            zeros_fed += self.chunk_samples.saturating_sub(chunk.len());
            chunk.resize(self.chunk_samples, 0.0);
            let text = self
                .model
                .transcribe_chunk(&chunk)
                .context("nemotron flush chunk")?;
            self.transcript.push_str(&text);
        }
        let silence = vec![0.0f32; self.chunk_samples];
        while zeros_fed < FLUSH_SILENCE_SAMPLES {
            let text = self
                .model
                .transcribe_chunk(&silence)
                .context("nemotron flush silence")?;
            self.transcript.push_str(&text);
            zeros_fed += self.chunk_samples;
        }
        let transcript = std::mem::take(&mut self.transcript);
        self.buf.clear();
        // Clear the cache-aware decoder state between utterances so context
        // from the previous dictation can't bleed into the next one. This
        // also fires the processor's `on_reset`, which publishes the
        // utterance's `bias_hits` before clearing them.
        self.model.reset();
        // A term reload that arrived mid-utterance applies now, with the
        // trie state freshly cleared.
        if let Some(terms) = self.bias.as_mut().and_then(|s| s.pending_terms.take()) {
            self.apply_terms(terms);
        }
        Ok(transcript.trim().to_string())
    }

    /// Samples this engine consumes per encoder call.
    ///
    /// This is a property of the ARTIFACT, not of Rekody: the cache-aware
    /// streaming profile is baked into the exported ONNX graph. 8960 (560 ms)
    /// for a `[70,6]` export, 2560 (160 ms) for the `[70,1]` export shipped
    /// today. There is deliberately no constant to reach for instead: a
    /// hardcoded slice size silently stalls the encoder when it is too small
    /// and silently drops frames when it is too large, and neither failure
    /// surfaces as an error. Always ask the engine.
    pub fn chunk_samples(&self) -> usize {
        self.chunk_samples
    }

    /// The transcript accumulated so far for the current utterance.
    pub fn transcript(&self) -> &str {
        &self.transcript
    }
}
