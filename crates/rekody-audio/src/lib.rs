//! Audio capture, resampling, and voice activity detection for rekody.
//!
//! Captures audio from the system microphone via cpal, resamples to
//! 16kHz mono via rubato, and filters silence using energy-based VAD.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use rubato::{FftFixedIn, Resampler};
use thiserror::Error;
use tokio::sync::mpsc;

/// Target sample rate for STT processing. All captured audio — both
/// [`AudioSegment::samples`] and live-tap chunks — is 16kHz mono, so
/// `samples / TARGET_SAMPLE_RATE` is the exact captured-speech duration.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Number of samples per VAD frame at 16kHz (30ms frames).
const VAD_FRAME_SAMPLES: usize = 480;

/// Minimum speech duration in seconds to emit a segment.
const MIN_SPEECH_DURATION_SECS: f32 = 0.15;

/// Trailing silence duration (in seconds) before finalizing a speech segment.
const SILENCE_TAIL_SECS: f32 = 0.6;

/// Maximum recording duration in seconds to prevent unbounded memory growth.
/// 30 minutes headroom for hands-free rambles (the configurable
/// max_recording_secs deadman is the real per-user limit). At 16kHz mono
/// f32, 30 min = ~115 MB RAM. NOTE: Groq's 25 MB WAV upload limit means
/// cloud STT effectively caps near 10 min regardless.
const MAX_RECORDING_SECS: f32 = 1800.0;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no input device available")]
    NoInputDevice,
    #[error("failed to open audio stream: {0}")]
    StreamError(String),
    #[error("microphone permission denied")]
    PermissionDenied,
}

/// Result of a lightweight microphone permission probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicStatus {
    /// Default input device is accessible — microphone permission granted.
    Granted,
    /// A "permission denied" error was surfaced by the OS.
    Denied,
    /// No input device is attached (not the same as denied).
    NoDevice,
    /// Some other error (format, hardware, driver). Treated as inconclusive.
    Unknown,
}

/// Briefly open the default input device to probe microphone permission.
///
/// On macOS this triggers the TCC prompt on first call from a new
/// "responsible process" (typically the parent terminal). It also surfaces
/// `Denied` synchronously if the user has already rejected access.
///
/// The stream is opened, played, and dropped within ~50 ms. No audio is
/// retained. Safe to call from any thread — the stream is created and
/// destroyed on the calling thread so its `!Send` bound is respected.
pub fn probe_microphone() -> MicStatus {
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => return MicStatus::NoDevice,
    };

    // default_input_config() is where cpal-on-macOS enforces microphone
    // TCC. It returns a "permission denied" error synchronously if the
    // user has blocked access, and triggers the TCC prompt on first access.
    let supported_config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            return if msg.contains("permission") || msg.contains("denied") {
                MicStatus::Denied
            } else {
                MicStatus::Unknown
            };
        }
    };

    let sample_format = supported_config.sample_format();
    let input_config: StreamConfig = supported_config.into();

    // Build + play a minimal stream so the OS sees real audio access.
    // Some macOS versions defer the prompt until stream.play() rather than
    // default_input_config(), so we do both to be reliable.
    let err_cb = |err: cpal::StreamError| {
        tracing::trace!(%err, "mic probe stream error");
    };

    let stream_result = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &input_config,
            |_data: &[f32], _: &cpal::InputCallbackInfo| {},
            err_cb,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            &input_config,
            |_data: &[i16], _: &cpal::InputCallbackInfo| {},
            err_cb,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            &input_config,
            |_data: &[u16], _: &cpal::InputCallbackInfo| {},
            err_cb,
            None,
        ),
        _ => return MicStatus::Unknown,
    };

    let stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            return if msg.contains("permission") || msg.contains("denied") {
                MicStatus::Denied
            } else {
                MicStatus::Unknown
            };
        }
    };

    if let Err(e) = stream.play() {
        let msg = e.to_string().to_lowercase();
        return if msg.contains("permission") || msg.contains("denied") {
            MicStatus::Denied
        } else {
            MicStatus::Unknown
        };
    }

    // Hold the stream briefly so macOS registers actual audio access.
    std::thread::sleep(std::time::Duration::from_millis(50));

    drop(stream);
    MicStatus::Granted
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
    /// If true, bypass VAD entirely while recording — capture every frame
    /// from press to release. Useful for transcribing low-energy input
    /// (e.g. phone-speaker playback into the mic) where VAD would otherwise
    /// drop everything as silence.
    pub record_all_audio: bool,
    /// Preferred input device, matched by name (case-insensitive). `None`
    /// (or `"system"`) follows the OS default input — which can drift to
    /// whatever was plugged in last (e.g. AirPods). Set a device name to
    /// pin capture to it regardless of the system default. Resolved fresh
    /// each recording, so a disconnected device transparently falls back.
    pub input_device: Option<String>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            vad_threshold: 0.01,
            record_all_audio: false,
            input_device: None,
        }
    }
}

