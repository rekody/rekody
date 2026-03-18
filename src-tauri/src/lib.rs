use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};

// ---------------------------------------------------------------------------
// Shared state managed by Tauri
// ---------------------------------------------------------------------------

/// Shared pipeline status, accessible from IPC commands.
struct PipelineState {
    status_manager: chamgei_core::status::StatusManager,
}

// ---------------------------------------------------------------------------
// Existing commands (preserved)
// ---------------------------------------------------------------------------

/// Get default config as JSON for the frontend.
#[tauri::command]
fn get_default_config() -> String {
    let config = chamgei_core::ChamgeiConfig::default();
    serde_json::to_string(&config).unwrap_or_default()
}

fn history_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("chamgei").join("history.json"))
}

/// Read the history file and return its contents as a JSON string.
#[tauri::command]
fn get_history() -> String {
    let Some(path) = history_path() else {
        return "[]".to_string();
    };
    fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string())
}

/// Delete the history file.
#[tauri::command]
fn clear_history() -> Result<(), String> {
    let Some(path) = history_path() else {
        return Ok(());
    };
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Copy text to the system clipboard.
#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Onboarding IPC commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PermissionStatus {
    mic: bool,
    accessibility: bool,
}

/// Check if microphone and accessibility permissions are granted (macOS).
#[tauri::command]
fn check_permissions() -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        let mic = check_mic_permission();
        let accessibility = check_accessibility_permission();
        PermissionStatus { mic, accessibility }
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionStatus {
            mic: true,
            accessibility: true,
        }
    }
}

/// Open System Settings to the Microphone privacy pane.
#[tauri::command]
fn open_mic_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn();
    }
}

/// Open System Settings to the Accessibility privacy pane.
#[tauri::command]
fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }
}

/// Return the current microphone RMS level (for the mic test screen).
///
/// Opens the default input device, captures a short buffer (~100ms),
/// and computes the RMS energy. Returns 0.0 on error.
#[tauri::command]
fn get_audio_level() -> f32 {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => return 0.0,
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(_) => return 0.0,
    };

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let samples_clone = Arc::clone(&samples);
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = Arc::clone(&done);

    let stream_config: cpal::StreamConfig = config.clone().into();
    let sample_format = config.sample_format();

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if !done_clone.load(Ordering::Relaxed) {
                    if let Ok(mut buf) = samples_clone.lock() {
                        buf.extend_from_slice(data);
                        // Collect ~100ms at any sample rate
                        if buf.len() >= (stream_config.sample_rate.0 as usize / 10) {
                            done_clone.store(true, Ordering::Relaxed);
                        }
                    }
                }
            },
            |_err| {},
            None,
        ),
        _ => return 0.0,
    };

    let stream = match stream {
        Ok(s) => s,
        Err(_) => return 0.0,
    };

    let _ = stream.play();

    // Wait up to 200ms for enough samples
    let start = std::time::Instant::now();
    while !done.load(std::sync::atomic::Ordering::Relaxed) {
        if start.elapsed() > std::time::Duration::from_millis(200) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    drop(stream);

    let buf = samples.lock().unwrap();
    if buf.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = buf.iter().map(|&s| s * s).sum();
    (sum_sq / buf.len() as f32).sqrt()
}

/// Save configuration TOML to ~/.config/chamgei/config.toml
#[tauri::command]
fn save_config(config: String) -> Result<(), String> {
    let config_dir = dirs::home_dir()
        .map(|h| h.join(".config").join("chamgei"))
        .ok_or_else(|| "cannot determine home directory".to_string())?;
    fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
    let path = config_dir.join("config.toml");
    fs::write(&path, config.as_bytes()).map_err(|e| e.to_string())?;

    // Restrict permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        let _ = fs::set_permissions(&path, perms);
    }

    Ok(())
}

/// Read configuration TOML from ~/.config/chamgei/config.toml
#[tauri::command]
fn load_config() -> String {
    let path = dirs::home_dir()
        .map(|h| h.join(".config").join("chamgei").join("config.toml"))
        .unwrap_or_default();
    fs::read_to_string(path).unwrap_or_default()
}

