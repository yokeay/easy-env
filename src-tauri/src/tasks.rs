use crate::SoftwareItem;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

pub struct StepResult {
    pub success: bool,
    pub message: String,
}

fn downloads_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Main install flow with cancellation support:
/// 1. Download to system Downloads folder
/// 2. Extract / run installer to target directory
/// 3. Clean up downloaded file
/// 4. Configure PATH if env_var is true
pub async fn install_software(
    soft: &SoftwareItem,
    app: &AppHandle,
    env_id: u32,
    idx: usize,
    cancel: &Arc<AtomicBool>,
) -> StepResult {
    let display = if soft.name.is_empty() { &soft.folder } else { &soft.name };

    if soft.url.is_empty() {
        emit_progress(app, env_id, idx, 100.0, "done",
            &format!("{} - no URL, skipped", display));
        return StepResult { success: true, message: format!("{} skipped", display) };
    }

    // ── Download ──
    if cancel.load(Ordering::Relaxed) {
        return StepResult { success: false, message: "Cancelled".into() };
    }

    emit_progress(app, env_id, idx, 5.0, "downloading",
        &format!("Downloading {}...", display));

    let filename = soft.url.rsplit('/').next().unwrap_or("download");
    let download_dir = downloads_dir();
    let _ = std::fs::create_dir_all(&download_dir);
    let download_path = download_dir.join(filename);

    if let Err(e) = download_file(&soft.url, &download_path).await {
        return StepResult { success: false, message: format!("Download failed: {}", e) };
    }

    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&download_path);
        return StepResult { success: false, message: "Cancelled".into() };
    }

    emit_progress(app, env_id, idx, 40.0, "downloading",
        &format!("{} downloaded", display));

    // ── Install ──
    let parent_dir = if soft.dir.is_empty() {
        dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| ".".into())
    } else {
        soft.dir.clone()
    };
    let install_dir = PathBuf::from(&parent_dir).join(&soft.folder);

    emit_progress(app, env_id, idx, 50.0, "extracting",
        &format!("Installing {}...", display));

    let is_archive = filename.ends_with(".zip")
        || filename.ends_with(".tar.gz") || filename.ends_with(".tgz")
        || filename.ends_with(".tar.xz") || filename.ends_with(".tar.bz2");

    if is_archive {
        let _ = std::fs::create_dir_all(&install_dir);
        let result = if filename.ends_with(".zip") {
            extract_zip(&download_path, &install_dir)
        } else {
            extract_tar(&download_path, &install_dir)
        };
        if let Err(e) = result {
            let _ = std::fs::remove_file(&download_path);
            return StepResult { success: false, message: format!("Extract failed: {}", e) };
        }
        let _ = std::fs::remove_file(&download_path);
    } else if filename.ends_with(".exe") || filename.ends_with(".msi") {
        if let Err(e) = run_installer(&download_path) {
            let _ = std::fs::remove_file(&download_path);
            return StepResult { success: false, message: format!("Installer failed: {}", e) };
        }
        let _ = std::fs::remove_file(&download_path);
    } else if filename.ends_with(".pkg") {
        match Command::new("sudo").args(["installer","-pkg"]).arg(&download_path).args(["-target","/"]).output() {
            Ok(o) if o.status.success() => { let _ = std::fs::remove_file(&download_path); }
            Ok(o) => {
                let _ = std::fs::remove_file(&download_path);
                return StepResult { success: false, message: format!("pkg failed: {}", String::from_utf8_lossy(&o.stderr)) };
            }
            Err(e) => {
                let _ = std::fs::remove_file(&download_path);
                return StepResult { success: false, message: e.to_string() };
            }
        }
    } else if filename.ends_with(".sh") {
        let _ = Command::new("bash").arg(&download_path).output();
        let _ = std::fs::remove_file(&download_path);
    } else if filename.ends_with(".dmg") {
        if let Err(e) = install_dmg(&download_path) {
            let _ = std::fs::remove_file(&download_path);
            return StepResult { success: false, message: format!("DMG failed: {}", e) };
        }
        let _ = std::fs::remove_file(&download_path);
    } else {
        let _ = std::fs::create_dir_all(&install_dir);
        let dest = install_dir.join(filename);
        let _ = std::fs::rename(&download_path, &dest);
    }

    if cancel.load(Ordering::Relaxed) {
        return StepResult { success: false, message: "Cancelled".into() };
    }

    // ── Configure PATH ──
    if soft.env_var {
        emit_progress(app, env_id, idx, 85.0, "configuring",
            &format!("Configuring PATH for {}...", display));
        configure_path(&install_dir);
    }

    emit_progress(app, env_id, idx, 100.0, "done",
        &format!("{} installed", display));

    StepResult { success: true, message: format!("{} installed", display) }
}

// ============================================================
// PATH configuration
// ============================================================

/// Find the right directory to add to PATH:
/// 1. install_dir/bin with executables → add bin
/// 2. install_dir root with executables → add root
/// 3. install_dir/*/bin one level deep (e.g. flutter_sdk/flutter/bin)
/// 4. Fallback: add install_dir itself
fn configure_path(install_dir: &Path) {
    let bin = install_dir.join("bin");
    if bin.is_dir() && has_executables(&bin) {
        let _ = add_to_system_path(&bin);
        return;
    }
    if has_executables(install_dir) {
        let _ = add_to_system_path(install_dir);
        return;
    }
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let sub_bin = p.join("bin");
                if sub_bin.is_dir() && has_executables(&sub_bin) {
                    let _ = add_to_system_path(&sub_bin);
                    return;
                }
                if has_executables(&p) {
                    let _ = add_to_system_path(&p);
                    return;
                }
            }
        }
    }
    let _ = add_to_system_path(install_dir);
}

