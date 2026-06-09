//! Nemotron cache-aware streaming STT engine (feature `nemotron`).
//!
//! Wraps `parakeet_rs::Nemotron` (ONNX Runtime, CPU EP — CoreML EP is
//! unstable for this model per upstream) for push-to-talk streaming: feed raw
//! 16kHz mono samples as they arrive from the live audio tap, get incremental
//! text back, and collect the final transcript at key release.
//!
//! Measured on an M2 Air with the int8 English export: ~81ms compute per
//! 560ms chunk (6.8× realtime), model load ~3.4s (do it once, off the hot
//! path). The decode happens DURING the recording, so the transcript is
//! ready ~immediately at release — unlike batch Whisper, which only starts
//! when the key is released.

use anyhow::{Context, Result};
use parakeet_rs::Nemotron;

/// Samples per streaming chunk: 560ms at 16kHz. Fixed by the published
/// cache-aware ONNX exports.
pub const CHUNK_SAMPLES: usize = 8960;

/// Streaming engine state for one or more push-to-talk utterances.
///
/// Not `Sync`; intended to be owned by a single dedicated thread that
/// receives live audio chunks and feeds them here.
pub struct NemotronStreamingEngine {
    model: Nemotron,
    /// Pending samples not yet forming a full chunk.
    buf: Vec<f32>,
    /// Accumulated transcript for the current utterance.
    transcript: String,
}

impl NemotronStreamingEngine {
    /// Load the model from a directory containing `encoder.onnx`,
    /// `decoder_joint.onnx`, and `tokenizer.model`. Takes a few seconds —
    /// call once at daemon startup, not per utterance.
    pub fn new(model_dir: &str) -> Result<Self> {
        let model = Nemotron::from_pretrained(model_dir, None)
            .with_context(|| format!("loading Nemotron streaming model from {model_dir}"))?;
        Ok(Self {
            model,
            buf: Vec::with_capacity(CHUNK_SAMPLES * 2),
            transcript: String::new(),
        })
    }

    /// Feed raw 16kHz mono samples. Processes any complete 560ms chunks and
    /// returns newly emitted text (empty string if no chunk completed or the
    /// chunk produced no tokens).
    pub fn feed(&mut self, samples: &[f32]) -> Result<String> {
        self.buf.extend_from_slice(samples);
        let mut emitted = String::new();
        while self.buf.len() >= CHUNK_SAMPLES {
            let chunk: Vec<f32> = self.buf.drain(..CHUNK_SAMPLES).collect();
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
        if !self.buf.is_empty() {
            let mut chunk = std::mem::take(&mut self.buf);
            chunk.resize(CHUNK_SAMPLES, 0.0);
            let text = self
                .model
                .transcribe_chunk(&chunk)
                .context("nemotron flush chunk")?;
            self.transcript.push_str(&text);
        }
        let transcript = std::mem::take(&mut self.transcript);
        self.buf.clear();
        // Clear the cache-aware decoder state between utterances so context
        // from the previous dictation can't bleed into the next one.
        self.model.reset();
        Ok(transcript.trim().to_string())
    }

    /// The transcript accumulated so far for the current utterance.
    pub fn transcript(&self) -> &str {
        &self.transcript
    }
}
