#[cfg(unix)]
use pmman_protocol::UNIX_SOCKET_PATH;
#[cfg(windows)]
use pmman_protocol::WINDOWS_PIPE_NAME;
use pmman_protocol::{Action, ClientSnapshot, IpcRequest, IpcResponse};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime, WindowEvent, Wry,
};

struct AppState(Mutex<ClientSnapshot>);
static SERVICE_PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
#[cfg(windows)]
static SINGLE_INSTANCE_MUTEX: OnceLock<isize> = OnceLock::new();

/// Keep the desktop client single-instance before it starts the elevated
/// helper. A named kernel mutex works across the normal user and elevated
/// process boundary and does not depend on the installation directory.
#[cfg(windows)]
pub fn acquire_single_instance() -> bool {
    use std::{os::windows::ffi::OsStrExt, ptr::null};
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = std::ffi::OsStr::new("Global\\PmmanNetworkClient")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    if handle.is_null() {
        // Fail closed: starting a second tunnel-capable UI is riskier than
        // reporting a startup failure when the OS cannot create the guard.
        return false;
    }
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already_exists {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return false;
    }
    let _ = SINGLE_INSTANCE_MUTEX.set(handle as isize);
    true
}

#[cfg(not(windows))]
pub fn acquire_single_instance() -> bool {
    true
}

struct Lifecycle {
    quitting: AtomicBool,
}

struct TrayState {
    status: MenuItem<Wry>,
    action: MenuItem<Wry>,
    operation_in_flight: AtomicBool,
}

fn service_process() -> &'static Mutex<Option<Child>> {
    SERVICE_PROCESS.get_or_init(|| Mutex::new(None))
}

fn snapshot_label(snapshot: &ClientSnapshot) -> &'static str {
    match snapshot.state.as_str() {
        "connected" if snapshot.tunnel_mode.as_deref() == Some("wireguard") => "已连接",
        "connecting" => "正在连接…",
        "disconnecting" => "正在断开…",
        "error" => "连接异常",
        _ => "未连接",
    }
}

fn update_tray<R: Runtime>(app: &AppHandle<R>, snapshot: &ClientSnapshot) {
    let Some(tray) = app.try_state::<TrayState>() else {
        return;
    };
    let connected =
        snapshot.state == "connected" && snapshot.tunnel_mode.as_deref() == Some("wireguard");
    let pending = matches!(snapshot.state.as_str(), "connecting" | "disconnecting");
    let label = snapshot_label(snapshot);
    let _ = tray.status.set_text(format!("状态：{label}"));
    let _ = tray.action.set_text(if pending {
        "正在处理…"
    } else if connected {
        "断开连接"
    } else {
        "连接网络"
    });
    let _ = tray.action.set_enabled(!pending);
}

fn save_snapshot<R: Runtime>(app: &AppHandle<R>, snapshot: ClientSnapshot) -> ClientSnapshot {
    let state = app.state::<AppState>();
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(app, &snapshot);
    snapshot
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn tray_toggle_connection(app: &AppHandle<Wry>) {
    let tray = app.state::<TrayState>();
    if tray.operation_in_flight.swap(true, Ordering::AcqRel) {
        return;
    }
    let connected = {
        let snapshot = app.state::<AppState>();
        let snapshot = snapshot.0.lock().expect("client state poisoned");
        snapshot.state == "connected" && snapshot.tunnel_mode.as_deref() == Some("wireguard")
    };
    let action = if connected {
        Action::Disconnect
    } else {
        Action::Connect
    };
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(action)));
        match result {
            Ok(snapshot) => {
                save_snapshot(&app, snapshot);
            }
            Err(error) => {
                let snapshot = {
                    let state = app.state::<AppState>();
                    let mut snapshot = state.0.lock().expect("client state poisoned").clone();
                    snapshot.state = "error".into();
                    snapshot.message = error;
                    snapshot
                };
                save_snapshot(&app, snapshot);
            }
        }
        app.state::<TrayState>()
            .operation_in_flight
            .store(false, Ordering::Release);
    });
}

