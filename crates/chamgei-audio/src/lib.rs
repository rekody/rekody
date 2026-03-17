//! Audio capture, resampling, and voice activity detection for Chamgei.
//!
//! Captures audio from the system microphone via cpal, resamples to
//! 16kHz mono via rubato, and filters silence using energy-based VAD.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use rubato::{FftFixedIn, Resampler};
use thiserror::Error;
use tokio::sync::mpsc;

/// Target sample rate for STT processing.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Number of samples per VAD frame at 16kHz (30ms frames).
const VAD_FRAME_SAMPLES: usize = 480;

/// Minimum speech duration in seconds to emit a segment.
const MIN_SPEECH_DURATION_SECS: f32 = 0.15;

/// Trailing silence duration (in seconds) before finalizing a speech segment.
const SILENCE_TAIL_SECS: f32 = 0.6;

/// Maximum recording duration in seconds to prevent unbounded memory growth.
/// 10 minutes — beats Wispr Flow's 6-minute limit. At 16kHz mono f32,
/// 10 min = ~38 MB RAM, well within Groq's 25 MB WAV upload limit
/// (the WAV is PCM16 = half the size = ~19 MB).
const MAX_RECORDING_SECS: f32 = 600.0;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no input device available")]
    NoInputDevice,
    #[error("failed to open audio stream: {0}")]
    StreamError(String),
    #[error("microphone permission denied")]
    PermissionDenied,
}

/// A captured audio segment ready for STT processing.
#[derive(Debug, Clone)]
pub struct AudioSegment {
    /// PCM samples at 16kHz mono, f32 format.
    pub samples: Vec<f32>,
    /// Duration in seconds.
    pub duration_secs: f32,
}

/// Configuration for audio capture.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// RMS energy threshold for VAD. Frames with RMS above this
    /// are considered speech. Typical range: 0.005 - 0.05.
    /// The `default()` value of 0.01 works well for most microphones.
    pub vad_threshold: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            vad_threshold: 0.01,
        }
    }
}

/// Manages the audio capture lifecycle.
///
/// Call [`AudioCapture::new`] to initialize, then [`start_recording`](AudioCapture::start_recording)
/// and [`stop_recording`](AudioCapture::stop_recording) to control capture.
/// Completed speech segments are emitted through the returned channel receiver.
pub struct AudioCapture {
    recording: Arc<AtomicBool>,
    /// Signals the capture thread to shut down entirely.
    shutdown: Arc<AtomicBool>,
    /// Signals the processing thread to flush any buffered speech immediately.
    flush: Arc<AtomicBool>,
}


