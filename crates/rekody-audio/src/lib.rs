//! Audio capture, resampling, and voice activity detection for rekody.
//!
//! Captures audio from the system microphone via cpal, resamples to
//! 16kHz mono via rubato, and filters silence using energy-based VAD.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

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

/// Minimum speech duration in seconds for the VAD to close an utterance on
/// its own. Applies only while no key is held; see [`SpeechSegmenter`].
const MIN_SPEECH_DURATION_SECS: f32 = 0.15;

/// Minimum audio a push-to-talk release must have CAPTURED for there to be
/// anything worth transcribing. Under this the key was tapped, not held, so
/// no dictation exists to send.
///
/// This is a floor on how much audio arrived, never on how loud it was. The
/// old flush gate measured the buffer the VAD had let through, so a quiet
/// microphone produced an empty buffer, a discarded recording, and a "no
/// speech detected" error on audio local Whisper transcribes perfectly
/// (issue #145). Deciding whether captured audio contains words is the
/// engine's job, not an RMS threshold's.
const MIN_CAPTURED_DURATION_SECS: f32 = 0.15;

/// Warning logged when a push-to-talk hold was too short to capture audio.
/// The UI layers match [`NO_AUDIO_MARKER`] and show these strings verbatim,
/// so the wording lives here, once.
pub const NO_AUDIO_TOO_SHORT: &str =
    "no audio captured: hold the key while you speak, then release";

/// Warning logged when audio arrived but every sample was digital silence:
/// the input device is muted, or is not delivering signal at all.
pub const NO_AUDIO_SILENT_DEVICE: &str =
    "no audio captured: the microphone is muted or sending silence";

/// Substring shared by every "nothing was captured" warning, so the terminal
/// UI and the HUD pill can match one marker and print the specific reason.
pub const NO_AUDIO_MARKER: &str = "no audio captured";

/// Trailing silence duration (in seconds) before finalizing a speech segment.
const SILENCE_TAIL_SECS: f32 = 0.6;

/// Belt-and-braces wake for the idle capture thread.
///
/// The thread is woken directly by [`AudioCapture::start_recording`], so this
/// timeout is never what starts a dictation. It exists only so a wakeup lost
/// to a bug cannot strand the thread forever.
const IDLE_WAKE_INTERVAL: Duration = Duration::from_millis(500);

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

/// What a push-to-talk release produced. Every release resolves to exactly
/// one of these, so the pill can never sit on a working verb with no answer.
#[derive(Debug)]
enum FlushOutcome {
    /// Audio to transcribe. Whether it contains words is the engine's call.
    Segment(AudioSegment),
    /// The hold was shorter than [`MIN_CAPTURED_DURATION_SECS`]: a tap, not
    /// a dictation. There is no audio to send.
    TooShort { captured_secs: f32 },
    /// Audio arrived and every sample is exactly zero. A live microphone
    /// always carries some room tone, so this means the device is muted or
    /// is not delivering signal, and the recognizer would only hallucinate
    /// on it.
    SilentDevice { captured_secs: f32 },
}

/// The capture buffer and its voice-activity state machine.
///
/// Two jobs, and only one of them is the VAD's:
///
/// * **A key is held** (push-to-talk, or a hands-free latch): every frame is
///   kept. Holding the key, speaking, and releasing it is an explicit request
///   to transcribe that audio, so nothing decides on the user's behalf that it
///   was too quiet to mean anything. Mid-recording splits were already
///   disabled here, so the VAD had no segmenting job in this window; all it
///   did was drop the audio of anyone whose microphone ran quiet (#145).
/// * **The microphone is open with no key held**: the VAD earns its keep,
///   cutting the continuous stream into utterances on the trailing-silence
///   rule ([`SILENCE_TAIL_SECS`]) and dropping bursts under
///   [`MIN_SPEECH_DURATION_SECS`]. Unchanged.
///
/// Pure and free of cpal, so both behaviors are unit-testable without a
/// microphone.
struct SpeechSegmenter {
    vad_threshold: f32,
    /// Capture every frame even outside a recording window.
    record_all_audio: bool,
    /// Longest buffer to hold before force-emitting it, in seconds.
    max_secs: f32,
    buf: Vec<f32>,
    in_speech: bool,
    consecutive_silence: usize,
    silence_frames_limit: usize,
    last_rms: f32,
}

