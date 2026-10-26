use crate::PortForwardProtocol;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationForwardGrant {
    pub enabled: bool,
    pub max_active_forwards: u32,
    pub allowed_entry_ids: Vec<String>,
    pub allowed_protocols: Vec<PortForwardProtocol>,
    pub allowed_target_ports: Option<Vec<u16>>,
    pub allowed_families: Vec<String>,
}
impl Default for InvitationForwardGrant {
    fn default() -> Self {
        Self {
            enabled: false,
            max_active_forwards: 0,
            allowed_entry_ids: vec![],
            allowed_protocols: vec![PortForwardProtocol::Tcp],
            allowed_target_ports: None,
            allowed_families: vec!["ipv4".into(), "ipv6".into()],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationGrant {
    pub allow_ipv4_global: bool,
    pub allow_ipv6_global: bool,
    pub allow_ipv4_split: bool,
    pub allow_ipv6_split: bool,
    pub port_forward: InvitationForwardGrant,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationPolicy {
    pub enabled: bool,
    pub max_unused: u32,
    pub daily_limit: u32,
    pub valid_days: u32,
    pub grant: InvitationGrant,
}
impl Default for InvitationPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_unused: 10,
            daily_limit: 20,
            valid_days: 7,
            grant: InvitationGrant::default(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitationPolicyView {
    pub account_enabled: bool,
    pub policy: InvitationPolicy,
    pub revision: i64,
    pub unused: i64,
    pub generated_today: i64,
    pub remaining_unused: i64,
    pub remaining_today: i64,
    pub resets_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitationView {
    pub id: String,
    pub prefix: String,
    pub inviter_username: String,
    pub creator_username: String,
    pub source: String,
    pub kind: String,
    pub state: String,
    pub note: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub use_count: i64,
    pub grant: InvitationGrant,
    pub policy_revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitationRegistration {
    pub id: String,
    pub invitation_id: String,
    pub prefix: String,
    pub inviter_username: String,
    pub username: String,
    pub created_at: String,
    pub grant: InvitationGrant,
    pub policy_revision: i64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InvitationQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub inviter: Option<String>,
    pub username: Option<String>,
    pub kind: Option<String>,
    pub state: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitationPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatedInvitation {
    pub invitation: InvitationView,
    pub code: String,
    /// Relative to the configured master origin; no forwarded Host is trusted.
    pub registration_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvitationRequest {
    Register {
        code: String,
        username: String,
        password: String,
    },
    Policy {},
    List {
        query: InvitationQuery,
    },
    Create {},
    Revoke {
        id: String,
    },
    Registrations {
        query: InvitationQuery,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum InvitationResult {
    Registered { username: String },
    Policy(InvitationPolicyView),
    List(InvitationPage<InvitationView>),
    Created(CreatedInvitation),
    Revoked,
    Registrations(InvitationPage<InvitationRegistration>),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_ipc_and_registration_reject_elevation() {
        let old: crate::IpcRequest = serde_json::from_str(r#"{"action":"status"}"#).unwrap();
        assert!(old.invitation_request.is_none());
        assert!(serde_json::from_value::<InvitationRequest>(
            serde_json::json!({"operation":"create","role":"admin"})
        )
        .is_err());
        let mut request = crate::IpcRequest::new(crate::Action::Invitation);
        request.invitation_request = Some(InvitationRequest::Revoke { id: "test".into() });
        assert!(serde_json::from_str::<crate::IpcRequest>(
            &serde_json::to_string(&request).unwrap()
        )
        .unwrap()
        .invitation_request
        .is_some());
    }
}
