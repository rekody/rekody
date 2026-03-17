use std::fs;
use std::path::PathBuf;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Build the tray menu
            let quit_item = MenuItem::with_id(app, "quit", "Quit Chamgei", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit_item])?;

            // Build the tray icon
            TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Chamgei — Voice Dictation")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_default_config, get_history, clear_history, copy_to_clipboard])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