/// List available Ollama models as a JSON array.
#[tauri::command]
fn list_ollama_models() -> String {
    let models = chamgei_llm::list_ollama_models();
    let items: Vec<serde_json::Value> = models
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name,
                "size": m.size,
                "size_human": chamgei_llm::format_model_size(m.size),
            })
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// Get current pipeline status as a string: "idle", "recording", "processing", "injecting", or "error: ...".
#[tauri::command]
fn get_pipeline_status(state: tauri::State<'_, PipelineState>) -> String {
    let status = state.status_manager.get_status();
    match status {
        chamgei_core::status::PipelineStatus::Idle => "idle".to_string(),
        chamgei_core::status::PipelineStatus::Recording => "recording".to_string(),
        chamgei_core::status::PipelineStatus::Processing => "processing".to_string(),
        chamgei_core::status::PipelineStatus::Injecting => "injecting".to_string(),
        chamgei_core::status::PipelineStatus::Error(msg) => format!("error: {msg}"),
    }
}

/// Download a Whisper model with progress events emitted to the frontend.
#[tauri::command]
async fn download_whisper_model(app: AppHandle, size: String) -> Result<(), String> {
    let (filename, url) = match size.to_lowercase().as_str() {
        "tiny" => (
            "ggml-tiny.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        ),
        "small" => (
            "ggml-small.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        ),
        "medium" => (
            "ggml-medium.en.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        ),
        "large" => (
            "ggml-large.bin",
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large.bin",
        ),
        _ => return Err(format!("unknown model size: {size}")),
    };

    let model_dir = resolve_model_dir();
    let model_path = model_dir.join(filename);

    if model_path.exists() {
        let _ = app.emit("whisper-download-progress", serde_json::json!({
            "percent": 100,
            "done": true,
        }));
        return Ok(());
    }

    fs::create_dir_all(&model_dir).map_err(|e| e.to_string())?;

    // Download in a blocking task so we don't block the async runtime.
    let app_clone = app.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        use std::io::{Read, Write};

        let response = reqwest::blocking::get(url).map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }

        let total = response.content_length().unwrap_or(0);
        let mut reader = std::io::BufReader::new(response);
        let mut file = fs::File::create(&model_path).map_err(|e| e.to_string())?;
        let mut downloaded: u64 = 0;
        let mut buf = [0u8; 8192];
        let mut last_percent: u64 = 0;

        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            downloaded += n as u64;

            if total > 0 {
                let percent = (downloaded * 100) / total;
                if percent != last_percent {
                    last_percent = percent;
                    let _ = app_clone.emit(
                        "whisper-download-progress",
                        serde_json::json!({
                            "percent": percent,
                            "downloaded": downloaded,
                            "total": total,
                            "done": false,
                        }),
                    );
                }
            }
        }

        let _ = app_clone.emit(
            "whisper-download-progress",
            serde_json::json!({
                "percent": 100,
                "downloaded": downloaded,
                "total": total,
                "done": true,
            }),
        );

        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Check if first-run onboarding is needed.
#[tauri::command]
fn needs_onboarding() -> bool {
    chamgei_core::onboarding::needs_onboarding()
}

// ---------------------------------------------------------------------------
// macOS permission helpers
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn check_mic_permission() -> bool {
    // Use osascript to query AVCaptureDevice authorization status.
    // Status 3 = authorized.
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "use framework \"AVFoundation\"",
            "-e",
            "set status to current application's AVCaptureDevice's authorizationStatusForMediaType:(current application's AVMediaTypeAudio)",
            "-e",
            "return status as integer",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            s == "3" // AVAuthorizationStatusAuthorized
        }
        _ => false,
    }
}