pub fn stop_owned_service() {
    let mut process = service_process()
        .lock()
        .expect("service process state poisoned");
    if let Some(mut child) = process.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn quit_application(app: &AppHandle<Wry>) {
    let lifecycle = app.state::<Lifecycle>();
    if lifecycle.quitting.swap(true, Ordering::AcqRel) {
        return;
    }
    // Remove the client peer before terminating the helper process. This is
    // intentionally best-effort so an unreachable control plane cannot trap
    // the user in the tray menu.
    let _ = send_ipc(IpcRequest::new(Action::Disconnect));
    stop_owned_service();
    app.exit(0);
}

fn build_tray(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "状态：未连接", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "打开主界面", true, None::<&str>)?;
    let action = MenuItem::with_id(app, "toggle", "连接网络", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 Pmman Network", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&status, &open, &action, &separator, &quit])?;
    app.manage(TrayState {
        status,
        action,
        operation_in_flight: AtomicBool::new(false),
    });
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;
    TrayIconBuilder::with_id("pmman-network-client")
        .icon(icon)
        .tooltip("Pmman Network")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "toggle" => tray_toggle_connection(app),
            "quit" => quit_application(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(windows)]
fn open_windows_pipe() -> Result<std::fs::File, std::io::Error> {
    let mut last_error = None;
    for _ in 0..50 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(WINDOWS_PIPE_NAME)
        {
            Ok(pipe) => return Ok(pipe),
            Err(error) => {
                // Retry startup races and busy instances, not an ACL failure.
                if !matches!(error.raw_os_error(), Some(2 | 231)) {
                    return Err(error);
                }
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    Err(last_error.expect("pipe retry loop always runs"))
}

fn ensure_service_started() -> Result<(), String> {
    let mut process = service_process()
        .lock()
        .expect("service process state poisoned");
    if let Some(child) = process.as_mut() {
        match child.try_wait() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) => *process = None,
            Err(error) => return Err(format!("无法检查后台服务状态：{error}")),
        }
    }
    let path = if let Ok(path) = std::env::var("PMMAN_SERVICE_PATH") {
        PathBuf::from(path)
    } else {
        let mut path = std::env::current_exe().map_err(|e| e.to_string())?;
        path.set_file_name(if cfg!(windows) {
            "pmman-network-service.exe"
        } else {
            "pmman-network-service"
        });
        path
    };
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        Command::new(&path)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };
    #[cfg(not(windows))]
    let child = Command::new(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let child = child.map_err(|e| format!("无法启动后台服务 {}：{e}", path.display()))?;
    *process = Some(child);
    Ok(())
}

fn send_ipc(request: IpcRequest) -> Result<ClientSnapshot, String> {
    let request = format!(
        "{}\n",
        serde_json::to_string(&request).map_err(|e| e.to_string())?
    );
    #[cfg(windows)]
    let stream = open_windows_pipe().map_err(|e| match e.raw_os_error() {
        Some(231) => "后台服务繁忙，请稍后重试（IPC 231）".to_string(),
        Some(5) => "无法访问后台服务，请退出 Pmman Network 后重新打开（IPC 5）".to_string(),
        _ => format!("无法连接后台服务：{e}"),
    })?;
    #[cfg(unix)]
    let stream = std::os::unix::net::UnixStream::connect(UNIX_SOCKET_PATH)
        .map_err(|e| format!("无法连接后台服务：{e}"))?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    writer
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let response: IpcResponse = serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
    if response.ok {
        Ok(response.status)
    } else {
        Err(response.status.message)
    }
}

#[tauri::command]
async fn invitation_request(
    request: pmman_protocol::InvitationRequest,
) -> Result<pmman_protocol::InvitationResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut ipc = IpcRequest::new(Action::Invitation);
        ipc.invitation_request = Some(request);
        ensure_service_started()?;
        send_ipc(ipc)?
            .invitation_result
            .ok_or_else(|| "邀请服务未返回结果".into())
    })
    .await
    .map_err(|_| "邀请请求线程异常".to_string())?
}

#[tauri::command]
async fn get_status(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::Status)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            // Losing IPC does not establish that the tunnel disconnected.
            // Keep the last authoritative state and flag its freshness in message.
            current.message = format!("后台服务暂时不可达，当前显示上次已知状态：{error}");
        }
        Err(error) => {
            current.message =
                format!("后台服务暂时不可达，当前显示上次已知状态（请求线程异常退出）：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

async fn update_request(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    action: Action,
    channel: String,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::update(action, channel)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            current.client_update.state = "error".into();
            current.client_update.message = error;
        }
        Err(error) => {
            current.client_update.state = "error".into();
            current.client_update.message = format!("后台服务请求线程异常退出：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn check_update(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    channel: String,
) -> Result<ClientSnapshot, String> {
    update_request(state, app, Action::CheckUpdate, channel).await
}

#[tauri::command]
async fn upload_diagnostics(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::UploadDiagnostics)))
    })
    .await;
    match result {
        Ok(Ok(snapshot)) => {
            *state.0.lock().expect("client state poisoned") = snapshot.clone();
            update_tray(&app, &snapshot);
            Ok(snapshot)
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(format!("后台服务请求线程异常退出：{error}")),
    }
}

#[tauri::command]
async fn download_update(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    channel: String,
) -> Result<ClientSnapshot, String> {
    update_request(state, app, Action::DownloadUpdate, channel).await
}

