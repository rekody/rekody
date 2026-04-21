//! Cross-platform text injection for rekody.
//!
//! Injects formatted text at the cursor position using platform-native
//! APIs with a clipboard-paste fallback.

use anyhow::Result;
use thiserror::Error;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("clipboard error: {0}")]
    Clipboard(String),
    #[error("injection failed: {0}")]
    InjectionFailed(String),
    #[error("accessibility permission required")]
    PermissionRequired,
}

/// Method used for text injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionMethod {
    /// Native keyboard simulation (CGEvent/SendInput/xdotool).
    Native,
    /// Clipboard paste fallback (arboard + Cmd/Ctrl+V).
    Clipboard,
}

/// Injects text at the current cursor position.
///
/// When `method` is `Native`, attempts platform-native injection first and
/// falls back to clipboard-based injection on failure.
pub fn inject_text(text: &str, method: InjectionMethod) -> Result<()> {
    match method {
        InjectionMethod::Native => {
            debug!("attempting native text injection");
            match inject_native(text) {
                Ok(()) => Ok(()),
                Err(e) => {
                    warn!("native injection failed ({e}), falling back to clipboard");
                    inject_clipboard(text)
                }
            }
        }
        InjectionMethod::Clipboard => inject_clipboard(text),
    }
}

// ---------------------------------------------------------------------------
// Clipboard-based injection (cross-platform)
// ---------------------------------------------------------------------------

/// Clipboard-based text injection (Phase 1 default).
fn inject_clipboard(text: &str) -> Result<()> {
    use arboard::Clipboard;

    debug!("injecting text via clipboard paste");

    let mut clipboard = Clipboard::new().map_err(|e| InjectError::Clipboard(e.to_string()))?;

    // Save current clipboard contents
    let previous = clipboard.get_text().ok();

    // Set our text
    clipboard
        .set_text(text)
        .map_err(|e| InjectError::Clipboard(e.to_string()))?;

    // Brief delay after setting clipboard before simulating paste to avoid
    // race conditions where the paste fires before the clipboard is ready.
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Simulate paste keystroke. Restore clipboard regardless of success/failure.
    let paste_result = simulate_paste();

    // Restore previous clipboard contents after a short delay so the paste
    // has time to be processed by the focused application.
    std::thread::sleep(std::time::Duration::from_millis(100));
    if let Some(prev) = previous {
        let _ = clipboard.set_text(prev);
    }

    paste_result
}

// ---------------------------------------------------------------------------
// Paste keystroke simulation (per-platform)
// ---------------------------------------------------------------------------

/// Simulate Cmd+V (macOS) or Ctrl+V (Windows/Linux).
#[cfg(target_os = "macos")]
fn simulate_paste() -> Result<()> {
    macos::simulate_cmd_v()
}

#[cfg(target_os = "windows")]
fn simulate_paste() -> Result<()> {
    windows_impl::simulate_ctrl_v()
}

