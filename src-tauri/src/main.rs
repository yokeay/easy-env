#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tasks;

use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

// ============================================================
// State
// ============================================================
struct AppState {
    cancel_flags: Mutex<HashMap<u32, Arc<AtomicBool>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self { cancel_flags: Mutex::new(HashMap::new()) }
    }
}

// ============================================================
// Types
// ============================================================
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SoftwareItem {
    pub name: String,
    pub folder: String,
    pub dir: String,
    pub url: String,
    #[serde(rename = "envVar")]
    pub env_var: bool,
    #[serde(default)]
    pub is_local: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ScriptItem {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub file_path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct EnvConfig {
    id: u32,
    name: String,
    software: Vec<SoftwareItem>,
    #[serde(default)]
    scripts: Vec<ScriptItem>,
    status: String,
    progress: f64,
}

#[derive(Deserialize)]
struct InstallPayload {
    id: u32,
    name: String,
    software: Vec<SoftwareItem>,
    #[serde(default)]
    scripts: Vec<ScriptItem>,
}

#[derive(Serialize, Clone)]
pub struct ProgressEvent {
    pub id: u32,
    pub software_index: usize,
    pub progress: f64,
    pub status: String,
    pub message: String,
    pub operation: String,
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
// Logging
// ============================================================
fn log_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
        .join("easyenv")
        .join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn create_log_file() -> PathBuf {
    let name = Local::now().format("%Y%m%d_%H%M%S_log.txt").to_string();
    log_dir().join(name)
}

pub fn write_log(log_path: &PathBuf, msg: &str) {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(log_path) {
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

// ============================================================
// Persistence
// ============================================================
fn config_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
        .join("easyenv");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[tauri::command]
fn load_config() -> Vec<EnvConfig> {
    let path = config_dir().join("environments.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn save_config(envs: Vec<EnvConfig>) -> Result<(), String> {
    let path = config_dir().join("environments.json");
    let json = serde_json::to_string_pretty(&envs).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
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
    let log_path = create_log_file();

    for env in envs {
        let app2 = app.clone();
        let env_id = env.id;
        let log = log_path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            if let Ok(mut flags) = state.cancel_flags.lock() {
                flags.insert(env_id, cancel.clone());
            }
        }

        tauri::async_runtime::spawn(async move {
            write_log(&log, &format!("=== Start: {} ===", env.name));
            let mut installed_paths: Vec<PathBuf> = Vec::new();

            for (i, soft) in env.software.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    write_log(&log, "Cancelled, rolling back");
                    for p in &installed_paths {
                        if p.is_dir() { let _ = std::fs::remove_dir_all(p); }
                    }
                    let _ = app2.emit("env-done", EnvDoneEvent {
                        id: env_id, status: "cancelled".into(), message: "Rolled back".into(),
                    });
                    return;
                }

                let result = tasks::install_software(soft, &app2, env_id, i, &cancel, &log).await;

                if cancel.load(Ordering::Relaxed) {
                    for p in &installed_paths { if p.is_dir() { let _ = std::fs::remove_dir_all(p); } }
                    let _ = app2.emit("env-done", EnvDoneEvent {
                        id: env_id, status: "cancelled".into(), message: "Rolled back".into(),
                    });
                    return;
                }

                if !result.success {
                    write_log(&log, &format!("FAILED: {}", result.message));
                    for p in &installed_paths { if p.is_dir() { let _ = std::fs::remove_dir_all(p); } }
                    let _ = app2.emit("env-done", EnvDoneEvent {
                        id: env_id, status: "failed".into(), message: result.message,
                    });
                    return;
                }

                if !soft.dir.is_empty() && !soft.folder.is_empty() {
                    installed_paths.push(PathBuf::from(&soft.dir).join(&soft.folder));
                }
            }

            // Execute scripts
            for (i, script) in env.scripts.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) { break; }
                let desc = if !script.command.is_empty() {
                    format!("Script: {}", &script.command[..script.command.len().min(60)])
                } else {
                    format!("Script file: {}", &script.file_path)
                };
                write_log(&log, &desc);
                let _ = app2.emit("install-progress", ProgressEvent {
                    id: env_id, software_index: env.software.len() + i, progress: 50.0,
                    status: "script".into(), message: desc.clone(), operation: "executing script".into(),
                });
                let result = tasks::run_script(script).await;
                write_log(&log, &format!("Script: {}", if result.success { "OK" } else { &result.message }));
                let _ = app2.emit("install-progress", ProgressEvent {
                    id: env_id, software_index: env.software.len() + i, progress: 100.0,
                    status: if result.success { "done" } else { "error" }.into(),
                    message: result.message, operation: "script done".into(),
                });
                if !result.success { break; }
            }

            write_log(&log, &format!("=== Done: {} ===", env.name));
            let _ = app2.emit("env-done", EnvDoneEvent {
                id: env_id, status: "installed".into(), message: "Complete".into(),
            });
        });
    }
    Ok("started".into())
}

#[tauri::command]
async fn stop_install(state: tauri::State<'_, AppState>, env_id: u32) -> Result<(), String> {
    if let Ok(flags) = state.cancel_flags.lock() {
        if let Some(f) = flags.get(&env_id) {
            f.store(true, Ordering::Relaxed);
        }
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
        .map_err(|e| format!("Network: {}", e))?;
    if !resp.status().is_success() {
        return Ok(UpdateInfo {
            has_update: false, current: current.clone(), latest: current, download_url: String::new(),
        });
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = body["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string();
    let url = body["html_url"].as_str().unwrap_or("https://github.com/yokeay/easy-env/releases").to_string();
    let has = version_gt(&latest, &current);
    Ok(UpdateInfo { has_update: has, current, latest, download_url: url })
}

fn version_gt(a: &str, b: &str) -> bool {
    let p = |s: &str| -> Vec<u32> { s.split('.').filter_map(|x| x.parse().ok()).collect() };
    let (va, vb) = (p(a), p(b));
    for i in 0..va.len().max(vb.len()) {
        let (x, y) = (va.get(i).copied().unwrap_or(0), vb.get(i).copied().unwrap_or(0));
        if x > y { return true; }
        if x < y { return false; }
    }
    false
}

// ============================================================
// Window control
// ============================================================
#[tauri::command]
async fn win_minimize(window: tauri::WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
async fn win_toggle_maximize(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.is_maximized().unwrap_or(false) {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn win_close(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[tauri::command]
async fn win_quit(app: AppHandle) -> Result<(), String> {
    app.exit(0);
    Ok(())
}

// ============================================================
// App
// ============================================================
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .setup(|app| {
            // Try to set up tray - non-fatal if it fails
            setup_tray(app).unwrap_or_else(|e| {
                eprintln!("Tray setup failed (non-fatal): {}", e);
            });

            // Window close -> hide
            if let Some(window) = app.get_webview_window("main") {
                let w2 = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w2.hide();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            install_environments, stop_install, load_config, save_config,
            check_update, win_minimize, win_toggle_maximize, win_close, win_quit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    let show = MenuItemBuilder::with_id("show", "显示界面 / Show").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出 / Quit").build(app)?;
    let menu = MenuBuilder::new(app).item(&show).item(&quit).build()?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("easyenv")
        .menu(&menu)
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "show" => {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
                "quit" => { app.exit(0); }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click { .. } = event {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        });

    // Try to set icon, but don't fail if unavailable
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

fn main() {
    run();
}
