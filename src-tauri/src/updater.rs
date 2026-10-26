use pmman_protocol::release::Manifest;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

fn log_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pmman Network")
        .join("update.log")
}

fn log_event(event: &str) {
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= 256 * 1024) {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(&path, rotated);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        let _ = writeln!(file, "{timestamp} {event}");
    }
}

fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}
fn wait(child: &mut Child, seconds: u64) -> Result<bool, String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            log_event(&format!("process_pid={} exit_status={status}", child.id()));
            return Ok(status.success());
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err("installer timeout; recovery required".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// canonicalize() returns \\?\D:\... on Windows. NSIS uses legacy Win32 path
// handling, so /D must receive a normal absolute path with no extended prefix.
#[cfg(windows)]
fn wait_for_parent_process(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject,
    };

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const GRACE_PERIOD_MS: u32 = 5_000;
    const TERMINATION_WAIT_MS: u32 = 10_000;

    // Wait on the kernel process object instead of a PowerShell process
    // snapshot. The old implementation could observe a stale Process object
    // and roll back even after the UI had exited.
    let handle = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        if error == ERROR_INVALID_PARAMETER {
            log_event(&format!("parent_already_exited pid={pid}"));
            return Ok(());
        }
        return Err(format!(
            "cannot open old client process {pid}: win32={error}"
        ));
    }
    let result = unsafe { WaitForSingleObject(handle, GRACE_PERIOD_MS) };
    match result {
        WAIT_OBJECT_0 => {
            unsafe { CloseHandle(handle) };
            Ok(())
        }
        WAIT_TIMEOUT => {
            // Older clients used Tauri's asynchronous `app.exit(0)` and can
            // remain alive indefinitely while their event loop is unwinding.
            // The user has already explicitly confirmed this update and the
            // caller has disconnected the tunnel before starting us, so the
            // updater owns this exact parent PID and may terminate it to
            // release the executable lock. This is also what makes updates
            // from pre-0.0.7 clients recoverable instead of timing out and
            // relaunching a second copy of the old client.
            log_event(&format!(
                "parent_exit_timeout pid={pid}; terminating_old_client"
            ));
            let terminated = unsafe { TerminateProcess(handle, 0) };
            if terminated == 0 {
                let error = unsafe { GetLastError() };
                unsafe { CloseHandle(handle) };
                return Err(format!(
                    "old client did not exit and could not be terminated: win32={error}"
                ));
            }
            let terminated_result = unsafe { WaitForSingleObject(handle, TERMINATION_WAIT_MS) };
            unsafe { CloseHandle(handle) };
            if terminated_result == WAIT_OBJECT_0 {
                log_event(&format!("old_client_terminated pid={pid}"));
                Ok(())
            } else if terminated_result == WAIT_TIMEOUT {
                Err("old client did not exit after termination request".into())
            } else {
                Err(format!(
                    "waiting for terminated old client failed: win32={terminated_result}"
                ))
            }
        }
        value => {
            unsafe { CloseHandle(handle) };
            Err(format!("waiting for old client failed: win32={value}"))
        }
    }
}

fn nsis_directory(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_string()
    }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    const SYNCHRONIZE: u32 = 0x0010_0000;
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    unsafe { CloseHandle(handle) };
    alive
}

#[cfg(not(windows))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