#[cfg(target_os = "linux")]
fn simulate_paste() -> Result<()> {
    linux::simulate_ctrl_v()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn simulate_paste() -> Result<()> {
    Err(InjectError::InjectionFailed("unsupported platform".into()).into())
}

// ---------------------------------------------------------------------------
// Native text injection (per-platform)
// ---------------------------------------------------------------------------

/// Native text injection — delegates to platform-specific implementation.
#[cfg(target_os = "macos")]
fn inject_native(text: &str) -> Result<()> {
    macos::inject_native(text)
}

#[cfg(target_os = "windows")]
fn inject_native(text: &str) -> Result<()> {
    windows_impl::inject_native(text)
}

#[cfg(target_os = "linux")]
fn inject_native(text: &str) -> Result<()> {
    linux::inject_native(text)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn inject_native(_text: &str) -> Result<()> {
    Err(
        InjectError::InjectionFailed("native injection not supported on this platform".into())
            .into(),
    )
}

// ===========================================================================
// macOS implementation — Core Graphics CGEvent API
// ===========================================================================

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    /// Virtual key code for "V" on macOS (kVK_ANSI_V).
    const KV_ANSI_V: CGKeyCode = 0x09;

    /// Default delay between keystrokes in milliseconds for character-by-character
    /// injection.
    const DEFAULT_KEYSTROKE_DELAY_MS: u64 = 5;

    /// Lookup table mapping ASCII characters to (keycode, needs_shift) pairs.
    /// Uses the US ANSI keyboard layout virtual key codes.
    fn char_to_keycode(c: char) -> Option<(CGKeyCode, bool)> {
        // macOS virtual key codes (kVK_ANSI_*)
        const KV_ANSI_A: CGKeyCode = 0x00;
        const KV_ANSI_S: CGKeyCode = 0x01;
        const KV_ANSI_D: CGKeyCode = 0x02;
        const KV_ANSI_F: CGKeyCode = 0x03;
        const KV_ANSI_H: CGKeyCode = 0x04;
        const KV_ANSI_G: CGKeyCode = 0x05;
        const KV_ANSI_Z: CGKeyCode = 0x06;
        const KV_ANSI_X: CGKeyCode = 0x07;
        const KV_ANSI_C: CGKeyCode = 0x08;
        // 0x09 = V (already defined above)
        const KV_ANSI_B: CGKeyCode = 0x0B;
        const KV_ANSI_Q: CGKeyCode = 0x0C;
        const KV_ANSI_W: CGKeyCode = 0x0D;
        const KV_ANSI_E: CGKeyCode = 0x0E;
        const KV_ANSI_R: CGKeyCode = 0x0F;
        const KV_ANSI_Y: CGKeyCode = 0x10;
        const KV_ANSI_T: CGKeyCode = 0x11;
        const KV_ANSI_1: CGKeyCode = 0x12;
        const KV_ANSI_2: CGKeyCode = 0x13;
        const KV_ANSI_3: CGKeyCode = 0x14;
        const KV_ANSI_4: CGKeyCode = 0x15;
        const KV_ANSI_6: CGKeyCode = 0x16;
        const KV_ANSI_5: CGKeyCode = 0x17;
        const KV_ANSI_EQUAL: CGKeyCode = 0x18;
        const KV_ANSI_9: CGKeyCode = 0x19;
        const KV_ANSI_7: CGKeyCode = 0x1A;
        const KV_ANSI_MINUS: CGKeyCode = 0x1B;
        const KV_ANSI_8: CGKeyCode = 0x1C;
        const KV_ANSI_0: CGKeyCode = 0x1D;
        const KV_ANSI_RIGHT_BRACKET: CGKeyCode = 0x1E;
        const KV_ANSI_O: CGKeyCode = 0x1F;
        const KV_ANSI_U: CGKeyCode = 0x20;
        const KV_ANSI_LEFT_BRACKET: CGKeyCode = 0x21;
        const KV_ANSI_I: CGKeyCode = 0x22;
        const KV_ANSI_P: CGKeyCode = 0x23;
        const KV_RETURN: CGKeyCode = 0x24;
        const KV_ANSI_L: CGKeyCode = 0x25;
        const KV_ANSI_J: CGKeyCode = 0x26;
        const KV_ANSI_QUOTE: CGKeyCode = 0x27;
        const KV_ANSI_K: CGKeyCode = 0x28;
        const KV_ANSI_SEMICOLON: CGKeyCode = 0x29;
        const KV_ANSI_BACKSLASH: CGKeyCode = 0x2A;
        const KV_ANSI_COMMA: CGKeyCode = 0x2B;
        const KV_ANSI_SLASH: CGKeyCode = 0x2C;
        const KV_ANSI_N: CGKeyCode = 0x2D;
        const KV_ANSI_M: CGKeyCode = 0x2E;
        const KV_ANSI_PERIOD: CGKeyCode = 0x2F;
        const KV_TAB: CGKeyCode = 0x30;
        const KV_SPACE: CGKeyCode = 0x31;
        const KV_ANSI_GRAVE: CGKeyCode = 0x32;

        match c {
            // Lowercase letters
            'a' => Some((KV_ANSI_A, false)),
            'b' => Some((KV_ANSI_B, false)),
            'c' => Some((KV_ANSI_C, false)),
            'd' => Some((KV_ANSI_D, false)),
            'e' => Some((KV_ANSI_E, false)),
            'f' => Some((KV_ANSI_F, false)),
            'g' => Some((KV_ANSI_G, false)),
            'h' => Some((KV_ANSI_H, false)),
            'i' => Some((KV_ANSI_I, false)),
            'j' => Some((KV_ANSI_J, false)),
            'k' => Some((KV_ANSI_K, false)),
            'l' => Some((KV_ANSI_L, false)),
            'm' => Some((KV_ANSI_M, false)),
            'n' => Some((KV_ANSI_N, false)),
            'o' => Some((KV_ANSI_O, false)),
            'p' => Some((KV_ANSI_P, false)),
            'q' => Some((KV_ANSI_Q, false)),
            'r' => Some((KV_ANSI_R, false)),
            's' => Some((KV_ANSI_S, false)),
            't' => Some((KV_ANSI_T, false)),
            'u' => Some((KV_ANSI_U, false)),
            'v' => Some((KV_ANSI_V, false)),
            'w' => Some((KV_ANSI_W, false)),
            'x' => Some((KV_ANSI_X, false)),
            'y' => Some((KV_ANSI_Y, false)),
            'z' => Some((KV_ANSI_Z, false)),
            // Uppercase letters (same keycode, needs shift)
            'A' => Some((KV_ANSI_A, true)),
            'B' => Some((KV_ANSI_B, true)),
            'C' => Some((KV_ANSI_C, true)),
            'D' => Some((KV_ANSI_D, true)),
            'E' => Some((KV_ANSI_E, true)),
            'F' => Some((KV_ANSI_F, true)),
            'G' => Some((KV_ANSI_G, true)),
            'H' => Some((KV_ANSI_H, true)),
            'I' => Some((KV_ANSI_I, true)),
            'J' => Some((KV_ANSI_J, true)),
            'K' => Some((KV_ANSI_K, true)),
            'L' => Some((KV_ANSI_L, true)),
            'M' => Some((KV_ANSI_M, true)),
            'N' => Some((KV_ANSI_N, true)),
            'O' => Some((KV_ANSI_O, true)),
            'P' => Some((KV_ANSI_P, true)),
            'Q' => Some((KV_ANSI_Q, true)),
            'R' => Some((KV_ANSI_R, true)),
            'S' => Some((KV_ANSI_S, true)),
            'T' => Some((KV_ANSI_T, true)),
            'U' => Some((KV_ANSI_U, true)),
            'V' => Some((KV_ANSI_V, true)),
            'W' => Some((KV_ANSI_W, true)),
            'X' => Some((KV_ANSI_X, true)),
            'Y' => Some((KV_ANSI_Y, true)),
            'Z' => Some((KV_ANSI_Z, true)),
            // Digits
            '0' => Some((KV_ANSI_0, false)),
            '1' => Some((KV_ANSI_1, false)),
            '2' => Some((KV_ANSI_2, false)),
            '3' => Some((KV_ANSI_3, false)),
            '4' => Some((KV_ANSI_4, false)),
            '5' => Some((KV_ANSI_5, false)),
            '6' => Some((KV_ANSI_6, false)),
            '7' => Some((KV_ANSI_7, false)),
            '8' => Some((KV_ANSI_8, false)),
            '9' => Some((KV_ANSI_9, false)),
            // Symbols (unshifted)
            ' ' => Some((KV_SPACE, false)),
            '\t' => Some((KV_TAB, false)),
            '\n' => Some((KV_RETURN, false)),
            '-' => Some((KV_ANSI_MINUS, false)),
            '=' => Some((KV_ANSI_EQUAL, false)),
            '[' => Some((KV_ANSI_LEFT_BRACKET, false)),
            ']' => Some((KV_ANSI_RIGHT_BRACKET, false)),
            '\\' => Some((KV_ANSI_BACKSLASH, false)),
            ';' => Some((KV_ANSI_SEMICOLON, false)),
            '\'' => Some((KV_ANSI_QUOTE, false)),
            ',' => Some((KV_ANSI_COMMA, false)),
            '.' => Some((KV_ANSI_PERIOD, false)),
            '/' => Some((KV_ANSI_SLASH, false)),
            '`' => Some((KV_ANSI_GRAVE, false)),
            // Symbols (shifted)
            '!' => Some((KV_ANSI_1, true)),
            '@' => Some((KV_ANSI_2, true)),
            '#' => Some((KV_ANSI_3, true)),
            '$' => Some((KV_ANSI_4, true)),
            '%' => Some((KV_ANSI_5, true)),
            '^' => Some((KV_ANSI_6, true)),
            '&' => Some((KV_ANSI_7, true)),
            '*' => Some((KV_ANSI_8, true)),
            '(' => Some((KV_ANSI_9, true)),
            ')' => Some((KV_ANSI_0, true)),
            '_' => Some((KV_ANSI_MINUS, true)),
            '+' => Some((KV_ANSI_EQUAL, true)),
            '{' => Some((KV_ANSI_LEFT_BRACKET, true)),
            '}' => Some((KV_ANSI_RIGHT_BRACKET, true)),
            '|' => Some((KV_ANSI_BACKSLASH, true)),
            ':' => Some((KV_ANSI_SEMICOLON, true)),
            '"' => Some((KV_ANSI_QUOTE, true)),
            '<' => Some((KV_ANSI_COMMA, true)),
            '>' => Some((KV_ANSI_PERIOD, true)),
            '?' => Some((KV_ANSI_SLASH, true)),
            '~' => Some((KV_ANSI_GRAVE, true)),
            _ => None,
        }
    }

    /// Create a CGEventSource for synthetic events.
    fn event_source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| InjectError::PermissionRequired.into())
    }

    /// Post a key-down then key-up for `keycode` with the given modifier flags.
    fn post_key(keycode: CGKeyCode, flags: CGEventFlags) -> Result<()> {
        let source = event_source()?;

        let key_down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
            .map_err(|_| InjectError::InjectionFailed("failed to create key-down event".into()))?;
        key_down.set_flags(flags);

        let key_up = CGEvent::new_keyboard_event(source, keycode, false)
            .map_err(|_| InjectError::InjectionFailed("failed to create key-up event".into()))?;
        key_up.set_flags(flags);

        key_down.post(CGEventTapLocation::HID);
        key_up.post(CGEventTapLocation::HID);

        Ok(())
    }

    /// Simulate Cmd+V (paste) on macOS via CGEvent.
    pub(super) fn simulate_cmd_v() -> Result<()> {
        debug!("simulating Cmd+V via CGEvent");
        post_key(KV_ANSI_V, CGEventFlags::CGEventFlagCommand)?;
        // Short delay to let the receiving app process the paste.
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Type text character-by-character using CGEvent key events.
    ///
    /// This is useful for applications that intercept or block clipboard paste.
    /// Each character is mapped to a virtual keycode and posted as a key-down/key-up
    /// pair. Characters that are not in the lookup table (e.g. non-ASCII, emoji)
    /// cause this function to return an error so the caller can fall back to
    /// clipboard-based injection.
    ///
    /// `delay_ms` controls the pause between each keystroke (default: 5ms).
    pub(super) fn inject_native_keystrokes(text: &str, delay_ms: Option<u64>) -> Result<()> {
        let delay =
            std::time::Duration::from_millis(delay_ms.unwrap_or(DEFAULT_KEYSTROKE_DELAY_MS));

        debug!(
            "injecting {} characters via CGEvent keystrokes (delay={}ms)",
            text.len(),
            delay.as_millis()
        );

        for (i, c) in text.chars().enumerate() {
            let (keycode, needs_shift) = char_to_keycode(c).ok_or_else(|| {
                InjectError::InjectionFailed(format!(
                    "unsupported character '{}' (U+{:04X}) at position {}",
                    c, c as u32, i
                ))
            })?;

            let flags = if needs_shift {
                CGEventFlags::CGEventFlagShift
            } else {
                CGEventFlags::empty()
            };

            post_key(keycode, flags)?;

            if i < text.len().saturating_sub(1) {
                std::thread::sleep(delay);
            }
        }

        Ok(())
    }

    /// Native injection on macOS: tries character-by-character keystroke injection
    /// first, then falls back to clipboard + Cmd+V if the text contains characters
    /// that cannot be mapped to keycodes.
    pub(super) fn inject_native(text: &str) -> Result<()> {
        debug!("native macOS injection: attempting keystroke injection");

        match inject_native_keystrokes(text, None) {
            Ok(()) => {
                debug!("keystroke injection succeeded");
                Ok(())
            }
            Err(e) => {
                warn!("keystroke injection failed ({e}), falling back to clipboard + Cmd+V");
                inject_native_clipboard(text)
            }
        }
    }

    /// Clipboard-based native injection on macOS: sets clipboard, waits briefly,
    /// then simulates Cmd+V.
    fn inject_native_clipboard(text: &str) -> Result<()> {
        use arboard::Clipboard;

        debug!("native macOS injection via CGEvent Cmd+V");

        let mut clipboard = Clipboard::new().map_err(|e| InjectError::Clipboard(e.to_string()))?;

        let previous = clipboard.get_text().ok();

        clipboard
            .set_text(text)
            .map_err(|e| InjectError::Clipboard(e.to_string()))?;

        // Brief delay after setting clipboard to avoid race conditions.
        std::thread::sleep(std::time::Duration::from_millis(10));

        simulate_cmd_v()?;

        // Restore after the paste completes.
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(prev) = previous {
            let _ = clipboard.set_text(prev);
        }

        Ok(())
    }
}

