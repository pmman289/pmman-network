//! Only manage this application's tunnel service. Never kill WireGuard processes:
//! other tunnels may share the installed runtime.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Missing,
    Deleting,
    Stopped,
    StopPending,
    Other(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Done,
    Wait,
    Stop,
    Delete,
}

fn action(state: State, stop_sent: bool) -> Action {
    match state {
        State::Missing => Action::Done,
        State::Stopped => Action::Delete,
        State::Deleting | State::StopPending => Action::Wait,
        State::Other(_) if stop_sent => Action::Wait,
        State::Other(_) => Action::Stop,
    }
}

trait Backend {
    fn query(&mut self) -> Result<State, String>;
    fn apply(&mut self, action: Action) -> Result<bool, String>;
}

fn cleanup(
    backend: &mut impl Backend,
    timeout: Duration,
    interval: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let mut stop_sent = false;
    let mut last = None;
    loop {
        let state = backend.query()?;
        if last != Some(state) {
            crate::diagnostics::log(
                "tunnel",
                &format!(
                    "stop_state={state:?} elapsed_ms={}",
                    started.elapsed().as_millis()
                ),
            );
            last = Some(state);
        }
        let next = action(state, stop_sent);
        if next == Action::Done {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            let hint = if state == State::Deleting {
                "请关闭可能占用隧道服务的服务管理窗口后重试"
            } else {
                "Windows 尚未完成隧道停止，请稍后重试；可上报诊断日志"
            };
            return Err(format!(
                "WireGuard 隧道服务停止超时（状态 {state:?}，等待 {} 秒）；{hint}",
                started.elapsed().as_secs()
            ));
        }
        if backend.apply(next)? && next == Action::Stop {
            stop_sent = true;
        }
        std::thread::sleep(interval);
    }
}

