//! Command mode for text transformation via voice instructions.
//!
//! Lets users select text, press a hotkey, speak an instruction (e.g.,
//! "make this more concise", "translate to Spanish"), and have the
//! selected text transformed by an LLM according to the instruction.
//!
//! # Flow
//!
//! 1. User selects text in any application.
//! 2. User presses the command-mode hotkey.
//! 3. [`CommandMode::capture_selection`] copies the selection to the clipboard
//!    and reads it back.
//! 4. The user speaks a voice instruction (captured via the normal audio
//!    pipeline and transcribed by STT).
//! 5. [`CommandMode::build_command_prompt`] builds an LLM prompt combining
//!    the selected text and the voice instruction.
//! 6. The LLM returns the transformed text.
//! 7. [`CommandMode::replace_selection`] pastes the transformed text over
//!    the original selection.

use anyhow::{Context, Result};
use tracing::{debug, warn};

/// Orchestrates the command-mode workflow: capture selected text,
/// build an LLM prompt from the selection + voice instruction, and
/// replace the selection with the LLM output.
pub struct CommandMode {
    /// Short delay (ms) after simulating keystrokes to let the OS process them.
    keystroke_delay_ms: u64,
}

impl Default for CommandMode {
    fn default() -> Self {
        Self {
            keystroke_delay_ms: 100,
        }
    }
}

impl CommandMode {
    /// Create a new `CommandMode` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new `CommandMode` with a custom keystroke delay.
    ///
    /// The delay is inserted after simulating copy/paste keystrokes to
    /// give the target application time to process them.
    pub fn with_keystroke_delay(mut self, delay_ms: u64) -> Self {
        self.keystroke_delay_ms = delay_ms;
        self
    }

    /// Capture the currently selected text.
    ///
    /// Simulates Cmd+C (macOS) or Ctrl+C (Linux/Windows) to copy the
    /// selection, waits briefly for the OS to process it, then reads the
    /// clipboard contents.
    pub fn capture_selection(&self) -> Result<String> {
        debug!("capturing selected text via simulated copy");

        // Simulate the copy keystroke.
        simulate_copy()?;

        // Wait for the OS and target app to process the copy.
        std::thread::sleep(std::time::Duration::from_millis(self.keystroke_delay_ms));

        // Read back from the clipboard.
        let text = read_clipboard()?;

        if text.is_empty() {
            warn!("clipboard was empty after copy — no text may have been selected");
        } else {
            debug!(len = text.len(), "captured selected text");
        }

        Ok(text)
    }

    /// Build an LLM prompt that instructs the model to transform the
    /// selected text according to the user's voice instruction.
    ///
    /// The returned prompt is a self-contained system+user message intended
    /// to be sent to the LLM provider chain. The LLM should output **only**
    /// the transformed text with no preamble or explanation.
    pub fn build_command_prompt(selected_text: &str, voice_instruction: &str) -> String {
        format!(
            "You are a text transformation assistant. The user has selected text and given a \
             voice instruction describing how to transform it.\n\
             \n\
             Rules:\n\
             - Apply the instruction to the selected text.\n\
             - Output ONLY the transformed text.\n\
             - Do NOT include any explanation, preamble, quotes, or markdown formatting.\n\
             - Do NOT wrap the output in code blocks or backticks.\n\
             - Preserve the original formatting style (e.g., indentation, line breaks) unless \
               the instruction explicitly asks to change it.\n\
             \n\
             ## Selected text\n\
             {selected_text}\n\
             \n\
             ## Instruction\n\
             {voice_instruction}"
        )
    }

    /// Replace the current selection with new (transformed) text.
    ///
    /// Uses [`rekody_inject::inject_text`] with the clipboard injection
    /// method, which sets the clipboard to `new_text` and simulates a paste
    /// keystroke (Cmd+V / Ctrl+V). Because the original text was selected
    /// before command mode was activated, the paste replaces it.
    pub fn replace_selection(new_text: &str) -> Result<()> {
        debug!(
            len = new_text.len(),
            "replacing selection with transformed text"
        );
        rekody_inject::inject_text(new_text, rekody_inject::InjectionMethod::Clipboard)
            .context("failed to replace selection with transformed text")
    }
}

