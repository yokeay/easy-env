use crate::{SoftwareItem, ScriptItem, write_log};
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
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads"))).unwrap_or_else(|| PathBuf::from("."))
}

pub async fn install_software(
    soft: &SoftwareItem, app: &AppHandle, env_id: u32, idx: usize,
    cancel: &Arc<AtomicBool>, log: &PathBuf,
) -> StepResult {
    let display = if soft.name.is_empty() { &soft.folder } else { &soft.name };

    if soft.url.is_empty() {
        emit(app, env_id, idx, 100.0, "done", &format!("{} skipped (no source)", display), "skipped");
        write_log(log, &format!("[{}] Skipped - no URL", display));
        return StepResult { success: true, message: format!("{} skipped", display) };
    }

    // ── Determine source: local file or download ──
    let file_path: PathBuf;
    let filename: String;
    let need_cleanup: bool;

    if soft.is_local {
        // Local file: use directly
        file_path = PathBuf::from(&soft.url);
        filename = file_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or("file".into());
        need_cleanup = false;
        emit(app, env_id, idx, 10.0, "downloading", &format!("Using local file: {}", filename), "reading local file");
        write_log(log, &format!("[{}] Using local file: {}", display, soft.url));
        if !file_path.exists() {
            let msg = format!("Local file not found: {}", soft.url);
            write_log(log, &msg);
            emit(app, env_id, idx, 0.0, "error", &msg, "error");
            return StepResult { success: false, message: msg };
        }
    } else {
        // Download from URL
        if cancel.load(Ordering::Relaxed) { return StepResult { success: false, message: "Cancelled".into() }; }

        filename = soft.url.rsplit('/').next().unwrap_or("download").to_string();
        let dl_dir = downloads_dir();
        let _ = std::fs::create_dir_all(&dl_dir);
        file_path = dl_dir.join(&filename);
        need_cleanup = true;

        emit(app, env_id, idx, 5.0, "downloading", &format!("Downloading {}...", display), "downloading");
        write_log(log, &format!("[{}] Downloading from {}", display, soft.url));

        if let Err(e) = download_file(&soft.url, &file_path).await {
            let msg = format!("Download failed: {}", e);
            write_log(log, &msg);
            emit(app, env_id, idx, 0.0, "error", &msg, "download error");
            return StepResult { success: false, message: msg };
        }

        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&file_path);
            return StepResult { success: false, message: "Cancelled".into() };
        }
        write_log(log, &format!("[{}] Downloaded to {}", display, file_path.display()));
    }

    emit(app, env_id, idx, 40.0, "extracting", &format!("Installing {}...", display), "preparing install");

    // ── Install ──
    let parent = if soft.dir.is_empty() { dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or(".".into()) } else { soft.dir.clone() };
    let install_dir = PathBuf::from(&parent).join(&soft.folder);

    let is_archive = filename.ends_with(".zip") || filename.ends_with(".tar.gz") || filename.ends_with(".tgz") || filename.ends_with(".tar.xz");

    if is_archive {
        let _ = std::fs::create_dir_all(&install_dir);
        emit(app, env_id, idx, 55.0, "extracting", &format!("Extracting {}...", display), "extracting archive");
        write_log(log, &format!("[{}] Extracting to {}", display, install_dir.display()));
        let r = if filename.ends_with(".zip") { extract_zip(&file_path, &install_dir) } else { extract_tar(&file_path, &install_dir) };
        if let Err(e) = r {
            if need_cleanup { let _ = std::fs::remove_file(&file_path); }
            let msg = format!("Extract failed: {}", e);
            write_log(log, &msg);
            return StepResult { success: false, message: msg };
        }
        if need_cleanup { let _ = std::fs::remove_file(&file_path); }
    } else if filename.ends_with(".exe") || filename.ends_with(".msi") {
        emit(app, env_id, idx, 55.0, "extracting", &format!("Running installer {}...", display), "running installer");
        write_log(log, &format!("[{}] Running installer", display));
        if let Err(e) = run_installer(&file_path) {
            if need_cleanup { let _ = std::fs::remove_file(&file_path); }
            return StepResult { success: false, message: format!("Installer failed: {}", e) };
        }
        if need_cleanup { let _ = std::fs::remove_file(&file_path); }
    } else if filename.ends_with(".pkg") {
        write_log(log, &format!("[{}] Running .pkg installer", display));
        match Command::new("sudo").args(["installer","-pkg"]).arg(&file_path).args(["-target","/"]).output() {
            Ok(o) if o.status.success() => { if need_cleanup { let _ = std::fs::remove_file(&file_path); } }
            Ok(o) => { if need_cleanup { let _ = std::fs::remove_file(&file_path); } return StepResult { success: false, message: String::from_utf8_lossy(&o.stderr).into() }; }
            Err(e) => { if need_cleanup { let _ = std::fs::remove_file(&file_path); } return StepResult { success: false, message: e.to_string() }; }
        }
    } else if filename.ends_with(".sh") {
        write_log(log, &format!("[{}] Running shell script", display));
        let _ = Command::new("bash").arg(&file_path).output();
        if need_cleanup { let _ = std::fs::remove_file(&file_path); }
    } else if filename.ends_with(".dmg") {
        write_log(log, &format!("[{}] Installing DMG", display));
        if let Err(e) = install_dmg(&file_path) {
            if need_cleanup { let _ = std::fs::remove_file(&file_path); }
            return StepResult { success: false, message: format!("DMG: {}", e) };
        }
        if need_cleanup { let _ = std::fs::remove_file(&file_path); }
    } else {
        let _ = std::fs::create_dir_all(&install_dir);
        let dest = install_dir.join(&filename);
        let _ = std::fs::copy(&file_path, &dest);
        if need_cleanup { let _ = std::fs::remove_file(&file_path); }
    }

    if cancel.load(Ordering::Relaxed) { return StepResult { success: false, message: "Cancelled".into() }; }

    // ── PATH ──
    if soft.env_var {
        emit(app, env_id, idx, 85.0, "configuring", &format!("Configuring PATH for {}...", display), "configuring PATH");
        write_log(log, &format!("[{}] Configuring PATH", display));
        configure_path(&install_dir);
    }

    emit(app, env_id, idx, 100.0, "done", &format!("{} installed", display), "complete");
    write_log(log, &format!("[{}] Installed successfully", display));
    StepResult { success: true, message: format!("{} installed", display) }
}

