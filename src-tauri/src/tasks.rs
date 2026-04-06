use crate::SoftwareItem;
use std::path::Path;
use std::process::Command;
use tauri::{AppHandle, Emitter};

pub struct StepResult {
    pub success: bool,
    pub message: String,
}

pub async fn install_software(
    soft: &SoftwareItem,
    app: &AppHandle,
    env_id: u32,
    idx: usize,
) -> StepResult {
    let display = if soft.name.is_empty() {
        &soft.folder
    } else {
        &soft.name
    };

    // Skip if no URL provided (sub-tools installed via parent)
    if soft.url.is_empty() {
        return StepResult {
            success: true,
            message: format!("{} - no URL, skipped", display),
        };
    }

    let target_dir = if soft.dir.is_empty() {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    } else {
        soft.dir.clone()
    };

    // Ensure target directory exists
    let _ = std::fs::create_dir_all(&target_dir);

    // Download
    let _ = app.emit(
        "install-progress",
        crate::ProgressEvent {
            id: env_id,
            software_index: idx,
            progress: 10.0,
            status: "downloading".into(),
            message: format!("Downloading {}...", display),
        },
    );

    let url = &soft.url;
    let filename = url.rsplit('/').next().unwrap_or("download");
    let download_path = format!("{}/{}", target_dir, filename);

    match download_file(url, &download_path).await {
        Ok(_) => {}
        Err(e) => {
            return StepResult {
                success: false,
                message: format!("Download failed: {}", e),
            };
        }
    }

    let _ = app.emit(
        "install-progress",
        crate::ProgressEvent {
            id: env_id,
            software_index: idx,
            progress: 60.0,
            status: "extracting".into(),
            message: format!("Extracting {}...", display),
        },
    );

    // Extract if archive
    if filename.ends_with(".zip")
        || filename.ends_with(".tar.gz")
        || filename.ends_with(".tgz")
    {
        let extract_to = format!("{}/{}", target_dir, soft.folder);
        let _ = std::fs::create_dir_all(&extract_to);

        let result = if filename.ends_with(".zip") {
            extract_zip(&download_path, &extract_to)
        } else {
            extract_tar(&download_path, &extract_to)
        };

        if let Err(e) = result {
            return StepResult {
                success: false,
                message: format!("Extract failed: {}", e),
            };
        }
        // Clean up archive
        let _ = std::fs::remove_file(&download_path);
    }

    let _ = app.emit(
        "install-progress",
        crate::ProgressEvent {
            id: env_id,
            software_index: idx,
            progress: 90.0,
            status: "configuring".into(),
            message: format!("Configuring {}...", display),
        },
    );

    // Run installer if it's an exe/msi/pkg/sh
    if filename.ends_with(".exe") || filename.ends_with(".msi") {
        let _ = run_installer(&download_path);
    } else if filename.ends_with(".sh") {
        let _ = Command::new("sh").arg(&download_path).output();
    }

    StepResult {
        success: true,
        message: format!("{} installed successfully", display),
    }
}

async fn download_file(url: &str, path: &str) -> Result<(), String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Read failed: {}", e))?;

    std::fs::write(path, &bytes).map_err(|e| format!("Write failed: {}", e))?;
    Ok(())
}

fn extract_zip(archive: &str, dest: &str) -> Result<(), String> {
    let os = std::env::consts::OS;
    let output = if os == "windows" {
        Command::new("powershell")
            .args([
                "-Command",
                &format!("Expand-Archive -Path '{}' -DestinationPath '{}' -Force", archive, dest),
            ])
            .output()
    } else {
        Command::new("unzip")
            .args(["-o", archive, "-d", dest])
            .output()
    };
    match output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn extract_tar(archive: &str, dest: &str) -> Result<(), String> {
    let output = Command::new("tar")
        .args(["-xzf", archive, "-C", dest])
        .output();
    match output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn run_installer(path: &str) -> Result<(), String> {
    let output = if path.ends_with(".msi") {
        Command::new("msiexec")
            .args(["/i", path, "/quiet", "/norestart"])
            .output()
    } else {
        Command::new(path).arg("/S").output()
    };
    match output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).to_string()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn add_to_path(dir: &str) -> Result<(), String> {
    let os = std::env::consts::OS;
    if os == "windows" {
        let cmd = format!(
            "$p = [Environment]::GetEnvironmentVariable('Path','User'); \
             if($p -notlike '*{}*'){{ \
               [Environment]::SetEnvironmentVariable('Path',\"$p;{}\", 'User') \
             }}",
            dir, dir
        );
        let _ = Command::new("powershell")
            .args(["-Command", &cmd])
            .output();
    } else {
        // Append to .bashrc and .zshrc
        let home = dirs::home_dir().unwrap_or_default();
        for rc in &[".bashrc", ".zshrc", ".profile"] {
            let rc_path = home.join(rc);
            if rc_path.exists() || *rc == ".bashrc" {
                let line = format!("\nexport PATH=\"{}:$PATH\"\n", dir);
                let content =
                    std::fs::read_to_string(&rc_path).unwrap_or_default();
                if !content.contains(dir) {
                    let _ = std::fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&rc_path)
                        .and_then(|mut f| {
                            use std::io::Write;
                            f.write_all(line.as_bytes())
                        });
                }
            }
        }
    }
    Ok(())
}
