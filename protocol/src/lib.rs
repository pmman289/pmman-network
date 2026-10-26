use serde::{Deserialize, Serialize};
pub mod dns;
pub mod geosite;
pub mod invitations;
pub mod p2p;
pub mod release;
pub use invitations::*;

/// Per-client node bandwidth in decimal Mbps. Zero means unlimited.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeRateLimit {
    pub upload_mbps: u32,
    pub download_mbps: u32,
}
impl NodeRateLimit {
    pub fn validate(&self) -> Result<(), String> {
        if self.upload_mbps > 100_000 || self.download_mbps > 100_000 {
            Err("节点上下行限速必须为 0–100000 的整数 Mbps（0 表示不限速）".into())
        } else {
            Ok(())
        }
    }
}

/// Cumulative traffic counters reported by the client. Counters are monotonic
/// for one device session and are kept separate by transport path so the UI
/// and control plane can distinguish node traffic from P2P traffic.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficStats {
    pub node_tx_bytes: u64,
    pub node_rx_bytes: u64,
    pub p2p_direct_tx_bytes: u64,
    pub p2p_direct_rx_bytes: u64,
    pub p2p_relay_tx_bytes: u64,
    pub p2p_relay_rx_bytes: u64,
}

impl TrafficStats {
    pub fn accumulate(&mut self, other: Self) {
        self.node_tx_bytes = self.node_tx_bytes.saturating_add(other.node_tx_bytes);
        self.node_rx_bytes = self.node_rx_bytes.saturating_add(other.node_rx_bytes);
        self.p2p_direct_tx_bytes = self
            .p2p_direct_tx_bytes
            .saturating_add(other.p2p_direct_tx_bytes);
        self.p2p_direct_rx_bytes = self
            .p2p_direct_rx_bytes
            .saturating_add(other.p2p_direct_rx_bytes);
        self.p2p_relay_tx_bytes = self
            .p2p_relay_tx_bytes
            .saturating_add(other.p2p_relay_tx_bytes);
        self.p2p_relay_rx_bytes = self
            .p2p_relay_rx_bytes
            .saturating_add(other.p2p_relay_rx_bytes);
    }

    pub fn delta_since(self, previous: Self) -> Self {
        Self {
            node_tx_bytes: self.node_tx_bytes.saturating_sub(previous.node_tx_bytes),
            node_rx_bytes: self.node_rx_bytes.saturating_sub(previous.node_rx_bytes),
            p2p_direct_tx_bytes: self
                .p2p_direct_tx_bytes
                .saturating_sub(previous.p2p_direct_tx_bytes),
            p2p_direct_rx_bytes: self
                .p2p_direct_rx_bytes
                .saturating_sub(previous.p2p_direct_rx_bytes),
            p2p_relay_tx_bytes: self
                .p2p_relay_tx_bytes
                .saturating_sub(previous.p2p_relay_tx_bytes),
            p2p_relay_rx_bytes: self
                .p2p_relay_rx_bytes
                .saturating_sub(previous.p2p_relay_rx_bytes),
        }
    }

