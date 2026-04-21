//! Global hotkey listener for rekody voice dictation.
//!
//! Uses macOS `CGEventTap` directly for global keyboard monitoring.
//! This avoids the `rdev` crash on non-main-dispatch-queue threads
//! and works correctly inside Tauri processes.
//!
//! ## Hotkey bindings
//!
//! | Action | Shortcut |
//! |--------|----------|
//! | Push-to-talk (hold to record, release to stop) | `Option+Space` or `Fn` |
//! | Toggle (press to start, press to stop) | same keys in toggle mode |
//! | Command mode (transform selected text) | `Option+Space + Enter` |

use anyhow::Result;
use std::sync::atomic::{AtomicPtr, Ordering};
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

/// Which physical key combination triggers dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriggerKey {
    /// ⌥Space — traditional default, works on all keyboards.
    /// Conflict risk: Raycast, Alfred frequently bind this combo.
    #[default]
    OptionSpace,
    /// Fn/Globe key alone — recommended for Apple built-in keyboards.
    /// Requires System Settings → Keyboard → "Press 🌐 key to" → "Do Nothing".
    FnKey,
}

#[derive(Debug, Clone)]
pub struct HotkeyConfig {
    pub activation_mode: ActivationMode,
    pub trigger_key: TriggerKey,
    /// Maximum continuous recording duration in seconds before a RecordStop is
    /// force-sent (deadman switch). `0` means no limit. Default: 300 (5 min).
    pub max_recording_secs: u64,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            activation_mode: ActivationMode::PushToTalk,
            trigger_key: TriggerKey::OptionSpace,
            max_recording_secs: 300,
        }
    }
}

/// Per-event mutable state shared between the tap callback and the watchdog.
#[derive(Debug, Default)]
struct KeyState {
    /// Whether the Option+Space trigger combo is currently held.
    trigger_held: bool,
    /// Whether a recording session is active.
    is_recording: bool,
    /// Whether the Fn key flag is currently down (FnKey trigger mode).
    fn_flag_down: bool,
    /// When the current recording session started (for the deadman switch).
    recording_start: Option<std::time::Instant>,
}

impl KeyState {
    /// Mark recording as started and capture the start time.
    fn start_recording(&mut self) {
        self.is_recording = true;
        self.recording_start = Some(std::time::Instant::now());
    }

    /// Mark recording as stopped and clear the start time.
    fn stop_recording(&mut self) {
        self.is_recording = false;
        self.recording_start = None;
    }

    /// Returns `true` (and clears state) if the deadman switch should fire.
    fn deadman_triggered(&mut self, max_secs: u64) -> bool {
        if !self.is_recording || max_secs == 0 {
            return false;
        }
        let elapsed = self
            .recording_start
            .map(|s| s.elapsed().as_secs())
            .unwrap_or(0);
        if elapsed >= max_secs {
            tracing::warn!(
                elapsed_secs = elapsed,
                max_secs,
                "deadman switch triggered — force-stopping recording"
            );
            self.trigger_held = false;
            self.fn_flag_down = false;
            self.stop_recording();
            return true;
        }
        false
    }
}