/// Names of all available input devices, for pickers and `rekody doctor`.
/// Returns an empty list if the host can't enumerate (never panics).
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Index of the device whose name matches `want` (case-insensitive): an
/// exact match wins, otherwise the first substring match. `None` if nothing
/// matches. Pure helper, unit-tested.
fn match_device_name(available: &[String], want: &str) -> Option<usize> {
    let want = want.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    available
        .iter()
        .position(|n| n.to_lowercase() == want)
        .or_else(|| {
            available
                .iter()
                .position(|n| n.to_lowercase().contains(&want))
        })
}

/// Resolve the input device to capture from: the configured name if it
/// matches an available device, otherwise the system default. A configured
/// name that no longer matches (device unplugged, renamed) logs a warning
/// and falls back — capture never hard-fails on a stale preference.
fn resolve_input_device(host: &cpal::Host, configured: Option<&str>) -> Option<cpal::Device> {
    let want = configured
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("system"));

    if let Some(want) = want {
        let devices: Vec<cpal::Device> = host
            .input_devices()
            .map(|it| it.collect())
            .unwrap_or_default();
        let names: Vec<String> = devices
            .iter()
            .map(|d| d.name().unwrap_or_default())
            .collect();
        if let Some(idx) = match_device_name(&names, want) {
            tracing::info!(device = %names[idx], "using configured input device");
            return devices.into_iter().nth(idx);
        }
        tracing::warn!(
            configured = %want,
            "configured input_device not found — falling back to system default"
        );
    }
    host.default_input_device()
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
    /// Latest VAD frame RMS energy, stored as `f32::to_bits()`. Updated by
    /// the processing thread on every VAD frame; read by UI threads to
    /// render a live audio level meter.
    latest_rms_bits: Arc<AtomicU32>,
    /// Optional live tap: 16kHz mono samples forwarded as they are produced
    /// while recording, for streaming STT engines. Set via
    /// [`live_chunks`](Self::live_chunks) BEFORE [`open`](Self::open).
    live_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<Vec<f32>>>>,
}

impl AudioCapture {
    /// Create a new `AudioCapture` with the given configuration.
    pub fn new(config: AudioConfig) -> Self {
        let _ = config; // stored implicitly via open()
        Self {
            recording: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            flush: Arc::new(AtomicBool::new(false)),
            latest_rms_bits: Arc::new(AtomicU32::new(0)),
            live_tx: std::sync::Mutex::new(None),
        }
    }