#[cfg(target_os = "macos")]
fn check_accessibility_permission() -> bool {
    // Link to ApplicationServices which provides AXIsProcessTrusted.
    // We use a small osascript call instead to avoid raw FFI.
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "use framework \"ApplicationServices\"",
            "-e",
            "return (current application's AXIsProcessTrusted()) as boolean",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
            s == "true"
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Keychain API-key commands
// ---------------------------------------------------------------------------

/// Save an API key to the macOS Keychain.
/// service = "com.chamgei.voice", account = provider name (e.g., "groq", "deepgram")
#[tauri::command]
fn save_api_key(provider: String, key: String) -> Result<(), String> {
    let entry = keyring::Entry::new("com.chamgei.voice", &provider)
        .map_err(|e| e.to_string())?;
    entry.set_password(&key).map_err(|e| e.to_string())?;
    Ok(())
}

/// Get a masked version of the stored API key (e.g., "gsk_...a8T2").
/// Never returns the full key to the frontend.
#[tauri::command]
fn get_api_key_masked(provider: String) -> Result<String, String> {
    let entry = keyring::Entry::new("com.chamgei.voice", &provider)
        .map_err(|e| e.to_string())?;
    match entry.get_password() {
        Ok(key) if key.len() > 8 => {
            let prefix = &key[..4];
            let suffix = &key[key.len()-4..];
            Ok(format!("{}...{}", prefix, suffix))
        }
        Ok(key) if !key.is_empty() => Ok("****".to_string()),
        Ok(_) => Err("no key stored".to_string()),
        Err(_) => Err("no key stored".to_string()),
    }
}

/// Get the full API key from Keychain (for internal pipeline use only).
/// This is called by the backend, NOT exposed to frontend JS.
fn get_api_key_full(provider: &str) -> Option<String> {
    let entry = keyring::Entry::new("com.chamgei.voice", provider).ok()?;
    entry.get_password().ok().filter(|k| !k.is_empty())
}

/// Delete an API key from the Keychain.
#[tauri::command]
fn delete_api_key(provider: String) -> Result<(), String> {
    let entry = keyring::Entry::new("com.chamgei.voice", &provider)
        .map_err(|e| e.to_string())?;
    entry.delete_credential().map_err(|e| e.to_string())?;
    Ok(())
}

/// Test if an API key works by making a lightweight API call.
#[tauri::command]
async fn test_api_key(provider: String) -> Result<String, String> {
    let key = get_api_key_full(&provider).ok_or("no key stored")?;
    let client = reqwest::Client::new();
    let (url, auth_header) = match provider.as_str() {
        "groq" => ("https://api.groq.com/openai/v1/models", format!("Bearer {}", key)),
        "deepgram" => ("https://api.deepgram.com/v1/projects", format!("Token {}", key)),
        "openai" => ("https://api.openai.com/v1/models", format!("Bearer {}", key)),
        "anthropic" => ("https://api.anthropic.com/v1/messages", format!("Bearer {}", key)),
        "cerebras" => ("https://api.cerebras.ai/v1/models", format!("Bearer {}", key)),
        _ => return Err("unknown provider".to_string()),
    };
    let resp = client.get(url)
        .header("Authorization", &auth_header)
        .send().await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() || resp.status().as_u16() == 400 {
        Ok("valid".to_string())
    } else if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
        Err("invalid key".to_string())
    } else {
        Ok(format!("status: {}", resp.status()))
    }
}

// ---------------------------------------------------------------------------
// Migration: move API keys from config.toml into Keychain
// ---------------------------------------------------------------------------