impl SpeechSegmenter {
    fn new(vad_threshold: f32, record_all_audio: bool) -> Self {
        Self {
            vad_threshold,
            record_all_audio,
            max_secs: MAX_RECORDING_SECS,
            buf: Vec::new(),
            in_speech: false,
            consecutive_silence: 0,
            silence_frames_limit: (SILENCE_TAIL_SECS * TARGET_SAMPLE_RATE as f32) as usize
                / VAD_FRAME_SAMPLES,
            last_rms: 0.0,
        }
    }

    /// RMS of the most recent frame, for the live level meter.
    fn last_rms(&self) -> f32 {
        self.last_rms
    }

    fn buffered_secs(&self) -> f32 {
        self.buf.len() as f32 / TARGET_SAMPLE_RATE as f32
    }

    /// Feed one 30ms frame. `recording` is the live push-to-talk flag.
    ///
    /// Returns a segment only when the VAD closes an utterance on its own
    /// (possible only with no key held) or when the buffer hits the runaway
    /// cap. A held key accumulates into one segment, flushed at release.
    fn push_frame(&mut self, frame: &[f32], recording: bool) -> Option<AudioSegment> {
        self.last_rms = compute_rms(frame);

        if recording || self.record_all_audio {
            self.in_speech = true;
            self.consecutive_silence = 0;
            self.buf.extend_from_slice(frame);
            return self.cap_runaway();
        }

        if self.last_rms > self.vad_threshold {
            self.consecutive_silence = 0;
            if !self.in_speech {
                self.in_speech = true;
                tracing::trace!(rms = self.last_rms, "speech start detected");
            }
            self.buf.extend_from_slice(frame);
        } else if self.in_speech {
            self.buf.extend_from_slice(frame);
            self.consecutive_silence += 1;

            if self.consecutive_silence >= self.silence_frames_limit {
                let trailing = self.silence_frames_limit * VAD_FRAME_SAMPLES;
                let trimmed_len = self.buf.len().saturating_sub(trailing);
                self.buf.truncate(trimmed_len);

                let segment = self.take_segment(MIN_SPEECH_DURATION_SECS);
                if let Some(ref s) = segment {
                    tracing::debug!(duration = s.duration_secs, "emitting audio segment");
                }
                self.in_speech = false;
                self.consecutive_silence = 0;
                return segment;
            }
        }

        self.cap_runaway()
    }

    /// End of a push-to-talk hold. Whatever was captured goes to the engine.
    fn flush(&mut self) -> FlushOutcome {
        self.in_speech = false;
        self.consecutive_silence = 0;

        let captured_secs = self.buffered_secs();
        if captured_secs < MIN_CAPTURED_DURATION_SECS {
            self.buf.clear();
            return FlushOutcome::TooShort { captured_secs };
        }
        // Bit-exact, not a level threshold: a muted device sends zeros, and
        // there is no volume at which a real microphone does.
        if !self.buf.iter().any(|s| *s != 0.0) {
            self.buf.clear();
            return FlushOutcome::SilentDevice { captured_secs };
        }

        match self.take_segment(0.0) {
            Some(segment) => FlushOutcome::Segment(segment),
            // Unreachable: the buffer cleared the duration floor above.
            None => FlushOutcome::TooShort { captured_secs },
        }
    }

    /// Final drain when the capture thread stops. Unlike a release, this is
    /// not a user gesture, so the VAD's own minimum still applies.
    fn finish(&mut self) -> Option<AudioSegment> {
        self.take_segment(MIN_SPEECH_DURATION_SECS)
    }

    /// Hand over the buffer when it holds at least `min_secs` of audio, and
    /// drop it otherwise. Always leaves the buffer empty.
    fn take_segment(&mut self, min_secs: f32) -> Option<AudioSegment> {
        let duration_secs = self.buffered_secs();
        if self.buf.is_empty() || duration_secs < min_secs {
            self.buf.clear();
            return None;
        }
        Some(AudioSegment {
            samples: std::mem::take(&mut self.buf),
            duration_secs,
        })
    }

