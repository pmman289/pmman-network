//! Share renewed bearer sessions across heartbeat, P2P and foreground requests.
use super::*;

const AUTH_REJECTED: &str = "登录凭据已失效，请重新登录";

pub(super) fn is_permanent_auth_error(error: &str) -> bool {
    error == AUTH_REJECTED
}

#[derive(Clone)]
pub(super) struct ConnectionAuth {
    expected: AuthSession,
    auth: Arc<Mutex<Option<AuthSession>>>,
    generation: Arc<AtomicU64>,
    refresh_gate: Arc<Mutex<()>>,
    epoch: u64,
}

impl ConnectionAuth {
    pub(super) fn new(
        expected: AuthSession,
        auth: Arc<Mutex<Option<AuthSession>>>,
        generation: Arc<AtomicU64>,
        refresh_gate: Arc<Mutex<()>>,
    ) -> Self {
        let epoch = generation.load(Ordering::Acquire);
        Self {
            expected,
            auth,
            generation,
            refresh_gate,
            epoch,
        }
    }

    fn current(&self) -> Result<AuthSession, String> {
        let auth = self.auth.lock().expect("auth state poisoned");
        auth.as_ref()
            .filter(|session| {
                self.generation.load(Ordering::Acquire) == self.epoch
                    && session.user_id == self.expected.user_id
                    && session.username == self.expected.username
            })
            .cloned()
            .ok_or_else(|| "登录状态已变更，请重新连接".into())
    }

    pub(super) fn refresh(&self) -> Result<AuthSession, String> {
        self.refresh_with(|| {
            let (username, password) =
                load_login_credentials()?.ok_or_else(|| AUTH_REJECTED.to_string())?;
            if username != self.expected.username {
                return Err("登录状态已变更，请重新登录".into());
            }
            control_plane_login(&username, &password).map_err(|error| {
                if is_session_expired_error(&error) {
                    AUTH_REJECTED.into()
                } else {
                    error
                }
            })
        })
    }

    fn refresh_with(
        &self,
        login: impl FnOnce() -> Result<AuthSession, String>,
    ) -> Result<AuthSession, String> {
        self.refresh_with_cleanup(login, |session| {
            if control_plane_logout(session).is_err() {
                diagnostics::log("auth", "late_session_revoke_failed");
            }
        })
    }

    fn refresh_with_cleanup(
        &self,
        login: impl FnOnce() -> Result<AuthSession, String>,
        discard: impl FnOnce(&AuthSession),
    ) -> Result<AuthSession, String> {
        // Never hold the UI snapshot or tunnel mutation lock during login.
        let _refresh = self.refresh_gate.lock().expect("auth refresh poisoned");
        let current = self.current()?;
        if current.token != self.expected.token {
            return Ok(current);
        }
        let next = login()?;
        if next.user_id != current.user_id || next.username != current.username {
            discard(&next);
            return Err("自动登录账号与当前连接不一致，请重新登录".into());
        }
        let mut auth = self.auth.lock().expect("auth state poisoned");
        if self.generation.load(Ordering::Acquire) != self.epoch
            || auth.as_ref().map(|session| session.token.as_str()) != Some(current.token.as_str())
        {
            drop(auth);
            discard(&next);
            return Err("登录状态已变更，请重新连接".into());
        }
        *auth = Some(next.clone());
        diagnostics::log("auth", "session_recovered");
        Ok(next)
    }

