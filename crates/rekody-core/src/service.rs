//! LaunchAgent management — run the rekody daemon at login with no terminal
//! (`rekody service install|uninstall|status`).
//!
//! Writes `~/Library/LaunchAgents/com.rekody.daemon.plist` pointing at the
//! current executable, with `KeepAlive` so a crashed daemon restarts itself,
//! and logs at `~/Library/Logs/rekody/`. The HUD helper path can be baked in
//! via `--hud-bin` so the pill comes up at login too.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

const LABEL: &str = "com.rekody.daemon";

fn plist_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home directory")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn log_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home directory")?
        .join("Library/Logs/rekody"))
}

fn gui_domain() -> String {
    // SAFETY-free libc avoidance: launchctl wants gui/<uid>.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

/// Render the LaunchAgent plist for `daemon` (+ optional HUD helper path).
fn render_plist(daemon: &str, hud_bin: Option<&str>, logs: &str) -> String {
    let env_block = match hud_bin {
        Some(hud) => format!(
            "  <key>EnvironmentVariables</key>\n  <dict>\n    \
             <key>REKODY_HUD_BIN</key>\n    <string>{hud}</string>\n  </dict>\n"
        ),
        None => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{daemon}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
{env_block}  <key>StandardOutPath</key>
  <string>{logs}/daemon.log</string>
  <key>StandardErrorPath</key>
  <string>{logs}/daemon.err</string>
</dict>
</plist>
"#
    )
}

/// Install (or reinstall) the LaunchAgent and start it now.
pub fn install(hud_bin: Option<PathBuf>) -> Result<()> {
    let daemon = std::env::current_exe().context("resolving daemon path")?;
    let logs = log_dir()?;
    std::fs::create_dir_all(&logs).context("creating log dir")?;
    let plist = plist_path()?;
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).context("creating LaunchAgents dir")?;
    }

    let hud = hud_bin.map(|p| p.to_string_lossy().into_owned());
    let contents = render_plist(
        &daemon.to_string_lossy(),
        hud.as_deref(),
        &logs.to_string_lossy(),
    );

    // Stop any prior incarnation before replacing the plist.
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{}/{LABEL}", gui_domain())])
        .output();
    std::fs::write(&plist, contents).context("writing plist")?;
    let out = Command::new("launchctl")
        .args(["bootstrap", &gui_domain()])
        .arg(&plist)
        .output()
        .context("running launchctl bootstrap")?;
    if !out.status.success() {
        anyhow::bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    println!("✓ installed — rekody now starts at login and restarts if it crashes");
    println!("  agent   {}", plist.display());
    println!("  daemon  {}", daemon.display());
    if let Some(h) = hud {
        println!("  hud     {h}");
    }
    println!("  logs    {}/daemon.log", logs.display());
    println!("  remove with: rekody service uninstall");
    Ok(())
}

/// Stop and remove the LaunchAgent.
pub fn uninstall() -> Result<()> {
    let plist = plist_path()?;
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{}/{LABEL}", gui_domain())])
        .output();
    match std::fs::remove_file(&plist) {
        Ok(()) => println!("✓ uninstalled — rekody no longer starts at login"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("nothing installed ({} not present)", plist.display())
        }
        Err(e) => return Err(e).context("removing plist"),
    }
    Ok(())
}

/// Report whether the agent is installed and running.
pub fn status() -> Result<()> {
    let plist = plist_path()?;
    if !plist.exists() {
        println!("not installed — run: rekody service install");
        return Ok(());
    }
    let out = Command::new("launchctl")
        .args(["print", &format!("{}/{LABEL}", gui_domain())])
        .output()
        .context("running launchctl print")?;
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let pid = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("pid = "))
            .unwrap_or("?");
        println!("✓ installed and loaded — daemon pid {pid}");
    } else {
        println!("installed but not loaded — run: rekody service install");
    }
    println!("  agent {}", plist.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_daemon_and_hud_env() {
        let p = render_plist(
            "/opt/homebrew/bin/rekody",
            Some("/usr/local/bin/rekody-hud"),
            "/tmp/logs",
        );
        assert!(p.contains("<string>/opt/homebrew/bin/rekody</string>"));
        assert!(p.contains("<key>REKODY_HUD_BIN</key>"));
        assert!(p.contains("<string>/usr/local/bin/rekody-hud</string>"));
        assert!(p.contains("<key>KeepAlive</key>"));
        assert!(p.contains("/tmp/logs/daemon.log"));
    }

    #[test]
    fn plist_omits_env_without_hud() {
        let p = render_plist("/opt/homebrew/bin/rekody", None, "/tmp/logs");
        assert!(!p.contains("EnvironmentVariables"));
    }
}