    /// Force out a buffer that has grown past the cap so a forgotten
    /// hands-free session cannot eat memory forever. Keyed on the buffer
    /// itself: with a held key the buffer fills whether or not the VAD ever
    /// called anything speech.
    fn cap_runaway(&mut self) -> Option<AudioSegment> {
        if self.buffered_secs() < self.max_secs {
            return None;
        }
        tracing::warn!(
            max_secs = self.max_secs,
            "max recording duration reached, auto-flushing"
        );
        let segment = self.take_segment(0.0);
        self.in_speech = false;
        self.consecutive_silence = 0;
        segment
    }
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
    /// Preferred input devices in order, matched by name (case-insensitive).
    /// Empty (or every entry blank/`"system"`) follows the OS default input,
    /// which can drift to whatever was plugged in last (e.g. AirPods). One
    /// entry pins capture to that device; several entries form a preference
    /// chain where the first connected device wins. Resolved fresh at each
    /// recording start, so plugging or unplugging a device needs no restart
    /// and a fully disconnected chain transparently falls back.
    pub input_device: Vec<String>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            vad_threshold: 0.01,
            record_all_audio: false,
            input_device: Vec::new(),
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
/// matches. Pure helper, unit-tested; also used by `rekody doctor` so its
/// display agrees with what capture will actually resolve.
pub fn match_device_name(available: &[String], want: &str) -> Option<usize> {
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

/// Walk a device preference chain against the available device names and
/// return `(chain_index, device_index)` of the first entry that matches a
/// connected device (same matching as [`match_device_name`]). Blank entries
/// and the `"system"` sentinel are skipped. `None` when nothing in the chain
/// is connected; the caller falls back to the system default. Pure helper,
/// unit-tested.
pub fn resolve_device_chain(available: &[String], chain: &[String]) -> Option<(usize, usize)> {
    chain.iter().enumerate().find_map(|(chain_idx, want)| {
        let want = want.trim();
        if want.is_empty() || want.eq_ignore_ascii_case("system") {
            return None;
        }
        match_device_name(available, want).map(|device_idx| (chain_idx, device_idx))
    })
}

/// Resolve the input device to capture from: the first configured name that
/// matches an available device, otherwise the system default. A chain where
/// nothing is connected (devices unplugged, renamed) logs a warning naming
/// what was tried and falls back; capture never hard-fails on a stale
/// preference.
fn resolve_input_device(host: &cpal::Host, configured: &[String]) -> Option<cpal::Device> {
    let wanted: Vec<&str> = configured
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("system"))
        .collect();

    if !wanted.is_empty() {
        let devices: Vec<cpal::Device> = host
            .input_devices()
            .map(|it| it.collect())
            .unwrap_or_default();
        let names: Vec<String> = devices
            .iter()
            .map(|d| d.name().unwrap_or_default())
            .collect();
        if let Some((_, idx)) = resolve_device_chain(&names, configured) {
            tracing::info!(device = %names[idx], "using configured input device");
            return devices.into_iter().nth(idx);
        }
        if let [only] = wanted.as_slice() {
            // Today's single-pin message, unchanged for existing configs.
            tracing::warn!(
                configured = %only,
                "configured input_device not found — falling back to system default"
            );
        } else {
            tracing::warn!(
                tried = %wanted.join(", "),
                "no device in the input_device chain is connected; \
                 falling back to system default"
            );
        }
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
    /// Wakes the idle capture thread the instant `recording` or `shutdown`
    /// flips, instead of leaving it to notice on its next tick. The mutex
    /// guards nothing but the flag transition itself: writers take it, store,
    /// release, then notify, and the waiter re-checks the flags while holding
    /// it, so a start landing between the check and the wait cannot be missed.
    wake: Arc<(Mutex<()>, Condvar)>,
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
            wake: Arc::new((Mutex::new(()), Condvar::new())),
        }
    }

    /// Flip a capture-thread flag and wake the thread immediately.
    fn signal(&self, set: impl FnOnce()) {
        let (lock, cv) = &*self.wake;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        set();
        drop(guard);
        cv.notify_all();
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
        let wake = Arc::clone(&self.wake);

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
                        // Sleep until start_recording()/shutdown() wakes
                        // us, rather than waking 100 times a second to
                        // discover nothing has happened. A keypress used to
                        // wait an average of 5ms just for this thread to
                        // notice it. The flags are re-checked under the same
                        // lock the writers take, so a start landing in this
                        // window is seen rather than slept through.
                        let (lock, cv) = &*wake;
                        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        if !recording.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
                            let _ = cv.wait_timeout(guard, IDLE_WAKE_INTERVAL);
                        }
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
        self.signal(|| self.recording.store(true, Ordering::Relaxed));
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
        self.signal(|| self.shutdown.store(true, Ordering::Relaxed));
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
    let device =
        resolve_input_device(&host, &config.input_device).ok_or(AudioError::NoInputDevice)?;

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