// ===========================================================================
// Windows implementation — SendInput API
// ===========================================================================

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
        SendInput, VK_CONTROL, VK_V,
    };

    /// Build a KEYBDINPUT for the given virtual key.
    fn kbd_input(vk: u16, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// Simulate Ctrl+V on Windows via SendInput.
    pub(super) fn simulate_ctrl_v() -> Result<()> {
        debug!("simulating Ctrl+V via SendInput");

        let inputs = [
            kbd_input(VK_CONTROL.0, 0),                 // Ctrl down
            kbd_input(VK_V.0, 0),                       // V down
            kbd_input(VK_V.0, KEYEVENTF_KEYUP.0),       // V up
            kbd_input(VK_CONTROL.0, KEYEVENTF_KEYUP.0), // Ctrl up
        ];

        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };

        if sent != inputs.len() as u32 {
            return Err(InjectError::InjectionFailed(
                "SendInput did not process all events".into(),
            )
            .into());
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Native injection on Windows: clipboard + Ctrl+V via SendInput.
    pub(super) fn inject_native(text: &str) -> Result<()> {
        use arboard::Clipboard;

        debug!("native Windows injection via SendInput Ctrl+V");

        let mut clipboard = Clipboard::new().map_err(|e| InjectError::Clipboard(e.to_string()))?;

        let previous = clipboard.get_text().ok();

        clipboard
            .set_text(text)
            .map_err(|e| InjectError::Clipboard(e.to_string()))?;

        // Brief delay after setting clipboard to avoid race conditions.
        std::thread::sleep(std::time::Duration::from_millis(10));

        simulate_ctrl_v()?;

        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(prev) = previous {
            let _ = clipboard.set_text(prev);
        }

        Ok(())
    }
}

// ===========================================================================
// Linux implementation — wtype (Wayland) / xdotool (X11)
// ===========================================================================

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::process::Command;

    /// Strip control characters (below 0x20) except newline and tab to prevent
    /// injection of escape sequences through xdotool/wtype.
    fn sanitize_for_injection(text: &str) -> String {
        text.chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect()
    }

    /// Returns true if the session is running under a Wayland compositor.
    fn is_wayland() -> bool {
        std::env::var("WAYLAND_DISPLAY").is_ok()
    }

    /// Simulate Ctrl+V on Linux.
    ///
    /// On Wayland, tries `wtype` first; falls back to `xdotool` on X11 or
    /// if `wtype` is unavailable.
    pub(super) fn simulate_ctrl_v() -> Result<()> {
        if is_wayland() {
            debug!("Wayland detected, trying wtype for Ctrl+V");
            match simulate_ctrl_v_wtype() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!("wtype Ctrl+V failed ({e}), falling back to xdotool");
                }
            }
        }

        simulate_ctrl_v_xdotool()
    }

    /// Simulate Ctrl+V via wtype (Wayland).
    fn simulate_ctrl_v_wtype() -> Result<()> {
        debug!("simulating Ctrl+V via wtype");

        let status = Command::new("wtype")
            .args(["-M", "ctrl", "-P", "v", "-p", "v", "-m", "ctrl"])
            .status()
            .map_err(|e| {
                InjectError::InjectionFailed(format!("failed to run wtype (is it installed?): {e}"))
            })?;

        if !status.success() {
            return Err(
                InjectError::InjectionFailed(format!("wtype exited with status {status}")).into(),
            );
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Simulate Ctrl+V via xdotool (X11).
    fn simulate_ctrl_v_xdotool() -> Result<()> {
        debug!("simulating Ctrl+V via xdotool");

        let status = Command::new("xdotool")
            .args(["key", "ctrl+v"])
            .status()
            .map_err(|e| {
                InjectError::InjectionFailed(format!(
                    "failed to run xdotool (is it installed?): {e}"
                ))
            })?;

        if !status.success() {
            return Err(InjectError::InjectionFailed(format!(
                "xdotool exited with status {status}"
            ))
            .into());
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    /// Native injection on Linux: use wtype (Wayland) or xdotool (X11) for
    /// direct text entry.
    ///
    /// On Wayland, tries `wtype` first; falls back to `xdotool` on X11 or
    /// if `wtype` is unavailable.
    pub(super) fn inject_native(text: &str) -> Result<()> {
        if is_wayland() {
            debug!("Wayland detected, trying wtype for native injection");
            match inject_native_wtype(text) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!("wtype injection failed ({e}), falling back to xdotool");
                }
            }
        }

        inject_native_xdotool(text)
    }

    /// Native injection via wtype (Wayland).
    fn inject_native_wtype(text: &str) -> Result<()> {
        let text = sanitize_for_injection(text);
        debug!("native Linux injection via wtype");

        let status = Command::new("wtype")
            .args(["--", &text])
            .status()
            .map_err(|e| {
                InjectError::InjectionFailed(format!("failed to run wtype (is it installed?): {e}"))
            })?;

        if !status.success() {
            return Err(
                InjectError::InjectionFailed(format!("wtype exited with status {status}")).into(),
            );
        }

        Ok(())
    }

    /// Native injection via xdotool (X11).
    fn inject_native_xdotool(text: &str) -> Result<()> {
        let text = sanitize_for_injection(text);
        debug!("native Linux injection via xdotool type");

        let status = Command::new("xdotool")
            .args(["type", "--clearmodifiers", "--", &text])
            .status()
            .map_err(|e| {
                InjectError::InjectionFailed(format!(
                    "failed to run xdotool (is it installed?): {e}"
                ))
            })?;

        if !status.success() {
            return Err(InjectError::InjectionFailed(format!(
                "xdotool exited with status {status}"
            ))
            .into());
        }

        Ok(())
    }
}