// ============================================================
// Script execution
// ============================================================
pub async fn run_script(script: &ScriptItem) -> StepResult {
    let output = if !script.command.is_empty() {
        if cfg!(target_os = "windows") {
            Command::new("cmd").args(["/C", &script.command]).output()
        } else {
            Command::new("bash").args(["-c", &script.command]).output()
        }
    } else if !script.file_path.is_empty() {
        let p = &script.file_path;
        if cfg!(target_os = "windows") {
            if p.ends_with(".ps1") { Command::new("powershell").args(["-NoProfile","-File",p]).output() }
            else { Command::new("cmd").args(["/C",p]).output() }
        } else {
            Command::new("bash").arg(p).output()
        }
    } else {
        return StepResult { success: true, message: "Empty script, skipped".into() };
    };

    match output {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            StepResult {
                success: o.status.success(),
                message: if o.status.success() { out.chars().take(200).collect() } else { err.chars().take(200).collect() },
            }
        }
        Err(e) => StepResult { success: false, message: e.to_string() },
    }
}

// ============================================================
// PATH
// ============================================================
fn configure_path(install_dir: &Path) {
    let bin = install_dir.join("bin");
    if bin.is_dir() && has_exe(&bin) { let _ = add_path(&bin); return; }
    if has_exe(install_dir) { let _ = add_path(install_dir); return; }
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let sb = p.join("bin");
                if sb.is_dir() && has_exe(&sb) { let _ = add_path(&sb); return; }
                if has_exe(&p) { let _ = add_path(&p); return; }
            }
        }
    }
    let _ = add_path(install_dir);
}

fn has_exe(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for e in entries.flatten() {
        let p = e.path(); if !p.is_file() { continue; }
        if cfg!(target_os="windows") {
            if let Some(ext) = p.extension() {
                let e = ext.to_string_lossy().to_lowercase();
                if e=="exe"||e=="cmd"||e=="bat" { return true; }
            }
        } else {
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = p.metadata() { if m.permissions().mode() & 0o111 != 0 { return true; } }
            }
        }
    }
    false
}