    // Capture buffer + VAD state.
    let mut segmenter = SpeechSegmenter::new(config.vad_threshold, config.record_all_audio);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Recording just stopped. A release is an explicit request to
        // transcribe, so the captured audio goes to the engine and the
        // engine decides whether it holds words. The only outcomes that
        // send nothing are a hold too short to capture audio and a device
        // sending pure silence, and both say so out loud: the UI must never
        // be left waiting on a verb that no event will ever clear.
        if flush.load(Ordering::Relaxed) {
            flush.store(false, Ordering::Relaxed);
            match segmenter.flush() {
                FlushOutcome::Segment(segment) => {
                    tracing::info!(
                        duration = segment.duration_secs,
                        "flushing audio segment (recording stopped)"
                    );
                    let _ = segment_tx.send(segment);
                }
                FlushOutcome::TooShort { captured_secs } => {
                    tracing::warn!(captured_secs, "{NO_AUDIO_TOO_SHORT}");
                }
                FlushOutcome::SilentDevice { captured_secs } => {
                    tracing::warn!(captured_secs, "{NO_AUDIO_SILENT_DEVICE}");
                }
            }

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

        // Feed the segmenter in 30ms frames. While a key is held every
        // frame is kept and nothing is emitted until the release flush;
        // with the mic open and no key held, the VAD splits the stream
        // into utterances (see SpeechSegmenter).
        let currently_recording = recording.load(Ordering::Relaxed);

        while resampled_buf.len() >= VAD_FRAME_SAMPLES {
            let frame: Vec<f32> = resampled_buf.drain(..VAD_FRAME_SAMPLES).collect();
            let segment = segmenter.push_frame(&frame, currently_recording);
            latest_rms_bits.store(segmenter.last_rms().to_bits(), Ordering::Relaxed);

            if let Some(segment) = segment
                && segment_tx.send(segment).is_err()
            {
                tracing::info!("segment receiver dropped, stopping capture");
                return Ok(());
            }
        }
    }

    drop(stream);
    tracing::info!("mic stream closed");

