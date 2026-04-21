//! Pipeline status management for UI feedback.
//!
//! Provides a lock-free [`StatusManager`] that the pipeline updates as it
//! transitions through stages. The Tauri frontend can poll [`StatusManager::get_status`]
//! or register a callback via [`StatusManager::on_status_change`] to receive
//! real-time updates.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

// ── PipelineStatus ──────────────────────────────────────────────────────────

/// Describes which stage the voice-dictation pipeline is currently in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum PipelineStatus {
    /// Not recording — waiting for a hotkey event.
    Idle,
    /// Actively capturing audio from the microphone.
    Recording,
    /// The STT / LLM pipeline is processing captured audio.
    Processing,
    /// Processed text is being injected into the active application.
    Injecting,
    /// An error occurred during one of the pipeline stages.
    Error(String),
}

impl fmt::Display for PipelineStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Recording => write!(f, "Recording"),
            Self::Processing => write!(f, "Processing"),
            Self::Injecting => write!(f, "Injecting"),
            Self::Error(msg) => write!(f, "Error: {msg}"),
        }
    }
}

// Discriminant values stored in the AtomicU8.
const STATUS_IDLE: u8 = 0;
const STATUS_RECORDING: u8 = 1;
const STATUS_PROCESSING: u8 = 2;
const STATUS_INJECTING: u8 = 3;
const STATUS_ERROR: u8 = 4;

impl PipelineStatus {
    /// Map the enum to a compact `u8` discriminant for lock-free storage.
    fn to_discriminant(&self) -> u8 {
        match self {
            Self::Idle => STATUS_IDLE,
            Self::Recording => STATUS_RECORDING,
            Self::Processing => STATUS_PROCESSING,
            Self::Injecting => STATUS_INJECTING,
            Self::Error(_) => STATUS_ERROR,
        }
    }

    /// Reconstruct a [`PipelineStatus`] from its discriminant.
    ///
    /// Error messages are not recoverable from the discriminant alone; the
    /// caller must supply the message separately.
    fn from_discriminant(d: u8, error_msg: Option<String>) -> Self {
        match d {
            STATUS_RECORDING => Self::Recording,
            STATUS_PROCESSING => Self::Processing,
            STATUS_INJECTING => Self::Injecting,
            STATUS_ERROR => Self::Error(error_msg.unwrap_or_default()),
            _ => Self::Idle,
        }
    }
}

// ── StatusManager ───────────────────────────────────────────────────────────

/// Callback type invoked whenever the pipeline status changes.
pub type StatusCallback = Box<dyn Fn(&PipelineStatus) + Send + Sync>;

/// Thread-safe, lock-free (for reads) pipeline status manager.
///
/// The discriminant is stored in an [`AtomicU8`] so that hot-path reads from
/// the UI thread never block. The optional error message is kept behind a
/// [`Mutex`] and only accessed when the status is [`PipelineStatus::Error`].
pub struct StatusManager {
    discriminant: Arc<AtomicU8>,
    error_message: Arc<Mutex<Option<String>>>,
    callbacks: Arc<Mutex<Vec<StatusCallback>>>,
}

