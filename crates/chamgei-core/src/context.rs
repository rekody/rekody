//! Active application context detection.
//!
//! Detects which application is currently in the foreground so the LLM
//! post-processor can make context-aware formatting decisions.

use chamgei_llm::AppContext;

/// Detect the currently active (frontmost) application.
///
/// Returns an [`AppContext`] with the application name and, where available,
/// the bundle identifier.
pub fn detect_active_app() -> AppContext {
    platform::detect()
}

// ── macOS ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use chamgei_llm::AppContext;
    use std::process::Command;

    pub fn detect() -> AppContext {
        // Use NSWorkspace via osascript — more reliable than System Events
        // for detecting the frontmost app.
        let output = Command::new("osascript")
            .arg("-e")
            .arg(
                r#"use framework "AppKit"
set activeApp to current application's NSWorkspace's sharedWorkspace()'s frontmostApplication()
set appName to (activeApp's localizedName()) as text
set bundleID to (activeApp's bundleIdentifier()) as text
return appName & ", " & bundleID"#,
            )
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                parse_osascript_output(stdout.trim())
            }
            Ok(out) => {
                tracing::warn!(
                    "osascript failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                fallback()
            }
            Err(e) => {
                tracing::warn!("failed to run osascript: {e}");
                fallback()
            }
        }
    }

    /// Parse the AppleScript output format: `"AppName, com.example.bundle"`
    fn parse_osascript_output(raw: &str) -> AppContext {
        if let Some((name, bundle)) = raw.split_once(", ") {
            AppContext {
                app_name: name.to_string(),
                bundle_id: Some(bundle.to_string()),
            }
        } else {
            AppContext {
                app_name: raw.to_string(),
                bundle_id: None,
            }
        }
    }

    fn fallback() -> AppContext {
        AppContext {
            app_name: "Unknown".into(),
            bundle_id: None,
        }
    }
}

// ── Linux ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod platform {
    use chamgei_llm::AppContext;
    use std::process::Command;

    pub fn detect() -> AppContext {
        let output = Command::new("xdotool")
            .args(["getactivewindow", "getwindowname"])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                AppContext {
                    app_name: name,
                    bundle_id: None,
                }
            }
            _ => AppContext {
                app_name: "Unknown".into(),
                bundle_id: None,
            },
        }
    }
}

// ── Windows ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use chamgei_llm::AppContext;

    pub fn detect() -> AppContext {
        // TODO: Implement via win32 APIs (GetForegroundWindow + GetWindowText).
        AppContext {
            app_name: "Unknown".into(),
            bundle_id: None,
        }
    }
}