fn phase(root: &Path, value: &str) -> Result<(), String> {
    log_event(&format!("phase={value}"));
    fs::write(
        root.join("result.json"),
        serde_json::to_vec(&serde_json::json!({"phase":value}))
            .map_err(|error| format!("write update phase failed: {error}"))?,
    )
    .map_err(|e| e.to_string())
}
fn arg(name: &str) -> Result<String, String> {
    std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
        .ok_or_else(|| format!("missing {name}"))
}
fn apply() -> Result<(), String> {
    log_event("updater_started");
    let installer = PathBuf::from(arg("--installer")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let root = installer.parent().ok_or("update parent")?;
    let target = PathBuf::from(arg("--install-dir")?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    log_event(&format!(
        "installer={} install_dir={}",
        installer.display(),
        target.display()
    ));
    if !target.join("pmman-client.exe").is_file()
        || !target.join("pmman-network-service.exe").is_file()
    {
        return Err("not an installed Pmman Network directory".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    manifest.verify(
        include_str!("../../config/client-update-public-key.txt"),
        &manifest.channel,
    )?;
    let bytes = fs::read(&installer).map_err(|e| e.to_string())?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if hash != manifest.sha256.to_lowercase() || bytes.len() as u64 != manifest.size {
        return Err("installer integrity failed".into());
    }
    let expected_hash = arg("--sha256")?;
    if hash != expected_hash.to_lowercase() {
        return Err("installer hash does not match the downloaded update".into());
    }
    let pid: u32 = arg("--parent-pid")?.parse().map_err(|_| "pid")?;
    log_event(&format!("waiting_for_parent_pid={pid}"));
    #[cfg(windows)]
    wait_for_parent_process(pid)?;
    let _ = pid;
    log_event("parent_exited");
    let backup = root.join("backup");
    fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
    let mut files = vec![
        "pmman-client.exe".to_string(),
        "pmman-network-service.exe".to_string(),
    ];
    for entry in fs::read_dir(&target).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map_err(|e| e.to_string())?.is_file()
            && name.to_lowercase().contains("uninstall")
            && name.ends_with(".exe")
        {
            files.push(name);
        }
    }
    for name in &files {
        fs::copy(target.join(name), backup.join(name)).map_err(|e| e.to_string())?;
    }
    let config = target.join("pmman-client.json");
    if config.is_file() {
        fs::copy(&config, backup.join("pmman-client.json")).map_err(|e| e.to_string())?;
    }
    phase(root, "installing")?;
    // NSIS accepts /D only as the final argument. It is deliberately derived
    // from the running executable, so an update always replaces the existing
    // per-machine installation directory instead of choosing a new location.
    // /S suppresses all installer pages; the per-machine UAC prompt, if
    // required, is the only expected user-visible step.
    let mut install = command(&installer)
        .arg("/S")
        .arg("/UPDATE")
        .arg(format!("/D={}", nsis_directory(&target)))
        .spawn()
        .map_err(|e| e.to_string())?;
    // Never restore while an installer still writes; timeout leaves backups and
    // an explicit recovery state, rather than racing partial file replacement.
    let installed = wait(&mut install, 180)?;
    log_event(&format!("installer_exit_success={installed}"));
    // The installer must never replace the operator's control-plane endpoint.
    // Restore it even when NSIS removed the file during a failed or partial
    // install; checking `config.is_file()` here would lose production config.
    if backup.join("pmman-client.json").is_file() {
        fs::copy(backup.join("pmman-client.json"), &config).map_err(|e| e.to_string())?;
    }
    let health = root.join("healthy.json");
    let _ = fs::remove_file(&health);
    let mut new_client = if installed {
        Some(
            command(target.join("pmman-client.exe"))
                .args([
                    "--update-health-file",
                    &health.to_string_lossy(),
                    "--expected-version",
                    &manifest.version,
                ])
                .spawn()
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let start = Instant::now();
    while installed && start.elapsed() < Duration::from_secs(60) {
        if let Ok(bytes) = fs::read(&health) {
            if serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .is_some_and(|v| {
                    v["version"] == manifest.version && v["ui"] == true && v["ipc"] == true
                })
            {
                // The probe exits without showing the user window. Wait for
                // it to release the IPC helper before starting the real UI.
                if let Some(probe) = new_client.as_mut() {
                    if !wait(probe, 10)? {
                        return Err("new client health probe failed to exit cleanly".into());
                    }
                }
                command(target.join("pmman-client.exe"))
                    .spawn()
                    .map_err(|e| format!("new client relaunch failed: {e}"))?;
                log_event("new_client_restarted");
                log_event("health_check=passed");
                phase(root, "complete")?;
                return Ok(());
            }
        }
        if new_client
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_some())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    phase(root, "rolling_back")?;
    log_event("health_check=failed; rolling_back");
    if let Some(mut child) = new_client {
        let _ = child.kill();
        let _ = child.wait();
    }
    // UI owns its sidecar. Explicit health failure exit also stops the sidecar.
    std::thread::sleep(Duration::from_secs(2));
    for name in &files {
        fs::copy(backup.join(name), target.join(name))
            .map_err(|e| format!("rollback {name}: {e}"))?;
    }
    if backup.join("pmman-client.json").is_file() {
        fs::copy(backup.join("pmman-client.json"), &config)
            .map_err(|e| format!("rollback pmman-client.json: {e}"))?;
    }
    phase(root, "rolled_back")?;
    Err("new client failed startup; restored previous files".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsis_receives_plain_absolute_install_paths() {
        assert_eq!(
            nsis_directory(Path::new(r"\\?\D:\programs\Pmman Network")),
            r"D:\programs\Pmman Network"
        );
        assert_eq!(
            nsis_directory(Path::new(r"\\?\UNC\server\share\Pmman Network")),
            r"\\server\share\Pmman Network"
        );
        assert_eq!(
            nsis_directory(Path::new(r"D:\programs\Pmman Network")),
            r"D:\programs\Pmman Network"
        );
    }
}
pub fn run() -> ! {
    let result = apply();
    if let Err(ref error) = result {
        log_event(&format!("update_failed={error}"));
        if let Ok(path) = arg("--installer") {
            if let Some(root) = Path::new(&path).parent() {
                let _ = fs::write(root.join("error.txt"), error);
            }
        }
        // If the failure happened before NSIS replaced anything, bring the
        // existing client back so a failed silent update never leaves the user
        // without a UI. The target path is supplied by the original client.
        let parent_alive = arg("--parent-pid")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(process_is_alive);
        if parent_alive {
            log_event("old_client_still_running; skip_duplicate_restart");
        } else if let Ok(target) = arg("--install-dir") {
            let client = PathBuf::from(target).join("pmman-client.exe");
            if client.is_file() {
                let _ = command(client).spawn();
                log_event("old_client_restarted");
            }
        }
    } else {
        log_event("update_complete");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
