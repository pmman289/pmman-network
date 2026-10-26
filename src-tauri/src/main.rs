// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod updater;

#[cfg(windows)]
fn configure_webview_runtime() {
    if std::env::var_os("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER").is_some() {
        return;
    }

    let mut roots = Vec::new();
    for variable in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            roots.push(std::path::PathBuf::from(root).join("Microsoft/EdgeWebView/Application"));
        }
    }

    // Some Evergreen installer builds can briefly leave the registry pv value
    // one patch behind the directory actually deployed on disk. Prefer the
    // newest directory containing the real executable so the loader does not
    // resolve a non-existent version from that stale value.
    let mut candidates = Vec::new();
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
                    && path.join("msedgewebview2.exe").is_file()
                {
                    candidates.push(path);
                }
            }
        }
    }
    candidates.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    if let Some(path) = candidates.pop() {
        // SAFETY: this is set before Tauri creates its WebView2 environment.
        unsafe { std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", path) };
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--apply-update") {
        updater::run();
    }
    #[cfg(windows)]
    configure_webview_runtime();

    // Use the same loader as the actual UI, before creating a window or service.
    // NSIS and acceptance tests must not confuse a folder with a usable runtime.
    if std::env::args().any(|arg| arg == "--check-webview2") {
        let available = tauri::webview_version().is_ok();
        std::process::exit(if available { 0 } else { 2 });
    }
    if let Some(file) = argument_value("--update-health-file") {
        let expected = argument_value("--expected-version").unwrap_or_default();
        tauri_app_lib::run_update_health(&file, &expected);
    }
    if !std::env::args().any(|arg| arg == "--check-webview2")
        && !tauri_app_lib::acquire_single_instance()
    {
        // The existing process owns the named mutex. It also owns the tray
        // icon and the helper service, so a second UI must not start either.
        std::process::exit(0);
    }
    tauri_app_lib::run()
}

fn argument_value(name: &str) -> Option<String> {
    std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}