impl StatusManager {
    /// Create a new [`StatusManager`] initialised to [`PipelineStatus::Idle`].
    pub fn new() -> Self {
        Self {
            discriminant: Arc::new(AtomicU8::new(STATUS_IDLE)),
            error_message: Arc::new(Mutex::new(None)),
            callbacks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Update the current pipeline status.
    ///
    /// If the status actually changed, all registered callbacks are invoked
    /// synchronously on the calling thread.
    pub fn set_status(&self, status: PipelineStatus) {
        let new_disc = status.to_discriminant();

        // Store the error message (if any) *before* updating the discriminant
        // so that a concurrent `get_status` never sees `Error` with a stale
        // message.
        if let PipelineStatus::Error(ref msg) = status
            && let Ok(mut guard) = self.error_message.lock()
        {
            *guard = Some(msg.clone());
        }

        let old_disc = self.discriminant.swap(new_disc, Ordering::SeqCst);

        if old_disc != new_disc {
            tracing::debug!(status = %status, "pipeline status changed");
            self.fire_callbacks(&status);
        }
    }

    /// Read the current pipeline status without blocking (lock-free for
    /// non-error states).
    pub fn get_status(&self) -> PipelineStatus {
        let disc = self.discriminant.load(Ordering::SeqCst);
        let error_msg = if disc == STATUS_ERROR {
            self.error_message
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
        } else {
            None
        };
        PipelineStatus::from_discriminant(disc, error_msg)
    }

    /// Register a callback that is invoked whenever the status changes.
    ///
    /// Multiple callbacks may be registered; they are called in registration
    /// order.
    pub fn on_status_change(&self, callback: StatusCallback) {
        if let Ok(mut cbs) = self.callbacks.lock() {
            cbs.push(callback);
        }
    }

    /// Invoke all registered callbacks with the given status.
    fn fire_callbacks(&self, status: &PipelineStatus) {
        if let Ok(cbs) = self.callbacks.lock() {
            for cb in cbs.iter() {
                cb(status);
            }
        }
    }
}

impl Default for StatusManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for StatusManager {
    fn clone(&self) -> Self {
        Self {
            discriminant: Arc::clone(&self.discriminant),
            error_message: Arc::clone(&self.error_message),
            callbacks: Arc::clone(&self.callbacks),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn default_status_is_idle() {
        let mgr = StatusManager::new();
        assert_eq!(mgr.get_status(), PipelineStatus::Idle);
    }

    #[test]
    fn set_and_get_status() {
        let mgr = StatusManager::new();
        mgr.set_status(PipelineStatus::Recording);
        assert_eq!(mgr.get_status(), PipelineStatus::Recording);

        mgr.set_status(PipelineStatus::Processing);
        assert_eq!(mgr.get_status(), PipelineStatus::Processing);

        mgr.set_status(PipelineStatus::Injecting);
        assert_eq!(mgr.get_status(), PipelineStatus::Injecting);

        mgr.set_status(PipelineStatus::Error("mic failed".into()));
        assert_eq!(mgr.get_status(), PipelineStatus::Error("mic failed".into()));

        mgr.set_status(PipelineStatus::Idle);
        assert_eq!(mgr.get_status(), PipelineStatus::Idle);
    }

    #[test]
    fn callback_fires_on_change() {
        let mgr = StatusManager::new();
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);

        mgr.on_status_change(Box::new(move |status| {
            assert_eq!(*status, PipelineStatus::Recording);
            called_clone.store(true, Ordering::SeqCst);
        }));

        mgr.set_status(PipelineStatus::Recording);
        assert!(called.load(Ordering::SeqCst));
    }

    #[test]
    fn callback_does_not_fire_on_same_status() {
        let mgr = StatusManager::new();
        mgr.set_status(PipelineStatus::Recording);

        let call_count = Arc::new(AtomicU8::new(0));
        let count_clone = Arc::clone(&call_count);

        mgr.on_status_change(Box::new(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        // Setting the same status again should not fire the callback.
        mgr.set_status(PipelineStatus::Recording);
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn display_impl() {
        assert_eq!(PipelineStatus::Idle.to_string(), "Idle");
        assert_eq!(PipelineStatus::Recording.to_string(), "Recording");
        assert_eq!(PipelineStatus::Processing.to_string(), "Processing");
        assert_eq!(PipelineStatus::Injecting.to_string(), "Injecting");
        assert_eq!(
            PipelineStatus::Error("boom".into()).to_string(),
            "Error: boom"
        );
    }

    #[test]
    fn serialize_to_json() {
        let json = serde_json::to_string(&PipelineStatus::Idle).unwrap();
        assert_eq!(json, r#"{"kind":"Idle"}"#);

        let json = serde_json::to_string(&PipelineStatus::Error("oops".into())).unwrap();
        assert_eq!(json, r#"{"kind":"Error","message":"oops"}"#);
    }

    #[test]
    fn clone_shares_state() {
        let mgr = StatusManager::new();
        let mgr2 = mgr.clone();

        mgr.set_status(PipelineStatus::Processing);
        assert_eq!(mgr2.get_status(), PipelineStatus::Processing);
    }
}