    pub fn retain_max(&mut self, other: Self) {
        self.node_tx_bytes = self.node_tx_bytes.max(other.node_tx_bytes);
        self.node_rx_bytes = self.node_rx_bytes.max(other.node_rx_bytes);
        self.p2p_direct_tx_bytes = self.p2p_direct_tx_bytes.max(other.p2p_direct_tx_bytes);
        self.p2p_direct_rx_bytes = self.p2p_direct_rx_bytes.max(other.p2p_direct_rx_bytes);
        self.p2p_relay_tx_bytes = self.p2p_relay_tx_bytes.max(other.p2p_relay_tx_bytes);
        self.p2p_relay_rx_bytes = self.p2p_relay_rx_bytes.max(other.p2p_relay_rx_bytes);
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardQuota {
    pub enabled: bool,
    pub limit: u32,
    pub used: u32,
    pub available: u32,
}

pub const WINDOWS_PIPE_NAME: &str = r"\\.\pipe\pmman-network";
pub const UNIX_SOCKET_PATH: &str = "/tmp/pmman-network.sock";
pub const UDP_PING_MAGIC: &[u8; 8] = b"PMMPING1";
pub const UDP_PING_SIZE: usize = 24;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PortForwardProtocol {
    Tcp,
    Udp,
}

impl PortForwardProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NodePortForward {
    pub forward_id: String,
    pub config_version: u64,
    pub protocols: Vec<PortForwardProtocol>,
    pub listen_address: String,
    pub listen_port: u16,
    pub target_address: String,
    pub target_port: u16,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppliedPortForward {
    pub forward_id: String,
    pub config_version: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PortForwardSyncError {
    pub forward_id: String,
    pub config_version: u64,
    pub error: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PortForwardEntryView {
    pub entry_id: String,
    pub node_id: String,
    pub name: String,
    pub public_address: String,
    pub hostname: Option<String>,
    pub family: String,
    pub protocols: Vec<PortForwardProtocol>,
    pub port_ranges: Vec<PortForwardRange>,
    pub reserved_ports: Vec<u16>,
    pub enabled: bool,
    pub node_healthy: bool,
    pub total_ports: u32,
    pub allocated_ports: u32,
    pub available_ports: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PortForwardRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PortForwardView {
    pub forward_id: String,
    pub user_id: String,
    pub username: String,
    pub device_id: String,
    pub entry_id: String,
    pub node_id: String,
    pub protocols: Vec<PortForwardProtocol>,
    pub public_address: String,
    pub public_host: Option<String>,
    pub public_port: u16,
    pub target_address: String,
    pub target_port: u16,
    pub state: String,
    pub config_version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct NodeNetworkConfig {
    pub ipv4_split_routes: Option<Vec<String>>,
    pub ipv6_split_routes: Option<Vec<String>>,
    pub dns_servers: Vec<String>,
    /// Upstream resolvers used by the node-local pmmandns forwarder. Values
    /// may be IP addresses or IP:port endpoints; port 53 is assumed when it
    /// is omitted.
    #[serde(default)]
    pub pmmandns_upstreams: Vec<String>,
    /// mrouter-compatible domestic resolver group.
    #[serde(default)]
    pub pmmandns_domestic_upstreams: Vec<String>,
    /// mrouter-compatible foreign resolver group.
    #[serde(default)]
    pub pmmandns_foreign_upstreams: Vec<String>,
    /// Domain suffixes routed to the domestic resolver group. Entries may be
    /// plain suffixes or mrouter-style `domain:example.com` lines.
    #[serde(default)]
    pub pmmandns_domestic_rules: Vec<String>,
    /// Domain suffixes routed to the foreign resolver group.
    #[serde(default)]
    pub pmmandns_foreign_rules: Vec<String>,
    /// Resolver group for domains not covered by a rule. Defaults to foreign.
    #[serde(default)]
    pub pmmandns_default_group: String,
    /// Optional longest-suffix DNS routing rules. A matching rule is tried
    /// before the node's default pmmandns upstreams.
    #[serde(default)]
    pub pmmandns_rules: Vec<dns::RouteRule>,
    /// Master-managed geosite data used by the node-local pmmandns resolver.
    /// The binary data is fetched separately from the master; only its
    /// authenticated manifest is carried in the heartbeat/config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pmmandns_geosite: Option<GeositeConfig>,
    /// Opt-in per node: installing a data file must not silently enable DNS.
    #[serde(default)]
    pub pmmandns_geosite_enabled: bool,
    /// The addresses configured on the node's WireGuard gateway interface.
    /// They are reported by the node and must not be inferred by clients from
    /// the address-pool defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_ipv4: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_ipv6: Option<String>,
}

impl NodeNetworkConfig {
    fn gateway_dns_requested(&self) -> bool {
        self.dns_servers.iter().any(|dns| {
            dns.parse::<std::net::IpAddr>().ok().is_some_and(|ip| {
                [self.gateway_ipv4.as_deref(), self.gateway_ipv6.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|cidr| cidr.split('/').next()?.parse::<std::net::IpAddr>().ok())
                    .any(|gateway| gateway == ip)
            })
        })
    }

    /// Generate an editable initial policy only when no upstream was chosen.
    /// Never overwrite a custom group or enable DNS on an opted-out node.
    pub fn apply_managed_dns_defaults(&mut self) -> bool {
        if !self.pmmandns_upstreams.is_empty()
            || !self.pmmandns_domestic_upstreams.is_empty()
            || !self.pmmandns_foreign_upstreams.is_empty()
            || !(self.gateway_dns_requested() || self.managed_dns_enabled())
        {
            return false;
        }
        self.pmmandns_domestic_upstreams = vec!["223.5.5.5".into(), "114.114.114.114".into()];
        self.pmmandns_foreign_upstreams = vec!["8.8.8.8".into(), "1.1.1.1".into()];
        if self.pmmandns_default_group.trim().is_empty() {
            self.pmmandns_default_group = "foreign".into();
        }
        true
    }

    pub fn managed_dns_enabled(&self) -> bool {
        !self.pmmandns_upstreams.is_empty()
            || !self.pmmandns_domestic_upstreams.is_empty()
            || !self.pmmandns_foreign_upstreams.is_empty()
            || !self.pmmandns_domestic_rules.is_empty()
            || !self.pmmandns_foreign_rules.is_empty()
            || !self.pmmandns_rules.is_empty()
            || self.pmmandns_geosite.is_some()
            || self.pmmandns_geosite_enabled
    }

    /// External DNS servers remain valid without a local resolver. A gateway
    /// advertised as DNS, however, must have a complete managed resolver policy.
    pub fn validate_managed_dns(&self) -> Result<(), String> {
        if !self.managed_dns_enabled() {
            return if self.gateway_dns_requested() {
                Err("节点网关被配置为 DNS，但未配置 pmmandns 上游；请配置国内和国外上游 DNS，或使用外部 DNS 地址".into())
            } else {
                Ok(())
            };
        }
        if self.pmmandns_upstreams.is_empty()
            && (self.pmmandns_domestic_upstreams.is_empty()
                || self.pmmandns_foreign_upstreams.is_empty())
        {
            return Err("pmmandns 必须同时配置国内和国外上游 DNS，或配置兼容的默认上游 DNS".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod managed_dns_tests {
    use super::*;

    #[test]
    fn defaults_repair_legacy_gateway_dns_once_without_overwriting_custom_policy() {
        let mut network = NodeNetworkConfig {
            gateway_ipv4: Some("10.88.0.1/24".into()),
            dns_servers: vec!["10.88.0.1".into()],
            ..Default::default()
        };
        assert!(network.apply_managed_dns_defaults());
        assert!(network.validate_managed_dns().is_ok());
        let once = network.clone();
        assert!(!network.apply_managed_dns_defaults());
        assert_eq!(network, once);
        network.pmmandns_foreign_upstreams = vec!["192.0.2.53".into()];
        assert!(!network.apply_managed_dns_defaults());
        assert_eq!(network.pmmandns_foreign_upstreams, vec!["192.0.2.53"]);
        network.pmmandns_domestic_upstreams.clear();
        assert!(!network.apply_managed_dns_defaults());
        assert!(network.validate_managed_dns().is_err());
        assert!(!NodeNetworkConfig::default().apply_managed_dns_defaults());
        network = NodeNetworkConfig {
            dns_servers: vec!["192.0.2.53".into()],
            ..Default::default()
        };
        assert!(!network.apply_managed_dns_defaults());
    }

    #[test]
    fn gateway_dns_requires_upstreams_for_both_address_families() {
        for dns in ["10.88.0.1", "fd88::1", "fd88:0::1"] {
            let mut network = NodeNetworkConfig {
                gateway_ipv4: Some("10.88.0.1/24".into()),
                gateway_ipv6: Some("fd88::1/64".into()),
                dns_servers: vec![dns.into()],
                ..Default::default()
            };
            assert!(network.validate_managed_dns().is_err());
            network.pmmandns_domestic_upstreams = vec!["223.5.5.5".into()];
            assert!(network.validate_managed_dns().is_err());
            network.pmmandns_foreign_upstreams = vec!["2001:4860:4860::8888".into()];
            assert!(network.validate_managed_dns().is_ok());
        }
    }

    #[test]
    fn external_dns_and_legacy_default_upstreams_remain_compatible() {
        let mut network = NodeNetworkConfig {
            gateway_ipv4: Some("10.88.0.1/24".into()),
            dns_servers: vec!["192.0.2.53".into()],
            ..Default::default()
        };
        assert!(network.validate_managed_dns().is_ok());
        assert!(!network.managed_dns_enabled());
        network.dns_servers = vec!["10.88.0.1".into()];
        network.pmmandns_upstreams = vec!["192.0.2.53".into()];
        assert!(network.validate_managed_dns().is_ok());
        network.pmmandns_upstreams.clear();
        network.pmmandns_geosite_enabled = true;
        assert!(network.managed_dns_enabled());
        assert!(network.validate_managed_dns().is_err());
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GeositeConfig {
    pub version: String,
    pub sha256: String,
    pub size: u64,
    pub domestic: String,
    pub foreign: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientDnsPolicy {
    #[serde(default)]
    pub geosite: Option<GeositeConfig>,
    #[serde(default)]
    pub domestic_rules: Vec<String>,
    #[serde(default)]
    pub foreign_rules: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Invitation,
    Status,
    CheckUpdate,
    DownloadUpdate,
    UploadDiagnostics,
    Login,
    ChangePassword,
    ListNodes,
    SelectNode,
    SetTrafficMode,
    SetP2p,
    ListPortForwardEntries,
    ListPortForwardQuota,
    ListPortForwards,
    CreatePortForward,
    ReleasePortForward,
    Logout,
    Connect,
    CancelConnect,
    Disconnect,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcRequest {
    #[serde(default)]
    pub invitation_request: Option<InvitationRequest>,
    pub action: Action,
    pub username: Option<String>,
    pub password: Option<String>,
    pub current_password: Option<String>,
    pub new_password: Option<String>,
    pub node_id: Option<String>,
    pub ipv4_mode: Option<String>,
    pub ipv6_mode: Option<String>,
    pub p2p_enabled: Option<bool>,
    pub update_channel: Option<String>,
    pub entry_id: Option<String>,
    pub device_id: Option<String>,
    pub forward_id: Option<String>,
    pub protocols: Option<Vec<PortForwardProtocol>>,
    pub target_port: Option<u16>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdate {
    #[serde(default)]
    pub release_notes: Vec<String>,
    pub current_version: String,
    pub available_version: Option<String>,
    pub state: String,
    pub progress: u8,
    pub message: String,
    pub ready: bool,
    pub installer_path: Option<String>,
    pub sha256: Option<String>,
    pub mandatory: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub node_id: String,
    pub name: String,
    pub wireguard_endpoint: String,
    pub probe_endpoint: String,
    pub region: Option<String>,
    pub healthy: bool,
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub rate_limit: NodeRateLimit,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSnapshot {
    /// Only populated in the response to an invitation request, never cached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitation_result: Option<InvitationResult>,
    #[serde(default)]
    pub client_update: ClientUpdate,
    /// The service restores the encrypted Windows login state after the IPC
    /// listener is ready. Keep the UI in its startup state until that attempt
    /// has completed instead of briefly showing a manual login form.
    #[serde(default)]
    pub auth_restore_pending: bool,
    #[serde(default)]
    pub node_rate_limit: NodeRateLimit,
    pub state: String,
    #[serde(default)]
    pub connection_waiting_cleanup: bool,
    pub authenticated: bool,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub role: Option<String>,
    pub device_id: Option<String>,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub message: String,
    pub tunnel_mode: Option<String>,
    pub nodes: Vec<NodeInfo>,
    pub selected_node_id: Option<String>,
    pub selected_node_name: Option<String>,
    pub node_selection: String,
    pub ipv4_mode: String,
    pub ipv6_mode: String,
    pub allow_ipv4_global: bool,
    pub allow_ipv6_global: bool,
    pub allow_ipv4_split: bool,
    pub allow_ipv6_split: bool,
    pub allowed_ips: Vec<String>,
    /// Details of the built-in cnipgen route snapshot used for split mode.
    #[serde(default)]
    pub split_route_source: Option<String>,
    #[serde(default)]
    pub split_route_version: Option<String>,
    #[serde(default)]
    pub split_route_snapshot: Option<String>,
    #[serde(default)]
    pub split_ipv4_prefix_count: u32,
    #[serde(default)]
    pub split_ipv6_prefix_count: u32,
    #[serde(default)]
    pub split_route_install_ms: u64,
    /// DNS servers advertised by the selected node, before local fallback.
    /// This is kept separate so the UI can explain which resolver is active.
    #[serde(default)]
    pub node_dns_servers: Vec<String>,
    /// Resolver addresses shown in connection diagnostics.
    pub dns_servers: Vec<String>,
    #[serde(default)]
    pub dns_split_enabled: bool,
    #[serde(default = "p2p::default_enabled")]
    pub p2p_enabled: bool,
    #[serde(default)]
    pub p2p_peers: Vec<p2p::PeerStatus>,
    #[serde(default)]
    pub p2p_message: String,
    #[serde(default)]
    pub port_forward_entries: Vec<PortForwardEntryView>,
    #[serde(default)]
    pub port_forward_quota: PortForwardQuota,
    #[serde(default)]
    pub traffic_stats: TrafficStats,
    #[serde(default)]
    pub port_forwards: Vec<PortForwardView>,
    #[serde(default)]
    pub port_forward_message: String,
    #[serde(default)]
    pub port_forward_firewall_warning: String,
    #[serde(default)]
    pub diagnostic_upload_state: String,
    #[serde(default)]
    pub diagnostic_upload_message: String,
    #[serde(default)]
    pub diagnostic_upload_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub status: ClientSnapshot,
}

impl IpcRequest {
    pub fn new(action: Action) -> Self {
        Self {
            action,
            invitation_request: None,
            username: None,
            password: None,
            current_password: None,
            new_password: None,
            node_id: None,
            ipv4_mode: None,
            ipv6_mode: None,
            p2p_enabled: None,
            update_channel: None,
            entry_id: None,
            device_id: None,
            forward_id: None,
            protocols: None,
            target_port: None,
        }
    }

    pub fn login(username: String, password: String) -> Self {
        Self {
            action: Action::Login,
            invitation_request: None,
            username: Some(username),
            password: Some(password),
            current_password: None,
            new_password: None,
            node_id: None,
            ipv4_mode: None,
            ipv6_mode: None,
            p2p_enabled: None,
            update_channel: None,
            entry_id: None,
            device_id: None,
            forward_id: None,
            protocols: None,
            target_port: None,
        }
    }

    pub fn select_node(node_id: Option<String>) -> Self {
        Self {
            action: Action::SelectNode,
            invitation_request: None,
            username: None,
            password: None,
            current_password: None,
            new_password: None,
            node_id,
            ipv4_mode: None,
            ipv6_mode: None,
            p2p_enabled: None,
            update_channel: None,
            entry_id: None,
            device_id: None,
            forward_id: None,
            protocols: None,
            target_port: None,
        }
    }

    pub fn set_traffic_mode(ipv4_mode: String, ipv6_mode: String) -> Self {
        Self {
            action: Action::SetTrafficMode,
            invitation_request: None,
            username: None,
            password: None,
            current_password: None,
            new_password: None,
            node_id: None,
            ipv4_mode: Some(ipv4_mode),
            ipv6_mode: Some(ipv6_mode),
            p2p_enabled: None,
            update_channel: None,
            entry_id: None,
            device_id: None,
            forward_id: None,
            protocols: None,
            target_port: None,
        }
    }

    pub fn set_p2p(enabled: bool) -> Self {
        let mut request = Self::new(Action::SetP2p);
        request.p2p_enabled = Some(enabled);
        request
    }

    pub fn change_password(current_password: String, new_password: String) -> Self {
        let mut request = Self::new(Action::ChangePassword);
        request.current_password = Some(current_password);
        request.new_password = Some(new_password);
        request
    }

    pub fn update(action: Action, channel: String) -> Self {
        let mut request = Self::new(action);
        request.update_channel = Some(channel);
        request
    }
}