// ── macOS CGEventTap implementation ──────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::os::raw::c_void;

    // ── Virtual keycodes ──────────────────────────────────────────────────────

    const KC_SPACE: i64 = 49;
    const KC_RETURN: i64 = 36;
    /// Arrow key range — Fn flag is erroneously set on these on some hardware.
    const KC_ARROW_LEFT: i64 = 123;
    const KC_ARROW_DOWN: i64 = 125;

    // ── CGEventTap constants ──────────────────────────────────────────────────

    const K_CGEVENT_TAP_LOCATION_HID: u32 = 0;
    const K_CGEVENT_TAP_PLACEMENT_HEAD: u32 = 0;
    const K_CGEVENT_TAP_OPTION_DEFAULT: u32 = 0; // active tap — can suppress

    // ── CGEventType raw values ────────────────────────────────────────────────

    const K_CGEVENT_KEY_DOWN: u32 = 10;
    const K_CGEVENT_KEY_UP: u32 = 11;
    const K_CGEVENT_FLAGS_CHANGED: u32 = 12;
    const K_CGEVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFFFFFE;
    const K_CGEVENT_TAP_DISABLED_BY_USER: u32 = 0xFFFFFFFF;

    // ── CGEventField ──────────────────────────────────────────────────────────

    const K_CGEVENT_KEYBOARD_KEYCODE: u32 = 9;

    // ── CGEventFlags bitmasks ─────────────────────────────────────────────────

    /// Option/Alt modifier.
    const K_CGEVENT_FLAG_ALTERNATE: u64 = 0x00080000;
    /// Fn key on Apple built-in keyboards. NOTE: also spuriously set on arrow
    /// keys (keycodes 123–126) on some hardware — must filter those out.
    const K_CGEVENT_FLAG_SECONDARY_FN: u64 = 0x00800000;

    // ── Raw FFI ───────────────────────────────────────────────────────────────

    type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void;

    type CFRunLoopTimerCallBack = unsafe extern "C" fn(timer: *mut c_void, info: *mut c_void);

    /// CFRunLoopTimerContext — matches the C struct layout exactly.
    #[repr(C)]
    struct CFRunLoopTimerContext {
        version: i64,
        info: *mut c_void,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
    }

    unsafe extern "C" {
        // ── CGEventTap ────────────────────────────────────────────────────────
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> *mut c_void; // CFMachPortRef
        fn CGEventTapEnable(tap: *mut c_void, enable: bool);
        fn CGEventTapIsEnabled(tap: *mut c_void) -> bool;
        fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        fn CGEventGetFlags(event: *mut c_void) -> u64;

        // ── Accessibility permission ───────────────────────────────────────────
        fn AXIsProcessTrusted() -> bool;

        // ── CFMachPort → CFRunLoopSource ──────────────────────────────────────
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: *mut c_void,
            order: i64,
        ) -> *mut c_void; // CFRunLoopSourceRef

        // ── CFRunLoop ─────────────────────────────────────────────────────────
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopRun();
        fn CFRunLoopStop(rl: *mut c_void);

        // ── CFRunLoopTimer ────────────────────────────────────────────────────
        fn CFRunLoopTimerCreate(
            allocator: *const c_void,
            fire_date: f64, // CFAbsoluteTime
            interval: f64,  // CFTimeInterval
            flags: u32,     // CFOptionFlags
            order: i64,     // CFIndex
            callback: CFRunLoopTimerCallBack,
            context: *mut CFRunLoopTimerContext,
        ) -> *mut c_void; // CFRunLoopTimerRef
        fn CFRunLoopAddTimer(rl: *mut c_void, timer: *mut c_void, mode: *const c_void);
        fn CFAbsoluteTimeGetCurrent() -> f64;
    }

    // kCFRunLoopCommonModes constant from CoreFoundation.
    unsafe extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    #[inline]
    fn has_option(flags: u64) -> bool {
        flags & K_CGEVENT_FLAG_ALTERNATE != 0
    }

    #[inline]
    fn has_fn(flags: u64) -> bool {
        flags & K_CGEVENT_FLAG_SECONDARY_FN != 0
    }

    /// Returns `true` for arrow keycodes that spuriously carry the Fn flag on
    /// some Apple hardware — must be filtered out to avoid false Fn triggers.
    #[inline]
    fn is_arrow_key(keycode: i64) -> bool {
        (KC_ARROW_LEFT..=KC_ARROW_DOWN).contains(&keycode)
    }

    // ── TapContext ────────────────────────────────────────────────────────────

    /// Shared context passed into CGEventTap callbacks via raw pointer.
    /// Lives for the process lifetime (Box::leak).
    struct TapContext {
        tx: mpsc::UnboundedSender<HotkeyEvent>,
        state: Arc<Mutex<KeyState>>,
        mode: ActivationMode,
        trigger_key: TriggerKey,
        max_recording_secs: u64,
        /// Populated after CGEventTapCreate so callbacks can re-enable the tap.
        tap: AtomicPtr<c_void>,
    }

    // ── Send helper ───────────────────────────────────────────────────────────

    /// Send a `HotkeyEvent` to the pipeline receiver. If the receiver has been
    /// dropped (pipeline shut down), stop the CFRunLoop so this thread exits
    /// cleanly instead of silently looping on a dead channel.
    ///
    /// # Safety
    /// Must be called only from within the hotkey thread (holds the run loop).
    unsafe fn send_event(ctx: &TapContext, event: HotkeyEvent) {
        if ctx.tx.send(event).is_err() {
            tracing::error!("hotkey channel closed — stopping run loop");
            let rl = unsafe { CFRunLoopGetCurrent() };
            unsafe { CFRunLoopStop(rl) };
        }
    }

    // ── Tap health watchdog ───────────────────────────────────────────────────

    /// CFRunLoopTimer callback — fires every 5 seconds to check tap health.
    ///
    /// If the tap was silently disabled (e.g. heavy load caused a timeout that
    /// the disabled-event callback missed), this re-enables it before the user
    /// notices the hotkey stopped responding.
    unsafe extern "C" fn tap_health_timer_callback(_timer: *mut c_void, user_info: *mut c_void) {
        let ctx = unsafe { &*(user_info as *const TapContext) };
        let tap = ctx.tap.load(Ordering::Acquire);
        if tap.is_null() {
            return;
        }
        if !unsafe { CGEventTapIsEnabled(tap) } {
            tracing::warn!("tap health watchdog: tap is disabled — re-enabling");

            // Synthesize RecordStop if stuck mid-recording.
            let mut state = match ctx.state.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            if state.trigger_held || state.is_recording {
                tracing::warn!("tap was disabled mid-recording — synthesizing RecordStop");
                state.trigger_held = false;
                state.fn_flag_down = false;
                state.stop_recording();
                drop(state);
                unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
            }

            unsafe { CGEventTapEnable(tap, true) };
            tracing::info!("tap re-enabled by health watchdog");
        }
    }

    // ── Tap callback ─────────────────────────────────────────────────────────

    /// CGEventTap callback — called on every keyboard event system-wide.
    ///
    /// Returns `null` to suppress the event (prevents ⌥Space from inserting a
    /// non-breaking space), or `event` to pass it through unchanged.
    unsafe extern "C" fn tap_callback(
        _proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        user_info: *mut c_void,
    ) -> *mut c_void {
        // Safety: TapContext is leaked in start_listener and lives forever.
        let ctx = unsafe { &*(user_info as *const TapContext) };

        // ── Tap disabled recovery ─────────────────────────────────────────────
        //
        // macOS disables an active CGEventTap if the callback consistently
        // takes too long (timeout) or if the user revokes Accessibility. When
        // this fires mid-recording, the subsequent KeyUp/FlagsChanged events
        // never arrive — leaving the UI stuck in the red "recording" state.
        if event_type == K_CGEVENT_TAP_DISABLED_BY_TIMEOUT
            || event_type == K_CGEVENT_TAP_DISABLED_BY_USER
        {
            tracing::warn!(event_type, "CGEventTap disabled — recovering");

            let mut state = match ctx.state.lock() {
                Ok(s) => s,
                Err(p) => p.into_inner(),
            };
            if state.trigger_held || state.is_recording {
                tracing::warn!(
                    trigger_held = state.trigger_held,
                    is_recording = state.is_recording,
                    "tap disabled mid-recording — synthesizing RecordStop"
                );
                state.trigger_held = false;
                state.fn_flag_down = false;
                state.stop_recording();
                drop(state);
                unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
            }

            // Timeout = recoverable in-place. User-disabled = needs restart.
            if event_type == K_CGEVENT_TAP_DISABLED_BY_TIMEOUT {
                let tap = ctx.tap.load(Ordering::Acquire);
                if !tap.is_null() {
                    unsafe { CGEventTapEnable(tap, true) };
                    tracing::info!("tap re-enabled after timeout");
                }
            }
            return event;
        }

        // ── Acquire shared state ──────────────────────────────────────────────
        let keycode = unsafe { CGEventGetIntegerValueField(event, K_CGEVENT_KEYBOARD_KEYCODE) };
        let flags = unsafe { CGEventGetFlags(event) };

        let mut state = match ctx.state.lock() {
            Ok(s) => s,
            Err(p) => p.into_inner(),
        };

        // ── Deadman switch ────────────────────────────────────────────────────
        // Check on every event so the timer fires at most ~1 event late.
        if state.deadman_triggered(ctx.max_recording_secs) {
            drop(state);
            unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
            return event;
        }

        match event_type {
            // ── Key down ──────────────────────────────────────────────────────
            K_CGEVENT_KEY_DOWN => {
                match ctx.trigger_key {
                    TriggerKey::OptionSpace => {
                        let option_held = has_option(flags);

                        // ⌥Space → start dictation, suppress the key so the
                        // focused app doesn't receive a non-breaking space.
                        if keycode == KC_SPACE && option_held {
                            if state.trigger_held {
                                // Key-repeat: suppress, but don't re-fire RecordStart.
                                return std::ptr::null_mut();
                            }
                            state.trigger_held = true;

                            match ctx.mode {
                                ActivationMode::PushToTalk => {
                                    if !state.is_recording {
                                        state.start_recording();
                                        tracing::debug!("push-to-talk: RecordStart");
                                        drop(state);
                                        unsafe { send_event(ctx, HotkeyEvent::RecordStart) };
                                    }
                                }
                                ActivationMode::Toggle => {
                                    if state.is_recording {
                                        state.stop_recording();
                                        tracing::debug!("toggle: RecordStop");
                                        drop(state);
                                        unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
                                    } else {
                                        state.start_recording();
                                        tracing::debug!("toggle: RecordStart");
                                        drop(state);
                                        unsafe { send_event(ctx, HotkeyEvent::RecordStart) };
                                    }
                                }
                            }
                            return std::ptr::null_mut();
                        }

                        // ⌥Space held + Enter → command mode.
                        if state.trigger_held && keycode == KC_RETURN {
                            tracing::debug!("command mode (⌥Space+Enter)");
                            drop(state);
                            unsafe { send_event(ctx, HotkeyEvent::CommandMode) };
                            return std::ptr::null_mut();
                        }
                    }

                    TriggerKey::FnKey => {
                        // Fn key is pure FlagsChanged — no KeyDown events.
                        // Nothing to handle here for the Fn trigger.
                    }
                }
            }

            // ── Key up ────────────────────────────────────────────────────────
            // Space released — stop recording. Suppress the key-up so no stray
            // character reaches the focused app.
            K_CGEVENT_KEY_UP
                if ctx.trigger_key == TriggerKey::OptionSpace
                    && keycode == KC_SPACE
                    && (state.trigger_held || state.is_recording) =>
            {
                tracing::trace!(
                    trigger_held = state.trigger_held,
                    is_recording = state.is_recording,
                    "KeyUp Space"
                );
                state.trigger_held = false;

                if ctx.mode == ActivationMode::PushToTalk && state.is_recording {
                    state.stop_recording();
                    tracing::debug!("push-to-talk: RecordStop (Space released)");
                    drop(state);
                    unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
                }
                return std::ptr::null_mut();
            }

            // ── Flags changed ─────────────────────────────────────────────────
            K_CGEVENT_FLAGS_CHANGED => {
                let option_held = has_option(flags);
                let fn_held = has_fn(flags);

                match ctx.trigger_key {
                    TriggerKey::FnKey => {
                        // Filter arrow-key false positives: some hardware incorrectly
                        // sets maskSecondaryFn on arrow keys (123–126). Ignore them.
                        if is_arrow_key(keycode) {
                            return event;
                        }

                        // Fn pressed (0 → 1 transition)
                        if fn_held && !state.fn_flag_down {
                            state.fn_flag_down = true;
                            match ctx.mode {
                                ActivationMode::PushToTalk => {
                                    if !state.is_recording {
                                        state.start_recording();
                                        tracing::debug!("Fn push-to-talk: RecordStart");
                                        drop(state);
                                        unsafe { send_event(ctx, HotkeyEvent::RecordStart) };
                                    }
                                }
                                ActivationMode::Toggle => {
                                    if state.is_recording {
                                        state.stop_recording();
                                        tracing::debug!("Fn toggle: RecordStop");
                                        drop(state);
                                        unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
                                    } else {
                                        state.start_recording();
                                        tracing::debug!("Fn toggle: RecordStart");
                                        drop(state);
                                        unsafe { send_event(ctx, HotkeyEvent::RecordStart) };
                                    }
                                }
                            }
                            // Suppress Fn so it doesn't trigger "Change Input
                            // Source" / "Show Emoji" system actions.
                            return std::ptr::null_mut();
                        }

                        // Fn released (1 → 0 transition)
                        if !fn_held && state.fn_flag_down {
                            state.fn_flag_down = false;
                            if ctx.mode == ActivationMode::PushToTalk && state.is_recording {
                                state.stop_recording();
                                tracing::debug!("Fn push-to-talk: RecordStop (Fn released)");
                                drop(state);
                                unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
                            }
                            return std::ptr::null_mut();
                        }
                    }

                    TriggerKey::OptionSpace => {
                        // Option released while recording → stop immediately.
                        if !option_held && (state.trigger_held || state.is_recording) {
                            tracing::trace!(
                                trigger_held = state.trigger_held,
                                is_recording = state.is_recording,
                                "FlagsChanged: Option released"
                            );
                            state.trigger_held = false;

                            if ctx.mode == ActivationMode::PushToTalk && state.is_recording {
                                state.stop_recording();
                                tracing::debug!("push-to-talk: RecordStop (Option released)");
                                drop(state);
                                unsafe { send_event(ctx, HotkeyEvent::RecordStop) };
                            }
                        }
                    }
                }
            }

            _ => {}
        }

        event
    }

    // ── start_listener ────────────────────────────────────────────────────────

    /// Start the global hotkey listener using a CGEventTap.
    ///
    /// Spawns a dedicated thread that:
    /// 1. Creates an active CGEventTap at the HID level (pre-window-server).
    /// 2. Schedules a `CFRunLoopTimer` every 5 seconds to check tap health.
    /// 3. Runs `CFRunLoopRun()` — stays alive until the process exits or the
    ///    pipeline channel closes.
    pub fn start_listener(config: HotkeyConfig) -> Result<mpsc::UnboundedReceiver<HotkeyEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(KeyState::default()));
        let mode = config.activation_mode;
        let trigger_key = config.trigger_key;
        let max_recording_secs = config.max_recording_secs;

        // Leak the context — lives for the entire process lifetime.
        let ctx = Box::leak(Box::new(TapContext {
            tx,
            state,
            mode,
            trigger_key,
            max_recording_secs,
            tap: AtomicPtr::new(std::ptr::null_mut()),
        }));

        std::thread::Builder::new()
            .name("rekody-hotkey".into())
            .spawn(move || {
                tracing::info!(
                    mode = ?mode,
                    trigger = ?trigger_key,
                    max_recording_secs,
                    "hotkey listener started"
                );

                // Require Accessibility permission — without it CGEventTapCreate
                // returns null and the hotkey does nothing.
                if !unsafe { AXIsProcessTrusted() } {
                    tracing::error!(
                        "Accessibility permission not granted.\n\
                         Triggering macOS permission prompt…\n\
                         Steps:\n\
                         1. Click \"Open System Settings\" on the dialog\n\
                         2. Toggle rekody ON in the Accessibility list\n\
                         3. Restart rekody\n\
                         (Run 'rekody doctor' to re-check)"
                    );
                    let _ = crate::request_accessibility_permission();
                    return;
                }

                let events_of_interest: u64 = (1u64 << K_CGEVENT_KEY_DOWN)
                    | (1u64 << K_CGEVENT_KEY_UP)
                    | (1u64 << K_CGEVENT_FLAGS_CHANGED);

                // Create an active (suppressing) event tap at the HID level.
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
                        "CGEventTapCreate returned null — grant Accessibility permission \
                         in System Settings → Privacy & Security → Accessibility"
                    );
                    return;
                }

                // Publish the tap pointer so callbacks can reference it.
                ctx.tap.store(tap, Ordering::Release);

                let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0) };
                if source.is_null() {
                    tracing::error!("CFMachPortCreateRunLoopSource returned null");
                    return;
                }

                unsafe {
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
                    CGEventTapEnable(tap, true);
                }

                // Schedule a health watchdog timer — fires every 5 seconds to
                // detect silent tap failures (e.g. missed timeout events).
                let fire_date = unsafe { CFAbsoluteTimeGetCurrent() + 5.0 };
                let mut timer_ctx = CFRunLoopTimerContext {
                    version: 0,
                    info: ctx as *mut TapContext as *mut c_void,
                    retain: std::ptr::null(),
                    release: std::ptr::null(),
                    copy_description: std::ptr::null(),
                };
                let timer = unsafe {
                    CFRunLoopTimerCreate(
                        std::ptr::null(),
                        fire_date,
                        5.0, // repeat every 5 seconds
                        0,
                        0,
                        tap_health_timer_callback,
                        &mut timer_ctx,
                    )
                };
                if timer.is_null() {
                    tracing::warn!("failed to create tap health watchdog timer");
                } else {
                    unsafe {
                        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopCommonModes);
                    }
                    tracing::info!("tap health watchdog scheduled (5 s interval)");
                }

                if trigger_key == TriggerKey::FnKey {
                    tracing::info!(
                        "CGEventTap registered — hold Fn to dictate\n\
                         Ensure System Settings → Keyboard → \"Press 🌐 key to\" \
                         is set to \"Do Nothing\"."
                    );
                } else {
                    tracing::info!("CGEventTap registered — hold ⌥Space to dictate");
                }

                unsafe { CFRunLoopRun() };

                tracing::warn!("hotkey run loop exited");
            })?;

        Ok(rx)
    }
}

