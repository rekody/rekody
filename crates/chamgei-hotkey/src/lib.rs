//! Global hotkey listener for Chamgei voice dictation.
//!
//! ## Hotkey bindings
//!
//! | Action | Shortcut |
//! |--------|----------|
//! | Push-to-talk (hold to record, release to stop) | `Fn` |
//! | Toggle (press to start, press to stop) | `Fn` (in toggle mode) |
//! | Command mode (transform selected text) | `Fn + Enter` |

use anyhow::Result;
use rdev::{Event, EventType, Key, listen};
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
    fn_pressed: bool,
    is_recording: bool,
}

fn is_fn_key(key: &Key) -> bool {
    matches!(key, Key::Function)
}

pub fn start_listener(config: HotkeyConfig) -> Result<mpsc::UnboundedReceiver<HotkeyEvent>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(Mutex::new(KeyState::default()));
    let mode = config.activation_mode;

    std::thread::spawn(move || {
        tracing::info!("hotkey listener started (mode: {:?})", mode);

        let callback = move |event: Event| {
            let mut state = match state.lock() {
                Ok(s) => s,
                Err(poisoned) => poisoned.into_inner(),
            };

            match event.event_type {
                EventType::KeyPress(key) => {
                    if is_fn_key(&key) {
                        // Debounce: ignore key-repeat events
                        if state.fn_pressed {
                            return;
                        }
                        state.fn_pressed = true;

                        match mode {
                            ActivationMode::PushToTalk => {
                                // Start recording on press
                                if !state.is_recording {
                                    state.is_recording = true;
                                    tracing::debug!("push-to-talk: RecordStart");
                                    let _ = tx.send(HotkeyEvent::RecordStart);
                                }
                            }
                            ActivationMode::Toggle => {
                                // Toggle on press (not release) for snappier feel
                                if state.is_recording {
                                    state.is_recording = false;
                                    tracing::debug!("toggle: RecordStop");
                                    let _ = tx.send(HotkeyEvent::RecordStop);
                                } else {
                                    state.is_recording = true;
                                    tracing::debug!("toggle: RecordStart");
                                    let _ = tx.send(HotkeyEvent::RecordStart);
                                }
                            }
                        }
                        return;
                    }

                    // Fn + Enter = command mode
                    if state.fn_pressed && key == Key::Return {
                        tracing::debug!("command mode (Fn+Enter)");
                        let _ = tx.send(HotkeyEvent::CommandMode);
                    }
                }

                EventType::KeyRelease(key) => {
                    if is_fn_key(&key) {
                        state.fn_pressed = false;

                        // In push-to-talk, release = stop recording
                        if mode == ActivationMode::PushToTalk && state.is_recording {
                            state.is_recording = false;
                            tracing::debug!("push-to-talk: RecordStop (Fn released)");
                            let _ = tx.send(HotkeyEvent::RecordStop);
                        }
                    }
                }

                _ => {}
            }
        };

        if let Err(e) = listen(callback) {
            tracing::error!("hotkey listener failed: {:?}", e);
        }
    });

    Ok(rx)
}
