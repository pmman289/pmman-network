use super::*;
use pmman_protocol::{InvitationQuery, InvitationRequest, InvitationResult};

fn query_url(path: &str, query: &InvitationQuery) -> Result<String, String> {
    // Percent-encode all query values without relying on user-provided paths.
    let fields = serde_json::to_value(query).map_err(|_| "查询参数无效".to_string())?;
    let parts = fields
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let value = v
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string());
            let encoded = value
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>();
            format!("{k}={encoded}")
        })
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!("{}{path}?{parts}", control_plane_url()?))
}
fn execute(request: &InvitationRequest, token: Option<&str>) -> Result<InvitationResult, String> {
    let base = control_plane_url()?;
    let authorization = format!("Bearer {}", token.unwrap_or_default());
    let response = match request {
        InvitationRequest::Register {
            code,
            username,
            password,
        } => control_agent()
            .post(&format!("{base}/v1/auth/register"))
            .send_json(serde_json::json!({"code":code,"username":username,"password":password})),
        InvitationRequest::Policy {} => control_agent()
            .get(&format!("{base}/v1/client/invitations/policy"))
            .header("Authorization", &authorization)
            .call(),
        InvitationRequest::List { query } => control_agent()
            .get(&query_url("/v1/client/invitations", query)?)
            .header("Authorization", &authorization)
            .call(),
        InvitationRequest::Create {} => control_agent()
            .post(&format!("{base}/v1/client/invitations"))
            .header("Authorization", &authorization)
            .send_empty(),
        InvitationRequest::Revoke { id } => {
            uuid::Uuid::parse_str(id).map_err(|_| "邀请码标识无效".to_string())?;
            control_agent()
                .post(&format!("{base}/v1/client/invitations/{id}/revoke"))
                .header("Authorization", &authorization)
                .send_empty()
        }
        InvitationRequest::Registrations { query } => control_agent()
            .get(&query_url("/v1/client/invitation-registrations", query)?)
            .header("Authorization", &authorization)
            .call(),
    }
    .map_err(|_| match request {
        InvitationRequest::Register { .. } => {
            "注册请求未完成；如果账号已经创建，请尝试登录".to_string()
        }
        _ => "无法访问邀请服务，请稍后重试".to_string(),
    })?;
    let status = response.status().as_u16();
    let mut body = response.into_body();
    if !(200..300).contains(&status) {
        if status >= 500 && matches!(request, InvitationRequest::Register { .. }) {
            return Err("注册服务暂不可用；如刚提交过注册，请先尝试登录".into());
        }
        let message = body
            .read_json::<ControlPlaneError>()
            .map(|v| v.error)
            .unwrap_or_else(|_| "服务请求失败".into());
        return Err(format!("主控 HTTP {status}；{message}"));
    }
    if matches!(request, InvitationRequest::Revoke { .. }) {
        return Ok(InvitationResult::Revoked);
    }
    match request {
        InvitationRequest::Register { username, .. } => Ok(InvitationResult::Registered {
            username: username.clone(),
        }),
        InvitationRequest::Policy {} => body
            .read_json()
            .map(InvitationResult::Policy)
            .map_err(|_| "邀请授权响应无效".into()),
        InvitationRequest::List { .. } => body
            .read_json()
            .map(InvitationResult::List)
            .map_err(|_| "邀请码列表响应无效".into()),
        InvitationRequest::Create {} => body
            .read_json::<pmman_protocol::CreatedInvitation>()
            .map(|mut value| {
                value.registration_url = Some(format!(
                    "{}{}",
                    base.trim_end_matches('/'),
                    value.registration_path
                ));
                InvitationResult::Created(value)
            })
            .map_err(|_| "邀请码响应未完成，请在列表中检查；完整邀请码不能恢复".into()),
        InvitationRequest::Registrations { .. } => body
            .read_json()
            .map(InvitationResult::Registrations)
            .map_err(|_| "邀请记录响应无效".into()),
        InvitationRequest::Revoke { .. } => unreachable!(),
    }
}
pub fn handle(state: &ServiceState, request: Option<InvitationRequest>) -> IpcResponse {
    let operation = match request.as_ref() {
        Some(InvitationRequest::Register { .. }) => "register",
        Some(InvitationRequest::Policy {}) => "policy",
        Some(InvitationRequest::List { .. }) => "list",
        Some(InvitationRequest::Create {}) => "create",
        Some(InvitationRequest::Revoke { .. }) => "revoke",
        Some(InvitationRequest::Registrations { .. }) => "registrations",
        None => "invalid",
    };
    let mut snapshot = state.snapshot.lock().unwrap().clone();
    snapshot.invitation_result = None;
    let result = (|| {
        let request = request.ok_or_else(|| "邀请请求缺少参数".to_string())?;
        if matches!(&request, InvitationRequest::Register { .. }) {
            if snapshot.authenticated {
                return Err("请先退出当前账户再注册".into());
            }
            return execute(&request, None);
        }
        let session = state
            .auth
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "请先登录".to_string())?;
        match execute(&request, Some(&session.token)) {
            Err(e) if is_session_expired_error(&e) => {
                let refreshed = refresh_authenticated_session(state, &mut snapshot)?;
                execute(&request, Some(&refreshed.token))
            }
            result => result,
        }
    })();
    diagnostics::log(
        "invitation",
        &format!(
            "operation={operation} result={}",
            if result.is_ok() { "ok" } else { "error" }
        ),
    );
    response(snapshot, result)
}
fn response(mut snapshot: ClientSnapshot, result: Result<InvitationResult, String>) -> IpcResponse {
    // Do not cache a created invitation or mutate the tunnel state on errors.
    match result {
        Ok(value) => {
            snapshot.invitation_result = Some(value);
            IpcResponse {
                ok: true,
                status: snapshot,
            }
        }
        Err(error) => {
            snapshot.message = error;
            IpcResponse {
                ok: false,
                status: snapshot,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_preserve_connection_and_invitation_results_are_transient() {
        let snapshot = ClientSnapshot {
            state: "connected".into(),
            message: "connected".into(),
            ..Default::default()
        };
        let result = response(snapshot.clone(), Err("invitation error".into()));
        assert!(!result.ok);
        assert_eq!(result.status.state, "connected");
        assert_eq!(snapshot.message, "connected");
        let result = response(
            snapshot.clone(),
            Ok(InvitationResult::Registered {
                username: "new-user".into(),
            }),
        );
        assert!(result.status.invitation_result.is_some());
        assert!(snapshot.invitation_result.is_none());
        assert!(!serde_json::to_string(&snapshot)
            .unwrap()
            .contains("invitationResult"));
    }
}