    /// Subscribe to the live 16kHz mono sample stream.
    ///
    /// Returns a receiver that yields raw resampled sample runs (variable
    /// length, in capture order) while recording is active. Intended for
    /// streaming STT engines that decode during the recording instead of
    /// waiting for the final [`AudioSegment`]. Call BEFORE [`open`](Self::open);
    /// the existing VAD/segment path is unaffected.
    pub fn live_chunks(&self) -> mpsc::UnboundedReceiver<Vec<f32>> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.live_tx.lock().unwrap() = Some(tx);
        rx
    }

    /// Start the capture thread. The default input device is probed once so
    /// missing-device / permission errors surface synchronously, but the mic
    /// stream itself is NOT opened here.
    ///
    /// The stream opens on [`start_recording`](Self::start_recording) and
    /// closes after [`stop_recording`](Self::stop_recording)'s flush, so the
    /// OS mic-in-use indicator is lit only while rekody is actually
    /// listening. Each recording re-queries the default device, so switching
    /// microphones between dictations needs no restart.
    ///
    /// Returns a receiver that yields [`AudioSegment`]s whenever speech is
    /// detected.
    pub fn open(&self, config: AudioConfig) -> Result<mpsc::UnboundedReceiver<AudioSegment>> {
        let (segment_tx, segment_rx) = mpsc::unbounded_channel();

        let recording = Arc::clone(&self.recording);
        let shutdown = Arc::clone(&self.shutdown);
        let flush = Arc::clone(&self.flush);
        let latest_rms_bits = Arc::clone(&self.latest_rms_bits);
        let live_tx = self.live_tx.lock().unwrap().clone();

        // Use a oneshot channel so the audio thread can report init errors
        // back to the caller synchronously.
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<(), AudioError>>(1);

        // The cpal Stream type is !Send on macOS, so streams must be created
        // on the thread that keeps them alive — sessions run entirely here.
        std::thread::Builder::new()
            .name("rekody-audio-proc".into())
            .spawn(move || {
                // ----- startup probe (no stream kept open) -----
                // default_input_config() is where cpal-on-macOS surfaces
                // missing devices and TCC denial, so this preserves the
                // fail-at-startup behavior without holding the mic.
                {
                    let host = cpal::default_host();
                    let device = match host.default_input_device() {
                        Some(d) => d,
                        None => {
                            let _ = init_tx.send(Err(AudioError::NoInputDevice));
                            return;
                        }
                    };
                    if let Err(e) = device.default_input_config() {
                        let msg = e.to_string();
                        let err = if msg.to_lowercase().contains("permission") {
                            AudioError::PermissionDenied
                        } else {
                            AudioError::StreamError(msg)
                        };
                        let _ = init_tx.send(Err(err));
                        return;
                    }
                }
                let _ = init_tx.send(Ok(()));

                // ----- idle loop: wait for start_recording() -----
                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        tracing::info!("audio processing thread shutting down");
                        break;
                    }
                    if !recording.load(Ordering::Relaxed) {
                        // A stop with no live session (e.g. the open below
                        // failed mid-hold) leaves a stale flush; clear it so
                        // it can't truncate the NEXT utterance.
                        flush.store(false, Ordering::Relaxed);
                        std::thread::park_timeout(std::time::Duration::from_millis(10));
                        continue;
                    }

                    if let Err(e) = run_capture_session(
                        &config,
                        &recording,
                        &shutdown,
                        &flush,
                        &latest_rms_bits,
                        &live_tx,
                        &segment_tx,
                    ) {
                        tracing::error!(error = %e, "mic capture session failed");
                        // Hold off until the key is released so a dead device
                        // doesn't retry in a tight loop for the whole hold.
                        while recording.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed)
                        {
                            std::thread::park_timeout(std::time::Duration::from_millis(50));
                        }
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

    /// Begin capturing audio. The capture thread opens the mic stream
    /// (lighting the OS mic-in-use indicator) and emits speech segments
    /// through the channel returned by [`open`](Self::open).
    pub fn start_recording(&self) {
        tracing::info!("recording started");
        self.recording.store(true, Ordering::Relaxed);
    }

    /// Stop capturing audio. The capture thread flushes any buffered speech
    /// immediately, then closes the mic stream so the OS mic-in-use
    /// indicator turns off between dictations.
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

    /// Returns the most recent VAD frame's RMS energy. Updated continuously
    /// by the processing thread (~33x/sec at 16kHz with 30ms frames),
    /// regardless of whether recording is active. Useful for driving a
    /// live audio level meter in the UI.
    pub fn latest_rms(&self) -> f32 {
        f32::from_bits(self.latest_rms_bits.load(Ordering::Relaxed))
    }

    /// Returns a clone of the shared `Arc<AtomicU32>` holding the latest
    /// RMS bits. Lets callers hold their own reference (e.g. move into a
    /// UI polling task) without keeping an `AudioCapture` borrow alive.
    /// Decode with `f32::from_bits(handle.load(Ordering::Relaxed))`.
    pub fn rms_handle(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.latest_rms_bits)
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

/// Run one recording session: open the default input device, process audio
/// until the post-stop flush (or shutdown), then close the stream.
///
/// Must run on the capture thread — the cpal `Stream` is `!Send` on macOS
/// and lives only inside this call, which is what scopes the OS mic-in-use
/// indicator to the recording itself.
fn run_capture_session(
    config: &AudioConfig,
    recording: &Arc<AtomicBool>,
    shutdown: &Arc<AtomicBool>,
    flush: &Arc<AtomicBool>,
    latest_rms_bits: &Arc<AtomicU32>,
    live_tx: &Option<mpsc::UnboundedSender<Vec<f32>>>,
    segment_tx: &mpsc::UnboundedSender<AudioSegment>,
) -> Result<(), AudioError> {
    let open_started = std::time::Instant::now();

    // ----- device & stream setup -----
    let host = cpal::default_host();
    let device = resolve_input_device(&host, config.input_device.as_deref())
        .ok_or(AudioError::NoInputDevice)?;

    let supported_config = device.default_input_config().map_err(|e| {
        let msg = e.to_string();
        if msg.to_lowercase().contains("permission") {
            AudioError::PermissionDenied
        } else {
            AudioError::StreamError(msg)
        }
    })?;

    let sample_format = supported_config.sample_format();
    let input_config: StreamConfig = supported_config.into();
    let input_rate = input_config.sample_rate.0;
    let input_channels = input_config.channels as usize;

    // Channel to shuttle raw f32 samples from the cpal callback to this
    // processing thread.
    let (raw_tx, raw_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);

    let err_callback = |err: cpal::StreamError| {
        tracing::error!(%err, "audio stream error");
    };

    let stream_result = match sample_format {
        SampleFormat::F32 => {
            let rec = Arc::clone(recording);
            device.build_input_stream(
                &input_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if rec.load(Ordering::Relaxed) {
                        let _ = raw_tx.try_send(data.to_vec());
                    }
                },
                err_callback,
                None,
            )
        }
        SampleFormat::I16 => {
            let rec = Arc::clone(recording);
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
            let rec = Arc::clone(recording);
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
            return Err(AudioError::StreamError(format!(
                "unsupported sample format: {sample_format:?}"
            )));
        }
    };

    let stream = stream_result.map_err(|e| AudioError::StreamError(e.to_string()))?;
    stream
        .play()
        .map_err(|e| AudioError::StreamError(e.to_string()))?;

    tracing::info!(
        device = ?device.name().unwrap_or_default(),
        sample_rate = input_rate,
        channels = input_channels,
        format = ?sample_format,
        open_ms = open_started.elapsed().as_millis() as u64,
        "mic stream opened"
    );

    // ----- processing loop -----
    let vad_threshold = config.vad_threshold;
    let record_all_audio = config.record_all_audio;
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

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Check if we've been asked to flush buffered speech
        // (recording just stopped). Always emit *something* so the
        // UI never silently hangs on Recording — if VAD never
        // detected speech, surface that as a user-visible error
        // instead of dropping the recording with no signal.
        if flush.load(Ordering::Relaxed) {
            flush.store(false, Ordering::Relaxed);
            let duration_secs = speech_buf.len() as f32 / TARGET_SAMPLE_RATE as f32;
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
                tracing::warn!(
                    buffered_secs = duration_secs,
                    "no speech detected — speak louder or lower vad_threshold"
                );
                speech_buf.clear();
            }
            in_speech = false;
            consecutive_silence = 0;

            // Flush marks the end of a press-to-talk hold; once recording is
            // off, the session is over and the stream closes below.
            if !recording.load(Ordering::Relaxed) {
                break;
            }
        }

        let raw_samples = match raw_rx.recv_timeout(std::time::Duration::from_millis(100)) {
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

        // Resample (or pass through). New 16kHz mono samples are
        // also forwarded to the live tap (streaming STT) while
        // recording — the VAD/segment path below is unaffected.
        let live_recording = live_tx.is_some() && recording.load(Ordering::Relaxed);
        if let Some(ref mut rs) = resampler {
            let input_frames_needed = rs.input_frames_next();
            while mono_buf.len() >= input_frames_needed {
                let input_chunk: Vec<f32> = mono_buf.drain(..input_frames_needed).collect();
                let input_ref: Vec<&[f32]> = vec![&input_chunk];
                match rs.process(&input_ref, None) {
                    Ok(output) => {
                        if let Some(ch) = output.first() {
                            if live_recording && let Some(tx) = live_tx {
                                let _ = tx.send(ch.clone());
                            }
                            resampled_buf.extend_from_slice(ch);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(%e, "resampling error");
                    }
                }
            }
        } else {
            if live_recording
                && !mono_buf.is_empty()
                && let Some(tx) = live_tx
            {
                let _ = tx.send(mono_buf.clone());
            }
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
            let frame: Vec<f32> = resampled_buf.drain(..VAD_FRAME_SAMPLES).collect();

            // VAD-bypass mode: while recording is active, append
            // every frame unconditionally. Used for low-energy
            // input (speaker→mic playback) where VAD would drop
            // everything as silence. Outside of recording windows,
            // fall through to the normal VAD logic so idle
            // silence isn't accumulated forever.
            if record_all_audio && currently_recording {
                in_speech = true;
                consecutive_silence = 0;
                speech_buf.extend_from_slice(&frame);
                continue;
            }

            let rms = compute_rms(&frame);
            latest_rms_bits.store(rms.to_bits(), Ordering::Relaxed);
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

                    let duration_secs = speech_buf.len() as f32 / TARGET_SAMPLE_RATE as f32;

                    if duration_secs >= MIN_SPEECH_DURATION_SECS {
                        let segment = AudioSegment {
                            samples: std::mem::take(&mut speech_buf),
                            duration_secs,
                        };
                        tracing::debug!(duration = duration_secs, "emitting audio segment");
                        if segment_tx.send(segment).is_err() {
                            tracing::info!("segment receiver dropped, stopping capture");
                            return Ok(());
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
                tracing::warn!(
                    "max recording duration reached ({MAX_RECORDING_SECS}s), auto-flushing"
                );
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

    drop(stream);
    tracing::info!("mic stream closed");

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

    Ok(())
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
        assert!(config.input_device.is_none());
    }

    #[test]
    fn match_device_name_exact_and_substring() {
        let devices = vec![
            "MacBook Air Microphone".to_string(),
            "Tony's AirPods Pro".to_string(),
            "External USB Mic".to_string(),
        ];
        // Exact (case-insensitive) wins.
        assert_eq!(
            match_device_name(&devices, "macbook air microphone"),
            Some(0)
        );
        // Substring match.
        assert_eq!(match_device_name(&devices, "airpods"), Some(1));
        assert_eq!(match_device_name(&devices, "USB"), Some(2));
        // No match → None (caller falls back to system default).
        assert_eq!(match_device_name(&devices, "Studio Display"), None);
        // Empty / whitespace → None.
        assert_eq!(match_device_name(&devices, "   "), None);
    }

    #[test]
    fn match_device_name_prefers_exact_over_substring() {
        // "Mic" is a substring of #0 but an exact match of #1 — exact wins.
        let devices = vec!["Studio Mic Array".to_string(), "Mic".to_string()];
        assert_eq!(match_device_name(&devices, "Mic"), Some(1));
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