// ── Non-macOS stub ────────────────────────────────────────────────────────────

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn start_listener(_config: HotkeyConfig) -> Result<mpsc::UnboundedReceiver<HotkeyEvent>> {
        anyhow::bail!("global hotkeys are only supported on macOS currently")
    }
}

pub use platform::start_listener;

// ── Accessibility permission helpers ─────────────────────────────────────────

/// Silent check — returns `true` if the process has Accessibility permission.
/// Does not show any system dialog.
#[cfg(target_os = "macos")]
pub fn is_accessibility_trusted() -> bool {
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn is_accessibility_trusted() -> bool {
    true
}

/// Check Accessibility permission AND, if not granted, trigger the macOS
/// system dialog that prompts the user to grant it.
///
/// Returns `true` if already trusted. Returns `false` if the prompt was shown
/// (user must grant permission and restart rekody).
#[cfg(target_os = "macos")]
pub fn request_accessibility_permission() -> bool {
    use std::os::raw::{c_char, c_void};

    unsafe extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: i64,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    unsafe {
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"AXTrustedCheckOptionPrompt".as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if key.is_null() {
            return is_accessibility_trusted();
        }

        let keys = [key];
        let values = [kCFBooleanTrue];

        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );

        let trusted = if options.is_null() {
            AXIsProcessTrustedWithOptions(std::ptr::null())
        } else {
            let t = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            t
        };

        CFRelease(key);
        trusted
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility_permission() -> bool {
    true
}
