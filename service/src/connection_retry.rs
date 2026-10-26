//! Retry only explicit address-cleanup conflicts, never arbitrary registration errors.
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const WAIT_MESSAGE: &str = "正在切换地址，等待旧连接清理…";
pub const CANCELLED: &str = "已取消连接";
const TIMEOUT: &str = "旧连接清理尚未完成，请稍后重试或联系管理员";

pub enum AttemptError {
    CleanupPending,
    Failed(String),
}

pub fn cleanup_pending(status: u16, detail: &str) -> bool {
    status == 409
        && matches!(
            detail,
            "地址策略已变更，旧连接及端口映射尚未清理，请断开后稍候重试"
                | "旧节点尚未确认连接清理，请稍后重试"
        )
}

pub fn finish<T>(
    result: Result<T, String>,
    cancelled: bool,
    revoke: impl FnOnce(&T) -> Result<(), String>,
) -> Result<T, String> {
    match result {
        Ok(config) if cancelled => {
            revoke(&config).map_err(|error| format!("已取消连接，但主控撤销未完成：{error}"))?;
            Err(CANCELLED.into())
        }
        other => other,
    }
}

pub fn run<T>(
    budget: Duration,
    interval: Duration,
    cancelled: &AtomicBool,
    mut attempt: impl FnMut(Duration) -> Result<T, AttemptError>,
    mut pending: impl FnMut(u32, Duration),
) -> Result<T, String> {
    let started = Instant::now();
    let mut retries = 0;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(TIMEOUT.into());
        }
        match attempt(remaining) {
            Ok(value) => return Ok(value), // Caller revokes a successful late response on cancellation.
            Err(AttemptError::Failed(error)) => return Err(error),
            Err(AttemptError::CleanupPending) => {
                retries += 1;
                pending(retries, started.elapsed());
                let pause = interval.min(budget.saturating_sub(started.elapsed()));
                let wake = Instant::now() + pause;
                while Instant::now() < wake {
                    if cancelled.load(Ordering::Acquire) {
                        return Err(CANCELLED.into());
                    }
                    std::thread::sleep(
                        Duration::from_millis(100)
                            .min(wake.saturating_duration_since(Instant::now())),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_success_is_revoked_before_reporting_cancellation() {
        let mut revoked = None;
        let result = finish(Ok(42), true, |id| {
            revoked = Some(*id);
            Ok(())
        });
        assert_eq!(result.unwrap_err(), CANCELLED);
        assert_eq!(revoked, Some(42));
        assert_eq!(
            finish(Ok(7), false, |_| panic!("normal success revoked")).unwrap(),
            7
        );
        assert!(finish(Ok(42), true, |_| Err("HTTP 503".into()))
            .unwrap_err()
            .contains("撤销未完成"));
    }
    #[test]
    fn only_known_cleanup_conflicts_retry() {
        let detail = "地址策略已变更，旧连接及端口映射尚未清理，请断开后稍候重试";
        assert!(cleanup_pending(409, detail));
        assert!(cleanup_pending(409, "旧节点尚未确认连接清理，请稍后重试"));
        for status in [400, 401, 403, 429, 502, 503] {
            assert!(!cleanup_pending(status, detail));
        }
        assert!(!cleanup_pending(409, "固定地址已被其他用户或设备占用"));
        assert!(!cleanup_pending(
            409,
            "该账户已有活动连接，请先断开再变更地址或节点"
        ));
    }
    #[test]
    fn pending_then_success_does_not_surface_error() {
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let mut progress = 0;
        let value = run(
            Duration::from_secs(1),
            Duration::ZERO,
            &cancel,
            |_| {
                calls += 1;
                if calls < 3 {
                    Err(AttemptError::CleanupPending)
                } else {
                    Ok(42)
                }
            },
            |n, _| progress = n,
        )
        .unwrap();
        assert_eq!((value, calls, progress), (42, 3, 2));
    }
    #[test]
    fn terminal_errors_are_not_retried() {
        let mut calls = 0;
        let result = run::<()>(
            Duration::from_secs(1),
            Duration::ZERO,
            &AtomicBool::new(false),
            |_| {
                calls += 1;
                Err(AttemptError::Failed("HTTP 401".into()))
            },
            |_, _| panic!("must not publish waiting"),
        );
        assert_eq!(result.unwrap_err(), "HTTP 401");
        assert_eq!(calls, 1);
    }
    #[test]
    fn cleanup_wait_has_deadline() {
        let started = Instant::now();
        let result = run::<()>(
            Duration::from_millis(15),
            Duration::from_millis(2),
            &AtomicBool::new(false),
            |_| Err(AttemptError::CleanupPending),
            |_, _| {},
        );
        assert_eq!(result.unwrap_err(), TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn cancellation_stops_before_next_request() {
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let result = run::<()>(
            Duration::from_secs(1),
            Duration::from_secs(1),
            &cancel,
            |_| {
                calls += 1;
                Err(AttemptError::CleanupPending)
            },
            |_, _| cancel.store(true, Ordering::Release),
        );
        assert_eq!(result.unwrap_err(), CANCELLED);
        assert_eq!(calls, 1);
    }
}
