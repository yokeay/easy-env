mod tasks;

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri::tray::TrayIconBuilder;

#[derive(Default)]
struct AppState {
    running: Mutex<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SoftwareItem {
    pub name: String,
    pub folder: String,
    pub dir: String,
    pub url: String,
    #[serde(rename = "envVar")]
    pub env_var: bool,
}

#[derive(Deserialize)]
struct InstallPayload {
    id: u32,
    name: String,
    software: Vec<SoftwareItem>,
}

#[derive(Serialize, Clone)]
struct ProgressEvent {
    id: u32,
    software_index: usize,
    progress: f64,
    status: String,
    message: String,
}

#[derive(Serialize, Clone)]
struct EnvDoneEvent {
    id: u32,
    status: String,
}

#[tauri::command]
async fn install_environments(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    envs: Vec<InstallPayload>,
) -> Result<String, String> {
    for env in envs {
        let app2 = app.clone();
        let env_id = env.id;
        let env_name = env.name.clone();

        {
            let mut running = state.running.lock().unwrap();
            running.push(env_name.clone());
        }

        tauri::async_runtime::spawn(async move {
            let total = env.software.len();
            for (i, soft) in env.software.iter().enumerate() {
                let display = if soft.name.is_empty() {
                    &soft.folder
                } else {
                    &soft.name
                };

                let _ = app2.emit(
                    "install-progress",
                    ProgressEvent {
                        id: env_id,
                        software_index: i,
                        progress: 0.0,
                        status: "downloading".into(),
                        message: format!("Downloading {}", display),
                    },
                );

                let result = tasks::install_software(soft, &app2, env_id, i).await;

                if !result.success {
                    let _ = app2.emit(
                        "install-progress",
                        ProgressEvent {
                            id: env_id,
                            software_index: i,
                            progress: 0.0,
                            status: "error".into(),
                            message: result.message,
                        },
                    );
                    let _ = app2.emit(
                        "env-done",
                        EnvDoneEvent {
                            id: env_id,
                            status: "failed".into(),
                        },
                    );
                    return;
                }

                // Configure env var if needed
                if soft.env_var && !soft.dir.is_empty() && !soft.folder.is_empty() {
                    let path = format!("{}/{}", soft.dir, soft.folder);
                    let _ = tasks::add_to_path(&path);
                }

                let _ = app2.emit(
                    "install-progress",
                    ProgressEvent {
                        id: env_id,
                        software_index: i,
                        progress: 100.0,
                        status: "done".into(),
                        message: format!("{} installed", display),
                    },
                );
            }

            let _ = app2.emit(
                "env-done",
                EnvDoneEvent {
                    id: env_id,
                    status: "installed".into(),
                },
            );
        });
    }
    Ok("started".into())
}

#[tauri::command]
async fn stop_install(
    state: tauri::State<'_, AppState>,
    env_name: String,
) -> Result<(), String> {
    let mut running = state.running.lock().unwrap();
    running.retain(|n| n != &env_name);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::default())
        .setup(|app| {
            let _tray = TrayIconBuilder::new()
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
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![install_environments, stop_install])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run();
}