impl AudioCapture {
    /// Create a new `AudioCapture` with the given configuration.
    pub fn new(config: AudioConfig) -> Self {
        let _ = config; // stored implicitly via open()
        Self {
            recording: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            flush: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Open the default input device and start the capture thread.
    ///
    /// Returns a receiver that yields [`AudioSegment`]s whenever speech is
    /// detected. The capture thread runs in the background; use
    /// [`start_recording`](Self::start_recording) / [`stop_recording`](Self::stop_recording)
    /// to gate actual audio processing.
    ///
    /// The stream is kept alive even while not recording so that start/stop
    /// latency is minimal.
    pub fn open(&self, config: AudioConfig) -> Result<mpsc::UnboundedReceiver<AudioSegment>> {
        let (segment_tx, segment_rx) = mpsc::unbounded_channel();

        let recording = Arc::clone(&self.recording);
        let shutdown = Arc::clone(&self.shutdown);
        let flush = Arc::clone(&self.flush);

        // Use a oneshot channel so the audio thread can report init errors
        // back to the caller synchronously.
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), AudioError>>(1);

        // The cpal Stream type is !Send on macOS, so we must create the
        // stream on the same thread that will keep it alive.
        std::thread::Builder::new()
            .name("chamgei-audio-proc".into())
            .spawn(move || {
                // ----- device & stream setup (runs on this thread) -----
                let host = cpal::default_host();
                let device = match host.default_input_device() {
                    Some(d) => d,
                    None => {
                        let _ = init_tx.send(Err(AudioError::NoInputDevice));
                        return;
                    }
                };

                let supported_config = match device.default_input_config() {
                    Ok(c) => c,
                    Err(e) => {
                        let msg = e.to_string();
                        let err = if msg.to_lowercase().contains("permission") {
                            AudioError::PermissionDenied
                        } else {
                            AudioError::StreamError(msg)
                        };
                        let _ = init_tx.send(Err(err));
                        return;
                    }
                };

                let sample_format = supported_config.sample_format();
                let input_config: StreamConfig = supported_config.into();
                let input_rate = input_config.sample_rate.0;
                let input_channels = input_config.channels as usize;

                tracing::info!(
                    device = ?device.name().unwrap_or_default(),
                    sample_rate = input_rate,
                    channels = input_channels,
                    format = ?sample_format,
                    "opened default input device"
                );

                // Channel to shuttle raw f32 samples from the cpal callback
                // to this processing thread.
                let (raw_tx, raw_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);

                let recording_for_cb = Arc::clone(&recording);
                let err_callback = |err: cpal::StreamError| {
                    tracing::error!(%err, "audio stream error");
                };

                let stream_result = match sample_format {
                    SampleFormat::F32 => {
                        device.build_input_stream(
                            &input_config,
                            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                                if recording_for_cb.load(Ordering::Relaxed) {
                                    let _ = raw_tx.try_send(data.to_vec());
                                }
                            },
                            err_callback,
                            None,
                        )
                    }
                    SampleFormat::I16 => {
                        let raw_tx = raw_tx;
                        let rec = Arc::clone(&recording);
                        device.build_input_stream(
                            &input_config,
                            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                                if rec.load(Ordering::Relaxed) {
                                    let floats: Vec<f32> =
                                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                                    let _ = raw_tx.try_send(floats);
                                }
                            },
                            err_callback,
                            None,
                        )
                    }
                    SampleFormat::U16 => {
                        let raw_tx = raw_tx;
                        let rec = Arc::clone(&recording);
                        device.build_input_stream(
                            &input_config,
                            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                                if rec.load(Ordering::Relaxed) {
                                    let floats: Vec<f32> = data
                                        .iter()
                                        .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                                        .collect();
                                    let _ = raw_tx.try_send(floats);
                                }
                            },
                            err_callback,
                            None,
                        )
                    }
                    _ => {
                        let _ = init_tx.send(Err(AudioError::StreamError(
                            format!("unsupported sample format: {sample_format:?}"),
                        )));
                        return;
                    }
                };

                let stream = match stream_result {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = init_tx.send(Err(AudioError::StreamError(e.to_string())));
                        return;
                    }
                };

                if let Err(e) = stream.play() {
                    let _ = init_tx.send(Err(AudioError::StreamError(e.to_string())));
                    return;
                }

                // Signal success to the caller.
                let _ = init_tx.send(Ok(()));

                // ----- processing loop -----
                let vad_threshold = config.vad_threshold;
                let needs_resample = input_rate != TARGET_SAMPLE_RATE;

                let chunk_size = 1024_usize;
                let mut resampler = if needs_resample {
                    Some(
                        FftFixedIn::<f32>::new(
                            input_rate as usize,
                            TARGET_SAMPLE_RATE as usize,
                            chunk_size,
                            1, // sub_chunks
                            1, // mono after down-mix
                        )
                        .expect("failed to create resampler"),
                    )
                } else {
                    None
                };

                let mut mono_buf: Vec<f32> = Vec::with_capacity(chunk_size * 4);
                let mut resampled_buf: Vec<f32> = Vec::new();

                // VAD state
                let mut speech_buf: Vec<f32> = Vec::new();
                let silence_frames_limit =
                    (SILENCE_TAIL_SECS * TARGET_SAMPLE_RATE as f32) as usize / VAD_FRAME_SAMPLES;
                let mut consecutive_silence: usize = 0;
                let mut in_speech = false;

