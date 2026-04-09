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
// Startup debug log - writes to Desktop so easy to find
// ============================================================
fn debug_log(msg: &str) {
    let path = dirs::desktop_dir()
        .or_else(|| dirs::home_dir())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("easyenv_debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&path) {
        let ts = Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "[{}] {}", ts, msg);
    }
}

// ============================================================
// State
// ============================================================
struct AppState {
    cancel_flags: Mutex<HashMap<u32, Arc<AtomicBool>>>,
}
impl Default for AppState {
    fn default() -> Self { Self { cancel_flags: Mutex::new(HashMap::new()) } }
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
    #[allow(dead_code)]
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
        .join("easyenv").join("logs");
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
    std::fs::read_to_string(config_dir().join("environments.json"))
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn save_config(envs: Vec<EnvConfig>) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&envs).map_err(|e| e.to_string())?;
    std::fs::write(config_dir().join("environments.json"), json).map_err(|e| e.to_string())
}

// ============================================================
// Install
// ============================================================
#[tauri::command]
async fn install_environments(
    app: AppHandle, state: tauri::State<'_, AppState>, envs: Vec<InstallPayload>,
) -> Result<String, String> {
    let log_path = create_log_file();
    for env in envs {
        let app2 = app.clone();
        let env_id = env.id;
        let log = log_path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut flags) = state.cancel_flags.lock() { flags.insert(env_id, cancel.clone()); }

        tauri::async_runtime::spawn(async move {
            write_log(&log, &format!("=== Start: id={} ===", env_id));
            let mut installed: Vec<PathBuf> = Vec::new();
            for (i, soft) in env.software.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    for p in &installed { if p.is_dir() { let _ = std::fs::remove_dir_all(p); } }
                    let _ = app2.emit("env-done", EnvDoneEvent { id: env_id, status: "cancelled".into(), message: "Rolled back".into() });
                    return;
                }
                let result = tasks::install_software(soft, &app2, env_id, i, &cancel, &log).await;
                if !result.success {
                    for p in &installed { if p.is_dir() { let _ = std::fs::remove_dir_all(p); } }
                    let _ = app2.emit("env-done", EnvDoneEvent { id: env_id, status: "failed".into(), message: result.message });
                    return;
                }
                if !soft.dir.is_empty() && !soft.folder.is_empty() {
                    installed.push(PathBuf::from(&soft.dir).join(&soft.folder));
                }
            }
            for (i, script) in env.scripts.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) { break; }
                let _ = app2.emit("install-progress", ProgressEvent {
                    id: env_id, software_index: env.software.len() + i, progress: 50.0,
                    status: "script".into(), message: "Running script".into(), operation: "executing".into(),
                });
                let r = tasks::run_script(script).await;
                let _ = app2.emit("install-progress", ProgressEvent {
                    id: env_id, software_index: env.software.len() + i, progress: 100.0,
                    status: if r.success {"done"} else {"error"}.into(), message: r.message, operation: "done".into(),
                });
            }
            let _ = app2.emit("env-done", EnvDoneEvent { id: env_id, status: "installed".into(), message: "Done".into() });
        });
    }
    Ok("started".into())
}

#[tauri::command]
async fn stop_install(state: tauri::State<'_, AppState>, env_id: u32) -> Result<(), String> {
    if let Ok(flags) = state.cancel_flags.lock() {
        if let Some(f) = flags.get(&env_id) { f.store(true, Ordering::Relaxed); }
    }
    Ok(())
}

// ============================================================
// Update
// ============================================================
#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let client = reqwest::Client::builder().user_agent("easyenv").build().map_err(|e| e.to_string())?;
    let resp = client.get("https://api.github.com/repos/yokeay/easy-env/releases/latest").send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(UpdateInfo { has_update: false, current: current.clone(), latest: current, download_url: String::new() });
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = body["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string();
    let url = body["html_url"].as_str().unwrap_or("").to_string();
    let gt = { let p = |s: &str| -> Vec<u32> { s.split('.').filter_map(|x| x.parse().ok()).collect() }; let (a,b) = (p(&latest), p(&current)); a > b };
    Ok(UpdateInfo { has_update: gt, current, latest, download_url: url })
}

