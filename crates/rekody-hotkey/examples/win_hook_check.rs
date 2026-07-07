//! Windows runtime check for the WH_KEYBOARD_LL hotkey hook.
//!
//! Starts the real listener, synthesizes Ctrl+Space via SendInput (low-level
//! hooks receive injected input), and asserts the activation-mode state
//! machine emits the expected events — the Windows analog of the macOS
//! keysim harness.
//!
//! The listener is once-per-process (OnceLock), so the mode under test is
//! selected with REKODY_TEST_MODE = ptt | toggle | both. CI runs all three.

#[cfg(not(windows))]
fn main() {
    println!("win_hook_check is Windows-only; nothing to do on this platform.");
}

#[cfg(windows)]
fn main() {
    windows_only::run();
}

#[cfg(windows)]
mod windows_only {
    use rekody_hotkey::{ActivationMode, HotkeyConfig, HotkeyEvent, TriggerKey, start_listener};
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VIRTUAL_KEY, VK_CONTROL, VK_SPACE,
    };

    fn send_key(vk: VIRTUAL_KEY, up: bool) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        assert_eq!(sent, 1, "SendInput failed");
        std::thread::sleep(Duration::from_millis(30));
    }

    /// Press Ctrl+Space, hold for `hold_ms`, release (space first, then ctrl).
    fn ctrl_space(hold_ms: u64) {
        send_key(VK_CONTROL, false);
        send_key(VK_SPACE, false);
        std::thread::sleep(Duration::from_millis(hold_ms));
        send_key(VK_SPACE, true);
        send_key(VK_CONTROL, true);
    }

    fn name(ev: &HotkeyEvent) -> &'static str {
        match ev {
            HotkeyEvent::RecordStart => "Start",
            HotkeyEvent::RecordLatched => "Latched",
            HotkeyEvent::RecordStop => "Stop",
            HotkeyEvent::CommandMode => "CommandMode",
            HotkeyEvent::CycleSkill => "CycleSkill",
        }
    }

    async fn expect(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<HotkeyEvent>,
        what: &str,
        want: &[&str],
    ) {
        let mut got: Vec<&'static str> = Vec::new();
        for _ in 0..want.len() {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(ev)) => got.push(name(&ev)),
                Ok(None) => panic!("{what}: event channel closed (got {got:?}, want {want:?})"),
                Err(_) => panic!("{what}: timed out (got {got:?}, want {want:?})"),
            }
        }
        // No stragglers within a settle window.
        if let Ok(Some(extra)) = tokio::time::timeout(Duration::from_millis(600), rx.recv()).await {
            panic!(
                "{what}: unexpected extra event {:?} after {got:?}",
                name(&extra)
            );
        }
        assert_eq!(got, want, "{what}");
        println!("ok   — {what}: {got:?}");
    }

    pub fn run() {
        let mode_s = std::env::var("REKODY_TEST_MODE").unwrap_or_else(|_| "both".into());
        let (mode, deadman) = match mode_s.as_str() {
            "ptt" => (ActivationMode::PushToTalk, 600),
            "toggle" => (ActivationMode::Toggle, 600),
            "both" => (ActivationMode::Both, 600),
            "deadman" => (ActivationMode::Both, 2), // 2s cap to test the watchdog
            other => panic!("unknown REKODY_TEST_MODE: {other}"),
        };
        println!("== win_hook_check mode={mode_s} ==");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio rt");
        rt.block_on(async {
            let mut rx = start_listener(HotkeyConfig {
                activation_mode: mode,
                trigger_key: TriggerKey::OptionSpace, // unused on Windows (Ctrl+Space)
                max_recording_secs: deadman,
            })
            .expect("start_listener");
            // Let the hook thread install before sending input.
            tokio::time::sleep(Duration::from_millis(700)).await;

            match mode_s.as_str() {
                "ptt" => {
                    ctrl_space(600);
                    expect(&mut rx, "ptt hold→release", &["Start", "Stop"]).await;
                    ctrl_space(80);
                    expect(&mut rx, "ptt quick tap still stops", &["Start", "Stop"]).await;
                }
                "toggle" => {
                    ctrl_space(80);
                    expect(&mut rx, "toggle tap = start", &["Start"]).await;
                    ctrl_space(80);
                    expect(&mut rx, "toggle tap = stop", &["Stop"]).await;
                }
                "both" => {
                    ctrl_space(80);
                    expect(&mut rx, "both quick tap latches", &["Start", "Latched"]).await;
                    ctrl_space(80);
                    expect(&mut rx, "both tap-while-latched stops", &["Stop"]).await;
                    ctrl_space(600);
                    expect(&mut rx, "both hold acts as PTT", &["Start", "Stop"]).await;
                }
                "deadman" => {
                    ctrl_space(80);
                    expect(&mut rx, "latch on", &["Start", "Latched"]).await;
                    // Walk away: the 2s deadman watchdog must force a stop.
                    match tokio::time::timeout(Duration::from_secs(6), rx.recv()).await {
                        Ok(Some(HotkeyEvent::RecordStop)) => {
                            println!("ok   — deadman force-stopped the latched session")
                        }
                        other => panic!("deadman: expected RecordStop, got {other:?}"),
                    }
                }
                _ => unreachable!(),
            }
        });
        println!("win_hook_check {mode_s}: all checks passed");
    }
}
