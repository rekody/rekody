//! Daemon side of the rekody-hud IPC protocol (v1).
//!
//! See `docs/design/hud-protocol.md` for the contract. The daemon LISTENS on
//! a unix domain socket; the `rekody-hud` helper connects as a client and
//! renders a floating pill from the NDJSON event stream.
//!
//! Everything in this module is best-effort: a slow, hung, or missing helper
//! must never block or break dictation. Writes are non-blocking (stalled
//! clients are dropped) and every failure path logs and continues.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Pill state names for `{"e":"state","s":…}` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HudState {
    Idle,
    Listening,
    Working,
}

/// One protocol event, daemon → helper. Serializes to a single NDJSON
/// object tagged by the `e` field, e.g. `{"e":"level","rms":0.073}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "e", rename_all = "lowercase")]
pub enum HudEvent {
    Hello {
        version: String,
        position: String,
    },
    State {
        s: HudState,
        #[serde(skip_serializing_if = "Option::is_none")]
        handsfree: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        verb: Option<String>,
    },
    Level {
        rms: f32,
    },
    Partial {
        tail: String,
    },
    Done {
        words: u64,
        ms: u64,
    },
    Error {
        msg: String,
        recoverable: bool,
    },
    Bye,
}

impl HudEvent {
    pub fn idle() -> Self {
        HudEvent::State {
            s: HudState::Idle,
            handsfree: None,
            verb: None,
        }
    }

    pub fn listening(handsfree: bool) -> Self {
        HudEvent::State {
            s: HudState::Listening,
            handsfree: Some(handsfree),
            verb: None,
        }
    }

    pub fn working(verb: &str) -> Self {
        HudEvent::State {
            s: HudState::Working,
            handsfree: None,
            verb: Some(verb.to_string()),
        }
    }
}

/// Trim `text` to its last `max` chars (char-safe). When trimmed, the result
/// starts with `…` and the total length stays ≤ `max` chars.
pub fn trim_tail(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("…{tail}")
}

/// Default socket path per the protocol doc, overridable via
/// `$REKODY_HUD_SOCK`.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("REKODY_HUD_SOCK")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rekody")
        .join("hud.sock")
}

/// Handle to a supervised `rekody-hud` helper child process.
pub struct HelperHandle {
    shutdown: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl HelperHandle {
    /// Stop supervising and kill the current child (best-effort).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Locate the helper binary: `$REKODY_HUD_BIN`, else a `rekody-hud` sibling
/// of the running executable. `None` when neither exists.
fn find_helper_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("REKODY_HUD_BIN")
        && !p.is_empty()
    {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
        warn!("REKODY_HUD_BIN set but does not exist: {}", p.display());
        return None;
    }
    let sibling = std::env::current_exe().ok()?.parent()?.join("rekody-hud");
    sibling.exists().then_some(sibling)
}

/// Spawn and supervise the `rekody-hud` helper. Returns `Ok(None)` when no
/// helper binary is found (the HUD is simply off). On child exit the
/// supervisor relaunches after a 5s backoff until shut down.
pub fn spawn_helper(socket_path: &Path) -> std::io::Result<Option<HelperHandle>> {
    let Some(bin) = find_helper_binary() else {
        info!("rekody-hud helper not found — HUD off");
        return Ok(None);
    };

    let handle = HelperHandle {
        shutdown: Arc::new(AtomicBool::new(false)),
        child: Arc::new(Mutex::new(None)),
    };
    let shutdown = Arc::clone(&handle.shutdown);
    let child_slot = Arc::clone(&handle.child);
    let sock = socket_path.to_path_buf();

    std::thread::Builder::new()
        .name("hud-helper-supervisor".into())
        .spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                match Command::new(&bin).arg("--socket").arg(&sock).spawn() {
                    Ok(child) => {
                        debug!("rekody-hud helper started (pid {})", child.id());
                        if let Ok(mut slot) = child_slot.lock() {
                            *slot = Some(child);
                        }
                        // Wait for exit, polling so shutdown stays responsive.
                        loop {
                            if shutdown.load(Ordering::SeqCst) {
                                return;
                            }
                            let exited = match child_slot.lock() {
                                Ok(mut slot) => match slot.as_mut() {
                                    Some(c) => c.try_wait().map(|s| s.is_some()).unwrap_or(true),
                                    None => true, // taken by shutdown()
                                },
                                Err(_) => true,
                            };
                            if exited {
                                if let Ok(mut slot) = child_slot.lock() {
                                    *slot = None;
                                }
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(200));
                        }
                        warn!("rekody-hud helper exited — relaunching in 5s");
                    }
                    Err(e) => {
                        warn!("failed to spawn rekody-hud helper: {e} — retrying in 5s");
                    }
                }
                // 5s backoff, polling the shutdown flag.
                for _ in 0..50 {
                    if shutdown.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })?;

    Ok(Some(handle))
}

/// Unix-socket NDJSON broadcast server for the HUD helper.
///
/// Owns an accept-loop thread; every connected client receives each event
/// sent through [`HudServer::send`]. Writes never block the caller: client
/// sockets are non-blocking and any client whose write fails (including
/// `WouldBlock`, i.e. a stalled helper) is dropped.
pub struct HudServer {
    clients: Arc<Mutex<Vec<UnixStream>>>,
    shutdown: Arc<AtomicBool>,
    socket_path: PathBuf,
    helper: Mutex<Option<HelperHandle>>,
}