    // Flush remaining speech on shutdown.
    if let Some(segment) = segmenter.finish() {
        let _ = segment_tx.send(segment);
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

/// The capture buffer's two behaviors, driven frame by frame with no
/// microphone: a held key keeps everything, an open mic with no key held
/// keeps the VAD's utterance splitting.
#[cfg(test)]
mod segmenter_tests {
    use super::*;

    /// One 30ms frame whose RMS is exactly `amp`.
    fn frame(amp: f32) -> Vec<f32> {
        (0..VAD_FRAME_SAMPLES)
            .map(|i| if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    const THRESHOLD: f32 = 0.01;
    /// A microphone running well under the gate: this is the recording the
    /// old flush threw away, and the one Whisper transcribes correctly.
    const QUIET: f32 = 0.003;
    const LOUD: f32 = 0.08;

    fn hold(seg: &mut SpeechSegmenter, amp: f32, frames: usize) {
        for _ in 0..frames {
            assert!(
                seg.push_frame(&frame(amp), true).is_none(),
                "a held key must never emit mid-recording segments"
            );
        }
    }

    /// Issue #145: hold, speak quietly, release. Every frame is under
    /// `vad_threshold`, which used to mean an empty buffer, a discarded
    /// recording, and "no speech detected" on perfectly transcribable audio.
    #[test]
    fn quiet_push_to_talk_recording_reaches_the_engine() {
        assert!(
            compute_rms(&frame(QUIET)) < THRESHOLD,
            "the fixture must sit under the VAD gate for this to be the bug"
        );
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, QUIET, 100); // 3 seconds

        match seg.flush() {
            FlushOutcome::Segment(segment) => {
                assert_eq!(segment.samples.len(), 100 * VAD_FRAME_SAMPLES);
                assert!((segment.duration_secs - 3.0).abs() < 1e-3);
            }
            other => panic!("quiet push-to-talk audio was discarded: {other:?}"),
        }
    }

    /// A release hands over the captured audio unmodified, leading silence
    /// included: the engine gets the recording, not the VAD's opinion of it.
    #[test]
    fn push_to_talk_keeps_every_captured_sample() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        let mut expected: Vec<f32> = Vec::new();
        for amp in [0.0, 0.0, QUIET, LOUD, LOUD, 0.0] {
            let f = frame(amp);
            expected.extend_from_slice(&f);
            assert!(seg.push_frame(&f, true).is_none());
        }

        match seg.flush() {
            FlushOutcome::Segment(segment) => assert_eq!(segment.samples, expected),
            other => panic!("captured audio was discarded: {other:?}"),
        }
    }

    /// Hands-free with the mic open and no key held: the VAD still cuts the
    /// stream into utterances on trailing silence, trims that silence, and
    /// resets for the next one.
    #[test]
    fn open_mic_still_splits_utterances_on_trailing_silence() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        let tail = seg.silence_frames_limit;

        // Utterance one: 20 frames of speech, then the silence tail.
        for _ in 0..20 {
            assert!(seg.push_frame(&frame(LOUD), false).is_none());
        }
        let mut first = None;
        for _ in 0..tail {
            if let Some(s) = seg.push_frame(&frame(0.0), false) {
                first = Some(s);
            }
        }
        let first = first.expect("an utterance followed by silence must close");
        assert_eq!(
            first.samples.len(),
            20 * VAD_FRAME_SAMPLES,
            "the trailing silence must be trimmed back off"
        );
        assert!(!seg.in_speech && seg.consecutive_silence == 0);

        // Utterance two proves the state machine reset.
        for _ in 0..20 {
            assert!(seg.push_frame(&frame(LOUD), false).is_none());
        }
        let mut second = None;
        for _ in 0..tail {
            if let Some(s) = seg.push_frame(&frame(0.0), false) {
                second = Some(s);
            }
        }
        assert_eq!(
            second.expect("second utterance closes too").samples.len(),
            20 * VAD_FRAME_SAMPLES
        );
    }

    /// Still true with no key held: a burst too short to be speech is
    /// dropped, and idle silence never accumulates.
    #[test]
    fn open_mic_drops_short_bursts_and_ignores_idle_silence() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);

        for _ in 0..50 {
            assert!(seg.push_frame(&frame(0.0), false).is_none());
        }
        assert!(seg.buf.is_empty(), "idle silence must not be buffered");