                tracing::info!("audio processing loop started");

                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("audio processing thread shutting down");
                        break;
                    }

                    // Check if we've been asked to flush buffered speech
                    // (recording just stopped).
                    if flush.load(Ordering::Relaxed) {
                        flush.store(false, Ordering::Relaxed);
                        if !speech_buf.is_empty() {
                            let duration_secs =
                                speech_buf.len() as f32 / TARGET_SAMPLE_RATE as f32;
                            if duration_secs >= MIN_SPEECH_DURATION_SECS {
                                let segment = AudioSegment {
                                    samples: std::mem::take(&mut speech_buf),
                                    duration_secs,
                                };
                                tracing::info!(
                                    duration = duration_secs,
                                    "flushing audio segment (recording stopped)"
                                );
                                let _ = segment_tx.send(segment);
                            } else {
                                speech_buf.clear();
                            }
                            in_speech = false;
                            consecutive_silence = 0;
                        }
                    }

                    let raw_samples =
                        match raw_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                            Ok(s) => s,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        };

                    // Down-mix to mono.
                    if input_channels == 1 {
                        mono_buf.extend_from_slice(&raw_samples);
                    } else {
                        for frame in raw_samples.chunks(input_channels) {
                            let sum: f32 = frame.iter().sum();
                            mono_buf.push(sum / input_channels as f32);
                        }
                    }

                    // Resample (or pass through).
                    if let Some(ref mut rs) = resampler {
                        let input_frames_needed = rs.input_frames_next();
                        while mono_buf.len() >= input_frames_needed {
                            let input_chunk: Vec<f32> =
                                mono_buf.drain(..input_frames_needed).collect();
                            let input_ref: Vec<&[f32]> = vec![&input_chunk];
                            match rs.process(&input_ref, None) {
                                Ok(output) => {
                                    if let Some(ch) = output.first() {
                                        resampled_buf.extend_from_slice(ch);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(%e, "resampling error");
                                }
                            }
                        }
                    } else {
                        resampled_buf.append(&mut mono_buf);
                    }

                    // Run energy-based VAD on 30ms frames.
                    //
                    // While recording is active (push-to-talk held), we
                    // accumulate ALL speech into one buffer and never emit
                    // mid-recording segments. The single combined segment
                    // is flushed when stop_recording() sets the flush flag.
                    let currently_recording = recording.load(Ordering::Relaxed);

                    while resampled_buf.len() >= VAD_FRAME_SAMPLES {
                        let frame: Vec<f32> =
                            resampled_buf.drain(..VAD_FRAME_SAMPLES).collect();
                        let rms = compute_rms(&frame);
                        let is_speech = rms > vad_threshold;

                        if is_speech {
                            consecutive_silence = 0;
                            if !in_speech {
                                in_speech = true;
                                tracing::trace!("speech start detected (rms={rms:.4})");
                            }
                            speech_buf.extend_from_slice(&frame);
                        } else if in_speech {
                            speech_buf.extend_from_slice(&frame);
                            consecutive_silence += 1;

                            // Only split on silence when NOT actively recording.
                            // During recording, keep accumulating into one buffer.
                            if !currently_recording && consecutive_silence >= silence_frames_limit {
                                let trailing = silence_frames_limit * VAD_FRAME_SAMPLES;
                                let trimmed_len = speech_buf.len().saturating_sub(trailing);
                                speech_buf.truncate(trimmed_len);

                                let duration_secs =
                                    speech_buf.len() as f32 / TARGET_SAMPLE_RATE as f32;

                                if duration_secs >= MIN_SPEECH_DURATION_SECS {
                                    let segment = AudioSegment {
                                        samples: std::mem::take(&mut speech_buf),
                                        duration_secs,
                                    };
                                    tracing::debug!(
                                        duration = duration_secs,
                                        "emitting audio segment"
                                    );
                                    if segment_tx.send(segment).is_err() {
                                        tracing::info!(
                                            "segment receiver dropped, stopping capture"
                                        );
                                        return;
                                    }
                                } else {
                                    speech_buf.clear();
                                }

                                in_speech = false;
                                consecutive_silence = 0;
                            }
                        }

                        // Auto-flush to prevent unbounded memory growth.
                        let current_duration = speech_buf.len() as f32 / TARGET_SAMPLE_RATE as f32;
                        if in_speech && current_duration >= MAX_RECORDING_SECS {
                            tracing::warn!("max recording duration reached ({MAX_RECORDING_SECS}s), auto-flushing");
                            let duration_secs = current_duration;
                            let segment = AudioSegment {
                                samples: std::mem::take(&mut speech_buf),
                                duration_secs,
                            };
                            let _ = segment_tx.send(segment);
                            in_speech = false;
                            consecutive_silence = 0;
                        }
                    }
                }

                // Keep the stream alive until the loop exits.
                drop(stream);

                // Flush remaining speech on shutdown.
                if !speech_buf.is_empty() {
                    let duration_secs = speech_buf.len() as f32 / TARGET_SAMPLE_RATE as f32;
                    if duration_secs >= MIN_SPEECH_DURATION_SECS {
                        let segment = AudioSegment {
                            samples: speech_buf,
                            duration_secs,
                        };
                        let _ = segment_tx.send(segment);
                    }
                }
            })
            .map_err(|e| AudioError::StreamError(format!("failed to spawn audio thread: {e}")))?;

        // Wait for the audio thread to finish initialization.
        init_rx
            .recv()
            .map_err(|_| AudioError::StreamError("audio thread exited during init".into()))?
            .map_err(anyhow::Error::from)?;

        Ok(segment_rx)
    }

    /// Begin capturing audio. Frames are processed and speech segments are
    /// emitted through the channel returned by [`open`](Self::open).
    pub fn start_recording(&self) {
        tracing::info!("recording started");
        self.recording.store(true, Ordering::Relaxed);
    }

    /// Pause audio capture. The stream stays open but incoming samples are
    /// discarded, so resuming via [`start_recording`](Self::start_recording)
    /// is near-instant.
    pub fn stop_recording(&self) {
        tracing::info!("recording stopped");
        self.recording.store(false, Ordering::Relaxed);
        // Signal the processing thread to flush any buffered speech immediately
        // rather than waiting for the silence tail timeout.
        self.flush.store(true, Ordering::Relaxed);
    }

    /// Returns `true` if currently recording.
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }

    /// Permanently shut down the capture thread. After calling this the
    /// `AudioCapture` instance cannot be reused.
    pub fn shutdown(&self) {
        self.stop_recording();
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Compute RMS (root mean square) energy of a sample buffer.
fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Convenience function: starts audio capture and returns captured segments
/// via a channel. This is a simplified wrapper around [`AudioCapture`].
///
/// Recording begins immediately. Drop the returned receiver (or the
/// `AudioCapture`) to stop.
pub fn start_capture(
    config: AudioConfig,
) -> Result<(AudioCapture, mpsc::UnboundedReceiver<AudioSegment>)> {
    let capture = AudioCapture::new(config.clone());
    let rx = capture.open(config)?;
    capture.start_recording();
    Ok((capture, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rms_silence() {
        let silence = vec![0.0f32; 480];
        assert_eq!(compute_rms(&silence), 0.0);
    }

    #[test]
    fn test_rms_signal() {
        // A constant signal of 0.5 should have RMS = 0.5
        let signal = vec![0.5f32; 480];
        let rms = compute_rms(&signal);
        assert!((rms - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_rms_empty() {
        assert_eq!(compute_rms(&[]), 0.0);
    }

    #[test]
    fn test_audio_config_default() {
        let config = AudioConfig::default();
        assert!(config.vad_threshold > 0.0);
        assert!(config.vad_threshold < 1.0);
    }

    #[test]
    fn test_audio_segment_creation() {
        let seg = AudioSegment {
            samples: vec![0.1, 0.2, 0.3],
            duration_secs: 0.5,
        };
        assert_eq!(seg.samples.len(), 3);
        assert!((seg.duration_secs - 0.5).abs() < f32::EPSILON);
    }
}