fn has_executables(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() { continue; }
        if cfg!(target_os = "windows") {
            if let Some(ext) = p.extension() {
                let e = ext.to_string_lossy().to_lowercase();
                if e == "exe" || e == "cmd" || e == "bat" { return true; }
            }
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = p.metadata() {
                    if m.permissions().mode() & 0o111 != 0 { return true; }
                }
            }
        }
    }
    false
}

fn add_to_system_path(dir: &Path) -> Result<(), String> {
    let dir_str = dir.to_string_lossy().to_string();
    if cfg!(target_os = "windows") {
        let cmd = format!(
            "$p=[Environment]::GetEnvironmentVariable('Path','User');\
             if($p -and $p -notlike '*{0}*'){{[Environment]::SetEnvironmentVariable('Path',\"$p;{0}\",'User')}}\
             elseif(-not $p){{[Environment]::SetEnvironmentVariable('Path','{0}','User')}}",
            dir_str
        );
        let _ = Command::new("powershell").args(["-NoProfile","-Command",&cmd]).output();
        Ok(())
    } else {
        let home = dirs::home_dir().unwrap_or_default();
        let line = format!("export PATH=\"{}:$PATH\"", dir_str);
        let rcs: Vec<PathBuf> = if cfg!(target_os = "macos") {
            vec![home.join(".zshrc"), home.join(".bash_profile")]
        } else {
            vec![home.join(".bashrc"), home.join(".zshrc"), home.join(".profile")]
        };
        for rc in rcs {
            let should = rc.exists() || rc.file_name().map(|n| n == ".zshrc" || n == ".bashrc").unwrap_or(false);
            if should {
                let content = std::fs::read_to_string(&rc).unwrap_or_default();
                if !content.contains(&dir_str) {
                    let _ = std::fs::OpenOptions::new().append(true).create(true).open(&rc)
                        .and_then(|mut f| {
                            use std::io::Write;
                            writeln!(f, "\n# Added by easyenv")?;
                            writeln!(f, "{}", line)
                        });
                }
            }
        }
        Ok(())
    }
}

// ============================================================
// Download / Extract / Install helpers
// ============================================================

async fn download_file(url: &str, path: &Path) -> Result<(), String> {
    let resp = reqwest::get(url).await.map_err(|e| format!("Request: {}", e))?;
    if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status())); }
    let bytes = resp.bytes().await.map_err(|e| format!("Read: {}", e))?;
    std::fs::write(path, &bytes).map_err(|e| format!("Write: {}", e))
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let out = if cfg!(target_os="windows") {
        Command::new("powershell").args(["-NoProfile","-Command",
            &format!("Expand-Archive -Path '{}' -DestinationPath '{}' -Force", archive.display(), dest.display())
        ]).output()
    } else {
        Command::new("unzip").args(["-o"]).arg(archive).arg("-d").arg(dest).output()
    };
    match out { Ok(o) if o.status.success()=>Ok(()), Ok(o)=>Err(String::from_utf8_lossy(&o.stderr).into()), Err(e)=>Err(e.to_string()) }
}

fn extract_tar(archive: &Path, dest: &Path) -> Result<(), String> {
    match Command::new("tar").arg("-xf").arg(archive).arg("-C").arg(dest).output() {
        Ok(o) if o.status.success()=>Ok(()), Ok(o)=>Err(String::from_utf8_lossy(&o.stderr).into()), Err(e)=>Err(e.to_string())
    }
}

fn run_installer(path: &Path) -> Result<(), String> {
    let s = path.to_string_lossy();
    let out = if s.ends_with(".msi") {
        Command::new("msiexec").args(["/i",&s,"/quiet","/norestart"]).output()
    } else {
        Command::new(&*s).arg("/S").output()
    };
    match out { Ok(o) if o.status.success()=>Ok(()), Ok(o)=>Err(String::from_utf8_lossy(&o.stderr).into()), Err(e)=>Err(e.to_string()) }
}

fn install_dmg(dmg: &Path) -> Result<(), String> {
    let mount = Command::new("hdiutil").args(["attach",&dmg.to_string_lossy(),"-nobrowse","-quiet"]).output().map_err(|e|e.to_string())?;
    if !mount.status.success() { return Err("Mount failed".into()); }
    let stdout = String::from_utf8_lossy(&mount.stdout);
    let mp = stdout.lines().last().and_then(|l|l.split('\t').last()).map(|s|s.trim().to_string()).ok_or("No mount point")?;
    if let Ok(entries) = std::fs::read_dir(&mp) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().ends_with(".app") {
                let _ = Command::new("cp").arg("-R").arg(e.path()).arg("/Applications/").output();
            }
        }
    }
    let _ = Command::new("hdiutil").args(["detach",&mp,"-quiet"]).output();
    Ok(())
}

fn emit_progress(app: &AppHandle, id: u32, idx: usize, progress: f64, status: &str, msg: &str) {
    let _ = app.emit("install-progress", crate::ProgressEvent {
        id, software_index: idx, progress, status: status.into(), message: msg.into(),
    });
}
