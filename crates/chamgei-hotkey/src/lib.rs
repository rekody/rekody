//! Global hotkey listener for Chamgei voice dictation.
//!
//! Uses macOS `CGEventTap` directly for global keyboard monitoring.
//! This avoids the `rdev` crash on non-main-dispatch-queue threads
//! and works correctly inside Tauri processes.
//!
//! ## Hotkey bindings
//!
//! | Action | Shortcut |
//! |--------|----------|
//! | Push-to-talk (hold to record, release to stop) | `Option+Space` |
//! | Toggle (press to start, press to stop) | `Option+Space` (in toggle mode) |
//! | Command mode (transform selected text) | `Option+Space + Enter` |

use anyhow::Result;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum HotkeyError {
    #[error("failed to register hotkey: {0}")]
    Registration(String),
    #[error("hotkey listener error: {0}")]
    Listener(String),
}

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    RecordStart,
    RecordStop,
    CommandMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    PushToTalk,
    Toggle,
}

#[derive(Debug, Clone)]
pub struct HotkeyConfig {
    pub activation_mode: ActivationMode,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            activation_mode: ActivationMode::PushToTalk,
        }
    }
}

#[derive(Debug, Default)]
struct KeyState {
    /// Whether the trigger modifier+key combo is currently held.
    trigger_held: bool,
    is_recording: bool,
}