    pub(super) fn invalidate(&self) {
        let mut auth = self.auth.lock().expect("auth state poisoned");
        if self.generation.load(Ordering::Acquire) == self.epoch
            && auth
                .as_ref()
                .is_some_and(|session| session.user_id == self.expected.user_id)
        {
            *auth = None;
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(super) fn call<T>(
        &mut self,
        mut operation: impl FnMut(&AuthSession) -> Result<T, String>,
    ) -> Result<T, String> {
        self.call_retry(&mut operation, |auth| auth.refresh())
    }

    fn call_retry<T>(
        &mut self,
        mut operation: impl FnMut(&AuthSession) -> Result<T, String>,
        renew: impl FnOnce(&Self) -> Result<AuthSession, String>,
    ) -> Result<T, String> {
        self.expected = self.current()?;
        match operation(&self.expected) {
            Err(error) if is_session_expired_error(&error) => {
                diagnostics::log("auth", "session_expired retry=once");
                self.expected = renew(self)?;
                operation(&self.expected)
            }
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(token: &str) -> AuthSession {
        AuthSession {
            token: token.into(),
            user_id: "u1".into(),
            username: "user".into(),
            role: "user".into(),
            allow_ipv4_global: false,
            allow_ipv6_global: false,
            allow_ipv4_split: false,
            allow_ipv6_split: false,
        }
    }
    fn context() -> ConnectionAuth {
        ConnectionAuth::new(
            session("old"),
            Arc::new(Mutex::new(Some(session("old")))),
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(())),
        )
    }
    #[test]
    fn renewed_token_is_shared_and_concurrent_stale_refresh_does_not_login_again() {
        let context = context();
        let mut other = context.clone();
        assert_eq!(
            context.refresh_with(|| Ok(session("new"))).unwrap().token,
            "new"
        );
        assert_eq!(
            other
                .refresh_with(|| panic!("duplicate login"))
                .unwrap()
                .token,
            "new"
        );
        assert_eq!(other.call(|s| Ok(s.token.clone())).unwrap(), "new");
    }
    #[test]
    fn logout_during_login_cannot_restore_session() {
        let context = context();
        let mut discarded = false;
        let result = context.refresh_with_cleanup(
            || {
                context.generation.fetch_add(1, Ordering::AcqRel);
                *context.auth.lock().unwrap() = None;
                Ok(session("new"))
            },
            |session| {
                assert_eq!(session.token, "new");
                assert!(context.auth.try_lock().unwrap().is_none());
                discarded = true;
            },
        );
        assert!(result.is_err());
        assert!(discarded);
        assert!(context.auth.lock().unwrap().is_none());
    }
    #[test]
    fn failed_or_wrong_user_login_does_not_change_session() {
        let context = context();
        assert!(context
            .refresh_with(|| Err("network failure".into()))
            .is_err());
        let mut wrong = session("new");
        wrong.user_id = "other".into();
        let mut discarded = false;
        assert!(context
            .refresh_with_cleanup(
                || Ok(wrong),
                |_| {
                    discarded = true;
                }
            )
            .is_err());
        assert!(discarded);
        assert_eq!(context.current().unwrap().token, "old");
    }
    #[test]
    fn real_revocation_and_transient_failures_do_not_retry_login() {
        let mut context = context();
        for error in [
            "主控 HTTP 403",
            "主控 HTTP 404",
            "主控 HTTP 409",
            "主控 HTTP 503",
        ] {
            let mut calls = 0;
            let result = context.call::<()>(|_| {
                calls += 1;
                Err(error.into())
            });
            assert_eq!(result.unwrap_err(), error);
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn master_restart_reauthenticates_and_retries_exactly_once() {
        let mut context = context();
        let mut calls = 0;
        let result = context.call_retry(
            |session| {
                calls += 1;
                if session.token == "old" {
                    Err("主控 HTTP 401；登录会话已失效".into())
                } else {
                    Ok(session.token.clone())
                }
            },
            |auth| auth.refresh_with(|| Ok(session("renewed"))),
        );
        assert_eq!(result.unwrap(), "renewed");
        assert_eq!(calls, 2);
        let mut calls = 0;
        assert!(context
            .call_retry::<()>(
                |_| {
                    calls += 1;
                    Err("主控 HTTP 401".into())
                },
                |auth| auth.refresh_with(|| Ok(session("third")))
            )
            .is_err());
        assert_eq!(calls, 2);
    }
}