#[cfg(windows)]
pub(crate) fn remove(tunnel: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("上一次隧道清理仍在进行，请稍后重试".into());
    }
    // SCM calls can themselves block. Keep them off the caller and prevent a
    // timed-out worker racing a subsequent installation. All handles are closed
    // before BUSY is released, including on error/panic.
    struct BusyGuard;
    impl Drop for BusyGuard {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::Release);
        }
    }
    let guard = BusyGuard;
    let name = format!("WireGuardTunnel${tunnel}");
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("pmman-tunnel-cleanup".into())
        .spawn(move || {
            let _guard = guard;
            let started = Instant::now();
            let result = native::Services::open(&name).and_then(|mut scm| {
                cleanup(
                    &mut scm,
                    Duration::from_secs(15),
                    Duration::from_millis(150),
                )
            });
            crate::diagnostics::log(
                "tunnel",
                &format!(
                    "stop_complete service={name} elapsed_ms={} result={result:?}",
                    started.elapsed().as_millis()
                ),
            );
            drop(_guard);
            let _ = tx.send(result);
        })
        .map_err(|error| format!("无法启动隧道清理：{error}"))?;
    rx.recv_timeout(Duration::from_secs(16)).map_err(|error| {
        crate::diagnostics::log("tunnel", &format!("stop_wait_failed error={error}"));
        "Windows 服务管理器未及时响应隧道清理，请稍后重试".to_string()
    })?
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{
            GetLastError, ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_DOES_NOT_EXIST,
            ERROR_SERVICE_MARKED_FOR_DELETE, ERROR_SERVICE_NOT_ACTIVE,
        },
        System::Services::*,
    };

    struct Handle(SC_HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseServiceHandle(self.0);
            }
        }
    }
    pub(super) struct Services {
        manager: Handle,
        name: Vec<u16>,
        current: Option<Handle>,
    }
    fn api_error(operation: &str, code: u32) -> String {
        format!(
            "隧道服务{operation}失败：Windows 错误 {code}（{}）",
            std::io::Error::from_raw_os_error(code as i32)
        )
    }
    impl Services {
        pub(super) fn open(name: &str) -> Result<Self, String> {
            let manager =
                unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
            if manager.is_null() {
                return Err(api_error("访问", unsafe { GetLastError() }));
            }
            Ok(Self {
                manager: Handle(manager),
                name: name.encode_utf16().chain(Some(0)).collect(),
                current: None,
            })
        }
    }
    impl Backend for Services {
        fn query(&mut self) -> Result<State, String> {
            self.current.take();
            // DELETE is the standard service object access right (0x10000).
            let service = unsafe {
                OpenServiceW(
                    self.manager.0,
                    self.name.as_ptr(),
                    SERVICE_QUERY_STATUS | SERVICE_STOP | 0x10000,
                )
            };
            if service.is_null() {
                return match unsafe { GetLastError() } {
                    ERROR_SERVICE_DOES_NOT_EXIST => Ok(State::Missing),
                    ERROR_SERVICE_MARKED_FOR_DELETE => Ok(State::Deleting),
                    code => Err(api_error("打开", code)),
                };
            }
            let service = Handle(service);
            let mut status: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
            let mut needed = 0;
            if unsafe {
                QueryServiceStatusEx(
                    service.0,
                    SC_STATUS_PROCESS_INFO,
                    &mut status as *mut _ as *mut u8,
                    std::mem::size_of_val(&status) as u32,
                    &mut needed,
                )
            } == 0
            {
                return Err(api_error("查询", unsafe { GetLastError() }));
            }
            let state = match status.dwCurrentState {
                SERVICE_STOPPED => State::Stopped,
                SERVICE_STOP_PENDING => State::StopPending,
                value => State::Other(value),
            };
            crate::diagnostics::log(
                "tunnel",
                &format!(
                    "service_status state={} pid={} checkpoint={} wait_hint_ms={} win32_exit={}",
                    status.dwCurrentState,
                    status.dwProcessId,
                    status.dwCheckPoint,
                    status.dwWaitHint,
                    status.dwWin32ExitCode
                ),
            );
            self.current = Some(service);
            Ok(state)
        }
        fn apply(&mut self, action: Action) -> Result<bool, String> {
            let Some(service) = self.current.take() else {
                return Ok(false);
            };
            let success = match action {
                Action::Stop => {
                    let mut status = unsafe { std::mem::zeroed() };
                    unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) }
                }
                Action::Delete => unsafe { DeleteService(service.0) },
                _ => return Ok(false),
            };
            if success != 0 {
                return Ok(true);
            }
            match unsafe { GetLastError() } {
                ERROR_SERVICE_NOT_ACTIVE
                | ERROR_SERVICE_MARKED_FOR_DELETE
                | ERROR_SERVICE_DOES_NOT_EXIST => Ok(false),
                ERROR_SERVICE_CANNOT_ACCEPT_CTRL if action == Action::Stop => Ok(false),
                code => Err(api_error(
                    if action == Action::Stop {
                        "停止"
                    } else {
                        "删除"
                    },
                    code,
                )),
            }
            // service closes here, before polling: keeping a handle open would
            // prevent a service marked for deletion from disappearing.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fake {
        states: VecDeque<Result<State, String>>,
        actions: Vec<Action>,
    }
    impl Backend for Fake {
        fn query(&mut self) -> Result<State, String> {
            self.states.pop_front().expect("unexpected query")
        }
        fn apply(&mut self, action: Action) -> Result<bool, String> {
            self.actions.push(action);
            Ok(true)
        }
    }
    fn fake(states: &[State]) -> Fake {
        Fake {
            states: states.iter().map(|s| Ok(*s)).collect(),
            actions: vec![],
        }
    }
    #[test]
    fn missing_service_is_idempotent() {
        let mut b = fake(&[State::Missing]);
        cleanup(&mut b, Duration::from_secs(1), Duration::ZERO).unwrap();
        assert!(b.actions.is_empty());
    }
    #[test]
    fn stopped_service_is_deleted_before_reinstall() {
        let mut b = fake(&[State::Stopped, State::Deleting, State::Missing]);
        cleanup(&mut b, Duration::from_secs(1), Duration::ZERO).unwrap();
        assert_eq!(b.actions, [Action::Delete, Action::Wait]);
    }
    #[test]
    fn stop_is_sent_once_and_pending_is_not_success() {
        let mut b = fake(&[
            State::Other(4),
            State::Other(4),
            State::StopPending,
            State::Stopped,
            State::Missing,
        ]);
        cleanup(&mut b, Duration::from_secs(1), Duration::ZERO).unwrap();
        assert_eq!(
            b.actions,
            [Action::Stop, Action::Wait, Action::Wait, Action::Delete]
        );
    }
    #[test]
    fn pending_and_deletion_have_bounded_waits() {
        for state in [State::StopPending, State::Deleting] {
            let mut b = fake(&[state]);
            let error = cleanup(&mut b, Duration::ZERO, Duration::ZERO).unwrap_err();
            assert!(error.contains("停止超时"));
            assert!(error.contains(&format!("{state:?}")));
            assert!(b.actions.is_empty());
        }
    }
    #[test]
    fn access_errors_are_not_disguised_as_timeouts() {
        let mut b = Fake {
            states: [Err("Windows 错误 5".into())].into(),
            actions: vec![],
        };
        assert_eq!(
            cleanup(&mut b, Duration::from_secs(1), Duration::ZERO),
            Err("Windows 错误 5".into())
        );
    }
}