// macOS CGEventTap implementation
#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use core_foundation::base::TCFType;
    use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
    use std::os::raw::c_void;

    /// macOS virtual keycodes.
    const KC_SPACE: i64 = 49;
    const KC_RETURN: i64 = 36;

    // CGEventTap constants
    const K_CGEVENT_TAP_LOCATION_HID: u32 = 0;
    const K_CGEVENT_TAP_PLACEMENT_HEAD: u32 = 0;
    const K_CGEVENT_TAP_OPTION_DEFAULT: u32 = 0; // active tap — can suppress events

    // CGEventType raw values
    const K_CGEVENT_KEY_DOWN: u32 = 10;
    const K_CGEVENT_KEY_UP: u32 = 11;
    const K_CGEVENT_FLAGS_CHANGED: u32 = 12;
    const K_CGEVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
    const K_CGEVENT_TAP_DISABLED_BY_USER: u32 = 0xFFFFFFFF;

    // CGEventField for keycode
    const K_CGEVENT_KEYBOARD_KEYCODE: u32 = 9;

    // CGEventFlags bitmask for Option/Alt
    const K_CGEVENT_FLAG_ALTERNATE: u64 = 0x00080000;

    // Raw FFI for CGEventTap functions not exposed by the core-graphics crate.
    type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void;

    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> *mut c_void; // CFMachPortRef

        fn CGEventTapEnable(tap: *mut c_void, enable: bool);

        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: *mut c_void,
            order: i64,
        ) -> *mut c_void; // CFRunLoopSourceRef

        fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        fn CGEventGetFlags(event: *mut c_void) -> u64;
    }

    /// Check if Option (Alt) flag is set.
    fn has_option(flags: u64) -> bool {
        flags & K_CGEVENT_FLAG_ALTERNATE != 0
    }

    /// Shared context passed into the CGEventTap callback via a raw pointer.
    struct TapContext {
        tx: mpsc::UnboundedSender<HotkeyEvent>,
        state: Arc<Mutex<KeyState>>,
        mode: ActivationMode,
    }

    /// The CGEventTap callback. Called on every keyboard event system-wide.
    ///
    /// Returns `null` to suppress the event (prevents Option+Space from
    /// inserting a non-breaking space into the focused app), or `event` to
    /// pass it through unchanged.
    unsafe extern "C" fn tap_callback(
        _proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void {
        // Safety: we control the lifetime of TapContext in start_listener.
        let ctx = unsafe { &*(user_info as *const TapContext) };

        // Re-enable the tap if macOS disabled it (happens under heavy load).
        if event_type == K_CGEVENT_TAP_DISABLED_BY_TIMEOUT
            || event_type == K_CGEVENT_TAP_DISABLED_BY_USER
        {
            tracing::warn!("CGEventTap was disabled, re-enabling");
            return event;
        }

        let keycode = unsafe { CGEventGetIntegerValueField(event, K_CGEVENT_KEYBOARD_KEYCODE) };
        let flags = unsafe { CGEventGetFlags(event) };
        let option_held = has_option(flags);

        let mut state = match ctx.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };

        match event_type {
            K_CGEVENT_KEY_DOWN => {
                // Option+Space triggers dictation — suppress the key so the
                // focused app never receives it (no non-breaking space inserted).
                if keycode == KC_SPACE && option_held {
                    if state.trigger_held {
                        // Key-repeat: suppress but don't re-fire RecordStart.
                        return std::ptr::null_mut();
                    }
                    state.trigger_held = true;

                    match ctx.mode {
                        ActivationMode::PushToTalk => {
                            if !state.is_recording {
                                state.is_recording = true;
                                tracing::debug!("push-to-talk: RecordStart");
                                let _ = ctx.tx.send(HotkeyEvent::RecordStart);
                            }
                        }
                        ActivationMode::Toggle => {
                            if state.is_recording {
                                state.is_recording = false;
                                tracing::debug!("toggle: RecordStop");
                                let _ = ctx.tx.send(HotkeyEvent::RecordStop);
                            } else {
                                state.is_recording = true;
                                tracing::debug!("toggle: RecordStart");
                                let _ = ctx.tx.send(HotkeyEvent::RecordStart);
                            }
                        }
                    }
                    // Return null to swallow the event.
                    return std::ptr::null_mut();
                }

                // Trigger held + Enter = command mode (also suppress).
                if state.trigger_held && keycode == KC_RETURN {
                    tracing::debug!("command mode (Option+Space+Enter)");
                    let _ = ctx.tx.send(HotkeyEvent::CommandMode);
                    return std::ptr::null_mut();
                }
            }

            K_CGEVENT_KEY_UP => {
                // Space released — stop recording. Suppress the key-up too so
                // no stray characters reach the focused app.
                if keycode == KC_SPACE && (state.trigger_held || state.is_recording) {
                    tracing::trace!(
                        trigger_held = state.trigger_held,
                        is_recording = state.is_recording,
                        "KeyUp Space"
                    );
                    state.trigger_held = false;

                    if ctx.mode == ActivationMode::PushToTalk && state.is_recording {
                        state.is_recording = false;
                        tracing::debug!("push-to-talk: RecordStop (Space released)");
                        let _ = ctx.tx.send(HotkeyEvent::RecordStop);
                    }
                    return std::ptr::null_mut();
                }
            }

            K_CGEVENT_FLAGS_CHANGED => {
                // Option released while recording — stop immediately.
                if !option_held && (state.trigger_held || state.is_recording) {
                    tracing::trace!(
                        trigger_held = state.trigger_held,
                        is_recording = state.is_recording,
                        "FlagsChanged: Option released"
                    );
                    state.trigger_held = false;

                    if ctx.mode == ActivationMode::PushToTalk && state.is_recording {
                        state.is_recording = false;
                        tracing::debug!("push-to-talk: RecordStop (Option released)");
                        let _ = ctx.tx.send(HotkeyEvent::RecordStop);
                    }
                }
            }

            _ => {}
        }

        event
    }

    /// Start the global hotkey listener using a CGEventTap.
    ///
    /// This spawns a dedicated thread that creates the event tap and runs a
    /// `CFRunLoop`. Unlike `rdev`, the tap is passive (listen-only) and does
    /// not require the main dispatch queue — it works from any thread.
    pub fn start_listener(config: HotkeyConfig) -> Result<mpsc::UnboundedReceiver<HotkeyEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(KeyState::default()));
        let mode = config.activation_mode;

        // Leak the context so it lives for the entire process. The hotkey
        // listener thread runs until the app exits, so this is intentional.
        let ctx = Box::leak(Box::new(TapContext { tx, state, mode }));

        std::thread::Builder::new()
            .name("chamgei-hotkey".into())
            .spawn(move || {
                tracing::info!("hotkey listener started (mode: {:?})", mode);

                let events_of_interest: u64 = (1u64 << K_CGEVENT_KEY_DOWN)
                    | (1u64 << K_CGEVENT_KEY_UP)
                    | (1u64 << K_CGEVENT_FLAGS_CHANGED);

                // Create an active event tap so we can suppress Option+Space
                // before it reaches the focused application.
                // Requires Accessibility permission (System Settings →
                // Privacy & Security → Accessibility).
                let tap = unsafe {
                    CGEventTapCreate(
                        K_CGEVENT_TAP_LOCATION_HID,
                        K_CGEVENT_TAP_PLACEMENT_HEAD,
                        K_CGEVENT_TAP_OPTION_DEFAULT,
                        events_of_interest,
                        tap_callback,
                        ctx as *mut TapContext as *mut c_void,
                    )
                };

                if tap.is_null() {
                    tracing::error!(
                        "failed to create CGEventTap — grant Input Monitoring permission \
                         in System Settings → Privacy & Security → Input Monitoring"
                    );
                    return;
                }

                // Create a CFRunLoopSource from the CGEventTap (which is a CFMachPort).
                let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0) };

                if source.is_null() {
                    tracing::error!("failed to create CFRunLoopSource from CGEventTap");
                    return;
                }

                // Wrap as a CFRunLoopSource and add to this thread's run loop.
                let source = unsafe {
                    core_foundation::runloop::CFRunLoopSource::wrap_under_create_rule(
                        source as *mut _,
                    )
                };

                let run_loop = CFRunLoop::get_current();
                unsafe {
                    run_loop.add_source(&source, kCFRunLoopCommonModes);
                }

                // Enable the tap.
                unsafe {
                    CGEventTapEnable(tap, true);
                }

                tracing::info!(
                    "CGEventTap registered, entering run loop (Option+Space to dictate)"
                );
                CFRunLoop::run_current();

                tracing::warn!("hotkey run loop exited unexpectedly");
            })?;

        Ok(rx)
    }
}

// Stub for non-macOS platforms (compile gate).
#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn start_listener(_config: HotkeyConfig) -> Result<mpsc::UnboundedReceiver<HotkeyEvent>> {
        anyhow::bail!("global hotkeys are only supported on macOS currently")
    }
}

pub use platform::start_listener;