#[tauri::command]
async fn login(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    username: String,
    password: String,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::login(username, password)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            current.message = error;
        }
        Err(error) => {
            current.message = format!("后台服务请求线程异常退出：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn change_password(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    current_password: String,
    new_password: String,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ensure_service_started()
            .and_then(|_| send_ipc(IpcRequest::change_password(current_password, new_password)))
    })
    .await;
    match result {
        Ok(Ok(snapshot)) => {
            *state.0.lock().expect("client state poisoned") = snapshot.clone();
            update_tray(&app, &snapshot);
            Ok(snapshot)
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(format!("后台服务请求线程异常退出：{error}")),
    }
}

#[tauri::command]
async fn list_nodes(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::ListNodes)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => return Err(error),
        Err(error) => return Err(format!("后台服务请求线程异常退出：{error}")),
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn select_node(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    node_id: Option<String>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::select_node(node_id)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => return Err(error),
        Err(error) => return Err(format!("后台服务请求线程异常退出：{error}")),
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn set_p2p(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    enabled: bool,
) -> Result<ClientSnapshot, String> {
    let snapshot = tauri::async_runtime::spawn_blocking(move || {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::set_p2p(enabled)))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn list_port_forward_entries(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let snapshot = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started()
            .and_then(|_| send_ipc(IpcRequest::new(Action::ListPortForwardEntries)))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn list_port_forward_quota(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let snapshot = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started()
            .and_then(|_| send_ipc(IpcRequest::new(Action::ListPortForwardQuota)))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn list_port_forwards(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let snapshot = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::ListPortForwards)))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn create_port_forward(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    device_id: Option<String>,
    entry_id: Option<String>,
    protocols: Vec<pmman_protocol::PortForwardProtocol>,
    target_port: u16,
) -> Result<ClientSnapshot, String> {
    let snapshot = tauri::async_runtime::spawn_blocking(move || {
        let mut request = IpcRequest::new(Action::CreatePortForward);
        request.device_id = device_id;
        request.entry_id = entry_id;
        request.protocols = Some(protocols);
        request.target_port = Some(target_port);
        ensure_service_started().and_then(|_| send_ipc(request))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn release_port_forward(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    forward_id: String,
) -> Result<ClientSnapshot, String> {
    let snapshot = tauri::async_runtime::spawn_blocking(move || {
        let mut request = IpcRequest::new(Action::ReleasePortForward);
        request.forward_id = Some(forward_id);
        ensure_service_started().and_then(|_| send_ipc(request))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.0.lock().expect("client state poisoned") = snapshot.clone();
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn set_traffic_mode(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
    ipv4_mode: String,
    ipv6_mode: String,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        ensure_service_started()
            .and_then(|_| send_ipc(IpcRequest::set_traffic_mode(ipv4_mode, ipv6_mode)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            current.state = "error".into();
            current.message = error;
        }
        Err(error) => {
            current.state = "error".into();
            current.message = format!("后台服务请求线程异常退出：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn connect(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::Connect)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            current.state = "error".into();
            current.message = error;
        }
        Err(error) => {
            current.state = "error".into();
            current.message = format!("后台服务请求线程异常退出：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn cancel_connect() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        send_ipc(IpcRequest::new(Action::CancelConnect)).map(|_| ())
    })
    .await
    .map_err(|_| "取消连接请求未完成".to_string())?
}

#[tauri::command]
async fn disconnect(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::Disconnect)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            current.state = "error".into();
            current.message = error;
        }
        Err(error) => {
            current.state = "error".into();
            current.message = format!("后台服务请求线程异常退出：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn logout(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<ClientSnapshot, String> {
    let result = tauri::async_runtime::spawn_blocking(|| {
        ensure_service_started().and_then(|_| send_ipc(IpcRequest::new(Action::Logout)))
    })
    .await;
    let mut current = state.0.lock().expect("client state poisoned");
    match result {
        Ok(Ok(snapshot)) => *current = snapshot,
        Ok(Err(error)) => {
            current.state = "error".into();
            current.message = error;
        }
        Err(error) => {
            current.state = "error".into();
            current.message = format!("后台服务请求线程异常退出：{error}");
        }
    }
    let snapshot = current.clone();
    drop(current);
    update_tray(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
async fn install_update(
    state: tauri::State<'_, AppState>,
    app: AppHandle<Wry>,
) -> Result<(), String> {
    let update = state
        .0
        .lock()
        .expect("client state poisoned")
        .client_update
        .clone();
    if !update.ready {
        return Err("客户端更新尚未下载完成".into());
    }
    let installer = update
        .installer_path
        .ok_or_else(|| "客户端更新文件不存在".to_string())?;
    let sha256 = update
        .sha256
        .ok_or_else(|| "客户端更新缺少完整性摘要".to_string())?;
    if !std::path::Path::new(&installer).is_file() {
        return Err("客户端更新文件不存在".into());
    }
    // Always remove the tunnel before replacing the elevated helper binary.
    let _ = send_ipc(IpcRequest::new(Action::Disconnect));
    stop_owned_service();
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let install_dir = executable
        .parent()
        .ok_or_else(|| "无法确定客户端安装目录".to_string())?
        .to_path_buf();
    // Do not run the updater from the installed executable itself. Windows
    // keeps that image locked until the updater process exits, which prevents
    // NSIS from replacing pmman-client.exe. A per-run copy can be replaced
    // independently while the installed files are free.
    let updater_dir = std::env::temp_dir()
        .join("Pmman Network")
        .join("updater")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&updater_dir)
        .map_err(|error| format!("无法创建更新器临时目录：{error}"))?;
    let updater = updater_dir.join("pmman-network-updater.exe");
    std::fs::copy(&executable, &updater).map_err(|error| format!("无法准备独立更新器：{error}"))?;
    let child = {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            Command::new(&updater)
                .args([
                    "--apply-update",
                    "--installer",
                    &installer,
                    "--install-dir",
                    &install_dir.to_string_lossy(),
                    "--sha256",
                    &sha256,
                    "--parent-pid",
                    &std::process::id().to_string(),
                ])
                .creation_flags(0x08000000)
                .spawn()
        }
        #[cfg(not(windows))]
        {
            Command::new(&updater)
                .args([
                    "--apply-update",
                    "--installer",
                    &installer,
                    "--install-dir",
                    &install_dir.to_string_lossy(),
                    "--sha256",
                    &sha256,
                    "--parent-pid",
                    &std::process::id().to_string(),
                ])
                .spawn()
        }
    }
    .map_err(|error| format!("无法启动客户端更新器：{error}"))?;
    let _ = child.id();
    // `app.exit(0)` only schedules Tauri's graceful shutdown. The updater
    // waits on this process object before replacing the installed executable,
    // so returning from the command can leave the old UI alive long enough
    // for the updater to hit its timeout. Terminate the parent immediately
    // after the updater has been spawned; the updater owns recovery and the
    // relaunch path from this point onward.
    app.exit(0);
    terminate_after_update_spawn();
}

/// End the old UI after the updater copy has been launched. `app.exit()` only
/// schedules Tauri shutdown; on Windows the event loop can remain alive long
/// enough for NSIS to observe the old image lock. ExitProcess releases the
/// image deterministically and leaves recovery to the updater process.
fn terminate_after_update_spawn() -> ! {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Threading::ExitProcess(0);
    }
    #[cfg(not(windows))]
    std::process::exit(0);
}

/// A short-lived startup probe used by the updater. It proves that the newly
/// installed executable can load, start its sidecar, and answer IPC before the
/// updater replaces the previous backup. It deliberately never shows a second
/// application window.
pub fn run_update_health(path: &str, expected_version: &str) -> ! {
    let ipc = ensure_service_started()
        .and_then(|_| send_ipc(IpcRequest::new(Action::Status)))
        .is_ok();
    let version = match option_env!("PMMAN_CLIENT_VERSION") {
        Some(value) => value,
        None => env!("CARGO_PKG_VERSION"),
    };
    let version_ok = expected_version.is_empty() || expected_version == version;
    let result = serde_json::json!({
        "version": version,
        "ui": true,
        "ipc": ipc && version_ok,
    });
    let _ = std::fs::write(path, serde_json::to_vec(&result).unwrap_or_default());
    // The health probe is short-lived; it must not leave its helper service
    // running after it has answered the updater's IPC check.
    stop_owned_service();
    std::process::exit(if ipc && version_ok { 0 } else { 1 });
}

// The blocking IPC and WireGuard wait run in Tauri's blocking pool, keeping the WebView responsive.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = ensure_service_started();
    tauri::Builder::default()
        .manage(AppState(Mutex::new(ClientSnapshot {
            state: "disconnected".into(),
            message: "尚未连接".into(),
            ..Default::default()
        })))
        .manage(Lifecycle {
            quitting: AtomicBool::new(false),
        })
        .setup(|app| {
            build_tray(&app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let lifecycle = window.app_handle().state::<Lifecycle>();
                if !lifecycle.quitting.load(Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            invitation_request,
            get_status,
            check_update,
            upload_diagnostics,
            download_update,
            login,
            change_password,
            list_nodes,
            select_node,
            set_traffic_mode,
            set_p2p,
            list_port_forward_entries,
            list_port_forward_quota,
            list_port_forwards,
            create_port_forward,
            release_port_forward,
            connect,
            disconnect,
            cancel_connect,
            logout,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