/// Migrate any API keys found in config.toml to the macOS Keychain, then
/// strip them from the file so secrets are no longer stored on disk.
fn migrate_keys_to_keychain() {
    let path = config_path();
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Simple key=value extraction (TOML files are flat enough for this).
    let key_fields: &[(&str, &str)] = &[
        ("groq_api_key", "groq"),
        ("deepgram_api_key", "deepgram"),
        ("cerebras_api_key", "cerebras"),
        ("stt_api_key", "stt"),
        ("llm_api_key", "llm"),
    ];

    let mut changed = false;
    let mut new_content = content.clone();

    for &(field, provider) in key_fields {
        // Match: field = "value" (with optional spaces)
        let pattern = format!("{} = \"", field);
        if let Some(start) = content.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(end) = content[value_start..].find('"') {
                let value = &content[value_start..value_start + end];
                if !value.is_empty() {
                    // Determine the actual provider for stt_api_key / llm_api_key
                    let actual_provider = match field {
                        "stt_api_key" => {
                            // Look for stt_engine in the config
                            extract_toml_value(&content, "stt_engine").unwrap_or_else(|| provider.to_string())
                        }
                        "llm_api_key" => {
                            extract_toml_value(&content, "llm_provider").unwrap_or_else(|| provider.to_string())
                        }
                        _ => provider.to_string(),
                    };

                    // Save to Keychain
                    if let Ok(entry) = keyring::Entry::new("com.chamgei.voice", &actual_provider) {
                        if entry.set_password(value).is_ok() {
                            tracing::info!("Migrated {} key to Keychain", actual_provider);
                            // Blank the value in config
                            let full_match = format!("{} = \"{}\"", field, value);
                            let replacement = format!("{} = \"\"", field);
                            new_content = new_content.replace(&full_match, &replacement);
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    if changed {
        let _ = fs::write(&path, new_content.as_bytes());
        tracing::info!("Config file updated — API keys removed from disk");
    }
}

/// Extract a simple string value from TOML content: key = "value"
fn extract_toml_value(content: &str, key: &str) -> Option<String> {
    let pattern = format!("{} = \"", key);
    let start = content.find(&pattern)? + pattern.len();
    let end = content[start..].find('"')?;
    let val = &content[start..start + end];
    if val.is_empty() { None } else { Some(val.to_string()) }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the Whisper model directory from env or defaults.
fn resolve_model_dir() -> PathBuf {
    std::env::var("CHAMGEI_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| {
                    h.join(".local")
                        .join("share")
                        .join("chamgei")
                        .join("models")
                })
                .unwrap_or_else(|| PathBuf::from("models"))
        })
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".config").join("chamgei").join("config.toml"))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(PipelineState {
            status_manager: chamgei_core::status::StatusManager::new(),
        })
        .setup(|app| {
            // --- Tray menu ---------------------------------------------------
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let history_item =
                MenuItem::with_id(app, "history", "History", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit Chamgei", true, None::<&str>)?;

            let menu = Menu::with_items(
                app,
                &[&settings_item, &history_item, &separator, &quit_item],
            )?;

            let app_handle = app.handle().clone();

            TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Chamgei — Voice Dictation")
                .on_menu_event(move |_app, event| match event.id.as_ref() {
                    "settings" | "history" => {
                        if let Some(window) = app_handle.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        std::process::exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { .. } = event {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                            } else {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // NOTE: The dictation pipeline is NOT auto-started in the GUI app.
            //
            // rdev::listen (used for global hotkeys) calls macOS
            // TSMGetInputSourceProperty which crashes when called from a
            // non-main-dispatch-queue thread. In the Tauri app, the setup
            // closure runs on the main thread but pipeline.run() spawns
            // rdev on a background thread, triggering the crash.
            //
            // The .dmg app provides: onboarding, settings, history.
            // The dictation pipeline runs via the `chamgei` CLI binary.
            //
            // TODO: Replace rdev with CGEventTap directly to fix this.
            // Migrate any API keys from config.toml into macOS Keychain
            migrate_keys_to_keychain();

            tracing::info!("Chamgei app started (dictation runs via CLI)");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_default_config,
            get_history,
            clear_history,
            copy_to_clipboard,
            check_permissions,
            open_mic_settings,
            open_accessibility_settings,
            get_audio_level,
            save_config,
            load_config,
            list_ollama_models,
            get_pipeline_status,
            download_whisper_model,
            needs_onboarding,
            save_api_key,
            get_api_key_masked,
            delete_api_key,
            test_api_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