// ---------------------------------------------------------------------------
// Platform-specific copy keystroke simulation
// ---------------------------------------------------------------------------

/// Simulate Cmd+C (macOS) or Ctrl+C (Linux/Windows) to copy selected text.
#[cfg(target_os = "macos")]
fn simulate_copy() -> Result<()> {
    debug!("simulating Cmd+C via osascript");

    let status = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to keystroke \"c\" using command down",
        ])
        .status()
        .context("failed to run osascript for copy simulation")?;

    if !status.success() {
        anyhow::bail!("osascript copy simulation exited with status {status}");
    }

    std::thread::sleep(std::time::Duration::from_millis(50));
    Ok(())
}

#[cfg(target_os = "linux")]
fn simulate_copy() -> Result<()> {
    debug!("simulating Ctrl+C via xdotool");

    let status = std::process::Command::new("xdotool")
        .args(["key", "ctrl+c"])
        .status()
        .context("failed to run xdotool (is it installed?)")?;

    if !status.success() {
        anyhow::bail!("xdotool exited with status {status}");
    }

    std::thread::sleep(std::time::Duration::from_millis(50));
    Ok(())
}

#[cfg(target_os = "windows")]
fn simulate_copy() -> Result<()> {
    debug!("simulating Ctrl+C via powershell SendKeys");

    // Use powershell to simulate Ctrl+C. This is a fallback approach;
    // a production implementation would use the SendInput API directly
    // (similar to rekody-inject's Windows paste implementation).
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('^c')",
        ])
        .status()
        .context("failed to run powershell for copy simulation")?;

    if !status.success() {
        anyhow::bail!("powershell copy simulation exited with status {status}");
    }

    std::thread::sleep(std::time::Duration::from_millis(50));
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn simulate_copy() -> Result<()> {
    anyhow::bail!("copy simulation not supported on this platform")
}

// ---------------------------------------------------------------------------
// Platform-specific clipboard reading
// ---------------------------------------------------------------------------

/// Read the current clipboard contents as a UTF-8 string.
#[cfg(target_os = "macos")]
fn read_clipboard() -> Result<String> {
    debug!("reading clipboard via pbpaste");
    let output = std::process::Command::new("pbpaste")
        .output()
        .context("failed to run pbpaste")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "linux")]
fn read_clipboard() -> Result<String> {
    debug!("reading clipboard via xclip");
    let output = std::process::Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        .context("failed to run xclip (is it installed?)")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "windows")]
fn read_clipboard() -> Result<String> {
    debug!("reading clipboard via powershell");
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "Get-Clipboard"])
        .output()
        .context("failed to run powershell for clipboard read")?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn read_clipboard() -> Result<String> {
    anyhow::bail!("clipboard reading not supported on this platform")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_command_prompt_contains_selected_text() {
        let prompt = CommandMode::build_command_prompt(
            "The quick brown fox jumps over the lazy dog.",
            "make this more concise",
        );
        assert!(prompt.contains("The quick brown fox jumps over the lazy dog."));
    }

    #[test]
    fn test_build_command_prompt_contains_instruction() {
        let prompt = CommandMode::build_command_prompt("Hello world", "translate to Spanish");
        assert!(prompt.contains("translate to Spanish"));
    }

    #[test]
    fn test_build_command_prompt_instructs_output_only() {
        let prompt = CommandMode::build_command_prompt("text", "instruction");
        assert!(prompt.contains("Output ONLY the transformed text"));
        assert!(prompt.contains("Do NOT include any explanation"));
    }

    #[test]
    fn test_build_command_prompt_structure() {
        let prompt = CommandMode::build_command_prompt("some code here", "add comments");
        // Verify the prompt has the expected sections.
        assert!(prompt.contains("## Selected text"));
        assert!(prompt.contains("## Instruction"));
        assert!(prompt.contains("text transformation assistant"));
    }

    #[test]
    fn test_default_keystroke_delay() {
        let cm = CommandMode::new();
        assert_eq!(cm.keystroke_delay_ms, 100);
    }

    #[test]
    fn test_custom_keystroke_delay() {
        let cm = CommandMode::new().with_keystroke_delay(200);
        assert_eq!(cm.keystroke_delay_ms, 200);
    }
}