// ============================================================
// Window
// ============================================================
#[tauri::command]
async fn win_minimize(w: tauri::WebviewWindow) -> Result<(), String> { w.minimize().map_err(|e| e.to_string()) }
#[tauri::command]
async fn win_toggle_maximize(w: tauri::WebviewWindow) -> Result<(), String> {
    if w.is_maximized().unwrap_or(false) { w.unmaximize().map_err(|e| e.to_string()) }
    else { w.maximize().map_err(|e| e.to_string()) }
}
#[tauri::command]
async fn win_close(w: tauri::WebviewWindow) -> Result<(), String> { w.hide().map_err(|e| e.to_string()) }
#[tauri::command]
async fn win_quit(app: AppHandle) -> Result<(), String> { app.exit(0); Ok(()) }

// ============================================================
// App
// ============================================================
fn main() {
    debug_log("========== easyenv starting ==========");

    debug_log("Step 1: Building tauri app...");

    let builder = tauri::Builder::default();
    debug_log("Step 2: Builder created");

    let builder = builder.plugin(tauri_plugin_shell::init());
    debug_log("Step 3: Shell plugin added");

    let builder = builder.plugin(tauri_plugin_dialog::init());
    debug_log("Step 4: Dialog plugin added");

    let builder = builder.manage(AppState::default());
    debug_log("Step 5: State managed");

    let builder = builder.setup(|app| {
        debug_log("Step 6: Setup running...");

        // Tray (optional, non-fatal)
        match setup_tray(app) {
            Ok(_) => debug_log("Step 7: Tray OK"),
            Err(e) => debug_log(&format!("Step 7: Tray FAILED (non-fatal): {}", e)),
        }

        // Window close -> hide
        match app.get_webview_window("main") {
            Some(window) => {
                debug_log("Step 8: Window found");
                let w2 = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w2.hide();
                    }
                });
            }
            None => debug_log("Step 8: Window NOT found"),
        }

        debug_log("Step 9: Setup complete");
        Ok(())
    });
    debug_log("Step 10: Setup handler registered");

    let builder = builder.invoke_handler(tauri::generate_handler![
        install_environments, stop_install, load_config, save_config,
        check_update, win_minimize, win_toggle_maximize, win_close, win_quit,
    ]);
    debug_log("Step 11: Invoke handlers registered");

    debug_log("Step 12: Calling .run()...");
    match builder.build(tauri::generate_context!()) {
        Ok(app) => {
            debug_log("Step 13: Build OK, running event loop...");
            app.run(|_, _| {});
        }
        Err(e) => {
            debug_log(&format!("Step 13: BUILD FAILED: {}", e));
        }
    }

    debug_log("Step 14: App exited");
}

fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    debug_log("  Tray: creating menu...");
    let show = MenuItemBuilder::with_id("show", "Show").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app).item(&show).item(&quit).build()?;
    debug_log("  Tray: menu built");

    let mut builder = TrayIconBuilder::new()
        .tooltip("easyenv")
        .menu(&menu)
        .on_menu_event(|app, event| {
            match event.id().as_ref() {
                "show" => { if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.set_focus(); } }
                "quit" => { app.exit(0); }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click { .. } = event {
                if let Some(w) = tray.app_handle().get_webview_window("main") {
                    let _ = w.show(); let _ = w.set_focus();
                }
            }
        });

    debug_log("  Tray: checking icon...");
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
        debug_log("  Tray: icon set");
    } else {
        debug_log("  Tray: no default icon, skipping");
    }

    debug_log("  Tray: building...");
    builder.build(app)?;
    debug_log("  Tray: built OK");
    Ok(())
}