#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tasks;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri::tray::TrayIconBuilder;

// ============================================================
// State
// ============================================================

struct AppState {
    /// Cancel flags keyed by environment id
    cancel_flags: Mutex<HashMap<u32, Arc<AtomicBool>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            cancel_flags: Mutex::new(HashMap::new()),
        }
    }
}

// ============================================================
// Shared types
// ============================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SoftwareItem {
    pub name: String,
    pub folder: String,
    pub dir: String,
    pub url: String,
    #[serde(rename = "envVar")]
    pub env_var: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct EnvConfig {
    id: u32,
    name: String,
    software: Vec<SoftwareItem>,
    status: String,
    progress: f64,
}

#[derive(Deserialize)]
struct InstallPayload {
    id: u32,
    name: String,
    software: Vec<SoftwareItem>,
}

#[derive(Serialize, Clone)]
pub struct ProgressEvent {
    pub id: u32,
    pub software_index: usize,
    pub progress: f64,
    pub status: String,
    pub message: String,
}

#[derive(Serialize, Clone)]
struct EnvDoneEvent {
    id: u32,
    status: String,
    message: String,
}

#[derive(Serialize)]
struct UpdateInfo {
    has_update: bool,
    current: String,
    latest: String,
    download_url: String,
}

// ============================================================
// Persistence
// ============================================================

fn config_dir() -> std::path::PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
        .join("easyenv");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn config_path() -> std::path::PathBuf {
    config_dir().join("environments.json")
}

#[tauri::command]
fn load_config() -> Vec<EnvConfig> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[tauri::command]
fn save_config(envs: Vec<EnvConfig>) -> Result<(), String> {
    let path = config_path();
    let json = serde_json::to_string_pretty(&envs)
        .map_err(|e| e.to_string())?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to save config: {}", e))?;
    Ok(())
}

// ============================================================
// Install
// ============================================================

#[tauri::command]
async fn install_environments(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    envs: Vec<InstallPayload>,
) -> Result<String, String> {
    for env in envs {
        let app2 = app.clone();
        let env_id = env.id;

        // Create cancel flag for this environment
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut flags = state.cancel_flags.lock().unwrap();
            flags.insert(env_id, cancel.clone());
        }

        tauri::async_runtime::spawn(async move {
            let mut installed_paths: Vec<std::path::PathBuf> = Vec::new();

            for (i, soft) in env.software.iter().enumerate() {
                // Check if cancelled before starting each item
                if cancel.load(Ordering::Relaxed) {
                    // Rollback: delete all files we installed
                    for path in &installed_paths {
                        if path.is_dir() {
                            let _ = std::fs::remove_dir_all(path);
                        } else if path.is_file() {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                    let _ = app2.emit("env-done", EnvDoneEvent {
                        id: env_id,
                        status: "cancelled".into(),
                        message: "Installation cancelled, rolled back".into(),
                    });
                    return;
                }

                let result = tasks::install_software(soft, &app2, env_id, i, &cancel).await;

                if cancel.load(Ordering::Relaxed) {
                    // Cancelled during install - rollback
                    for path in &installed_paths {
                        if path.is_dir() {
                            let _ = std::fs::remove_dir_all(path);
                        }
                    }
                    let _ = app2.emit("env-done", EnvDoneEvent {
                        id: env_id,
                        status: "cancelled".into(),
                        message: "Installation cancelled, rolled back".into(),
                    });
                    return;
                }

                if !result.success {
                    // Failed - rollback everything
                    for path in &installed_paths {
                        if path.is_dir() {
                            let _ = std::fs::remove_dir_all(path);
                        }
                    }
                    let _ = app2.emit("env-done", EnvDoneEvent {
                        id: env_id,
                        status: "failed".into(),
                        message: result.message,
                    });
                    return;
                }

                // Track installed path for potential rollback
                if !soft.dir.is_empty() && !soft.folder.is_empty() {
                    installed_paths.push(
                        std::path::PathBuf::from(&soft.dir).join(&soft.folder)
                    );
                }
            }

            let _ = app2.emit("env-done", EnvDoneEvent {
                id: env_id,
                status: "installed".into(),
                message: "All software installed successfully".into(),
            });
        });
    }
    Ok("started".into())
}

#[tauri::command]
async fn stop_install(
    state: tauri::State<'_, AppState>,
    env_id: u32,
) -> Result<(), String> {
    let flags = state.cancel_flags.lock().unwrap();
    if let Some(flag) = flags.get(&env_id) {
        flag.store(true, Ordering::Relaxed);
    }
    Ok(())
}

// ============================================================
// Version check
// ============================================================

#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();

    let client = reqwest::Client::builder()
        .user_agent("easyenv")
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get("https://api.github.com/repos/yokeay/easy-env/releases/latest")
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Ok(UpdateInfo {
            has_update: false,
            current: current.clone(),
            latest: current,
            download_url: String::new(),
        });
    }

    let body: serde_json::Value = resp.json().await
        .map_err(|e| format!("Parse error: {}", e))?;

    let latest = body["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();

    let download_url = body["html_url"]
        .as_str()
        .unwrap_or("https://github.com/yokeay/easy-env/releases")
        .to_string();

    let has_update = version_gt(&latest, &current);

    Ok(UpdateInfo {
        has_update,
        current,
        latest,
        download_url,
    })
}

/// Simple semver comparison: is `a` greater than `b`?
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    let va = parse(a);
    let vb = parse(b);
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x > y { return true; }
        if x < y { return false; }
    }
    false
}

// ============================================================
// App setup
// ============================================================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .setup(|app| {
            let icon = app.default_window_icon().cloned()
                .expect("no default window icon");
            let _tray = TrayIconBuilder::new()
                .icon(icon)
                .tooltip("easyenv")
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { .. } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            let window = app.get_webview_window("main").unwrap();
            let w2 = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = w2.hide();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            install_environments,
            stop_install,
            load_config,
            save_config,
            check_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run();
}