        // 4 frames = 120ms, under MIN_SPEECH_DURATION_SECS.
        for _ in 0..4 {
            assert!(seg.push_frame(&frame(LOUD), false).is_none());
        }
        for _ in 0..seg.silence_frames_limit {
            assert!(
                seg.push_frame(&frame(0.0), false).is_none(),
                "a 120ms burst is not an utterance"
            );
        }
        assert!(seg.buf.is_empty());
    }

    /// A tap that captured almost nothing has no dictation in it, and says so.
    #[test]
    fn a_tap_too_short_to_capture_audio_says_so() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, LOUD, 2); // 60ms

        match seg.flush() {
            FlushOutcome::TooShort { captured_secs } => {
                assert!((captured_secs - 0.06).abs() < 1e-3);
            }
            other => panic!("expected TooShort, got {other:?}"),
        }
        assert!(seg.buf.is_empty());
    }

    /// A muted device sends exact zeros. Whisper hallucinates words on that
    /// ("Thank you." on 3s of digital silence), so it is named, not sent.
    #[test]
    fn a_muted_device_is_reported_not_transcribed() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, 0.0, 100);

        match seg.flush() {
            FlushOutcome::SilentDevice { captured_secs } => {
                assert!((captured_secs - 3.0).abs() < 1e-3);
            }
            other => panic!("expected SilentDevice, got {other:?}"),
        }

        // One live sample is enough to make it a recording again.
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, 0.0, 100);
        let mut nudged = frame(0.0);
        nudged[0] = 1e-6;
        assert!(seg.push_frame(&nudged, true).is_none());
        assert!(matches!(seg.flush(), FlushOutcome::Segment(_)));
    }

    /// The runaway cap is keyed on the buffer, not on the VAD: a hands-free
    /// session left latched in a quiet room must still be bounded.
    #[test]
    fn the_runaway_cap_does_not_depend_on_the_vad_hearing_speech() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        seg.max_secs = 0.3; // 10 frames
        assert!(!seg.in_speech);

        let mut emitted = None;
        for _ in 0..10 {
            if let Some(s) = seg.push_frame(&frame(QUIET), true) {
                emitted = Some(s);
            }
        }
        let emitted = emitted.expect("the buffer must be capped even with no detected speech");
        assert_eq!(emitted.samples.len(), 10 * VAD_FRAME_SAMPLES);
        assert!(seg.buf.is_empty());
    }

    /// Every dictation starts clean, whatever the previous one did.
    #[test]
    fn flush_resets_state_for_the_next_dictation() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, LOUD, 20);
        assert!(matches!(seg.flush(), FlushOutcome::Segment(_)));
        assert!(seg.buf.is_empty());
        assert!(!seg.in_speech);
        assert_eq!(seg.consecutive_silence, 0);

        hold(&mut seg, QUIET, 20);
        match seg.flush() {
            FlushOutcome::Segment(s) => assert_eq!(s.samples.len(), 20 * VAD_FRAME_SAMPLES),
            other => panic!("expected a second segment, got {other:?}"),
        }
    }

    /// `record_all_audio` keeps its meaning outside a recording window; a
    /// held key now behaves the same way with the flag off.
    #[test]
    fn record_all_audio_keeps_frames_with_no_key_held() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, true);
        for _ in 0..10 {
            assert!(seg.push_frame(&frame(QUIET), false).is_none());
        }
        assert_eq!(seg.buf.len(), 10 * VAD_FRAME_SAMPLES);
    }

    /// The shutdown drain is not a user gesture, so the VAD's own minimum
    /// still applies there.
    #[test]
    fn shutdown_drain_keeps_the_vad_minimum() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, LOUD, 2);
        assert!(seg.finish().is_none());

        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        hold(&mut seg, LOUD, 20);
        assert!(seg.finish().is_some());
    }

    /// The level meter reads every frame, quiet ones included.
    #[test]
    fn last_rms_tracks_the_most_recent_frame() {
        let mut seg = SpeechSegmenter::new(THRESHOLD, false);
        seg.push_frame(&frame(QUIET), true);
        assert!((seg.last_rms() - QUIET).abs() < 1e-6);
        seg.push_frame(&frame(LOUD), true);
        assert!((seg.last_rms() - LOUD).abs() < 1e-6);
    }
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
        assert!(config.input_device.is_empty());
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

    fn fake_devices() -> Vec<String> {
        vec![
            "MacBook Air Microphone".to_string(),
            "Tony's AirPods Pro".to_string(),
            "External USB Mic".to_string(),
        ]
    }

    fn chain(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn chain_first_connected_wins() {
        // Both entries are connected: the first one captures.
        assert_eq!(
            resolve_device_chain(&fake_devices(), &chain(&["AirPods", "USB"])),
            Some((0, 1))
        );
    }

    #[test]
    fn chain_skips_absent_devices() {
        // The preferred desk mic is unplugged: the next connected entry wins.
        assert_eq!(
            resolve_device_chain(&fake_devices(), &chain(&["Shure MVX2U", "MacBook Air"])),
            Some((1, 0))
        );
    }

    #[test]
    fn chain_none_connected_falls_back() {
        // Nothing in the chain is connected: None, caller uses system default.
        assert_eq!(
            resolve_device_chain(&fake_devices(), &chain(&["Shure MVX2U", "Blue Yeti"])),
            None
        );
    }

    #[test]
    fn chain_empty_is_system_default() {
        assert_eq!(resolve_device_chain(&fake_devices(), &[]), None);
    }

    #[test]
    fn chain_skips_blank_and_system_entries() {
        assert_eq!(
            resolve_device_chain(&fake_devices(), &chain(&["  ", "system", "USB"])),
            Some((2, 2))
        );
    }

    #[test]
    fn chain_single_entry_behaves_like_pin() {
        assert_eq!(
            resolve_device_chain(&fake_devices(), &chain(&["airpods"])),
            Some((0, 1))
        );
        assert_eq!(
            resolve_device_chain(&fake_devices(), &chain(&["Studio Display"])),
            None
        );
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