fn add_path(dir: &Path) -> Result<(),String> {
    let s = dir.to_string_lossy().to_string();
    if cfg!(target_os="windows") {
        let cmd = format!("$p=[Environment]::GetEnvironmentVariable('Path','User');if($p -and $p -notlike '*{0}*'){{[Environment]::SetEnvironmentVariable('Path',\"$p;{0}\",'User')}}elseif(-not $p){{[Environment]::SetEnvironmentVariable('Path','{0}','User')}}", s);
        let _ = Command::new("powershell").args(["-NoProfile","-Command",&cmd]).output();
    } else {
        let home = dirs::home_dir().unwrap_or_default();
        let line = format!("export PATH=\"{}:$PATH\"", s);
        let rcs: Vec<PathBuf> = if cfg!(target_os="macos") { vec![home.join(".zshrc"),home.join(".bash_profile")] }
            else { vec![home.join(".bashrc"),home.join(".zshrc"),home.join(".profile")] };
        for rc in rcs {
            let ok = rc.exists() || rc.file_name().map(|n| n==".zshrc"||n==".bashrc").unwrap_or(false);
            if ok {
                let c = std::fs::read_to_string(&rc).unwrap_or_default();
                if !c.contains(&s) {
                    let _ = std::fs::OpenOptions::new().append(true).create(true).open(&rc)
                        .and_then(|mut f| { use std::io::Write; writeln!(f, "\n# Added by easyenv\n{}", line) });
                }
            }
        }
    }
    Ok(())
}

// ============================================================
// Helpers
// ============================================================
async fn download_file(url: &str, path: &Path) -> Result<(),String> {
    let r = reqwest::get(url).await.map_err(|e| format!("Request: {}",e))?;
    if !r.status().is_success() { return Err(format!("HTTP {}",r.status())); }
    let b = r.bytes().await.map_err(|e| format!("Read: {}",e))?;
    std::fs::write(path, &b).map_err(|e| format!("Write: {}",e))
}

fn extract_zip(a: &Path, d: &Path) -> Result<(),String> {
    let o = if cfg!(target_os="windows") {
        Command::new("powershell").args(["-NoProfile","-Command",&format!("Expand-Archive -Path '{}' -DestinationPath '{}' -Force",a.display(),d.display())]).output()
    } else { Command::new("unzip").args(["-o"]).arg(a).arg("-d").arg(d).output() };
    match o { Ok(o) if o.status.success()=>Ok(()), Ok(o)=>Err(String::from_utf8_lossy(&o.stderr).into()), Err(e)=>Err(e.to_string()) }
}

fn extract_tar(a: &Path, d: &Path) -> Result<(),String> {
    match Command::new("tar").arg("-xf").arg(a).arg("-C").arg(d).output() {
        Ok(o) if o.status.success()=>Ok(()), Ok(o)=>Err(String::from_utf8_lossy(&o.stderr).into()), Err(e)=>Err(e.to_string())
    }
}

fn run_installer(p: &Path) -> Result<(),String> {
    let s = p.to_string_lossy();
    let o = if s.ends_with(".msi") { Command::new("msiexec").args(["/i",&s,"/quiet","/norestart"]).output() }
        else { Command::new(&*s).arg("/S").output() };
    match o { Ok(o) if o.status.success()=>Ok(()), Ok(o)=>Err(String::from_utf8_lossy(&o.stderr).into()), Err(e)=>Err(e.to_string()) }
}

fn install_dmg(dmg: &Path) -> Result<(),String> {
    let m = Command::new("hdiutil").args(["attach",&dmg.to_string_lossy(),"-nobrowse","-quiet"]).output().map_err(|e|e.to_string())?;
    if !m.status.success() { return Err("Mount failed".into()); }
    let so = String::from_utf8_lossy(&m.stdout);
    let mp = so.lines().last().and_then(|l|l.split('\t').last()).map(|s|s.trim().to_string()).ok_or("No mount")?;
    if let Ok(es) = std::fs::read_dir(&mp) {
        for e in es.flatten() { if e.file_name().to_string_lossy().ends_with(".app") {
            let _ = Command::new("cp").arg("-R").arg(e.path()).arg("/Applications/").output();
        }}
    }
    let _ = Command::new("hdiutil").args(["detach",&mp,"-quiet"]).output();
    Ok(())
}

fn emit(app: &AppHandle, id: u32, idx: usize, progress: f64, status: &str, msg: &str, op: &str) {
    let _ = app.emit("install-progress", crate::ProgressEvent {
        id, software_index: idx, progress, status: status.into(), message: msg.into(), operation: op.into(),
    });
}