impl HudServer {
    /// Bind the listener and start the accept loop. Creates parent
    /// directories and removes a stale socket file before binding. Each new
    /// client immediately receives the `hello` event.
    pub fn new(socket_path: &Path, position: &str) -> anyhow::Result<Self> {
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Stale socket from a previous run blocks bind; remove it.
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }
        let listener = UnixListener::bind(socket_path)?;
        // Live partial transcripts flow over this socket — restrict it to
        // the owning user (default would be world-connectable).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
        }
        listener.set_nonblocking(true)?;

        let clients: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let hello = HudEvent::Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            position: position.to_string(),
        };
        let accept_clients = Arc::clone(&clients);
        let accept_shutdown = Arc::clone(&shutdown);
        std::thread::Builder::new()
            .name("hud-accept".into())
            .spawn(move || {
                while !accept_shutdown.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(e) = Self::register_client(&accept_clients, stream, &hello) {
                                debug!("hud client rejected: {e}");
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(e) => {
                            warn!("hud accept failed: {e}");
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    }
                }
            })?;

        Ok(Self {
            clients,
            shutdown,
            socket_path: socket_path.to_path_buf(),
            helper: Mutex::new(None),
        })
    }

    fn register_client(
        clients: &Mutex<Vec<UnixStream>>,
        mut stream: UnixStream,
        hello: &HudEvent,
    ) -> std::io::Result<()> {
        stream.set_nonblocking(true)?;
        let mut line = serde_json::to_string(hello).map_err(std::io::Error::other)?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;
        if let Ok(mut guard) = clients.lock() {
            guard.push(stream);
        }
        debug!("hud client connected");
        Ok(())
    }

    /// Number of currently connected clients.
    pub fn client_count(&self) -> usize {
        self.clients.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Hand the supervised helper to the server so it is killed on Drop.
    pub fn attach_helper(&self, handle: HelperHandle) {
        if let Ok(mut guard) = self.helper.lock() {
            *guard = Some(handle);
        }
    }

    /// Broadcast one event to every connected client. Never blocks: client
    /// sockets are non-blocking, and a client whose write fails or stalls
    /// (`WouldBlock`) is dropped on the spot.
    pub fn send(&self, event: &HudEvent) {
        let mut line = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(e) => {
                warn!("hud event serialization failed: {e}");
                return;
            }
        };
        line.push('\n');
        let Ok(mut clients) = self.clients.lock() else {
            return;
        };
        clients.retain_mut(|stream| match stream.write_all(line.as_bytes()) {
            Ok(()) => true,
            Err(e) => {
                debug!("hud client dropped: {e}");
                false
            }
        });
    }
}

impl Drop for HudServer {
    fn drop(&mut self) {
        self.send(&HudEvent::Bye);
        self.shutdown.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.helper.lock()
            && let Some(handle) = guard.take()
        {
            handle.shutdown();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(event: &HudEvent) -> String {
        serde_json::to_string(event).unwrap()
    }

    #[test]
    fn hello_json_shape() {
        let e = HudEvent::Hello {
            version: "0.5.11".into(),
            position: "bottom".into(),
        };
        assert_eq!(
            json(&e),
            r#"{"e":"hello","version":"0.5.11","position":"bottom"}"#
        );
    }

    #[test]
    fn state_json_shapes() {
        assert_eq!(json(&HudEvent::idle()), r#"{"e":"state","s":"idle"}"#);
        assert_eq!(
            json(&HudEvent::listening(false)),
            r#"{"e":"state","s":"listening","handsfree":false}"#
        );
        assert_eq!(
            json(&HudEvent::working("inserting…")),
            r#"{"e":"state","s":"working","verb":"inserting…"}"#
        );
    }

    #[test]
    fn level_json_shape() {
        assert_eq!(
            json(&HudEvent::Level { rms: 0.073 }),
            r#"{"e":"level","rms":0.073}"#
        );
    }

    #[test]
    fn partial_json_shape() {
        let e = HudEvent::Partial {
            tail: "…and the final text still lands the instant".into(),
        };
        assert_eq!(
            json(&e),
            r#"{"e":"partial","tail":"…and the final text still lands the instant"}"#
        );
    }

    #[test]
    fn done_json_shape() {
        assert_eq!(
            json(&HudEvent::Done { words: 94, ms: 52 }),
            r#"{"e":"done","words":94,"ms":52}"#
        );
    }

    #[test]
    fn error_json_shape() {
        let e = HudEvent::Error {
            msg: "mic lost — take saved".into(),
            recoverable: true,
        };
        assert_eq!(
            json(&e),
            r#"{"e":"error","msg":"mic lost — take saved","recoverable":true}"#
        );
    }

    #[test]
    fn bye_json_shape() {
        assert_eq!(json(&HudEvent::Bye), r#"{"e":"bye"}"#);
    }

    #[test]
    fn trim_tail_short_text_untouched() {
        assert_eq!(trim_tail("hello world", 60), "hello world");
    }

    #[test]
    fn trim_tail_long_text_keeps_last_chars_with_ellipsis() {
        let text = "a".repeat(100);
        let trimmed = trim_tail(&text, 60);
        assert_eq!(trimmed.chars().count(), 60);
        assert!(trimmed.starts_with('…'));
        assert!(trimmed.ends_with('a'));
    }

    #[test]
    fn trim_tail_is_char_safe() {
        // Multibyte chars must not be split.
        let text = "é".repeat(80);
        let trimmed = trim_tail(&text, 60);
        assert_eq!(trimmed.chars().count(), 60);
        assert_eq!(trimmed, format!("…{}", "é".repeat(59)));
    }
}
