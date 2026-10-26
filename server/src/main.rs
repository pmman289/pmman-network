use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use pmman_protocol::{NodeNetworkConfig, NodeRateLimit};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, OnceLock, RwLock},
};
use uuid::Uuid;
mod address_pools;
mod db;
mod diagnostic_admin;
mod invitations;
#[cfg(test)]
mod node_lock_tests;
mod p2p;
mod port_forward;
mod user_deletion;

static ADMIN_ASSETS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/admin-dist");

const DEFAULT_IPV4_POOL: &str = "10.88.0.0/24";
const DEFAULT_IPV6_POOL: &str = "fd88:1000::/64";
const DEFAULT_INTERNAL_CIDRS: &[&str] = &["10.20.0.0/16", "fd20:1000::/48"];
// The client renews every ten seconds. A crashed client must not leave an
// authorized gateway key forever; preserve its address lease for reconnect.
const CLIENT_AUTH_LEASE_SECONDS: i64 = 60;
static CLIENT_RELEASE_MUTATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static DIAGNOSTIC_UPLOADS: OnceLock<Mutex<HashMap<String, DateTime<Utc>>>> = OnceLock::new();

const MAX_CLIENT_DIAGNOSTIC_BYTES: usize = 768 * 1024;
const DIAGNOSTIC_UPLOAD_INTERVAL_SECONDS: i64 = 60;
const MAX_DIAGNOSTIC_REPORTS_PER_USER: usize = 5;

fn client_release_mutation_lock() -> &'static Mutex<()> {
    CLIENT_RELEASE_MUTATION_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone)]
struct AppState {
    database: Option<Arc<db::PgStore>>,
    p2p: Arc<RwLock<HashMap<Uuid, p2p::Registration>>>,
    devices: Arc<RwLock<HashMap<Uuid, Device>>>,
    users: Arc<RwLock<HashMap<String, UserAccount>>>,
    user_store: Option<PathBuf>,
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    pool: Arc<RwLock<AddressPool>>,
    address_pools: Arc<RwLock<Option<address_pools::Registry>>>,
    internal_cidrs: Arc<RwLock<Vec<String>>>,
    settings_store: Option<PathBuf>,
    nodes: Arc<RwLock<HashMap<String, Node>>>,
    node_store: Option<PathBuf>,
    node_resource_dir: Option<PathBuf>,
    port_forward_store: port_forward::PortForwardStore,
    geosite_dir: Option<PathBuf>,
    client_release_dir: Option<PathBuf>,
    client_release_private_key: Option<String>,
    diagnostic_dir: Option<PathBuf>,
    gateway: GatewayConfig,
}

#[derive(Clone)]
struct GatewayConfig {
    interface: Option<String>,
    server_public_key: String,
    endpoint: String,
    wg_binary: String,
}

impl GatewayConfig {
    fn from_environment() -> Self {
        Self {
            interface: env::var("PMMAN_WG_INTERFACE")
                .ok()
                .filter(|value| !value.is_empty()),
            server_public_key: env::var("PMMAN_WG_SERVER_PUBLIC_KEY").unwrap_or_default(),
            endpoint: env::var("PMMAN_WG_ENDPOINT").unwrap_or_default(),
            wg_binary: env::var("PMMAN_WG_CLI").unwrap_or_else(|_| "wg".into()),
        }
    }
}

#[derive(Clone)]
struct AddressPool {
    ipv4_network: u32,
    ipv4_prefix: u8,
    ipv6_network: u128,
    ipv6_prefix: u8,
}

impl AddressPool {
    fn from_environment() -> Result<Self, String> {
        Self::from_cidrs(
            &env::var("PMMAN_IPV4_POOL").unwrap_or_else(|_| DEFAULT_IPV4_POOL.into()),
            &env::var("PMMAN_IPV6_POOL").unwrap_or_else(|_| DEFAULT_IPV6_POOL.into()),
        )
    }

    fn from_cidrs(ipv4: &str, ipv6: &str) -> Result<Self, String> {
        let (ipv4_network, ipv4_prefix) = parse_ipv4_cidr(ipv4)?;
        let (ipv6_network, ipv6_prefix) = parse_ipv6_cidr(ipv6)?;
        if ipv4_prefix > 30 {
            return Err(
                "客户端 IPv4 地址池前缀必须为 /0 至 /30（排除网络、广播及网关地址）".into(),
            );
        }
        let ipv4_host_bits = 32 - ipv4_prefix;
        let ipv6_host_bits = 128 - ipv6_prefix;
        if ipv6_host_bits < ipv4_host_bits {
            return Err("IPv6 地址池容量不能小于 IPv4 地址池".into());
        }
        Ok(Self {
            ipv4_network,
            ipv4_prefix,
            ipv6_network,
            ipv6_prefix,
        })
    }

    fn ipv4_cidr(&self) -> String {
        format!("{}/{}", Ipv4Addr::from(self.ipv4_network), self.ipv4_prefix)
    }

    fn ipv6_cidr(&self) -> String {
        format!("{}/{}", Ipv6Addr::from(self.ipv6_network), self.ipv6_prefix)
    }

    fn capacity(&self) -> u64 {
        self.total_addresses() - 3
    }

    fn total_addresses(&self) -> u64 {
        1u64 << (32 - self.ipv4_prefix)
    }

    fn usable_offset(&self, offset: u64) -> bool {
        // Offset zero is the subnet, one is reserved for the gateway, and the
        // final IPv4 address is the broadcast. IPv6 has no broadcast address.
        offset >= 2 && offset < self.total_addresses() - 1
    }

    fn contains(&self, ipv4: &str, ipv6: &str) -> bool {
        let Ok(v4) = ipv4.parse::<Ipv4Addr>() else {
            return false;
        };
        let Ok(v6) = ipv6.parse::<Ipv6Addr>() else {
            return false;
        };
        let v4 = u32::from(v4);
        let v6 = u128::from(v6);
        let v4_mask = prefix_mask4(self.ipv4_prefix);
        let v6_mask = prefix_mask6(self.ipv6_prefix);
        (v4 & v4_mask) == self.ipv4_network
            && (v6 & v6_mask) == self.ipv6_network
            && self.usable_offset(u64::from(v4 - self.ipv4_network))
            && v6 - self.ipv6_network >= 2
    }

    fn validate_pool_pair(&self, ipv4: &str, ipv6: &str) -> Result<(), String> {
        if ipv4.contains('/') || ipv6.contains('/') {
            return Err("固定地址必须填写单个 IPv4/IPv6 地址，不能带 CIDR 前缀".into());
        }
        let v4 = ipv4
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("固定 IPv4 地址无效：{ipv4}"))?;
        let v6 = ipv6
            .parse::<Ipv6Addr>()
            .map_err(|_| format!("固定 IPv6 地址无效：{ipv6}"))?;
        let v4_value = u32::from(v4);
        let v6_value = u128::from(v6);
        if (v4_value & prefix_mask4(self.ipv4_prefix)) != self.ipv4_network {
            return Err(format!(
                "固定 IPv4 地址不在当前地址池 {} 内",
                self.ipv4_cidr()
            ));
        }
        if (v6_value & prefix_mask6(self.ipv6_prefix)) != self.ipv6_network {
            return Err(format!(
                "固定 IPv6 地址不在当前地址池 {} 内",
                self.ipv6_cidr()
            ));
        }
        let v4_offset = u64::from(v4_value - self.ipv4_network);
        if !self.usable_offset(v4_offset) {
            return Err("固定 IPv4 地址不能使用网络地址、网关地址或广播地址".into());
        }
        let v6_offset = v6_value - self.ipv6_network;
        if v6_offset < 2 || v6_offset >= u128::from(self.total_addresses()) {
            return Err("固定 IPv6 地址不能使用子网地址或网关地址".into());
        }
        Ok(())
    }

    fn complete_pair(
        &self,
        ipv4: Option<&str>,
        ipv6: Option<&str>,
    ) -> Result<(String, String), String> {
        let (ipv4, ipv6) = match (ipv4, ipv6) {
            (Some(v4), Some(v6)) => (v4.to_string(), v6.to_string()),
            (Some(v4), None) => {
                let value = u32::from(v4.parse::<Ipv4Addr>().map_err(|_| "IPv4 地址格式无效")?);
                let offset = value
                    .checked_sub(self.ipv4_network)
                    .ok_or("IPv4 地址不在池内")?;
                let v6 = self
                    .ipv6_network
                    .checked_add(u128::from(offset))
                    .ok_or("IPv6 地址溢出")?;
                (v4.to_string(), Ipv6Addr::from(v6).to_string())
            }
            (None, Some(v6)) => {
                let value = u128::from(v6.parse::<Ipv6Addr>().map_err(|_| "IPv6 地址格式无效")?);
                let offset = value
                    .checked_sub(self.ipv6_network)
                    .and_then(|offset| u32::try_from(offset).ok())
                    .ok_or("无法按 IPv6 偏移推导 IPv4，请同时指定 IPv4")?;
                let v4 = self
                    .ipv4_network
                    .checked_add(offset)
                    .ok_or("无法推导 IPv4，请同时指定 IPv4")?;
                (Ipv4Addr::from(v4).to_string(), v6.to_string())
            }
            _ => return Err("没有指定地址".into()),
        };
        self.validate_pool_pair(&ipv4, &ipv6)?;
        Ok((
            ipv4.parse::<Ipv4Addr>().unwrap().to_string(),
            ipv6.parse::<Ipv6Addr>().unwrap().to_string(),
        ))
    }

    fn allocate_dual(
        &self,
        used4: &HashSet<u32>,
        used6: &HashSet<u128>,
    ) -> Option<(String, String)> {
        // Map occupied IPv6 offsets into candidate IPv4 pairs. A manually
        // chosen pair may have different offsets; both must be excluded.
        let mut used = used4.clone();
        for address in used6 {
            if let Some(offset) = address
                .checked_sub(self.ipv6_network)
                .and_then(|n| u32::try_from(n).ok())
            {
                if let Some(v4) = self.ipv4_network.checked_add(offset) {
                    used.insert(v4);
                }
            }
        }
        self.allocate(&used)
    }

    fn allocate(&self, used: &HashSet<u32>) -> Option<(String, String)> {
        let capacity = self.total_addresses();
        let last = capacity - 1;
        // Keep the historical .10 address for the default pool, then fill in order.
        let preferred = if capacity > 11 { 10 } else { 2 };
        for offset in preferred..last {
            if let Some(result) = self.address_at(offset, used) {
                return Some(result);
            }
        }
        for offset in 2..preferred.min(last) {
            if let Some(result) = self.address_at(offset, used) {
                return Some(result);
            }
        }
        None
    }

    fn address_at(&self, offset: u64, used: &HashSet<u32>) -> Option<(String, String)> {
        if !self.usable_offset(offset) {
            return None;
        }
        let ipv4 = self.ipv4_network.checked_add(u32::try_from(offset).ok()?)?;
        if used.contains(&ipv4) {
            return None;
        }
        let ipv6 = self.ipv6_network.checked_add(u128::from(offset))?;
        Some((
            Ipv4Addr::from(ipv4).to_string(),
            Ipv6Addr::from(ipv6).to_string(),
        ))
    }
}

fn default_address_mode() -> String {
    "automatic".into()
}

fn prefix_mask4(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn prefix_mask6(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

fn parse_ipv4_cidr(value: &str) -> Result<(u32, u8), String> {
    let (address, prefix) = value
        .split_once('/')
        .ok_or_else(|| format!("IPv4 地址池格式无效：{value}"))?;
    let address = address
        .parse::<Ipv4Addr>()
        .map_err(|_| format!("IPv4 地址池地址无效：{address}"))?;
    let prefix = prefix
        .parse::<u8>()
        .map_err(|_| format!("IPv4 地址池前缀无效：{prefix}"))?;
    if prefix > 32 {
        return Err("IPv4 地址池前缀必须在 0 到 32 之间".into());
    }
    Ok((u32::from(address) & prefix_mask4(prefix), prefix))
}

fn parse_ipv6_cidr(value: &str) -> Result<(u128, u8), String> {
    let (address, prefix) = value
        .split_once('/')
        .ok_or_else(|| format!("IPv6 地址池格式无效：{value}"))?;
    let address = address
        .parse::<Ipv6Addr>()
        .map_err(|_| format!("IPv6 地址池地址无效：{address}"))?;
    let prefix = prefix
        .parse::<u8>()
        .map_err(|_| format!("IPv6 地址池前缀无效：{prefix}"))?;
    if prefix > 128 {
        return Err("IPv6 地址池前缀必须在 0 到 128 之间".into());
    }
    Ok((u128::from(address) & prefix_mask6(prefix), prefix))
}

#[derive(Clone, Serialize, Deserialize)]
struct Device {
    device_id: Uuid,
    user_id: String,
    username: String,
    public_key: String,
    node_id: String,
    ipv4: String,
    ipv6: String,
    ipv4_mode: String,
    ipv6_mode: String,
    status: String,
    config_version: u64,
    last_seen_at: DateTime<Utc>,
    #[serde(default)]
    traffic: pmman_protocol::TrafficStats,
    #[serde(skip)]
    session_traffic: pmman_protocol::TrafficStats,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
struct Node {
    #[serde(default)]
    rate_limit: NodeRateLimit,
    #[serde(default)]
    resource_version: Option<String>,
    #[serde(default)]
    desired_resource_version: Option<String>,
    node_id: String,
    name: String,
    wireguard_endpoint: String,
    #[serde(default)]
    probe_endpoint: String,
    #[serde(default)]
    network: NodeNetworkConfig,
    #[serde(default)]
    network_override: Option<NodeNetworkConfig>,
    #[serde(skip)]
    applied: Vec<AppliedNodePeer>,
    #[serde(skip)]
    sync_errors: Vec<NodeSyncError>,
    #[serde(skip)]
    applied_route_table: Option<String>,
    #[serde(skip)]
    cleanup_ok: bool,
    #[serde(skip)]
    port_forward_applied: Vec<pmman_protocol::AppliedPortForward>,
    #[serde(skip)]
    port_forward_errors: Vec<pmman_protocol::PortForwardSyncError>,
    region: Option<String>,
    server_public_key: String,
    #[serde(default)]
    wireguard_interface: Option<String>,
    route_table: Option<String>,
    node_token: String,
    last_seen_at: DateTime<Utc>,
    enabled: bool,
    #[serde(default = "default_true")]
    connectable: bool,
    registered: bool,
    #[serde(default)]
    wireguard_endpoint_override: Option<String>,
    #[serde(default)]
    probe_endpoint_override: Option<String>,
    #[serde(default)]
    region_override: Option<String>,
    #[serde(default)]
    server_public_key_override: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct UserAccount {
    user_id: String,
    username: String,
    password_hash: String,
    role: String,
    enabled: bool,
    allow_ipv4_global: bool,
    allow_ipv6_global: bool,
    allow_ipv4_split: bool,
    allow_ipv6_split: bool,
    created_at: DateTime<Utc>,
    #[serde(default)]
    must_change_password: bool,
    #[serde(default = "default_address_mode")]
    address_mode: String,
    #[serde(default)]
    reserved_ipv4: Option<String>,
    #[serde(default)]
    reserved_ipv6: Option<String>,
    // Automatic leases survive master restarts without persisting live keys
    // or treating a previously connected device as currently authorized.
    #[serde(default)]
    leased_ipv4: Option<String>,
    #[serde(default)]
    leased_ipv6: Option<String>,
}

#[derive(Clone)]
struct Session {
    user_id: String,
    username: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone)]
struct Principal {
    user_id: String,
    username: String,
    role: String,
    allow_ipv4_global: bool,
    allow_ipv6_global: bool,
    allow_ipv4_split: bool,
    allow_ipv6_split: bool,
    must_change_password: bool,
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
struct LoginResponse {
    token: String,
    user_id: String,
    username: String,
    role: String,
    allow_ipv4_global: bool,
    allow_ipv6_global: bool,
    allow_ipv4_split: bool,
    allow_ipv6_split: bool,
    expires_at: DateTime<Utc>,
    must_change_password: bool,
}

#[derive(Deserialize)]
struct RegisterRequest {
    public_key: String,
    #[serde(default = "internal_mode")]
    ipv4_mode: String,
    #[serde(default = "internal_mode")]
    ipv6_mode: String,
    #[serde(default)]
    split_route_source: Option<String>,
}

fn internal_mode() -> String {
    "internal".into()
}

#[derive(Deserialize)]
struct NodeRegistrationRequest {
    node_id: String,
    name: String,
    wireguard_endpoint: String,
    probe_endpoint: String,
    #[serde(default)]
    network: NodeNetworkConfig,
    region: Option<String>,
    server_public_key: String,
    route_table: Option<String>,
    #[serde(default)]
    wireguard_interface: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeRelease {
    version: String,
    file_name: String,
    sha256: String,
    size: u64,
    published_at: DateTime<Utc>,
}

use pmman_protocol::release::Manifest as ClientReleaseManifest;

#[derive(Serialize, Deserialize)]
struct ClientReleaseResponse {
    available: bool,
    manifest: Option<ClientReleaseManifest>,
    download: Option<String>,
}

#[derive(Deserialize, Default)]
struct ClientReleaseQuery {
    platform: Option<String>,
    arch: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminClientRelease {
    #[serde(flatten)]
    manifest: ClientReleaseManifest,
    active: bool,
}

struct UploadedClientRelease {
    channel: String,
    version: String,
    min_supported_version: String,
    mandatory_after: Option<String>,
    release_notes: Vec<String>,
    package_name: Option<String>,
    package: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeSummary {
    rate_limit: NodeRateLimit,
    node_id: String,
    name: String,
    probe_endpoint: String,
    region: Option<String>,
    server_public_key: String,
    wireguard_interface: Option<String>,
    route_table: Option<String>,
    enabled: bool,
    connectable: bool,
    healthy: bool,
    last_seen_at: DateTime<Utc>,
    registered: bool,
    wireguard_endpoint: String,
    resource_version: Option<String>,
    desired_resource_version: Option<String>,
    network: NodeNetworkConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeBirdConfigResponse {
    node_id: String,
    interface: Option<String>,
    route_table: Option<String>,
    ready: bool,
    warnings: Vec<String>,
    file_name: String,
    config: String,
    ospf_ipv4_filter_snippet: String,
    ospf_ipv6_filter_snippet: String,
    installation_steps: Vec<String>,
    verification_commands: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct NodeHeartbeatResponse {
    #[serde(default)]
    relay_peers: Vec<pmman_protocol::p2p::RelayPeer>,
    revoked: Vec<NodePeer>,
    active: Vec<ActiveNodePeer>,
    route_table: Option<String>,
    #[serde(default)]
    network: NodeNetworkConfig,
    #[serde(default)]
    release: Option<NodeRelease>,
    #[serde(default)]
    geosite: Option<pmman_protocol::GeositeConfig>,
    #[serde(default)]
    port_forwards: Vec<pmman_protocol::NodePortForward>,
    #[serde(default)]
    revoked_forward_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct NodePeer {
    device_id: Uuid,
    public_key: String,
    ipv4: String,
    ipv6: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct ActiveNodePeer {
    device_id: Uuid,
    public_key: String,
    ipv4: String,
    ipv6: String,
    config_version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct AppliedNodePeer {
    device_id: Uuid,
    public_key: String,
    config_version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct NodeSyncError {
    device_id: Uuid,
    public_key: String,
    config_version: u64,
    error: String,
}

#[derive(Default, Deserialize)]
struct NodeHeartbeatRequest {
    #[serde(default)]
    applied: Vec<AppliedNodePeer>,
    #[serde(default)]
    errors: Vec<NodeSyncError>,
    route_table: Option<String>,
    network: Option<NodeNetworkConfig>,
    #[serde(default)]
    cleanup_ok: bool,
    #[serde(default)]
    resource_version: Option<String>,
    #[serde(default)]
    applied_port_forwards: Vec<pmman_protocol::AppliedPortForward>,
    #[serde(default)]
    port_forward_errors: Vec<pmman_protocol::PortForwardSyncError>,
    #[serde(default)]
    released_forward_ids: Vec<String>,
}

#[derive(Clone, Serialize)]
struct PortForwardEntryView {
    entry_id: String,
    node_id: String,
    name: String,
    public_address: String,
    hostname: Option<String>,
    family: String,
    protocols: Vec<pmman_protocol::PortForwardProtocol>,
    port_ranges: Vec<pmman_protocol::PortForwardRange>,
    reserved_ports: Vec<u16>,
    enabled: bool,
    node_healthy: bool,
    total_ports: u32,
    allocated_ports: u32,
    available_ports: u32,
}

#[derive(Deserialize)]
struct PortForwardEntryRequest {
    entry_id: String,
    node_id: String,
    name: String,
    public_address: String,
    hostname: Option<String>,
    family: String,
    protocols: Vec<pmman_protocol::PortForwardProtocol>,
    port_ranges: Vec<port_forward::PortRange>,
    reserved_ports: Vec<u16>,
    enabled: bool,
}

#[derive(Deserialize)]
struct ClientPortForwardRequest {
    device_id: Uuid,
    entry_id: Option<String>,
    protocols: Vec<pmman_protocol::PortForwardProtocol>,
    target_port: u16,
}

#[derive(Deserialize)]
struct UserPortForwardPolicyRequest {
    enabled: bool,
    max_active_forwards: u32,
    allowed_entry_ids: Vec<String>,
    allowed_protocols: Vec<pmman_protocol::PortForwardProtocol>,
    allowed_target_ports: Option<Vec<u16>>,
    allowed_families: Vec<String>,
}

#[derive(Deserialize)]
struct ClientConnectionRequest {
    node_id: String,
    #[serde(flatten)]
    registration: RegisterRequest,
}

#[derive(Clone, Serialize, Deserialize)]
struct DeviceConfig {
    #[serde(default)]
    rate_limit: NodeRateLimit,
    device_id: Uuid,
    ipv4: String,
    ipv6: String,
    routes: Vec<String>,
    ipv4_mode: String,
    ipv6_mode: String,
    dns_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dns_policy: Option<pmman_protocol::ClientDnsPolicy>,
    server_public_key: String,
    endpoint: String,
    config_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_public_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_ipv4: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_ipv6: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    split_route_source: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize, Deserialize)]
struct UserSummary {
    user_id: String,
    username: String,
    role: String,
    enabled: bool,
    allow_ipv4_global: bool,
    allow_ipv6_global: bool,
    allow_ipv4_split: bool,
    allow_ipv6_split: bool,
    created_at: DateTime<Utc>,
    address_mode: String,
    reserved_ipv4: Option<String>,
    reserved_ipv6: Option<String>,
    leased_ipv4: Option<String>,
    leased_ipv6: Option<String>,
    #[serde(default)]
    address_policy: Option<address_pools::Policy>,
}

fn user_fixed_pair(user: &UserAccount) -> Result<Option<(String, String)>, String> {
    match user.address_mode.as_str() {
        "automatic" => {
            if user
                .reserved_ipv4
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || user
                    .reserved_ipv6
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
            {
                return Err("自动分配模式不能保存固定地址".into());
            }
            Ok(None)
        }
        "fixed" => {
            let ipv4 = user
                .reserved_ipv4
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "固定地址模式必须填写 IPv4 地址".to_string())?;
            let ipv6 = user
                .reserved_ipv6
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "固定地址模式必须填写 IPv6 地址".to_string())?;
            Ok(Some(validate_manual_pair(ipv4, ipv6)?))
        }
        _ => Err("地址分配方式只能是 automatic 或 fixed".into()),
    }
}

fn validate_manual_pair(ipv4: &str, ipv6: &str) -> Result<(String, String), String> {
    if ipv4.contains('/') || ipv6.contains('/') {
        return Err("手动地址必须填写单个 IPv4/IPv6 地址，不能带 CIDR 前缀".into());
    }
    let v4 = ipv4
        .parse::<Ipv4Addr>()
        .map_err(|_| format!("手动 IPv4 地址无效：{ipv4}"))?;
    let v6 = ipv6
        .parse::<Ipv6Addr>()
        .map_err(|_| format!("手动 IPv6 地址无效：{ipv6}"))?;
    if v4.is_unspecified() || v4 == Ipv4Addr::new(255, 255, 255, 255) || v4.is_multicast() {
        return Err("手动 IPv4 地址必须是合法单播主机地址".into());
    }
    if v6.is_unspecified() || v6.is_multicast() {
        return Err("手动 IPv6 地址必须是合法单播主机地址".into());
    }
    Ok((v4.to_string(), v6.to_string()))
}

fn user_summary(user: &UserAccount) -> UserSummary {
    UserSummary {
        user_id: user.user_id.clone(),
        username: user.username.clone(),
        role: user.role.clone(),
        enabled: user.enabled,
        allow_ipv4_global: user.allow_ipv4_global,
        allow_ipv6_global: user.allow_ipv6_global,
        allow_ipv4_split: user.allow_ipv4_split,
        allow_ipv6_split: user.allow_ipv6_split,
        created_at: user.created_at,
        address_mode: user.address_mode.clone(),
        reserved_ipv4: user.reserved_ipv4.clone(),
        reserved_ipv6: user.reserved_ipv6.clone(),
        leased_ipv4: user.leased_ipv4.clone(),
        leased_ipv6: user.leased_ipv6.clone(),
        address_policy: None,
    }
}

fn user_lease(user: &UserAccount) -> Option<(String, String)> {
    let (v4, v6) = if user.address_mode == "fixed" {
        (&user.reserved_ipv4, &user.reserved_ipv6)
    } else {
        (&user.leased_ipv4, &user.leased_ipv6)
    };
    Some((v4.as_ref()?.clone(), v6.as_ref()?.clone()))
}

fn address_occupancy(
    users: &HashMap<String, UserAccount>,
    devices: &HashMap<Uuid, Device>,
    exclude_user: Option<&str>,
) -> (HashSet<u32>, HashSet<u128>) {
    let mut used4 = HashSet::new();
    let mut used6 = HashSet::new();
    for user in users
        .values()
        .filter(|user| Some(user.user_id.as_str()) != exclude_user)
    {
        if let Some((v4, v6)) = user_lease(user) {
            if let Ok(v4) = v4.parse::<Ipv4Addr>() {
                used4.insert(u32::from(v4));
            }
            if let Ok(v6) = v6.parse::<Ipv6Addr>() {
                used6.insert(u128::from(v6));
            }
        }
    }
    for device in devices.values().filter(|device| {
        device.status != "released" && Some(device.user_id.as_str()) != exclude_user
    }) {
        if let Ok(v4) = device.ipv4.parse::<Ipv4Addr>() {
            used4.insert(u32::from(v4));
        }
        if let Ok(v6) = device.ipv6.parse::<Ipv6Addr>() {
            used6.insert(u128::from(v6));
        }
    }
    (used4, used6)
}

fn address_conflict(pair: &(String, String), used4: &HashSet<u32>, used6: &HashSet<u128>) -> bool {
    pair.0
        .parse::<Ipv4Addr>()
        .is_ok_and(|ip| used4.contains(&u32::from(ip)))
        || pair
            .1
            .parse::<Ipv6Addr>()
            .is_ok_and(|ip| used6.contains(&u128::from(ip)))
}

fn insert_pool_occupancy(pool: &AddressPool, ipv4: &str, ipv6: &str, occupied: &mut HashSet<u32>) {
    if let Ok(address) = ipv4.parse::<Ipv4Addr>() {
        let value = u32::from(address);
        if (value & prefix_mask4(pool.ipv4_prefix)) == pool.ipv4_network
            && pool.usable_offset(u64::from(value - pool.ipv4_network))
        {
            occupied.insert(value);
        }
    }
    if let Ok(address) = ipv6.parse::<Ipv6Addr>() {
        let value = u128::from(address);
        if (value & prefix_mask6(pool.ipv6_prefix)) == pool.ipv6_network {
            let offset = value - pool.ipv6_network;
            if offset >= 2 && offset < u128::from(pool.total_addresses()) {
                if let Ok(offset) = u32::try_from(offset) {
                    if let Some(v4) = pool.ipv4_network.checked_add(offset) {
                        occupied.insert(v4);
                    }
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
    role: Option<String>,
    #[serde(default)]
    allow_ipv4_global: bool,
    #[serde(default)]
    allow_ipv6_global: bool,
    #[serde(default)]
    allow_ipv4_split: bool,
    #[serde(default)]
    allow_ipv6_split: bool,
}

#[derive(Deserialize)]
struct UserCapabilitiesRequest {
    allow_ipv4_global: bool,
    allow_ipv6_global: bool,
    allow_ipv4_split: bool,
    allow_ipv6_split: bool,
}

#[derive(Deserialize)]
struct UserAddressLeaseRequest {
    address_mode: String,
    #[serde(default)]
    reserved_ipv4: Option<String>,
    #[serde(default)]
    reserved_ipv6: Option<String>,
}

#[derive(Deserialize)]
struct UserStatusRequest {
    enabled: bool,
}

#[derive(Deserialize)]
struct PasswordRequest {
    password: String,
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

#[derive(Serialize, Deserialize)]
struct PoolSettings {
    ipv4_pool: String,
    ipv6_pool: String,
    internal_cidrs: Vec<String>,
    capacity: u64,
    allocated: u64,
    available: u64,
}

#[derive(Deserialize)]
struct PoolSettingsRequest {
    ipv4_pool: String,
    ipv6_pool: String,
    #[serde(default)]
    internal_cidrs: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkSettings {
    ipv4_pool: String,
    ipv6_pool: String,
    internal_cidrs: Vec<String>,
}

fn persist_network_settings(state: &AppState, settings: &NetworkSettings) -> Result<(), String> {
    if let Some(database) = &state.database {
        let payload =
            serde_json::to_string(settings).map_err(|_| "网络配置编码失败".to_string())?;
        database.put_document("network_settings", &payload)?;
        let _ = database.append_audit(
            None,
            "network-settings.updated",
            Some("network_settings"),
            None,
            None,
        );
        return Ok(());
    }
    let Some(path) = &state.settings_store else {
        return Ok(());
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let temporary = parent.join(format!(".network-settings-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(parent)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&file, settings)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_| "地址池与内网 CIDR 保存失败，请检查主控存储路径和权限".into())
}

fn load_network_settings(state: &AppState) -> Result<(), String> {
    if let Some(database) = &state.database {
        let settings = match database.get_document("network_settings")? {
            Some(payload) => serde_json::from_str::<NetworkSettings>(&payload)
                .map_err(|_| "网络配置记录格式错误，拒绝回退到默认地址池")?,
            None => {
                let pool = state.pool.read().expect("address pool poisoned");
                let settings = NetworkSettings {
                    ipv4_pool: pool.ipv4_cidr(),
                    ipv6_pool: pool.ipv6_cidr(),
                    internal_cidrs: state
                        .internal_cidrs
                        .read()
                        .expect("internal route registry poisoned")
                        .clone(),
                };
                persist_network_settings(state, &settings)?;
                settings
            }
        };
        let pool = AddressPool::from_cidrs(&settings.ipv4_pool, &settings.ipv6_pool)?;
        let cidrs = normalize_internal_cidrs(settings.internal_cidrs.iter().map(String::as_str))?;
        *state.pool.write().expect("address pool poisoned") = pool;
        *state
            .internal_cidrs
            .write()
            .expect("internal route registry poisoned") = cidrs;
        return Ok(());
    }
    let Some(path) = &state.settings_store else {
        return Ok(());
    };
    let settings: NetworkSettings = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "网络配置文件格式错误，拒绝回退到默认地址池")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let pool = state.pool.read().expect("address pool poisoned");
            return persist_network_settings(
                state,
                &NetworkSettings {
                    ipv4_pool: pool.ipv4_cidr(),
                    ipv6_pool: pool.ipv6_cidr(),
                    internal_cidrs: state
                        .internal_cidrs
                        .read()
                        .expect("internal route registry poisoned")
                        .clone(),
                },
            );
        }
        Err(_) => return Err("无法读取网络配置文件，拒绝回退到默认地址池".into()),
    };
    let pool = AddressPool::from_cidrs(&settings.ipv4_pool, &settings.ipv6_pool)?;
    let cidrs = normalize_internal_cidrs(settings.internal_cidrs.iter().map(String::as_str))?;
    *state.pool.write().expect("address pool poisoned") = pool;
    *state
        .internal_cidrs
        .write()
        .expect("internal route registry poisoned") = cidrs;
    Ok(())
}

#[derive(Deserialize)]
struct NodeSettingsRequest {
    #[serde(default)]
    // Older callers changing routing must not silently remove a bandwidth cap.
    rate_limit: Option<NodeRateLimit>,
    route_table: Option<String>,
    enabled: bool,
    #[serde(default)]
    connectable: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    wireguard_endpoint: Option<String>,
    #[serde(default)]
    probe_endpoint: Option<String>,
    #[serde(default)]
    network: Option<NodeNetworkConfig>,
}

#[derive(Deserialize)]
struct NodeReleaseRequest {
    version: Option<String>,
}

#[derive(Deserialize)]
struct NodeEnrollmentRequest {
    node_id: String,
    name: String,
    region: Option<String>,
    route_table: Option<String>,
    #[serde(default)]
    network: NodeNetworkConfig,
    #[serde(default = "default_true")]
    connectable: bool,
    #[serde(default)]
    wireguard_interface: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct NodeEnrollmentResponse {
    node: NodeSummary,
    node_token: String,
}

#[cfg(test)]
fn config(device: &Device, gateway: &GatewayConfig) -> DeviceConfig {
    config_with_routes(
        device,
        gateway,
        DEFAULT_INTERNAL_CIDRS
            .iter()
            .map(|route| (*route).to_string())
            .collect(),
        None,
    )
}

fn config_with_routes(
    device: &Device,
    gateway: &GatewayConfig,
    routes: Vec<String>,
    previous_public_key: Option<String>,
) -> DeviceConfig {
    // Routes are injected from AppState by the caller. Keep this helper's
    // historical shape for tests and the legacy single-gateway endpoint.
    DeviceConfig {
        rate_limit: NodeRateLimit::default(),
        device_id: device.device_id,
        ipv4: format!("{}/32", device.ipv4),
        ipv6: format!("{}/128", device.ipv6),
        routes,
        ipv4_mode: device.ipv4_mode.clone(),
        ipv6_mode: device.ipv6_mode.clone(),
        dns_servers: Vec::new(),
        dns_policy: None,
        server_public_key: gateway.server_public_key.clone(),
        endpoint: gateway.endpoint.clone(),
        config_version: device.config_version,
        previous_public_key,
        gateway_ipv4: None,
        gateway_ipv6: None,
        split_route_source: None,
    }
}

fn configured_internal_cidrs() -> Result<Vec<String>, String> {
    let raw = env::var("PMMAN_INTERNAL_CIDRS").ok();
    let values = raw
        .as_deref()
        .map(|value| value.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_else(|| DEFAULT_INTERNAL_CIDRS.to_vec());
    normalize_internal_cidrs(values)
}

fn normalize_internal_cidrs<'a, I>(values: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let canonical = if value.contains(':') {
            let (network, prefix) = parse_ipv6_cidr(value)?;
            format!("{}/{}", Ipv6Addr::from(network), prefix)
        } else {
            let (network, prefix) = parse_ipv4_cidr(value)?;
            format!("{}/{}", Ipv4Addr::from(network), prefix)
        };
        if seen.insert(canonical.clone()) {
            result.push(canonical);
        }
    }
    if result.len() > 1000 {
        return Err("内网 CIDR 列表最多包含 1000 项".into());
    }
    if result.is_empty() {
        return Err("内网 CIDR 列表不能为空".into());
    }
    Ok(result)
}

fn node_is_healthy(node: &Node) -> bool {
    node.registered && node.enabled && node.last_seen_at > Utc::now() - Duration::seconds(15)
}

fn node_is_connectable(node: &Node) -> bool {
    node.connectable && node_is_healthy(node)
}

fn node_summary(node: &Node) -> NodeSummary {
    NodeSummary {
        rate_limit: node.rate_limit,
        node_id: node.node_id.clone(),
        name: node.name.clone(),
        probe_endpoint: node.probe_endpoint.clone(),
        region: node.region.clone(),
        server_public_key: node.server_public_key.clone(),
        wireguard_interface: node.wireguard_interface.clone(),
        route_table: node.route_table.clone(),
        enabled: node.enabled,
        connectable: node.connectable,
        healthy: node_is_healthy(node),
        last_seen_at: node.last_seen_at,
        registered: node.registered,
        wireguard_endpoint: node.wireguard_endpoint.clone(),
        resource_version: node.resource_version.clone(),
        desired_resource_version: node.desired_resource_version.clone(),
        network: node_network(node),
    }
}

const PMMAN_NETWORK_BIRD_TEMPLATE: &str = include_str!("../../config/bird/pmman_network.conf");

fn node_bird_config(node: &Node) -> NodeBirdConfigResponse {
    let interface = node
        .wireguard_interface
        .as_deref()
        .filter(|value| valid_linux_interface(value))
        .map(str::to_owned);
    let route_table = node.route_table.clone();
    let mut warnings = Vec::new();
    let numeric_table = route_table
        .as_deref()
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0);

    if route_table.is_none() {
        warnings.push("节点尚未配置返回路由表，BIRD 不会学习客户端 /32 和 /128 路由".into());
    } else if numeric_table.is_none() {
        warnings.push(format!(
            "返回路由表 {} 是名称而不是数字表号，请先在节点上查询对应的 Linux 数字表号",
            route_table.as_deref().unwrap_or_default()
        ));
    }
    if interface.is_none() {
        warnings.push(if node.wireguard_interface.is_some() {
            "节点保存的 WireGuard 接口名无效，请重新注册节点后再生成模板".into()
        } else {
            "节点尚未上报 WireGuard 接口名，请将模板中的接口占位值替换为实际接口".into()
        });
    }

    let table_value = numeric_table
        .map(|value| value.to_string())
        .unwrap_or_else(|| "0".into());
    let interface_value = interface
        .as_deref()
        .unwrap_or("REPLACE_WITH_WIREGUARD_INTERFACE");
    let mut config = PMMAN_NETWORK_BIRD_TEMPLATE
        .replace(
            "define PMMAN_NETWORK_KERNEL_TABLE = 52001;",
            &format!("define PMMAN_NETWORK_KERNEL_TABLE = {table_value};"),
        )
        .replace(
            "define PMMAN_NETWORK_INTERFACE = \"wgnetport\";",
            &format!("define PMMAN_NETWORK_INTERFACE = \"{interface_value}\";"),
        );
    if numeric_table.is_none() || interface.is_none() {
        config = format!(
            "# 未完成模板：应用前必须替换下方标记的占位值。\n# 主控不会自动修改节点上的 BIRD 配置。\n\n{config}"
        );
    }

    let ready = numeric_table.is_some() && interface.is_some();
    let node_id = node.node_id.clone();
    let table_for_commands = route_table
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or("<route-table>");
    let verification_commands = vec![
        "bird -p -c /etc/bird/bird.conf".into(),
        "birdc show protocols all".into(),
        "birdc show route protocol pmman_network_direct".into(),
        "birdc show route protocol pmman_network_kernel_v4".into(),
        "birdc show route protocol pmman_network_kernel_v6".into(),
        "birdc show route export internal_ospf".into(),
        "birdc show route export internal_ospf_v6".into(),
        format!("ip -4 route show table {table_for_commands}"),
        format!("ip -6 route show table {table_for_commands}"),
    ];
    let installation_steps = vec![
        "install -m 0644 <downloaded-file> /etc/bird/pmman_network.conf".into(),
        "在 /etc/bird/bird.conf 中加入：include \"/etc/bird/pmman_network.conf\";".into(),
        "将 IPv4/IPv6 OSPF export filter 中的 pmman_network 规则加入现有过滤器".into(),
        "将现有 kernel export filter 中的 pmman_network 协议加入拒绝规则，避免回写主表".into(),
        "bird -p -c /etc/bird/bird.conf && birdc configure".into(),
    ];
    NodeBirdConfigResponse {
        node_id: node_id.clone(),
        interface,
        route_table,
        ready,
        warnings,
        file_name: format!("pmman_network-{node_id}.conf"),
        config,
        ospf_ipv4_filter_snippet: "if proto = \"pmman_network_direct\" || proto = \"pmman_network_kernel_v4\" then accept;".into(),
        ospf_ipv6_filter_snippet: "if proto = \"pmman_network_direct\" || proto = \"pmman_network_kernel_v6\" then accept;".into(),
        installation_steps,
        verification_commands,
    }
}

fn node_network(node: &Node) -> NodeNetworkConfig {
    let mut network = node
        .network_override
        .clone()
        .unwrap_or_else(|| node.network.clone());
    network.apply_managed_dns_defaults();
    if network.dns_servers.is_empty() && pmmandns_enabled(&network) {
        network.dns_servers = [
            network.gateway_ipv4.as_deref(),
            network.gateway_ipv6.as_deref(),
        ]
        .into_iter()
        .filter_map(|value| value.and_then(|value| value.split('/').next()))
        .map(str::to_owned)
        .collect();
    }
    network
}

fn gateway_for_node(node: &Node) -> GatewayConfig {
    GatewayConfig {
        interface: None,
        server_public_key: node.server_public_key.clone(),
        endpoint: node.wireguard_endpoint.clone(),
        wg_binary: String::new(),
    }
}

fn add_gateway_peer(gateway: &GatewayConfig, device: &Device) -> Result<(), String> {
    let Some(interface) = &gateway.interface else {
        return Ok(());
    };
    let allowed_ips = format!("{}/32, {}/128", device.ipv4, device.ipv6);
    let output = Command::new(&gateway.wg_binary)
        .args([
            "set",
            interface,
            "peer",
            &device.public_key,
            "allowed-ips",
            &allowed_ips,
        ])
        .output()
        .map_err(|error| format!("无法执行 WireGuard 网关配置：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            "WireGuard 网关拒绝了客户端 peer 配置".into()
        } else {
            detail
        })
    }
}

fn remove_gateway_peer(
    gateway: &GatewayConfig,
    interface: &str,
    public_key: &str,
) -> Result<(), String> {
    let output = Command::new(&gateway.wg_binary)
        .args(["set", interface, "peer", public_key, "remove"])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().into())
    }
}

fn server_error(error: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::BAD_GATEWAY, Json(ErrorResponse { error }))
}

const PASSWORD_HASH_PREFIX: &str = "pbkdf2-sha256";
const PASSWORD_HASH_ROUNDS: u32 = 600_000;
const PASSWORD_SALT_BYTES: usize = 16;

type HmacSha256 = Hmac<Sha256>;

fn derive_password(password: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut output = [0u8; 32];
    let mut first_message = Vec::with_capacity(salt.len() + 4);
    first_message.extend_from_slice(salt);
    first_message.extend_from_slice(&1u32.to_be_bytes());
    let mut mac = HmacSha256::new_from_slice(password.as_bytes()).expect("HMAC accepts any key");
    mac.update(&first_message);
    let mut block = mac.finalize().into_bytes();
    output.copy_from_slice(&block);
    for _ in 1..rounds {
        let mut mac =
            HmacSha256::new_from_slice(password.as_bytes()).expect("HMAC accepts any key");
        mac.update(&block);
        block = mac.finalize().into_bytes();
        for (target, byte) in output.iter_mut().zip(block.iter()) {
            *target ^= byte;
        }
    }
    output
}

fn hash_password(password: &str) -> String {
    let salt = *Uuid::new_v4().as_bytes();
    let digest = derive_password(password, &salt, PASSWORD_HASH_ROUNDS);
    format!(
        "{PASSWORD_HASH_PREFIX}${PASSWORD_HASH_ROUNDS}${}${}",
        STANDARD.encode(salt),
        STANDARD.encode(digest)
    )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn legacy_hash_password(password: &str) -> String {
    let digest = Sha256::digest(format!("pmman-password-v1:{password}").as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_password(stored: &str, password: &str) -> (bool, bool) {
    let mut fields = stored.split('$');
    let Some(prefix) = fields.next() else {
        return (stored == legacy_hash_password(password), true);
    };
    let Some(rounds) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
        return (stored == legacy_hash_password(password), true);
    };
    let Some(salt) = fields.next().and_then(|value| STANDARD.decode(value).ok()) else {
        return (stored == legacy_hash_password(password), true);
    };
    let Some(expected) = fields.next().and_then(|value| STANDARD.decode(value).ok()) else {
        return (stored == legacy_hash_password(password), true);
    };
    if prefix != PASSWORD_HASH_PREFIX
        || !(100_000..=2_000_000).contains(&rounds)
        || salt.len() != PASSWORD_SALT_BYTES
        || expected.len() != 32
    {
        return (stored == legacy_hash_password(password), true);
    }
    let actual = derive_password(password, &salt, rounds);
    (
        constant_time_eq(&actual, &expected),
        rounds != PASSWORD_HASH_ROUNDS,
    )
}

fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && username.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn valid_linux_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 15
        && value.bytes().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-' | b'.')
        })
}

fn validate_optional_linux_interface(value: Option<&str>) -> Result<Option<String>, String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = value {
        if !valid_linux_interface(value) {
            return Err(format!("WireGuard 接口名无效：{value}"));
        }
        return Ok(Some(value.to_owned()));
    }
    Ok(None)
}

fn validate_route_table(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!("返回路由表名称无效：{value}"));
    }
    Ok(value.to_owned())
}

fn seed_users() -> HashMap<String, UserAccount> {
    let now = Utc::now();
    let admin_password = env::var("PMMAN_ADMIN_PASSWORD")
        .expect("PMMAN_ADMIN_PASSWORD must be set; refusing to start with a default password");
    let mut users = HashMap::new();
    users.insert(
        "admin".into(),
        UserAccount {
            user_id: "admin".into(),
            username: "admin".into(),
            password_hash: hash_password(&admin_password),
            role: "admin".into(),
            enabled: true,
            allow_ipv4_global: true,
            allow_ipv6_global: true,
            allow_ipv4_split: true,
            allow_ipv6_split: true,
            created_at: now,
            must_change_password: true,
            address_mode: default_address_mode(),
            reserved_ipv4: None,
            reserved_ipv6: None,
            leased_ipv4: None,
            leased_ipv6: None,
        },
    );
    users
}

fn persist_users(state: &AppState, users: &HashMap<String, UserAccount>) -> Result<(), String> {
    if let Some(database) = &state.database {
        let payload = serde_json::to_string(users).map_err(|_| "账户记录编码失败".to_string())?;
        database.put_document("users", &payload)?;
        let _ = database.append_audit(None, "users.snapshot.updated", Some("users"), None, None);
        return Ok(());
    }
    let Some(path) = &state.user_store else {
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(users).map_err(|_| "账户记录编码失败".to_string())?;
    atomic_write(path, &bytes, true)
        .map_err(|_| "账户记录保存失败，请检查主控存储路径和权限".into())
}

/// Write a complete generation beside the target and publish it with rename.
/// A UUID suffix prevents two concurrent requests or a crashed process from
/// reusing the same `.tmp` file and overwriting another request's data.
fn atomic_write(path: &PathBuf, bytes: &[u8], private: bool) -> Result<(), String> {
    use std::io::Write;
    #[cfg(not(unix))]
    let _ = private;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let temporary = parent.join(format!(".{name}.tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn upgrade_password_hash(
    state: &AppState,
    username: &str,
    previous_hash: &str,
    password: &str,
) -> Result<(), String> {
    let mut users = state.users.write().expect("user registry poisoned");
    let Some(current) = users.get(username) else {
        return Err("账户不存在".into());
    };
    // Do not overwrite a concurrent password change with a stale login.
    if current.password_hash != previous_hash {
        return Ok(());
    }
    let mut next = users.clone();
    if let Some(user) = next.get_mut(username) {
        user.password_hash = hash_password(password);
    }
    persist_users(state, &next)?;
    *users = next;
    Ok(())
}

fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::to_owned)
}

fn authenticate_any(state: &AppState, headers: &HeaderMap) -> Result<Principal, StatusCode> {
    let token = token_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let now = Utc::now();
    let session = {
        let mut sessions = state.sessions.write().expect("session registry poisoned");
        // Expired sessions are removed on authentication so a long-running
        // control plane cannot accumulate bearer tokens forever.
        sessions.retain(|_, session| session.expires_at > now);
        sessions.get(&token).cloned()
    }
    .ok_or(StatusCode::UNAUTHORIZED)?;
    let user = state
        .users
        .read()
        .expect("user registry poisoned")
        .get(&session.username)
        .cloned()
        .filter(|user| user.enabled && user.user_id == session.user_id)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    Ok(Principal {
        user_id: user.user_id,
        username: user.username,
        role: user.role,
        allow_ipv4_global: user.allow_ipv4_global,
        allow_ipv6_global: user.allow_ipv6_global,
        allow_ipv4_split: user.allow_ipv4_split,
        allow_ipv6_split: user.allow_ipv6_split,
        must_change_password: user.must_change_password,
    })
}

fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Principal, StatusCode> {
    authenticate_any(state, headers)
}

fn invalidate_user_sessions(state: &AppState, user_id: &str) {
    state
        .sessions
        .write()
        .expect("session registry poisoned")
        .retain(|_, session| session.user_id != user_id);
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<Principal, StatusCode> {
    let principal = authenticate(state, headers)?;
    if principal.role == "admin" && !principal.must_change_password {
        Ok(principal)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

fn node_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-pmman-node-token")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn require_node(state: &AppState, headers: &HeaderMap, node_id: &str) -> Result<Node, StatusCode> {
    let token = node_token_from_headers(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(node_id)
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // A disabled node may still heartbeat so it can receive revocations and
    // clean local peers. It is excluded from client selection by
    // `node_is_healthy`, and registration/resource download remain blocked.
    if node.registered && node.node_token == token {
        Ok(node)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn pool_settings(state: &AppState) -> PoolSettings {
    // Keep reserved addresses in the accounting even when the user has never
    // connected. A reservation must not make the pool look available to a
    // subsequent automatic allocation.
    let devices = state.devices.read().expect("device registry poisoned");
    let users = state.users.read().expect("user registry poisoned");
    if let Some(settings) = address_pools::compatibility_settings(state, &users, &devices) {
        return settings;
    }
    let pool = state.pool.read().expect("address pool poisoned");
    let mut allocated_addresses = HashSet::new();
    for device in devices
        .values()
        .filter(|device| device.status != "released")
    {
        insert_pool_occupancy(&pool, &device.ipv4, &device.ipv6, &mut allocated_addresses);
    }
    for user in users.values() {
        if let Some((ipv4, ipv6)) = user_lease(user) {
            insert_pool_occupancy(&pool, &ipv4, &ipv6, &mut allocated_addresses);
        }
    }
    let capacity = pool.capacity();
    PoolSettings {
        ipv4_pool: pool.ipv4_cidr(),
        ipv6_pool: pool.ipv6_cidr(),
        internal_cidrs: state
            .internal_cidrs
            .read()
            .expect("internal route registry poisoned")
            .clone(),
        capacity,
        allocated: allocated_addresses.len() as u64,
        available: capacity.saturating_sub(allocated_addresses.len() as u64),
    }
}

async fn healthz(State(state): State<AppState>) -> Result<&'static str, StatusCode> {
    if let Some(database) = &state.database {
        database
            .ping()
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }
    Ok("ok")
}

async fn release_version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "product": "pmman-network-server",
        "version": option_env!("PMMAN_SERVER_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
    }))
}

async fn admin_page() -> Response {
    admin_response("index.html")
}

async fn admin_asset(Path(path): Path<String>) -> Response {
    admin_response(&path)
}

fn admin_response(path: &str) -> Response {
    let Some(file) = ADMIN_ASSETS.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        file.contents(),
    )
        .into_response()
}

async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let mut user = state
        .users
        .read()
        .expect("user registry poisoned")
        .get(&input.username)
        .cloned()
        .ok_or_else(|| login_rejected("account_missing", &input.username))?;
    let (password_ok, needs_upgrade) = verify_password(&user.password_hash, &input.password);
    if !user.enabled {
        return Err(login_rejected("account_disabled", &input.username));
    }
    if !password_ok {
        return Err(login_rejected("password_mismatch", &input.username));
    }
    if needs_upgrade {
        upgrade_password_hash(&state, &user.username, &user.password_hash, &input.password)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        user = state
            .users
            .read()
            .expect("user registry poisoned")
            .get(&input.username)
            .filter(|current| current.user_id == user.user_id && current.enabled)
            .cloned()
            .ok_or_else(|| login_rejected("account_changed", &input.username))?;
        if !verify_password(&user.password_hash, &input.password).0 {
            return Err(login_rejected("account_changed", &input.username));
        }
    }
    issue_login_session(&state, &user)
}

fn login_rejected(reason: &'static str, username: &str) -> StatusCode {
    let username = if valid_username(username) {
        username
    } else {
        "invalid"
    };
    eprintln!("reason={reason} username={username}");
    StatusCode::UNAUTHORIZED
}

fn issue_login_session(
    state: &AppState,
    verified: &UserAccount,
) -> Result<Json<LoginResponse>, StatusCode> {
    // Verification happens before this boundary. Serialize account deletion,
    // replacement, disable and password changes with the actual session insert.
    // A deleted/recreated username never authenticates as its previous identity.
    let users = state.users.read().expect("user registry poisoned");
    let user = users
        .get(&verified.username)
        .filter(|current| {
            current.enabled
                && current.user_id == verified.user_id
                && current.password_hash == verified.password_hash
        })
        .ok_or_else(|| login_rejected("account_changed", &verified.username))?;
    let token = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + Duration::hours(8);
    state
        .sessions
        .write()
        .expect("session registry poisoned")
        .insert(
            token.clone(),
            Session {
                user_id: user.user_id.clone(),
                username: user.username.clone(),
                expires_at,
            },
        );
    Ok(Json(LoginResponse {
        token,
        user_id: user.user_id.clone(),
        username: user.username.clone(),
        role: user.role.clone(),
        allow_ipv4_global: user.allow_ipv4_global,
        allow_ipv6_global: user.allow_ipv6_global,
        allow_ipv4_split: user.allow_ipv4_split,
        allow_ipv6_split: user.allow_ipv6_split,
        expires_at,
        must_change_password: user.must_change_password,
    }))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> StatusCode {
    let Some(token) = token_from_headers(&headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    let removed = state
        .sessions
        .write()
        .expect("session registry poisoned")
        .remove(&token)
        .is_some();
    if removed {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ChangePasswordRequest>,
) -> StatusCode {
    let token = token_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED);
    let Ok(token) = token else {
        return StatusCode::UNAUTHORIZED;
    };
    let principal = match authenticate_any(&state, &headers) {
        Ok(principal) => principal,
        Err(status) => return status,
    };
    if input.new_password.len() < 6 || input.new_password.len() > 256 {
        return StatusCode::BAD_REQUEST;
    }
    let mut users = state.users.write().expect("user registry poisoned");
    let Some(user) = users.get(&principal.username) else {
        return StatusCode::UNAUTHORIZED;
    };
    if !verify_password(&user.password_hash, &input.current_password).0 {
        return StatusCode::UNAUTHORIZED;
    }
    // Build and persist a replacement snapshot before mutating the live map.
    // A failed disk write must not leave the running process accepting a
    // password that disappears after restart.
    let mut next = users.clone();
    if let Some(user) = next.get_mut(&principal.username) {
        user.password_hash = hash_password(&input.new_password);
        user.must_change_password = false;
    }
    if persist_users(&state, &next).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *users = next;
    drop(users);
    // Keep the current session, but invalidate every other session for this account.
    let mut sessions = state.sessions.write().expect("session registry poisoned");
    sessions.retain(|session_token, session| {
        session_token == &token || session.user_id != principal.user_id
    });
    StatusCode::NO_CONTENT
}

async fn register_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut input): Json<NodeRegistrationRequest>,
) -> Result<Json<NodeSummary>, StatusCode> {
    input.network.apply_managed_dns_defaults();
    let token = node_token_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    if !valid_node_id(&input.node_id)
        || input.name.trim().is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
        || input
            .region
            .as_ref()
            .is_some_and(|v| v.len() > 128 || v.chars().any(char::is_control))
        || !valid_wireguard_endpoint(&input.wireguard_endpoint)
        || !valid_wireguard_endpoint(&input.probe_endpoint)
        || !endpoints_use_distinct_ports(&input.wireguard_endpoint, &input.probe_endpoint)
        || validate_node_network(&input.network).is_err()
        || !valid_wireguard_key(&input.server_public_key)
        || input
            .route_table
            .as_deref()
            .is_some_and(|v| validate_route_table(v).is_err())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut nodes = state.nodes.write().expect("node registry poisoned");
    let existing = nodes
        .get(&input.node_id)
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if existing.node_token != token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let wireguard_interface =
        validate_optional_linux_interface(input.wireguard_interface.as_deref())
            .map_err(|_| StatusCode::BAD_REQUEST)?
            .or(existing.wireguard_interface.clone());
    let configured_network = existing
        .network_override
        .clone()
        .unwrap_or_else(|| input.network.clone());
    let wireguard_endpoint = existing
        .wireguard_endpoint_override
        .clone()
        .unwrap_or_else(|| input.wireguard_endpoint.clone());
    let probe_endpoint = existing
        .probe_endpoint_override
        .clone()
        .unwrap_or_else(|| input.probe_endpoint.clone());
    if !endpoints_use_distinct_ports(&wireguard_endpoint, &probe_endpoint) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let node = Node {
        rate_limit: existing.rate_limit,
        resource_version: existing.resource_version,
        desired_resource_version: existing.desired_resource_version,
        node_id: input.node_id.clone(),
        name: existing.name,
        wireguard_endpoint,
        probe_endpoint,
        network: configured_network,
        network_override: existing.network_override,
        applied: Vec::new(),
        sync_errors: Vec::new(),
        applied_route_table: None,
        cleanup_ok: false,
        port_forward_applied: Vec::new(),
        port_forward_errors: Vec::new(),
        region: existing.region_override.clone().or(input.region),
        // The node owns this identity.  The value reported during registration
        // must always win over any legacy administrator override persisted by
        // older releases; otherwise an innocent key rotation would make every
        // newly issued client configuration unusable.
        server_public_key: input.server_public_key,
        wireguard_interface,
        route_table: existing.route_table,
        node_token: token,
        // Registration alone is not a service heartbeat.  Keep the previous
        // timestamp until the node completes an authenticated heartbeat so a
        // newly enrolled node cannot be selected before it has applied the
        // current snapshot.
        last_seen_at: existing.last_seen_at,
        enabled: existing.enabled,
        connectable: existing.connectable,
        registered: true,
        wireguard_endpoint_override: existing.wireguard_endpoint_override,
        probe_endpoint_override: existing.probe_endpoint_override,
        region_override: existing.region_override,
        server_public_key_override: None,
    };
    let summary = node_summary(&node);
    let mut next = nodes.clone();
    next.insert(input.node_id, node);
    persist_nodes(&state, &next).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    *nodes = next;
    Ok(Json(summary))
}

fn valid_node_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn validate_node_network(network: &NodeNetworkConfig) -> Result<(), String> {
    network.validate_managed_dns()?;
    if let Some(routes) = &network.ipv4_split_routes {
        if routes.len() > 100_000 {
            return Err("IPv4 分流表过大".into());
        }
        for route in routes {
            parse_ipv4_cidr(route)?;
        }
    }
    if let Some(routes) = &network.ipv6_split_routes {
        if routes.len() > 100_000 {
            return Err("IPv6 分流表过大".into());
        }
        for route in routes {
            parse_ipv6_cidr(route)?;
        }
    }
    for dns in &network.dns_servers {
        dns.parse::<std::net::IpAddr>()
            .map_err(|_| "节点 DNS 地址无效")?;
    }
    for (name, upstreams) in [
        ("默认", &network.pmmandns_upstreams),
        ("国内", &network.pmmandns_domestic_upstreams),
        ("国外", &network.pmmandns_foreign_upstreams),
    ] {
        if upstreams.len() > 4 {
            return Err(format!("pmmandns {name} 上游 DNS 最多 4 项"));
        }
        for upstream in upstreams {
            parse_dns_upstream(upstream).map_err(|_| {
                format!("pmmandns {name} 上游 DNS 必须是 IP、IP:端口或 [IPv6]:端口")
            })?;
        }
    }
    if network.pmmandns_domestic_rules.len() + network.pmmandns_foreign_rules.len() > 50_000 {
        return Err("pmmandns 国内外域名规则最多 50000 条".into());
    }
    if network.pmmandns_geosite_enabled
        && (network.pmmandns_domestic_upstreams.is_empty()
            || network.pmmandns_foreign_upstreams.is_empty())
    {
        return Err("启用 geosite 必须配置国内和国外两组上游 DNS".into());
    }
    if let Some(geosite) = &network.pmmandns_geosite {
        if geosite.version.trim().is_empty()
            || geosite.size == 0
            || geosite.sha256.len() != 64
            || !geosite.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || geosite.domestic.trim().is_empty()
            || geosite.foreign.trim().is_empty()
        {
            return Err("pmmandns geosite 配置无效".into());
        }
    }
    let (primary, rules) = pmman_protocol::dns::compile_groups(
        &network.pmmandns_upstreams,
        &network.pmmandns_domestic_upstreams,
        &network.pmmandns_foreign_upstreams,
        &network.pmmandns_domestic_rules,
        &network.pmmandns_foreign_rules,
        &network.pmmandns_default_group,
        &network.pmmandns_rules,
    )?;
    if pmmandns_enabled(network) {
        if primary.is_empty() {
            return Err("pmmandns 已启用，但默认分组没有配置上游 DNS".into());
        }
        let gateway_ips: Vec<_> = [
            network.gateway_ipv4.as_deref(),
            network.gateway_ipv6.as_deref(),
        ]
        .into_iter()
        .filter_map(|v| v.and_then(|v| v.split('/').next()))
        .collect();
        if gateway_ips.is_empty()
            || network
                .dns_servers
                .iter()
                .any(|v| !gateway_ips.contains(&v.as_str()))
        {
            return Err("pmmandns 监听地址必须是节点 WireGuard 网关地址".into());
        }
        for upstream in primary
            .into_iter()
            .chain(rules.into_iter().flat_map(|(_, values)| values))
        {
            if upstream.port() == 53
                && gateway_ips
                    .iter()
                    .any(|ip| ip.parse::<std::net::IpAddr>().ok() == Some(upstream.ip()))
            {
                return Err("pmmandns 上游不能指向自身监听地址".into());
            }
        }
    }
    if let Some(gateway) = &network.gateway_ipv4 {
        validate_interface_cidr(gateway, false)?;
    }
    if let Some(gateway) = &network.gateway_ipv6 {
        validate_interface_cidr(gateway, true)?;
    }
    Ok(())
}

fn pmmandns_enabled(network: &NodeNetworkConfig) -> bool {
    network.managed_dns_enabled()
}

fn parse_dns_upstream(value: &str) -> Result<std::net::SocketAddr, String> {
    pmman_protocol::dns::endpoint(value)
}

fn validate_interface_cidr(value: &str, ipv6: bool) -> Result<(), String> {
    let (address, prefix) = value
        .split_once('/')
        .ok_or_else(|| "节点 WireGuard 网关地址必须包含 CIDR 前缀".to_string())?;
    if ipv6 {
        address
            .parse::<Ipv6Addr>()
            .map_err(|_| "节点 IPv6 WireGuard 网关地址无效")?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| "节点 IPv6 WireGuard 网关前缀无效")?;
        if prefix > 128 {
            return Err("节点 IPv6 WireGuard 网关前缀必须在 0 到 128 之间".into());
        }
    } else {
        address
            .parse::<Ipv4Addr>()
            .map_err(|_| "节点 IPv4 WireGuard 网关地址无效")?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| "节点 IPv4 WireGuard 网关前缀无效")?;
        if prefix > 32 {
            return Err("节点 IPv4 WireGuard 网关前缀必须在 0 到 32 之间".into());
        }
    }
    Ok(())
}

fn valid_wireguard_endpoint(value: &str) -> bool {
    let Ok(authority) = value.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    !authority.host().is_empty()
        && !authority.as_str().contains('@')
        && authority.port_u16().is_some_and(|p| p > 0)
}

fn endpoint_port(value: &str) -> Option<u16> {
    value
        .parse::<axum::http::uri::Authority>()
        .ok()
        .and_then(|authority| authority.port_u16())
}

fn endpoints_use_distinct_ports(wireguard: &str, probe: &str) -> bool {
    endpoint_port(wireguard)
        .zip(endpoint_port(probe))
        .is_some_and(|(a, b)| a != b)
}

fn entry_contains_port(entry: &port_forward::EntryRecord, port: u16) -> bool {
    entry
        .ranges
        .iter()
        .any(|range| range.start <= port && port <= range.end)
}

fn validate_entry_service_ports(
    entry: &port_forward::EntryRecord,
    node: &Node,
) -> Result<(), String> {
    // These services are part of the node's control/data plane.  An admin may
    // explicitly reserve one in an entry, but it must never be allocated to a
    // client by accident.
    let mut protected = [22u16, 53, 80, 443].into_iter().collect::<HashSet<_>>();
    if let Some(port) = endpoint_port(&node.wireguard_endpoint) {
        protected.insert(port);
    }
    if let Some(port) = endpoint_port(&node.probe_endpoint) {
        protected.insert(port);
    }
    for port in protected {
        if entry_contains_port(entry, port) && !entry.reserved_ports.contains(&port) {
            return Err(format!(
                "端口范围包含节点系统端口 {port}，请将其加入保留端口后再保存"
            ));
        }
    }
    Ok(())
}

fn valid_wireguard_key(value: &str) -> bool {
    // Canonical base64 encoding of a 32-byte key: 43 characters and one '='.
    value.len() == 44
        && value.ends_with('=')
        && value.as_bytes()[..43]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/'))
        && value.as_bytes()[42].is_ascii()
        && b"AEIMQUYcgkosw048".contains(&value.as_bytes()[42])
        && value != "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
}

fn persist_nodes(state: &AppState, nodes: &HashMap<String, Node>) -> Result<(), String> {
    if let Some(database) = &state.database {
        let payload = serde_json::to_string(nodes).map_err(|_| "节点记录编码失败".to_string())?;
        database.put_document("nodes", &payload)?;
        let _ = database.append_audit(None, "nodes.snapshot.updated", Some("nodes"), None, None);
        return Ok(());
    }
    let Some(path) = &state.node_store else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(nodes).map_err(|_| "节点记录编码失败".to_string())?;
    atomic_write(path, &bytes, true)
        .map_err(|_| "节点接入记录保存失败，请检查主控存储路径和权限".into())
}

fn persist_devices(state: &AppState, devices: &HashMap<Uuid, Device>) -> Result<(), String> {
    let Some(database) = &state.database else {
        return Ok(());
    };
    let payload = serde_json::to_string(devices).map_err(|_| "设备记录编码失败".to_string())?;
    database.put_document("devices", &payload)
}

fn load_device_registry(state: &AppState) -> Result<(), String> {
    let Some(database) = &state.database else {
        return Ok(());
    };
    let Some(payload) = database.get_document("devices")? else {
        return Ok(());
    };
    let mut devices: HashMap<Uuid, Device> =
        serde_json::from_str(&payload).map_err(|_| "设备记录格式错误")?;
    revoke_restored_devices(&mut devices);
    *state.devices.write().expect("device registry poisoned") = devices;
    Ok(())
}

fn revoke_restored_devices(devices: &mut HashMap<Uuid, Device>) {
    // A persisted device is an address/key history record, not proof that a
    // client survived a master restart. The next client connection creates a
    // fresh active authorization for the same identity/address.
    for device in devices.values_mut() {
        // Account-deletion tombstones whose node cleanup was acknowledged
        // retain their released addresses across master restarts.
        if device.status == "released" {
            continue;
        }
        device.status = "revoked".into();
        // Revoking live authorization does not erase historical activity.
        // Keep unknown legacy timestamps unchanged rather than inventing one.
    }
}

fn validate_release_file_name(value: &str) -> bool {
    value.len() <= 160
        && value.starts_with("pmman-node-")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_client_release_file_name(value: &str) -> bool {
    let path = std::path::Path::new(value);
    !value.is_empty()
        && value.len() <= 240
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}

fn validate_client_release_channel(value: &str) -> Result<(), String> {
    if matches!(value, "stable" | "beta") {
        Ok(())
    } else {
        Err("客户端发布渠道无效".into())
    }
}

fn validate_client_release_version(value: &str) -> Result<semver::Version, String> {
    semver::Version::parse(value).map_err(|_| "客户端发布版本无效".into())
}

fn current_client_release(
    state: &AppState,
    channel: &str,
) -> Result<Option<ClientReleaseManifest>, String> {
    validate_client_release_channel(channel)?;
    let Some(directory) = &state.client_release_dir else {
        return Ok(None);
    };
    let manifest_path = directory.join(channel).join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let release: ClientReleaseManifest = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|error| format!("无法读取客户端发布清单：{error}"))?,
    )
    .map_err(|error| format!("客户端发布清单格式错误：{error}"))?;
    release.validate(channel)?;
    #[cfg(not(test))]
    release.verify(
        include_str!("../../config/client-update-public-key.txt"),
        channel,
    )?;
    if release.product != "pmman-network-client"
        || release.channel != channel
        || release.version.is_empty()
        || release.min_supported_version.is_empty()
        || release.sha256.len() != 64
        || !release.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !validate_client_release_file_name(&release.installer_file)
    {
        return Err("客户端发布清单包含无效版本、渠道、摘要或文件名".into());
    }
    verify_release_file(state, &release)?;
    Ok(Some(release))
}

fn release_error(
    status: StatusCode,
    error: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
        }),
    )
}

fn client_release_path(
    state: &AppState,
    channel: &str,
    manifest: &ClientReleaseManifest,
) -> Result<PathBuf, String> {
    let directory = state
        .client_release_dir
        .as_ref()
        .ok_or_else(|| "主控尚未配置客户端发布目录".to_string())?;
    if manifest.channel != channel || !validate_client_release_file_name(&manifest.installer_file) {
        return Err("客户端发布清单文件路径无效".into());
    }
    Ok(directory.join(channel).join(&manifest.installer_file))
}

fn verify_release_file(state: &AppState, manifest: &ClientReleaseManifest) -> Result<(), String> {
    manifest.validate(&manifest.channel)?;
    #[cfg(not(test))]
    manifest.verify(
        include_str!("../../config/client-update-public-key.txt"),
        &manifest.channel,
    )?;
    let path = client_release_path(state, &manifest.channel, manifest)?;
    let metadata = fs::metadata(&path).map_err(|error| format!("客户端安装包不可用：{error}"))?;
    if !metadata.is_file() || metadata.len() != manifest.size {
        return Err("客户端安装包大小与发布清单不一致".into());
    }
    let digest =
        Sha256::digest(fs::read(&path).map_err(|error| format!("客户端安装包读取失败：{error}"))?);
    if format!("{digest:x}") != manifest.sha256.to_ascii_lowercase() {
        return Err("客户端安装包 SHA-256 与发布清单不一致".into());
    }
    Ok(())
}

fn read_client_release_manifests(
    state: &AppState,
    channel: &str,
) -> Result<Vec<ClientReleaseManifest>, String> {
    validate_client_release_channel(channel)?;
    let Some(directory) = &state.client_release_dir else {
        return Ok(Vec::new());
    };
    let channel_dir = directory.join(channel);
    let mut releases = Vec::new();
    if let Ok(entries) = fs::read_dir(&channel_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path().join("manifest.json");
            let Ok(bytes) = fs::read(path) else { continue };
            let Ok(manifest) = serde_json::from_slice::<ClientReleaseManifest>(&bytes) else {
                continue;
            };
            if manifest.channel == channel && verify_release_file(state, &manifest).is_ok() {
                releases.push(manifest);
            }
        }
    }
    // Keep compatibility with the first publication format, which only had a
    // channel-level manifest. New writes always create the version manifest.
    if releases.is_empty() {
        let path = channel_dir.join("manifest.json");
        if let Ok(bytes) = fs::read(path) {
            if let Ok(manifest) = serde_json::from_slice::<ClientReleaseManifest>(&bytes) {
                if manifest.channel == channel && verify_release_file(state, &manifest).is_ok() {
                    releases.push(manifest);
                }
            }
        }
    }
    releases.sort_by(|left, right| {
        semver::Version::parse(&right.version)
            .ok()
            .cmp(&semver::Version::parse(&left.version).ok())
    });
    Ok(releases)
}

fn refresh_client_release_pointer(state: &AppState, channel: &str) -> Result<(), String> {
    let Some(directory) = &state.client_release_dir else {
        return Ok(());
    };
    let pointer = directory.join(channel).join("manifest.json");
    let releases = read_client_release_manifests(state, channel)?;
    write_client_release_pointer(&pointer, releases.first())
}

fn write_client_release_pointer(
    pointer: &PathBuf,
    latest: Option<&ClientReleaseManifest>,
) -> Result<(), String> {
    if let Some(latest) = latest {
        let bytes = serde_json::to_vec_pretty(latest).map_err(|error| error.to_string())?;
        atomic_write(pointer, &bytes, false)?;
    } else if pointer.is_file() {
        fs::remove_file(pointer).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn sign_client_release(
    state: &AppState,
    mut manifest: ClientReleaseManifest,
) -> Result<ClientReleaseManifest, String> {
    let encoded = state
        .client_release_private_key
        .as_deref()
        .ok_or_else(|| "主控未配置客户端发布签名私钥".to_string())?;
    let bytes = STANDARD
        .decode(encoded.trim())
        .map_err(|_| "发布签名私钥格式无效")?;
    let key = SigningKey::from_bytes(&bytes.try_into().map_err(|_| "发布签名私钥长度无效")?);
    manifest.signature = STANDARD.encode(key.sign(&manifest.signing_bytes()?).to_bytes());
    manifest.verify(
        include_str!("../../config/client-update-public-key.txt"),
        &manifest.channel,
    )?;
    Ok(manifest)
}

async fn parse_client_release_upload(
    mut multipart: Multipart,
) -> Result<UploadedClientRelease, (StatusCode, Json<ErrorResponse>)> {
    let mut channel = "stable".to_string();
    let mut version = None;
    let mut minimum = "0.0.1-alpha".to_string();
    let mut mandatory_after = None;
    let mut release_notes = Vec::new();
    let mut package_name = None;
    let mut package = None;
    while let Some(field) = multipart.next_field().await.map_err(|error| {
        release_error(
            StatusCode::BAD_REQUEST,
            format!("读取发布表单失败：{error}"),
        )
    })? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "channel" => {
                channel = field
                    .text()
                    .await
                    .map_err(|error| {
                        release_error(StatusCode::BAD_REQUEST, format!("渠道字段无效：{error}"))
                    })?
                    .trim()
                    .to_string()
            }
            "version" => {
                version = Some(
                    field
                        .text()
                        .await
                        .map_err(|error| {
                            release_error(StatusCode::BAD_REQUEST, format!("版本字段无效：{error}"))
                        })?
                        .trim()
                        .to_string(),
                )
            }
            "minSupportedVersion" | "min_supported_version" => {
                minimum = field
                    .text()
                    .await
                    .map_err(|error| {
                        release_error(
                            StatusCode::BAD_REQUEST,
                            format!("最低版本字段无效：{error}"),
                        )
                    })?
                    .trim()
                    .to_string()
            }
            "mandatoryAfter" | "mandatory_after" => {
                let value = field.text().await.map_err(|error| {
                    release_error(
                        StatusCode::BAD_REQUEST,
                        format!("强制更新时间字段无效：{error}"),
                    )
                })?;
                mandatory_after = (!value.trim().is_empty()).then(|| value.trim().to_string());
            }
            "releaseNotes" | "release_notes" => {
                let value = field.text().await.map_err(|error| {
                    release_error(
                        StatusCode::BAD_REQUEST,
                        format!("更新日志字段无效：{error}"),
                    )
                })?;
                release_notes = serde_json::from_str(&value).unwrap_or_else(|_| {
                    value
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_string)
                        .collect()
                });
            }
            "package" | "installer" => {
                package_name = field.file_name().map(str::to_string);
                let bytes = field.bytes().await.map_err(|error| {
                    release_error(StatusCode::BAD_REQUEST, format!("读取安装包失败：{error}"))
                })?;
                if bytes.len() > 64 * 1024 * 1024 {
                    return Err(release_error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "安装包不能超过 64 MiB",
                    ));
                }
                package = Some(bytes.to_vec());
            }
            _ => {}
        }
    }
    Ok(UploadedClientRelease {
        channel,
        version: version.ok_or_else(|| release_error(StatusCode::BAD_REQUEST, "缺少版本号"))?,
        min_supported_version: minimum,
        mandatory_after,
        release_notes,
        package_name,
        package,
    })
}

fn validate_release_input(input: &UploadedClientRelease) -> Result<semver::Version, String> {
    if !matches!(input.channel.as_str(), "stable" | "beta") {
        return Err("发布渠道必须是 stable 或 beta".into());
    }
    let version = semver::Version::parse(&input.version)
        .map_err(|_| "版本号必须是有效的 SemVer，例如 0.3.0")?;
    let minimum = semver::Version::parse(&input.min_supported_version)
        .map_err(|_| "最低支持版本必须是有效的 SemVer")?;
    if minimum > version {
        return Err("最低支持版本不能高于发布版本".into());
    }
    if let Some(value) = &input.mandatory_after {
        DateTime::parse_from_rfc3339(value).map_err(|_| "强制更新时间必须是 RFC3339 时间")?;
    }
    if input.release_notes.len() > 50
        || input
            .release_notes
            .iter()
            .any(|note| note.len() > 500 || note.chars().any(char::is_control))
    {
        return Err("更新日志最多 50 条，每条不超过 500 字符".into());
    }
    Ok(version)
}

fn write_client_release(
    state: &AppState,
    manifest: &ClientReleaseManifest,
    package: Option<&[u8]>,
) -> Result<(), String> {
    let _guard = client_release_mutation_lock()
        .lock()
        .map_err(|_| "客户端发布锁已损坏".to_string())?;
    let directory = state
        .client_release_dir
        .as_ref()
        .ok_or_else(|| "主控尚未配置客户端发布目录".to_string())?
        .join(&manifest.channel);
    let release_dir = directory.join(&manifest.version);
    fs::create_dir_all(&release_dir).map_err(|error| error.to_string())?;
    if let Some(bytes) = package {
        let path = client_release_path(state, &manifest.channel, manifest)?;
        // The package name includes its digest, so editing a release never
        // overwrites the package referenced by the currently published
        // manifest.  Unreferenced old packages are harmless and can be
        // garbage-collected independently after a successful publication.
        atomic_write(&path, bytes, false)?;
    }
    let manifest_path = release_dir.join("manifest.json");
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    atomic_write(&manifest_path, &bytes, false)?;
    if let Some(database) = &state.database {
        let key = format!("client_release:{}:{}", manifest.channel, manifest.version);
        let payload = serde_json::to_string(manifest).map_err(|error| error.to_string())?;
        database.put_document(&key, &payload)?;
    }
    refresh_client_release_pointer(state, &manifest.channel)
}

async fn list_admin_client_releases(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<AdminClientRelease>>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| release_error(status, "需要管理员权限"))?;
    let active = current_client_release(&state, &channel)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?
        .map(|release| release.version);
    let releases = read_client_release_manifests(&state, &channel)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    Ok(Json(
        releases
            .into_iter()
            .map(|manifest| AdminClientRelease {
                active: active.as_deref() == Some(&manifest.version),
                manifest,
            })
            .collect(),
    ))
}

async fn create_admin_client_release(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<AdminClientRelease>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| release_error(status, "需要管理员权限"))?;
    let mut input = parse_client_release_upload(multipart).await?;
    input.channel = channel;
    let version = validate_release_input(&input)
        .map_err(|error| release_error(StatusCode::BAD_REQUEST, error))?;
    let package = input
        .package
        .as_deref()
        .ok_or_else(|| release_error(StatusCode::BAD_REQUEST, "创建版本必须上传 .exe 安装包"))?;
    let filename = input
        .package_name
        .as_deref()
        .ok_or_else(|| release_error(StatusCode::BAD_REQUEST, "安装包文件名缺失"))?;
    if !filename.to_ascii_lowercase().ends_with(".exe")
        || !validate_client_release_file_name(filename)
        || filename.contains('/')
        || filename.contains('\\')
    {
        return Err(release_error(
            StatusCode::BAD_REQUEST,
            "安装包必须是安全的 .exe 文件名",
        ));
    };
    let channel_dir = state
        .client_release_dir
        .as_ref()
        .ok_or_else(|| {
            release_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "主控尚未配置客户端发布目录",
            )
        })?
        .join(&input.channel);
    if channel_dir
        .join(version.to_string())
        .join("manifest.json")
        .is_file()
    {
        return Err(release_error(StatusCode::CONFLICT, "该版本已经存在"));
    }
    let manifest = ClientReleaseManifest {
        product: "pmman-network-client".into(),
        channel: input.channel.clone(),
        platform: "windows".into(),
        arch: "x64".into(),
        version: version.to_string(),
        min_supported_version: input.min_supported_version,
        mandatory_after: input.mandatory_after,
        installer_file: format!(
            "{}/pmman-network-{:x}.exe",
            version,
            Sha256::digest(package)
        ),
        size: package.len() as u64,
        sha256: format!("{:x}", Sha256::digest(package)),
        signature: String::new(),
        published_at: Utc::now().to_rfc3339(),
        release_notes: input.release_notes,
    };
    let manifest = sign_client_release(&state, manifest)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    write_client_release(&state, &manifest, Some(package))
        .map_err(|error| release_error(StatusCode::BAD_GATEWAY, error))?;
    let active = current_client_release(&state, &manifest.channel)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?
        .is_some_and(|release| release.version == manifest.version);
    Ok(Json(AdminClientRelease { manifest, active }))
}

async fn edit_admin_client_release(
    State(state): State<AppState>,
    Path((channel, version)): Path<(String, String)>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<AdminClientRelease>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| release_error(status, "需要管理员权限"))?;
    let old = read_client_release_manifests(&state, &channel)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?
        .into_iter()
        .find(|release| release.version == version)
        .ok_or_else(|| release_error(StatusCode::NOT_FOUND, "发布版本不存在"))?;
    let mut input = parse_client_release_upload(multipart).await?;
    input.channel = channel.clone();
    input.version = version.clone();
    let _ = validate_release_input(&input)
        .map_err(|error| release_error(StatusCode::BAD_REQUEST, error))?;
    let (installer_file, package) = if let Some(bytes) = input.package.as_deref() {
        let filename = input
            .package_name
            .as_deref()
            .ok_or_else(|| release_error(StatusCode::BAD_REQUEST, "安装包文件名缺失"))?;
        if !filename.to_ascii_lowercase().ends_with(".exe")
            || !validate_client_release_file_name(filename)
            || filename.contains('/')
            || filename.contains('\\')
        {
            return Err(release_error(
                StatusCode::BAD_REQUEST,
                "安装包必须是安全的 .exe 文件名",
            ));
        }
        (
            format!("{}/pmman-network-{:x}.exe", version, Sha256::digest(bytes)),
            Some(bytes),
        )
    } else {
        (old.installer_file.clone(), None)
    };
    let manifest = ClientReleaseManifest {
        product: old.product,
        channel: channel.clone(),
        platform: old.platform,
        arch: old.arch,
        version: version.clone(),
        min_supported_version: input.min_supported_version,
        mandatory_after: input.mandatory_after,
        installer_file,
        size: package.map(|bytes| bytes.len() as u64).unwrap_or(old.size),
        sha256: package
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .unwrap_or(old.sha256),
        signature: String::new(),
        published_at: old.published_at,
        release_notes: input.release_notes,
    };
    let manifest = sign_client_release(&state, manifest)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    write_client_release(&state, &manifest, package)
        .map_err(|error| release_error(StatusCode::BAD_GATEWAY, error))?;
    Ok(Json(AdminClientRelease {
        active: current_client_release(&state, &channel)
            .ok()
            .flatten()
            .is_some_and(|release| release.version == version),
        manifest,
    }))
}

async fn delete_admin_client_release(
    State(state): State<AppState>,
    Path((channel, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| release_error(status, "需要管理员权限"))?;
    let _guard = client_release_mutation_lock()
        .lock()
        .map_err(|_| release_error(StatusCode::SERVICE_UNAVAILABLE, "客户端发布锁已损坏"))?;
    validate_client_release_channel(&channel)
        .and_then(|_| validate_client_release_version(&version).map(|_| ()))
        .map_err(|error| release_error(StatusCode::BAD_REQUEST, error))?;
    let directory = state
        .client_release_dir
        .as_ref()
        .ok_or_else(|| {
            release_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "主控尚未配置客户端发布目录",
            )
        })?
        .join(&channel);
    let target = directory.join(&version);
    if !target.is_dir() {
        return Err(release_error(StatusCode::NOT_FOUND, "发布版本不存在"));
    }
    let active = current_client_release(&state, &channel)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let releases = read_client_release_manifests(&state, &channel)
        .map_err(|error| release_error(StatusCode::SERVICE_UNAVAILABLE, error))?;
    let replacement = releases
        .iter()
        .filter(|release| release.version != version)
        .collect::<Vec<_>>();
    let deleting_active = active
        .as_ref()
        .is_some_and(|release| release.version == version);
    if deleting_active && replacement.is_empty() {
        return Err(release_error(
            StatusCode::CONFLICT,
            "不能删除渠道唯一的当前客户端版本，请先发布替代版本",
        ));
    }
    // If the active version is being removed, publish a verified replacement
    // first. The old directory remains intact until the pointer commit has
    // succeeded, so a failed delete never leaves clients with a dead pointer.
    if deleting_active {
        let pointer = directory.join("manifest.json");
        write_client_release_pointer(&pointer, replacement.first().copied())
            .map_err(|error| release_error(StatusCode::BAD_GATEWAY, error))?;
    }
    fs::remove_dir_all(target).map_err(|error| {
        release_error(
            StatusCode::BAD_GATEWAY,
            format!("删除发布版本失败：{error}"),
        )
    })?;
    if let Some(database) = &state.database {
        let key = format!("client_release:{}:{}", channel, version);
        database
            .delete_document(&key)
            .map_err(|error| release_error(StatusCode::BAD_GATEWAY, error))?;
    }
    if !deleting_active {
        refresh_client_release_pointer(&state, &channel)
            .map_err(|error| release_error(StatusCode::BAD_GATEWAY, error))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn client_release_latest(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    Query(query): Query<ClientReleaseQuery>,
) -> Result<Json<ClientReleaseResponse>, StatusCode> {
    if query
        .platform
        .as_deref()
        .is_some_and(|value| value != "windows")
        || query.arch.as_deref().is_some_and(|value| value != "x64")
    {
        return Ok(Json(ClientReleaseResponse {
            available: false,
            manifest: None,
            download: None,
        }));
    }
    let release =
        current_client_release(&state, &channel).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let download = release.as_ref().map(|release| {
        format!(
            "/v1/client/releases/{}/{}/download",
            channel, release.version
        )
    });
    Ok(Json(ClientReleaseResponse {
        available: release.is_some(),
        manifest: release,
        download,
    }))
}

async fn client_release_download(
    State(state): State<AppState>,
    Path((channel, version)): Path<(String, String)>,
) -> Result<Response, StatusCode> {
    let release = current_client_release(&state, &channel)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if release.version != version {
        return Err(StatusCode::NOT_FOUND);
    }
    let directory = state
        .client_release_dir
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let file = tokio::fs::File::open(directory.join(&channel).join(&release.installer_file))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, release.size)
        .header(header::CACHE_CONTROL, "public, max-age=300")
        .header(header::ETAG, format!("\"{}\"", release.sha256))
        .header("x-pmman-client-version", release.version)
        .header("x-pmman-client-sha256", release.sha256)
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn current_node_release(state: &AppState) -> Result<Option<NodeRelease>, String> {
    let Some(directory) = &state.node_resource_dir else {
        return Ok(None);
    };
    let manifest_path = directory.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let release: NodeRelease = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|error| format!("无法读取节点资源清单：{error}"))?,
    )
    .map_err(|error| format!("节点资源清单格式错误：{error}"))?;
    if release.version.is_empty()
        || release.sha256.len() != 64
        || !release.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !validate_release_file_name(&release.file_name)
    {
        return Err("节点资源清单包含无效版本或文件名".into());
    }
    let resource_path = directory.join(&release.file_name);
    let metadata =
        fs::metadata(&resource_path).map_err(|error| format!("节点资源文件不可用：{error}"))?;
    if !metadata.is_file() || metadata.len() != release.size {
        return Err("节点资源文件大小与清单不一致".into());
    }
    Ok(Some(release))
}

#[derive(Deserialize, Default)]
struct NodeResourceQuery {
    version: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct GeositeManifest {
    version: String,
    file_name: String,
    sha256: String,
    size: u64,
    domestic: String,
    foreign: String,
}

fn current_geosite(state: &AppState) -> Result<Option<(GeositeManifest, PathBuf)>, String> {
    let Some(directory) = &state.geosite_dir else {
        return Ok(None);
    };
    let manifest_path = directory.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    // A heartbeat must not hash/decode a multi-megabyte file every second.
    type Cached = (
        std::time::SystemTime,
        std::time::SystemTime,
        GeositeManifest,
        PathBuf,
    );
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "geosite cache poisoned")?;
    let manifest_time = fs::metadata(&manifest_path)
        .and_then(|m| m.modified())
        .map_err(|error| error.to_string())?;
    if let Some((mt, ft, manifest, path)) = cache.get(directory) {
        if *mt == manifest_time
            && fs::metadata(path)
                .is_ok_and(|m| m.len() == manifest.size && m.modified().ok() == Some(*ft))
        {
            return Ok(Some((manifest.clone(), path.clone())));
        }
    }
    let manifest: GeositeManifest = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|error| format!("无法读取 geosite 清单：{error}"))?,
    )
    .map_err(|error| format!("geosite 清单格式错误：{error}"))?;
    if manifest.version.trim().is_empty()
        || manifest.file_name.is_empty()
        || matches!(manifest.file_name.as_str(), "." | "..")
        || manifest.file_name.contains(['/', '\\'])
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || manifest.size == 0
        || manifest.size > pmman_protocol::geosite::MAX_FILE_SIZE as u64
        || manifest.domestic.trim().is_empty()
        || manifest.foreign.trim().is_empty()
    {
        return Err("geosite 清单包含无效字段".into());
    }
    let path = directory.join(&manifest.file_name);
    let metadata = fs::metadata(&path).map_err(|error| format!("geosite 文件不可用：{error}"))?;
    if !metadata.is_file() || metadata.len() != manifest.size {
        return Err("geosite 文件大小与清单不一致".into());
    }
    let bytes = fs::read(&path).map_err(|error| format!("无法读取 geosite 文件：{error}"))?;
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != manifest.sha256.to_ascii_lowercase() {
        return Err("geosite 文件 SHA-256 校验失败".into());
    }
    validate_geosite_categories(&bytes, &manifest.domestic, &manifest.foreign)?;
    cache.insert(
        directory.clone(),
        (
            manifest_time,
            metadata.modified().map_err(|error| error.to_string())?,
            manifest.clone(),
            path.clone(),
        ),
    );
    Ok(Some((manifest, path)))
}

fn validate_geosite_categories(bytes: &[u8], domestic: &str, foreign: &str) -> Result<(), String> {
    for value in [domestic, foreign] {
        let selectors: Vec<_> = value
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect();
        if value.len() > 1024 || selectors.len() > 16 {
            return Err("geosite 分类最多 16 项，总长度最多 1024 字符".into());
        }
        pmman_protocol::geosite::rules(bytes, &selectors)?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeositeInfo {
    #[serde(flatten)]
    config: pmman_protocol::GeositeConfig,
    categories: Vec<String>,
}

async fn admin_geosite(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Option<GeositeInfo>>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| release_error(status, "需要管理员权限"))?;
    let info = current_geosite(&state)
        .map_err(server_error)?
        .map(|(manifest, path)| -> Result<_, String> {
            Ok(GeositeInfo {
                config: geosite_config(&manifest),
                categories: pmman_protocol::geosite::categories(
                    &fs::read(path).map_err(|error| error.to_string())?,
                )?,
            })
        })
        .transpose()
        .map_err(server_error)?;
    Ok(Json(info))
}

async fn save_admin_geosite(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Option<GeositeInfo>>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| release_error(status, "需要管理员权限"))?;
    let mut fields = HashMap::new();
    let mut package = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| release_error(StatusCode::BAD_REQUEST, error.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" {
            let bytes = field
                .bytes()
                .await
                .map_err(|error| release_error(StatusCode::BAD_REQUEST, error.to_string()))?;
            if bytes.len() > pmman_protocol::geosite::MAX_FILE_SIZE {
                return Err(release_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "geosite 文件最多 32 MiB",
                ));
            }
            package = Some(bytes.to_vec());
        } else if ["version", "domestic", "foreign"].contains(&name.as_str()) {
            fields.insert(
                name,
                field
                    .text()
                    .await
                    .map_err(|error| release_error(StatusCode::BAD_REQUEST, error.to_string()))?
                    .trim()
                    .to_owned(),
            );
        }
    }
    // No await while holding the mutation lock. Both file and manifest are
    // immutable/atomic generations; failed validation leaves the old pointer.
    {
        let _lock = client_release_mutation_lock()
            .lock()
            .map_err(|_| release_error(StatusCode::SERVICE_UNAVAILABLE, "资源发布锁错误"))?;
        let old = current_geosite(&state).map_err(server_error)?;
        let bytes = package
            .or_else(|| old.as_ref().and_then(|(_, path)| fs::read(path).ok()))
            .ok_or_else(|| release_error(StatusCode::BAD_REQUEST, "请上传 geosite.dat"))?;
        let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
        let version = get("version");
        if version.is_empty()
            || version.len() > 64
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(release_error(
                StatusCode::BAD_REQUEST,
                "版本号限 64 位字母、数字、点、短横线及下划线",
            ));
        }
        let domestic = get("domestic");
        let foreign = get("foreign");
        validate_geosite_categories(&bytes, &domestic, &foreign)
            .map_err(|error| release_error(StatusCode::BAD_REQUEST, error))?;
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let directory = state.geosite_dir.as_ref().ok_or_else(|| {
            release_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "主控未配置 geosite 资源目录",
            )
        })?;
        let manifest = GeositeManifest {
            version,
            file_name: format!("geosite-{hash}.dat"),
            sha256: hash,
            size: bytes.len() as u64,
            domestic,
            foreign,
        };
        atomic_write(&directory.join(&manifest.file_name), &bytes, true).map_err(server_error)?;
        atomic_write(
            &directory.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)
                .map_err(|error| server_error(error.to_string()))?,
            true,
        )
        .map_err(server_error)?;
    }
    admin_geosite(State(state), headers).await
}

fn geosite_config(manifest: &GeositeManifest) -> pmman_protocol::GeositeConfig {
    pmman_protocol::GeositeConfig {
        version: manifest.version.clone(),
        sha256: manifest.sha256.to_ascii_lowercase(),
        size: manifest.size,
        domestic: manifest.domestic.clone(),
        foreign: manifest.foreign.clone(),
    }
}

async fn node_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    Query(query): Query<NodeResourceQuery>,
) -> Result<Response, StatusCode> {
    let token = node_token_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // A node may be unpublished/disabled for client selection while it still
    // needs to receive control-plane resources and security updates. The
    // token remains permanently bound to this exact node ID.
    if node.node_token != token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let release = current_node_release(&state)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if query
        .version
        .as_deref()
        .is_some_and(|version| version != release.version)
    {
        return Err(StatusCode::CONFLICT);
    }
    let directory = state
        .node_resource_dir
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let bytes = fs::read(directory.join(&release.file_name))
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header("x-pmman-node-version", release.version)
        .header("x-pmman-node-sha256", release.sha256)
        .header("x-pmman-node-size", release.size)
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn geosite_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    Query(query): Query<NodeResourceQuery>,
) -> Result<Response, StatusCode> {
    let token = node_token_from_headers(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    // Unpublished nodes still receive node-managed resources. The enabled
    // flag controls client availability, not authenticated maintenance.
    if node.node_token != token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (manifest, path) = current_geosite(&state)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if query
        .version
        .as_deref()
        .is_some_and(|version| version != manifest.version)
    {
        return Err(StatusCode::CONFLICT);
    }
    let bytes = fs::read(path).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header("x-pmman-geosite-version", manifest.version)
        .header("x-pmman-geosite-sha256", manifest.sha256)
        .header("x-pmman-geosite-size", manifest.size)
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn client_geosite_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NodeResourceQuery>,
) -> Result<Response, StatusCode> {
    authenticate(&state, &headers)?;
    let (manifest, path) = current_geosite(&state)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if query
        .version
        .as_deref()
        .is_some_and(|version| version != manifest.version)
    {
        return Err(StatusCode::CONFLICT);
    }
    let bytes = fs::read(path).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header("x-pmman-geosite-version", manifest.version)
        .header("x-pmman-geosite-sha256", manifest.sha256)
        .header("x-pmman-geosite-size", manifest.size)
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn admin_node_release(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Option<NodeRelease>>, StatusCode> {
    require_admin(&state, &headers)?;
    current_node_release(&state)
        .map(Json)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

async fn admin_node_bird_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
) -> Result<Json<NodeBirdConfigResponse>, StatusCode> {
    require_admin(&state, &headers)?;
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(node_bird_config(&node)))
}

fn pending_node(mut input: NodeEnrollmentRequest, token: String) -> Result<Node, String> {
    input.network.apply_managed_dns_defaults();
    if !valid_node_id(&input.node_id)
        || input.name.trim().is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
        || input
            .region
            .as_ref()
            .is_some_and(|v| v.len() > 128 || v.chars().any(char::is_control))
    {
        return Err(
            "节点 ID 仅允许字母、数字、短横线和下划线（1–64 位），名称和区域最长 128 位".into(),
        );
    }
    validate_node_network(&input.network)?;
    let route_table = input
        .route_table
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(validate_route_table)
        .transpose()?;
    let wireguard_interface =
        validate_optional_linux_interface(input.wireguard_interface.as_deref())?;
    Ok(Node {
        rate_limit: NodeRateLimit::default(),
        resource_version: None,
        desired_resource_version: None,
        node_id: input.node_id,
        name: input.name.trim().into(),
        region: input.region,
        wireguard_endpoint: String::new(),
        probe_endpoint: String::new(),
        network: input.network,
        network_override: None,
        applied: Vec::new(),
        sync_errors: Vec::new(),
        applied_route_table: None,
        cleanup_ok: false,
        port_forward_applied: Vec::new(),
        port_forward_errors: Vec::new(),
        server_public_key: String::new(),
        wireguard_interface,
        route_table,
        node_token: token,
        last_seen_at: DateTime::UNIX_EPOCH,
        enabled: true,
        connectable: input.connectable,
        registered: false,
        wireguard_endpoint_override: None,
        probe_endpoint_override: None,
        region_override: None,
        server_public_key_override: None,
    })
}

async fn enroll_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<NodeEnrollmentRequest>,
) -> Result<Json<NodeEnrollmentResponse>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|s| {
        (
            s,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let node = pending_node(input, token.clone())
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    let mut nodes = state.nodes.write().expect("node registry poisoned");
    if nodes.contains_key(&node.node_id) {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "节点 ID 已存在".into(),
            }),
        ));
    }
    let response = NodeEnrollmentResponse {
        node: node_summary(&node),
        node_token: token,
    };
    let mut next = nodes.clone();
    next.insert(node.node_id.clone(), node);
    persist_nodes(&state, &next).map_err(server_error)?;
    *nodes = next;
    Ok(Json(response))
}

async fn cancel_enrollment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|s| {
        (
            s,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    let mut nodes = state.nodes.write().expect("node registry poisoned");
    let node = nodes.get(&node_id).ok_or((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "节点不存在".into(),
        }),
    ))?;
    if node.registered {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "已注册节点请使用停用功能，取消接入仅适用于待接入节点".into(),
            }),
        ));
    }
    let mut next = nodes.clone();
    next.remove(&node_id);
    persist_nodes(&state, &next).map_err(server_error)?;
    *nodes = next;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_client_nodes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<NodeSummary>>, StatusCode> {
    authenticate(&state, &headers)?;
    let mut nodes: Vec<_> = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .values()
        .filter(|node| node_is_connectable(node))
        .map(node_summary)
        .collect();
    nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    Ok(Json(nodes))
}

async fn node_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<NodeHeartbeatResponse>, StatusCode> {
    require_node(&state, &headers, &node_id)?;
    let mut input: NodeHeartbeatRequest = if body.is_empty() {
        NodeHeartbeatRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?
    };
    if let Some(network) = &mut input.network {
        network.apply_managed_dns_defaults();
    }
    for id in &input.released_forward_ids {
        if let Ok(Some(allocation)) = state.port_forward_store.allocation(id) {
            if allocation.node_id == node_id && allocation.state == "releasing" {
                let _ = state.port_forward_store.confirm_release(id);
            }
        }
    }
    address_pools::acknowledge_forward_cleanup(&state, &node_id, &input.released_forward_ids)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    for applied in &input.applied_port_forwards {
        if let Ok(Some(allocation)) = state.port_forward_store.allocation(&applied.forward_id) {
            if allocation.node_id == node_id
                && allocation.config_version == applied.config_version
                && matches!(allocation.state.as_str(), "pending" | "applying" | "active")
            {
                let _ =
                    state
                        .port_forward_store
                        .set_state(&applied.forward_id, "active", None, None);
            }
        }
    }
    for error in &input.port_forward_errors {
        if let Ok(Some(allocation)) = state.port_forward_store.allocation(&error.forward_id) {
            if allocation.node_id == node_id
                && allocation.config_version == error.config_version
                && matches!(allocation.state.as_str(), "pending" | "applying" | "active")
            {
                let _ = state.port_forward_store.set_state(
                    &error.forward_id,
                    "error",
                    Some(&error.error),
                    Some(Utc::now()),
                );
            }
        }
    }
    let _ = state.port_forward_store.expire_grace();
    if input
        .network
        .as_ref()
        .is_some_and(|network| validate_node_network(network).is_err())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    expire_client_leases(&state, Utc::now()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    user_deletion::confirm_device_cleanup(&state, &node_id, &input)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if let Some(node) = state
        .nodes
        .write()
        .expect("node registry poisoned")
        .get_mut(&node_id)
    {
        node.last_seen_at = Utc::now();
        node.applied = input.applied;
        node.sync_errors = input.errors;
        node.applied_route_table = input.route_table;
        node.cleanup_ok = input.cleanup_ok;
        node.port_forward_applied = input.applied_port_forwards;
        node.port_forward_errors = input.port_forward_errors;
        node.resource_version = input.resource_version;
        if let Some(network) = input.network {
            // An administrator override is the authoritative configuration
            // for client delivery.  Keep accepting the node's report when no
            // override exists so a freshly deployed node can self-register.
            if node.network_override.is_none() {
                node.network = network;
            }
        }
    }
    let devices = state.devices.read().expect("device registry poisoned");
    let revoked = devices
        .values()
        .filter(|device| {
            device.node_id == node_id && !matches!(device.status.as_str(), "active" | "released")
        })
        .map(|device| NodePeer {
            device_id: device.device_id,
            public_key: device.public_key.clone(),
            ipv4: device.ipv4.clone(),
            ipv6: device.ipv6.clone(),
        })
        .collect();
    let active = devices
        .values()
        .filter(|device| device.node_id == node_id && device.status == "active")
        .map(|device| ActiveNodePeer {
            device_id: device.device_id,
            public_key: device.public_key.clone(),
            ipv4: device.ipv4.clone(),
            ipv6: device.ipv6.clone(),
            config_version: device.config_version,
        })
        .collect();
    let route_table = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .and_then(|node| node.route_table.clone());
    let mut network = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .map(node_network)
        .unwrap_or_default();
    let geosite = current_geosite(&state)
        .ok()
        .flatten()
        .filter(|_| network.pmmandns_geosite_enabled)
        .map(|(manifest, _)| {
            let config = geosite_config(&manifest);
            network.pmmandns_geosite = Some(config.clone());
            config
        });
    let desired_resource_version = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .and_then(|node| node.desired_resource_version.clone());
    drop(devices);
    let release = current_node_release(&state)
        .ok()
        .flatten()
        .filter(|release| {
            desired_resource_version
                .as_deref()
                .is_some_and(|desired| desired == release.version)
        });
    let port_forwards = state
        .port_forward_store
        .desired_for_node(&node_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|allocation| {
            let entry = state
                .port_forward_store
                .entry(&allocation.entry_id)
                .ok()
                .flatten()?;
            Some(pmman_protocol::NodePortForward {
                forward_id: allocation.forward_id,
                config_version: allocation.config_version,
                protocols: allocation.protocols,
                listen_address: entry.public_address,
                listen_port: allocation.public_port,
                target_address: allocation.target_address,
                target_port: allocation.target_port,
            })
        })
        .collect();
    Ok(Json(NodeHeartbeatResponse {
        relay_peers: p2p::relay_peers(&state),
        revoked,
        active,
        route_table,
        network,
        release,
        geosite,
        port_forwards,
        revoked_forward_ids: state
            .port_forward_store
            .allocations(None)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .into_iter()
            .filter(|item| item.node_id == node_id && item.state == "releasing")
            .map(|item| item.forward_id)
            .chain(address_pools::pending_forward_cleanup(&state, &node_id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect(),
    }))
}

#[derive(Clone, Serialize)]
struct PortForwardView {
    forward_id: String,
    user_id: String,
    username: String,
    device_id: Uuid,
    entry_id: String,
    node_id: String,
    protocols: Vec<pmman_protocol::PortForwardProtocol>,
    public_address: String,
    public_host: Option<String>,
    public_port: u16,
    target_address: String,
    target_port: u16,
    state: String,
    config_version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    error_message: Option<String>,
}

fn entry_view(state: &AppState, entry: &port_forward::EntryRecord) -> PortForwardEntryView {
    let total_ports = entry
        .ranges
        .iter()
        .map(|range| u32::from(range.end) - u32::from(range.start) + 1)
        .sum::<u32>();
    let allocations = state
        .port_forward_store
        .allocations(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|item| {
            item.entry_id == entry.entry_id
                && !matches!(item.state.as_str(), "released" | "expired")
        })
        .collect::<Vec<_>>();
    let allocated_ports = allocations
        .iter()
        .map(|item| item.public_port)
        .collect::<HashSet<_>>()
        .len() as u32;
    // A reserved port outside the configured ranges is not a resource in
    // this entry and must not reduce its advertised capacity.
    let reserved = entry
        .reserved_ports
        .iter()
        .filter(|port| {
            entry
                .ranges
                .iter()
                .any(|range| range.start <= **port && **port <= range.end)
        })
        .collect::<HashSet<_>>()
        .len() as u32;
    let available_ports = total_ports
        .saturating_sub(reserved)
        .saturating_sub(allocated_ports);
    let node_healthy = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&entry.node_id)
        .is_some_and(node_is_healthy);
    PortForwardEntryView {
        entry_id: entry.entry_id.clone(),
        node_id: entry.node_id.clone(),
        name: entry.name.clone(),
        public_address: entry.public_address.clone(),
        hostname: entry.hostname.clone(),
        family: entry.family.clone(),
        protocols: entry.protocols.clone(),
        port_ranges: entry
            .ranges
            .iter()
            .map(|range| pmman_protocol::PortForwardRange {
                start: range.start,
                end: range.end,
            })
            .collect(),
        reserved_ports: entry.reserved_ports.clone(),
        enabled: entry.enabled,
        node_healthy,
        total_ports,
        allocated_ports,
        available_ports,
    }
}

fn forward_view(
    state: &AppState,
    allocation: port_forward::AllocationRecord,
) -> Result<PortForwardView, String> {
    let entry = state
        .port_forward_store
        .entry(&allocation.entry_id)?
        .ok_or_else(|| "端口转发入口不存在".to_string())?;
    Ok(PortForwardView {
        forward_id: allocation.forward_id,
        user_id: allocation.user_id,
        username: allocation.username,
        device_id: allocation.device_id,
        entry_id: allocation.entry_id,
        node_id: allocation.node_id,
        protocols: allocation.protocols,
        public_address: entry.public_address,
        public_host: entry.hostname,
        public_port: allocation.public_port,
        target_address: allocation.target_address,
        target_port: allocation.target_port,
        state: allocation.state,
        config_version: allocation.config_version,
        created_at: allocation.created_at,
        updated_at: allocation.updated_at,
        expires_at: allocation.expires_at,
        error_message: allocation.error_message,
    })
}

fn protocols_supported(
    requested: &[pmman_protocol::PortForwardProtocol],
    supported: &[pmman_protocol::PortForwardProtocol],
) -> bool {
    !requested.is_empty()
        && requested
            .iter()
            .all(|protocol| supported.contains(protocol))
}

fn target_port_allowed(policy: &port_forward::PolicyRecord, port: u16) -> bool {
    policy
        .allowed_target_ports
        .as_ref()
        .is_none_or(|ports| ports.contains(&port))
}

async fn list_client_forward_entries(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PortForwardEntryView>>, StatusCode> {
    let principal = authenticate(&state, &headers)?;
    let policy = state
        .port_forward_store
        .policy(&principal.user_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entries = state
        .port_forward_store
        .entries()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        entries
            .into_iter()
            .filter(|entry| {
                policy.enabled
                    && policy.max_active_forwards > 0
                    && entry.enabled
                    && policy.allowed_entry_ids.contains(&entry.entry_id)
                    && policy.allowed_families.contains(&entry.family)
                    && protocols_supported(&entry.protocols, &policy.allowed_protocols)
                    && state
                        .nodes
                        .read()
                        .expect("node registry poisoned")
                        .get(&entry.node_id)
                        .is_some_and(node_is_healthy)
            })
            .map(|entry| entry_view(&state, &entry))
            .collect(),
    ))
}

async fn get_client_forward_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<pmman_protocol::PortForwardQuota>, StatusCode> {
    let principal = authenticate(&state, &headers)?;
    let policy = state
        .port_forward_store
        .policy(&principal.user_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let used = state
        .port_forward_store
        .active_count(&principal.user_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(pmman_protocol::PortForwardQuota {
        enabled: policy.enabled,
        limit: policy.max_active_forwards,
        used,
        available: policy.max_active_forwards.saturating_sub(used),
    }))
}

async fn list_client_forwards(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PortForwardView>>, StatusCode> {
    let principal = authenticate(&state, &headers)?;
    let allocations = state
        .port_forward_store
        .allocations(Some(&principal.user_id))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        allocations
            .into_iter()
            .filter_map(|allocation| forward_view(&state, allocation).ok())
            .collect(),
    ))
}

async fn create_client_forward(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ClientPortForwardRequest>,
) -> Result<Json<PortForwardView>, (StatusCode, Json<ErrorResponse>)> {
    let principal = authenticate(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "客户端会话无效".into(),
            }),
        )
    })?;
    let allocation = {
        let devices = state.devices.read().expect("device registry poisoned");
        let users = state.users.read().expect("user registry poisoned");
        if !users
            .get(&principal.username)
            .is_some_and(|user| user.enabled && user.user_id == principal.user_id)
        {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "客户端会话无效".into(),
                }),
            ));
        }
        let policy = state
            .port_forward_store
            .policy(&principal.user_id)
            .map_err(server_error)?;
        if !policy.enabled || policy.max_active_forwards == 0 {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "当前账户未授权端口转发".into(),
                }),
            ));
        }
        if input.target_port == 0 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "目标端口必须在 1 到 65535 之间".into(),
                }),
            ));
        }
        if state
            .port_forward_store
            .active_count(&principal.user_id)
            .map_err(server_error)?
            >= policy.max_active_forwards
        {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "已达到端口转发最大并发数".into(),
                }),
            ));
        }
        if !protocols_supported(&input.protocols, &policy.allowed_protocols)
            || !target_port_allowed(&policy, input.target_port)
        {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "请求的协议或目标端口未获授权".into(),
                }),
            ));
        }
        let device = devices
            .get(&input.device_id)
            .filter(|device| device.user_id == principal.user_id && device.status == "active")
            .cloned()
            .ok_or((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "目标设备未连接".into(),
                }),
            ))?;
        let entries = state.port_forward_store.entries().map_err(server_error)?;
        let entry = input
            .entry_id
            .as_deref()
            .and_then(|id| entries.iter().find(|entry| entry.entry_id == id).cloned())
            .or_else(|| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry.enabled && policy.allowed_entry_ids.contains(&entry.entry_id)
                    })
                    .find(|entry| {
                        state
                            .nodes
                            .read()
                            .expect("node registry poisoned")
                            .get(&entry.node_id)
                            .is_some_and(node_is_healthy)
                    })
                    .cloned()
            })
            .ok_or((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "没有可用的端口转发入口".into(),
                }),
            ))?;
        let node_healthy = state
            .nodes
            .read()
            .expect("node registry poisoned")
            .get(&entry.node_id)
            .is_some_and(node_is_healthy);
        if !entry.enabled
            || !node_healthy
            || !policy.allowed_entry_ids.contains(&entry.entry_id)
            || !policy.allowed_families.contains(&entry.family)
            || !protocols_supported(&input.protocols, &entry.protocols)
        {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "入口不存在、未授权或不支持请求协议".into(),
                }),
            ));
        }
        let target_address = match entry.family.as_str() {
            "ipv4" => device.ipv4.clone(),
            "ipv6" => device.ipv6.clone(),
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "入口地址族无效".into(),
                    }),
                ))
            }
        };
        let allocation = state
            .port_forward_store
            .allocate(
                &principal.user_id,
                &principal.username,
                device.device_id,
                &entry,
                &input.protocols,
                &target_address,
                input.target_port,
            )
            .map_err(|error| (StatusCode::CONFLICT, Json(ErrorResponse { error })))?;
        allocation
    };
    state
        .port_forward_store
        .set_state(&allocation.forward_id, "applying", None, None)
        .map_err(server_error)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let current = state
            .port_forward_store
            .allocation(&allocation.forward_id)
            .map_err(server_error)?;
        match current {
            Some(current) if current.state == "active" => {
                return forward_view(&state, current)
                    .map(Json)
                    .map_err(server_error)
            }
            Some(current) if current.state == "error" => {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse {
                        error: current
                            .error_message
                            .unwrap_or_else(|| "节点未能应用端口转发".into()),
                    }),
                ))
            }
            _ if tokio::time::Instant::now() >= deadline => {
                let _ = state.port_forward_store.set_state(
                    &allocation.forward_id,
                    "released",
                    Some("等待节点应用超时"),
                    None,
                );
                return Err((
                    StatusCode::BAD_GATEWAY,
                    Json(ErrorResponse {
                        error: "等待节点应用端口转发超时".into(),
                    }),
                ));
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
}

async fn delete_client_forward(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> StatusCode {
    let Ok(principal) = authenticate(&state, &headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    let Ok(Some(allocation)) = state.port_forward_store.allocation(&id) else {
        return StatusCode::NOT_FOUND;
    };
    if allocation.user_id != principal.user_id {
        return StatusCode::FORBIDDEN;
    }
    match state
        .port_forward_store
        .set_state(&id, "released", None, None)
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn list_admin_forward_entries(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PortForwardEntryView>>, StatusCode> {
    require_admin(&state, &headers)?;
    Ok(Json(
        state
            .port_forward_store
            .entries()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .iter()
            .map(|entry| entry_view(&state, entry))
            .collect(),
    ))
}

async fn save_admin_forward_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Option<Path<String>>,
    Json(input): Json<PortForwardEntryRequest>,
) -> Result<Json<PortForwardEntryView>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    if let Some(Path(id)) = path {
        if id != input.entry_id {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "入口 ID 不能修改".into(),
                }),
            ));
        }
    }
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&input.node_id)
        .cloned();
    let Some(node) = node else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "节点不存在".into(),
            }),
        ));
    };
    let address = input.public_address.parse::<IpAddr>().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "公网地址无效".into(),
            }),
        )
    })?;
    if (input.family == "ipv4" && !address.is_ipv4())
        || (input.family == "ipv6" && !address.is_ipv6())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "公网地址与地址族不匹配".into(),
            }),
        ));
    }
    let entry = port_forward::EntryRecord {
        entry_id: input.entry_id,
        node_id: input.node_id,
        name: input.name,
        public_address: input.public_address,
        hostname: input.hostname,
        family: input.family,
        protocols: input.protocols,
        ranges: input.port_ranges,
        reserved_ports: input.reserved_ports,
        enabled: input.enabled,
    };
    validate_entry_service_ports(&entry, &node)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    state
        .port_forward_store
        .save_entry(&entry)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    Ok(Json(entry_view(&state, &entry)))
}

async fn delete_admin_forward_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> StatusCode {
    if require_admin(&state, &headers).is_err() {
        return StatusCode::FORBIDDEN;
    }
    match state.port_forward_store.delete_entry(&id) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::CONFLICT,
    }
}

async fn list_admin_forward_allocations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PortForwardView>>, StatusCode> {
    require_admin(&state, &headers)?;
    Ok(Json(
        state
            .port_forward_store
            .allocations(None)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .into_iter()
            .filter_map(|allocation| forward_view(&state, allocation).ok())
            .collect(),
    ))
}

async fn revoke_admin_forward(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> StatusCode {
    if require_admin(&state, &headers).is_err() {
        return StatusCode::FORBIDDEN;
    }
    match state
        .port_forward_store
        .set_state(&id, "released", None, None)
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::NOT_FOUND,
    }
}

async fn get_user_forward_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<port_forward::PolicyRecord>, StatusCode> {
    require_admin(&state, &headers)?;
    let users = state.users.read().expect("user registry poisoned");
    let user = users.get(&username).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(
        state
            .port_forward_store
            .policy(&user.user_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

async fn set_user_forward_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(input): Json<UserPortForwardPolicyRequest>,
) -> Result<Json<port_forward::PolicyRecord>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    let users = state.users.read().expect("user registry poisoned");
    let user = users.get(&username).ok_or((
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: "用户不存在".into(),
        }),
    ))?;
    let policy = port_forward::PolicyRecord {
        enabled: input.enabled,
        max_active_forwards: input.max_active_forwards,
        allowed_entry_ids: input.allowed_entry_ids,
        allowed_protocols: input.allowed_protocols,
        allowed_target_ports: input.allowed_target_ports,
        allowed_families: input.allowed_families,
    };
    let entries = state.port_forward_store.entries().map_err(server_error)?;
    if policy
        .allowed_entry_ids
        .iter()
        .any(|entry_id| !entries.iter().any(|entry| entry.entry_id == *entry_id))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "授权入口包含不存在的入口".into(),
            }),
        ));
    }
    state
        .port_forward_store
        .save_policy(&user.user_id, &policy)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    Ok(Json(policy))
}

async fn list_nodes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<NodeSummary>>, StatusCode> {
    require_admin(&state, &headers)?;
    let mut nodes: Vec<_> = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .values()
        .map(node_summary)
        .collect();
    nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    Ok(Json(nodes))
}

async fn update_node_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    Json(mut input): Json<NodeSettingsRequest>,
) -> Result<Json<NodeSummary>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    if let Some(limit) = input.rate_limit {
        limit
            .validate()
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    }
    let route_table = input
        .route_table
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(validate_route_table)
        .transpose()
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if input
        .name
        .is_some_and(|value| value.trim().is_empty() || value.len() > 128)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "节点名称不能为空且最多 128 个字符".into(),
            }),
        ));
    }
    if input
        .region
        .as_deref()
        .is_some_and(|value| value.len() > 128)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "节点区域最多 128 个字符".into(),
            }),
        ));
    }
    if input
        .wireguard_endpoint
        .as_deref()
        .is_some_and(|value| !valid_wireguard_endpoint(value.trim()))
        || input
            .probe_endpoint
            .as_deref()
            .is_some_and(|value| !valid_wireguard_endpoint(value.trim()))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "WireGuard 和 UDP 探测地址必须是 host:port 或 [IPv6]:port".into(),
            }),
        ));
    }
    if let Some(network) = &mut input.network {
        network.apply_managed_dns_defaults();
        validate_node_network(network)
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    }
    // Connection install checks and forwarding allocation take devices before
    // nodes. Never wait for devices while retaining a node write lock.
    let mut devices = state.devices.write().expect("device registry poisoned");
    let mut nodes = state.nodes.write().expect("node registry poisoned");
    let mut next = nodes.clone();
    let Some(node) = next.get_mut(&node_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "节点不存在".into(),
            }),
        ));
    };
    let effective_wireguard_endpoint = input
        .wireguard_endpoint
        .as_deref()
        .map(str::trim)
        .unwrap_or(node.wireguard_endpoint.as_str());
    let effective_probe_endpoint = input
        .probe_endpoint
        .as_deref()
        .map(str::trim)
        .unwrap_or(node.probe_endpoint.as_str());
    if !endpoints_use_distinct_ports(effective_wireguard_endpoint, effective_probe_endpoint) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "WireGuard 公网端口和 UDP 探测端口必须不同；两个服务不能监听同一个 UDP 端口"
                    .into(),
            }),
        ));
    }
    if let Some(name) = name {
        node.name = name;
    }
    if let Some(region) = input.region {
        node.region_override = Some(region.trim().to_owned());
        node.region = node.region_override.clone();
    }
    if let Some(endpoint) = input.wireguard_endpoint {
        let endpoint = endpoint.trim().to_owned();
        node.wireguard_endpoint_override = Some(endpoint.clone());
        node.wireguard_endpoint = endpoint;
    }
    if let Some(endpoint) = input.probe_endpoint {
        let endpoint = endpoint.trim().to_owned();
        node.probe_endpoint_override = Some(endpoint.clone());
        node.probe_endpoint = endpoint;
    }
    if let Some(network) = input.network {
        node.network = network.clone();
        node.network_override = Some(network);
    }
    node.route_table = route_table;
    if let Some(limit) = input.rate_limit {
        node.rate_limit = limit;
    }
    let was_enabled = node.enabled;
    let was_connectable = node.connectable;
    node.enabled = input.enabled;
    if let Some(connectable) = input.connectable {
        node.connectable = connectable;
    }
    let node_id = node.node_id.clone();
    // A disabled node must never be durably visible while its devices remain
    // active. Persist the revocations first; if the node write then fails the
    // safer state (devices revoked, node still enabled) is retained instead
    // of leaving active leases attached to a disabled node.
    let pending_revocations =
        if was_enabled && (!input.enabled || (was_connectable && !node.connectable)) {
            let mut next_devices = devices.clone();
            let mut revoked = Vec::new();
            for device in next_devices
                .values_mut()
                .filter(|device| device.node_id == node_id && device.status == "active")
            {
                device.status = "revoked".into();
                device.last_seen_at = Utc::now();
                revoked.push(device.device_id);
            }
            persist_devices(&state, &next_devices).map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "设备撤销状态保存失败，节点未停用".into(),
                    }),
                )
            })?;
            *devices = next_devices.clone();
            Some((next_devices, revoked))
        } else {
            None
        };
    persist_nodes(&state, &next).map_err(server_error)?;
    *nodes = next;
    drop(nodes);
    drop(devices);
    if let Some((_, revoked)) = pending_revocations {
        if !input.enabled {
            let _ = state.port_forward_store.disable_node(&node_id);
        }
        for device_id in revoked {
            let _ = state.port_forward_store.disconnect_device(device_id);
        }
    }
    let nodes = state.nodes.read().expect("node registry poisoned");
    let node = nodes.get(&node_id).expect("node disappeared");
    Ok(Json(node_summary(node)))
}

async fn update_node_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(node_id): Path<String>,
    Json(input): Json<NodeReleaseRequest>,
) -> Result<Json<NodeSummary>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    let release = current_node_release(&state).map_err(server_error)?.ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorResponse {
            error: "主控尚未发布节点资源，请先发布 pmman-node 资源".into(),
        }),
    ))?;
    let version = input.version.unwrap_or_else(|| release.version.clone());
    if version != release.version {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "请求的节点资源版本不存在".into(),
            }),
        ));
    }
    let mut nodes = state.nodes.write().expect("node registry poisoned");
    let mut next = nodes.clone();
    let Some(node) = next.get_mut(&node_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "节点不存在".into(),
            }),
        ));
    };
    node.desired_resource_version = Some(version);
    persist_nodes(&state, &next).map_err(server_error)?;
    *nodes = next;
    let node = nodes.get(&node_id).expect("node disappeared");
    Ok(Json(node_summary(node)))
}

async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Device>>, StatusCode> {
    require_admin(&state, &headers)?;
    expire_client_leases(&state, Utc::now()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut devices: Vec<_> = state
        .devices
        .read()
        .expect("device registry poisoned")
        .values()
        .cloned()
        .collect();
    devices.sort_by_key(|device| (device.status != "active", device.ipv4.clone()));
    Ok(Json(devices))
}

fn register_device(
    state: &AppState,
    principal: Principal,
    input: RegisterRequest,
    node_id: &str,
    gateway: &GatewayConfig,
    apply_gateway_peer: bool,
) -> Result<Json<DeviceConfig>, (StatusCode, Json<ErrorResponse>)> {
    let mode_allowed = |mode: &str, allow_global: bool, allow_split: bool| match mode {
        "internal" => true,
        "global" => allow_global,
        "split" => allow_split,
        _ => false,
    };
    if !mode_allowed(
        &input.ipv4_mode,
        principal.allow_ipv4_global,
        principal.allow_ipv4_split,
    ) || !mode_allowed(
        &input.ipv6_mode,
        principal.allow_ipv6_global,
        principal.allow_ipv6_split,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "所选流量模式未获主控授权".into(),
            }),
        ));
    }
    if input.public_key.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "客户端公钥不能为空".into(),
            }),
        ));
    }
    let mut devices = state.devices.write().expect("device registry poisoned");
    let mut users = state.users.write().expect("user registry poisoned");
    let pool = state.pool.read().expect("address pool poisoned");
    let account = users
        .get(&principal.username)
        .filter(|user| user.user_id == principal.user_id && user.enabled)
        .cloned()
        .ok_or_else(|| server_error("账户不存在或已停用".into()))?;
    let mut address_registry = state.address_pools.write().expect("address pools poisoned");
    let mut allocation_registry = address_registry.clone();
    if let Some(registry) = allocation_registry.as_mut() {
        registry.refresh(&users, &devices);
    }
    if !mode_allowed(
        &input.ipv4_mode,
        account.allow_ipv4_global,
        account.allow_ipv4_split,
    ) || !mode_allowed(
        &input.ipv6_mode,
        account.allow_ipv6_global,
        account.allow_ipv6_split,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse {
                error: "所选流量模式未获主控授权".into(),
            }),
        ));
    }
    let fixed_pair = user_fixed_pair(&account).map_err(server_error)?;
    let (used4, used6) = address_occupancy(&users, &devices, Some(&principal.user_id));
    let existing = devices
        .values()
        .find(|d| d.user_id == principal.user_id)
        .cloned();
    let old_pair = existing
        .as_ref()
        .map(|device| (device.ipv4.clone(), device.ipv6.clone()))
        .or_else(|| user_lease(&account));
    let pair = if let Some(registry) = allocation_registry.as_ref() {
        registry
            .allocate(state, &account, &users, &devices, old_pair.as_ref())
            .map_err(|error| (StatusCode::CONFLICT, Json(ErrorResponse { error })))?
    } else if let Some(pair) = fixed_pair {
        if address_conflict(&pair, &used4, &used6) {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "固定地址已被其他用户或设备占用".into(),
                }),
            ));
        }
        pair
    } else if let Some(pair) = old_pair
        .filter(|pair| pool.contains(&pair.0, &pair.1) && !address_conflict(pair, &used4, &used6))
    {
        pair
    } else {
        pool.allocate_dual(&used4, &used6).ok_or_else(|| {
            server_error(format!("地址池已耗尽（{}，可用地址：0）", pool.ipv4_cidr()))
        })?
    };
    if let Some(existing) = &existing {
        if address_registry.is_some() && (existing.ipv4 != pair.0 || existing.ipv6 != pair.1) {
            let forwards: Vec<_> = state
                .port_forward_store
                .allocations(Some(&existing.user_id))
                .map_err(server_error)?
                .into_iter()
                .filter(|a| {
                    a.device_id == existing.device_id
                        && ((existing.ipv4 != pair.0 && a.target_address == existing.ipv4)
                            || (existing.ipv6 != pair.1 && a.target_address == existing.ipv6))
                })
                .collect();
            if existing.status != "active" {
                for a in forwards
                    .iter()
                    .filter(|a| !matches!(a.state.as_str(), "released" | "expired" | "releasing"))
                {
                    state
                        .port_forward_store
                        .set_state(&a.forward_id, "releasing", Some("客户端地址即将变更"), None)
                        .map_err(server_error)?;
                }
            }
            let forwarding_clean = existing.status != "active"
                && address_pools::prepare_forward_cleanup(
                    state,
                    &mut address_registry,
                    &users,
                    &devices,
                    existing,
                    existing.ipv4 != pair.0,
                    existing.ipv6 != pair.1,
                )
                .map_err(server_error)?;
            if existing.status == "active"
                || !user_deletion::prior_device_cleanup_confirmed(state, existing)
                || !forwarding_clean
                || forwards
                    .iter()
                    .any(|a| !matches!(a.state.as_str(), "released" | "expired"))
            {
                return Err((
                    StatusCode::CONFLICT,
                    Json(ErrorResponse {
                        error: "地址策略已变更，旧连接及端口映射尚未清理，请断开后稍候重试".into(),
                    }),
                ));
            }
        }
        if existing.status == "active"
            && (existing.ipv4 != pair.0 || existing.ipv6 != pair.1 || existing.node_id != node_id)
        {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "该账户已有活动连接，请先断开再变更地址或节点".into(),
                }),
            ));
        }
        if existing.status == "revoked"
            && existing.node_id != node_id
            && !user_deletion::prior_device_cleanup_confirmed(state, existing)
        {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "旧节点尚未确认连接清理，请稍后重试".into(),
                }),
            ));
        }
    }
    let previous_public_key = existing
        .as_ref()
        .filter(|device| device.public_key != input.public_key)
        .map(|device| device.public_key.clone());
    let device = Device {
        device_id: existing
            .as_ref()
            .map(|device| device.device_id)
            .unwrap_or_else(Uuid::new_v4),
        user_id: principal.user_id.clone(),
        username: principal.username.clone(),
        public_key: input.public_key,
        node_id: node_id.into(),
        ipv4: pair.0.clone(),
        ipv6: pair.1.clone(),
        ipv4_mode: input.ipv4_mode,
        ipv6_mode: input.ipv6_mode,
        status: "active".into(),
        config_version: existing
            .as_ref()
            .map(|device| device.config_version + 1)
            .unwrap_or(1),
        last_seen_at: Utc::now(),
        traffic: existing
            .as_ref()
            .map(|device| device.traffic)
            .unwrap_or_default(),
        session_traffic: pmman_protocol::TrafficStats::default(),
    };
    // Persist only address ownership, never active authorizations or keys.
    // A failed save cannot install a peer or publish a new in-memory lease.
    if address_registry.is_none()
        && (account.leased_ipv4.as_deref() != Some(pair.0.as_str())
            || account.leased_ipv6.as_deref() != Some(pair.1.as_str()))
    {
        let mut next = users.clone();
        let user = next.get_mut(&principal.username).expect("user disappeared");
        user.leased_ipv4 = Some(pair.0.clone());
        user.leased_ipv6 = Some(pair.1.clone());
        persist_users(state, &next).map_err(server_error)?;
        *users = next;
    }
    if apply_gateway_peer {
        if let (Some(interface), Some(key)) = (&state.gateway.interface, &previous_public_key) {
            remove_gateway_peer(&state.gateway, interface, key).map_err(server_error)?;
        }
        add_gateway_peer(&state.gateway, &device).map_err(server_error)?;
    }
    let routes = address_pools::internal_routes_with_registry(state, address_registry.as_ref());
    let response = config_with_routes(&device, gateway, routes, previous_public_key);
    let restore_forwards = existing.as_ref().is_some_and(|old| {
        old.device_id == device.device_id
            && ((old.ipv4 == device.ipv4 && old.ipv6 == device.ipv6)
                || (address_registry.is_some()
                    && (old.ipv4 == device.ipv4 || old.ipv6 == device.ipv6)))
    });
    let device_id = device.device_id;
    let mut next_devices = devices.clone();
    next_devices.insert(device_id, device);
    if let Some(current) = address_registry.as_ref() {
        let mut next_users = users.clone();
        let user = next_users
            .get_mut(&principal.username)
            .expect("user disappeared");
        user.leased_ipv4 = Some(pair.0.clone());
        user.leased_ipv6 = Some(pair.1.clone());
        let mut next = current.clone();
        address_pools::save(
            state,
            current.revision,
            &mut next,
            &next_users,
            &next_devices,
            true,
        )
        .map_err(server_error)?;
        *users = next_users;
        *address_registry = Some(next);
    } else {
        persist_devices(state, &next_devices).map_err(server_error)?;
    }
    *devices = next_devices;
    if restore_forwards {
        let _ = state
            .port_forward_store
            .restore_device(device_id, &pair.0, &pair.1);
    }
    Ok(Json(response))
}

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<RegisterRequest>,
) -> Result<Json<DeviceConfig>, (StatusCode, Json<ErrorResponse>)> {
    let principal = authenticate(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "请先登录并提供有效会话".into(),
            }),
        )
    })?;
    register_device(&state, principal, input, "legacy", &state.gateway, true)
}

#[cfg(test)]
fn node_device_config(node: &Node, device: DeviceConfig) -> Result<DeviceConfig, String> {
    node_device_config_with_capability(
        node,
        device,
        &DEFAULT_INTERNAL_CIDRS
            .iter()
            .map(|route| (*route).to_string())
            .collect::<Vec<_>>(),
        None,
        None,
    )
}

fn node_device_config_with_routes(
    node: &Node,
    device: DeviceConfig,
    internal_cidrs: &[String],
) -> Result<DeviceConfig, String> {
    node_device_config_with_capability(node, device, internal_cidrs, None, None)
}

fn client_geosite(
    state: &AppState,
    node: &Node,
) -> Result<Option<pmman_protocol::GeositeConfig>, String> {
    let network = node_network(node);
    let required = !network.dns_servers.is_empty() || network.pmmandns_geosite_enabled;
    if !required {
        return Ok(None);
    }
    match current_geosite(state)? {
        Some((manifest, _)) => Ok(Some(geosite_config(&manifest))),
        None => Err("该节点需要客户端 DNS 分流，但主控没有可用的 geosite 资源".into()),
    }
}

fn node_device_config_with_capability(
    node: &Node,
    mut device: DeviceConfig,
    internal_cidrs: &[String],
    split_route_source: Option<&str>,
    geosite: Option<pmman_protocol::GeositeConfig>,
) -> Result<DeviceConfig, String> {
    let network = node_network(node);
    network.validate_managed_dns()?;
    device.rate_limit = node.rate_limit;
    device.routes = internal_cidrs.to_vec();
    device.gateway_ipv4 = network.gateway_ipv4.clone();
    device.gateway_ipv6 = network.gateway_ipv6.clone();
    if let Some(source) = split_route_source.filter(|source| *source != "client_builtin_v1") {
        return Err(format!("客户端声明了不支持的分流规则来源：{source}"));
    }
    device.split_route_source = split_route_source
        .filter(|source| *source == "client_builtin_v1")
        .map(str::to_owned);
    for (mode, ipv6) in [(&device.ipv4_mode, false), (&device.ipv6_mode, true)] {
        let routes = match (mode.as_str(), ipv6) {
            ("internal", _) => Vec::new(),
            ("global", false) => vec!["0.0.0.0/1".into(), "128.0.0.0/1".into()],
            ("global", true) => vec!["::/1".into(), "8000::/1".into()],
            ("split", false) if device.split_route_source.is_some() => Vec::new(),
            ("split", true) if device.split_route_source.is_some() => Vec::new(),
            ("split", false) => network
                .ipv4_split_routes
                .clone()
                .ok_or("节点未配置 IPv4 分流表，请升级客户端或配置旧版分流表")?,
            ("split", true) => network
                .ipv6_split_routes
                .clone()
                .ok_or("节点未配置 IPv6 分流表，请升级客户端或配置旧版分流表")?,
            _ => return Err("流量模式无效".into()),
        };
        device.routes.extend(routes);
    }
    // An internal-network connection always prefers the node DNS. The
    // Windows service appends the pre-existing local DNS servers as fallback.
    device.dns_servers = network.dns_servers;
    let mut foreign_rules = network.pmmandns_foreign_rules.clone();
    foreign_rules.extend(
        network
            .pmmandns_rules
            .iter()
            .map(|rule| rule.suffix.clone()),
    );
    if !device.dns_servers.is_empty()
        && (geosite.is_some()
            || !network.pmmandns_domestic_rules.is_empty()
            || !foreign_rules.is_empty())
    {
        device.dns_policy = Some(pmman_protocol::ClientDnsPolicy {
            geosite,
            domestic_rules: network.pmmandns_domestic_rules,
            foreign_rules,
        });
    }
    device.routes.sort();
    device.routes.dedup();
    Ok(device)
}

fn installation_status(
    state: &AppState,
    node_id: &str,
    device: &DeviceConfig,
    public_key: &str,
) -> Result<bool, String> {
    let devices = state.devices.read().expect("device registry poisoned");
    let Some(current) = devices.get(&device.device_id) else {
        return Err("租约已不存在".into());
    };
    if current.status != "active"
        || current.node_id != node_id
        || current.public_key != public_key
        || current.config_version != device.config_version
    {
        return Err("连接请求已被撤销或替代".into());
    }
    let nodes = state.nodes.read().expect("node registry poisoned");
    let node = nodes.get(node_id).ok_or("节点不存在")?;
    if !node_is_healthy(node) {
        return Err("节点已离线或停用".into());
    }
    if let Some(error) = node.sync_errors.iter().find(|error| {
        error.device_id == device.device_id
            && error.public_key == public_key
            && error.config_version == device.config_version
    }) {
        return Err(format!(
            "节点安装 peer/返回路由失败：{}",
            error.error.chars().take(200).collect::<String>()
        ));
    }
    Ok(node.applied_route_table == node.route_table
        && node.applied.iter().any(|peer| {
            peer.device_id == device.device_id
                && peer.public_key == public_key
                && peer.config_version == device.config_version
        }))
}

async fn client_connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<ClientConnectionRequest>,
) -> Result<Json<DeviceConfig>, (StatusCode, Json<ErrorResponse>)> {
    let principal = authenticate(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "客户端会话无效".into(),
            }),
        )
    })?;
    if !valid_wireguard_key(&input.registration.public_key) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "客户端 WireGuard 公钥无效".into(),
            }),
        ));
    }
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&input.node_id)
        .filter(|node| node_is_connectable(node))
        .cloned()
        .ok_or((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "所选节点已离线或停用，请刷新节点列表".into(),
            }),
        ))?;
    let geosite = client_geosite(&state, &node).map_err(|error| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse { error }),
        )
    })?;
    // Validate requested route policy before allocating a lease.
    let public_key = input.registration.public_key.clone();
    let split_route_source = input.registration.split_route_source.clone();
    let check = DeviceConfig {
        rate_limit: NodeRateLimit::default(),
        device_id: Uuid::nil(),
        ipv4: String::new(),
        ipv6: String::new(),
        routes: Vec::new(),
        ipv4_mode: input.registration.ipv4_mode.clone(),
        ipv6_mode: input.registration.ipv6_mode.clone(),
        dns_servers: Vec::new(),
        dns_policy: None,
        server_public_key: String::new(),
        endpoint: String::new(),
        config_version: 0,
        previous_public_key: None,
        gateway_ipv4: None,
        gateway_ipv6: None,
        split_route_source: None,
    };
    let internal_cidrs = address_pools::internal_routes(&state);
    node_device_config_with_capability(
        &node,
        check,
        &internal_cidrs,
        split_route_source.as_deref(),
        geosite.clone(),
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    let assigned = register_device(
        &state,
        principal,
        input.registration,
        &node.node_id,
        &gateway_for_node(&node),
        false,
    )?
    .0;
    let internal_cidrs = assigned.routes.clone();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    let failure = loop {
        match installation_status(&state, &node.node_id, &assigned, &public_key) {
            Ok(true) => {
                return node_device_config_with_capability(
                    &node,
                    assigned,
                    &internal_cidrs,
                    split_route_source.as_deref(),
                    geosite,
                )
                .map(Json)
                .map_err(server_error)
            }
            Err(error) => break error,
            Ok(false) if tokio::time::Instant::now() >= deadline => {
                break "等待节点安装确认超时，已撤销本次连接".into()
            }
            Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    };
    if let Err(error) = revoke_failed_connection(&state, &assigned, &public_key) {
        return Err(server_error(format!(
            "{failure}；连接失败状态保存异常：{error}"
        )));
    }
    Err(server_error(failure))
}

fn revoke_failed_connection(
    state: &AppState,
    assigned: &DeviceConfig,
    public_key: &str,
) -> Result<(), String> {
    let mut devices = state.devices.write().expect("device registry poisoned");
    if !devices.get(&assigned.device_id).is_some_and(|device| {
        device.status == "active"
            && device.public_key == public_key
            && device.config_version == assigned.config_version
    }) {
        return Ok(());
    }
    let mut next = devices.clone();
    let device = next
        .get_mut(&assigned.device_id)
        .expect("device disappeared");
    device.status = "revoked".into();
    device.last_seen_at = Utc::now();
    persist_devices(state, &next)?;
    *devices = next;
    Ok(())
}

async fn get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<DeviceConfig>, (StatusCode, Json<ErrorResponse>)> {
    let principal = authenticate(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "请先登录并提供有效会话".into(),
            }),
        )
    })?;
    expire_client_leases(&state, Utc::now()).map_err(server_error)?;
    let devices = state.devices.read().expect("device registry poisoned");
    let device = devices
        .get(&id)
        .filter(|device| {
            device.status == "active"
                && (principal.role == "admin" || device.user_id == principal.user_id)
        })
        .cloned()
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "设备不存在、已撤销或无权访问".into(),
            }),
        ))?;
    drop(devices);
    if device.node_id == "legacy" {
        let routes = address_pools::internal_routes(&state);
        return Ok(Json(config_with_routes(
            &device,
            &state.gateway,
            routes,
            None,
        )));
    }
    let node = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&device.node_id)
        .cloned()
        .ok_or_else(|| server_error("节点不存在".into()))?;
    let internal_cidrs = address_pools::internal_routes(&state);
    let assigned = config_with_routes(
        &device,
        &gateway_for_node(&node),
        internal_cidrs.clone(),
        None,
    );
    if !installation_status(&state, &device.node_id, &assigned, &device.public_key)
        .map_err(server_error)?
    {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "节点尚未确认安装".into(),
            }),
        ));
    }
    node_device_config_with_capability(
        &node,
        assigned,
        &internal_cidrs,
        None,
        client_geosite(&state, &node).map_err(server_error)?,
    )
    .map(Json)
    .map_err(server_error)
}

#[derive(Default, Deserialize)]
struct ClientHeartbeatRequest {
    #[serde(default)]
    traffic: Option<pmman_protocol::TrafficStats>,
    public_key: Option<String>,
    config_version: Option<u64>,
}

async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(principal) = authenticate(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if body.len() > 4096 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let input = if body.is_empty() {
        ClientHeartbeatRequest::default()
    } else {
        match serde_json::from_slice::<ClientHeartbeatRequest>(&body) {
            Ok(input) => input,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    // Do not let a late renewal resurrect a lease already past its deadline.
    if expire_client_leases(&state, Utc::now()).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let node_id = {
        let mut devices = state.devices.write().expect("device registry poisoned");
        let Some(existing) = devices
            .get_mut(&id)
            .filter(|device| device.status == "active" && device.user_id == principal.user_id)
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if input.traffic.is_some()
            && (input.public_key.as_deref() != Some(existing.public_key.as_str())
                || input.config_version != Some(existing.config_version))
        {
            return StatusCode::CONFLICT.into_response();
        }
        let node_id = existing.node_id.clone();
        let mut next = devices.clone();
        if let Some(device) = next.get_mut(&id) {
            device.last_seen_at = Utc::now();
            if let Some(traffic) = input.traffic {
                device
                    .traffic
                    .accumulate(traffic.delta_since(device.session_traffic));
                device.session_traffic.retain_max(traffic);
            }
        }
        if persist_devices(&state, &next).is_err() {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        *devices = next;
        node_id
    };
    let limit = state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&node_id)
        .map(|node| node.rate_limit)
        .unwrap_or_default();
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("x-pmman-upload-mbps", limit.upload_mbps.to_string())
        .header("x-pmman-download-mbps", limit.download_mbps.to_string())
        .body(Body::empty())
        .expect("valid heartbeat response")
}

fn expire_client_leases(state: &AppState, now: DateTime<Utc>) -> Result<(), String> {
    let deadline = now - Duration::seconds(CLIENT_AUTH_LEASE_SECONDS);
    let mut devices = state.devices.write().expect("device registry poisoned");
    let mut expired = Vec::new();
    let mut next = devices.clone();
    for device in next.values_mut().filter(|device| {
        device.node_id != "legacy" && device.status == "active" && device.last_seen_at <= deadline
    }) {
        // Node snapshots remove the peer/routes; directories omit revoked keys.
        // Keep last_seen_at as the last genuine client activity for diagnostics.
        device.status = "revoked".into();
        expired.push(device.device_id);
    }
    if expired.is_empty() {
        return Ok(());
    }
    persist_devices(state, &next)?;
    *devices = next;
    drop(devices);
    for device_id in expired {
        if let Err(error) = state.port_forward_store.disconnect_device(device_id) {
            eprintln!("清理过期设备端口转发失败 device_id={device_id}: {error}");
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct RevokeRequest {
    public_key: String,
}

async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<RevokeRequest>,
) -> StatusCode {
    let Ok(principal) = authenticate(&state, &headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    let (device, node_id) = {
        let mut devices = state.devices.write().expect("device registry poisoned");
        let Some(existing) = devices.get(&id).cloned() else {
            return StatusCode::NOT_FOUND;
        };
        if existing.user_id != principal.user_id || existing.public_key != input.public_key {
            return StatusCode::FORBIDDEN;
        }
        let mut next = devices.clone();
        let device = next.get_mut(&id).expect("device disappeared");
        device.status = "revoked".into();
        device.last_seen_at = Utc::now();
        let node_id = device.node_id.clone();
        let snapshot = next.clone();
        if persist_devices(&state, &snapshot).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        *devices = snapshot.clone();
        (existing, node_id)
    };
    let gateway_cleanup_failed = state.gateway.interface.as_ref().is_some_and(|interface| {
        remove_gateway_peer(&state.gateway, interface, &device.public_key).is_err()
    });
    if let Err(error) = state.port_forward_store.disconnect_device(device.device_id) {
        eprintln!(
            "清理已撤销设备端口转发失败 device_id={}: {error}",
            device.device_id
        );
    }
    if node_id == "legacy" {
        return if gateway_cleanup_failed {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::NO_CONTENT
        };
    }
    let requested_at = Utc::now();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let cleaned = state
            .nodes
            .read()
            .expect("node registry poisoned")
            .get(&node_id)
            .is_some_and(|node| {
                node.last_seen_at >= requested_at
                    && node.cleanup_ok
                    && !node
                        .applied
                        .iter()
                        .any(|peer| peer.device_id == id && peer.public_key == input.public_key)
            });
        if cleaned {
            return if gateway_cleanup_failed {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::NO_CONTENT
            };
        }
        if tokio::time::Instant::now() >= deadline {
            return if gateway_cleanup_failed {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::ACCEPTED
            };
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn admin_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> StatusCode {
    if require_admin(&state, &headers).is_err() {
        return StatusCode::FORBIDDEN;
    }
    // Publish the revocation to durable storage before touching the gateway.
    // The old order returned early when `wg set ... remove` failed and left an
    // active device in PostgreSQL, so a master restart could resurrect it.
    let device = {
        let mut devices = state.devices.write().expect("device registry poisoned");
        let Some(existing) = devices.get(&id).cloned() else {
            return StatusCode::NOT_FOUND;
        };
        // Released account-deletion history is terminal. Repeated historical
        // actions must not reserve a recycled address or resend old revocations.
        if existing.status == "released" {
            return StatusCode::NO_CONTENT;
        }
        let mut next = devices.clone();
        if let Some(device) = next.get_mut(&id) {
            device.status = "revoked".into();
            device.last_seen_at = Utc::now();
        }
        if persist_devices(&state, &next).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        *devices = next;
        existing
    };
    let mut result = StatusCode::NO_CONTENT;
    if let Some(interface) = &state.gateway.interface {
        if remove_gateway_peer(&state.gateway, interface, &device.public_key).is_err() {
            result = StatusCode::BAD_GATEWAY;
        }
    }
    let _ = state.port_forward_store.disconnect_device(device.device_id);
    result
}

async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<UserSummary>>, StatusCode> {
    require_admin(&state, &headers)?;
    let accounts = state.users.read().expect("user registry poisoned");
    let registry = state.address_pools.read().expect("address pools poisoned");
    let mut users: Vec<_> = accounts
        .values()
        .map(|user| {
            let mut summary = user_summary(user);
            summary.address_policy = registry.as_ref().map(|r| r.policy(user));
            summary
        })
        .collect();
    users.sort_by(|left, right| left.username.cmp(&right.username));
    Ok(Json(users))
}

async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CreateUserRequest>,
) -> Result<Json<UserSummary>, StatusCode> {
    require_admin(&state, &headers)?;
    if !valid_username(&input.username) || !(6..=256).contains(&input.password.len()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let role = input.role.unwrap_or_else(|| "user".into());
    if role != "user" && role != "admin" {
        return Err(StatusCode::BAD_REQUEST);
    }
    let now = Utc::now();
    let account = UserAccount {
        user_id: Uuid::new_v4().to_string(),
        username: input.username.clone(),
        password_hash: hash_password(&input.password),
        role,
        enabled: true,
        allow_ipv4_global: input.allow_ipv4_global,
        allow_ipv6_global: input.allow_ipv6_global,
        allow_ipv4_split: input.allow_ipv4_split,
        allow_ipv6_split: input.allow_ipv6_split,
        created_at: now,
        must_change_password: true,
        address_mode: default_address_mode(),
        reserved_ipv4: None,
        reserved_ipv6: None,
        leased_ipv4: None,
        leased_ipv6: None,
    };
    let summary = user_summary(&account);
    let mut users = state.users.write().expect("user registry poisoned");
    if users.contains_key(&account.username) {
        return Err(StatusCode::CONFLICT);
    }
    let mut next = users.clone();
    next.insert(account.username.clone(), account);
    if persist_users(&state, &next).is_err() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    *users = next;
    Ok(Json(summary))
}

async fn set_user_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(input): Json<UserStatusRequest>,
) -> StatusCode {
    if require_admin(&state, &headers).is_err() {
        return StatusCode::FORBIDDEN;
    }
    let mut users = state.users.write().expect("user registry poisoned");
    let Some(existing) = users.get(&username) else {
        return StatusCode::NOT_FOUND;
    };
    let user_id = existing.user_id.clone();
    let mut next = users.clone();
    if let Some(user) = next.get_mut(&username) {
        user.enabled = input.enabled;
    } else {
        return StatusCode::NOT_FOUND;
    }
    if persist_users(&state, &next).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *users = next;
    drop(users);
    if !input.enabled {
        invalidate_user_sessions(&state, &user_id);
        revoke_devices_for_user(&state, &user_id)
    } else {
        StatusCode::NO_CONTENT
    }
}

async fn reset_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(input): Json<PasswordRequest>,
) -> StatusCode {
    if require_admin(&state, &headers).is_err() {
        return StatusCode::FORBIDDEN;
    }
    if !(6..=256).contains(&input.password.len()) {
        return StatusCode::BAD_REQUEST;
    }
    let mut users = state.users.write().expect("user registry poisoned");
    let Some(user_id) = users.get(&username).map(|user| user.user_id.clone()) else {
        return StatusCode::NOT_FOUND;
    };
    let mut next = users.clone();
    if let Some(user) = next.get_mut(&username) {
        user.password_hash = hash_password(&input.password);
        user.must_change_password = true;
    }
    if persist_users(&state, &next).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *users = next;
    drop(users);
    // A password reset is a credential change: every previously issued
    // bearer token must stop working immediately, including the administrator
    // token that performed the reset.
    invalidate_user_sessions(&state, &user_id);
    StatusCode::NO_CONTENT
}

async fn set_user_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(input): Json<UserCapabilitiesRequest>,
) -> StatusCode {
    if require_admin(&state, &headers).is_err() {
        return StatusCode::FORBIDDEN;
    }
    let mut devices = state.devices.write().expect("device registry poisoned");
    let mut users = state.users.write().expect("user registry poisoned");
    let Some(existing) = users.get(&username) else {
        return StatusCode::NOT_FOUND;
    };
    let user_id = existing.user_id.clone();
    let mut next = users.clone();
    if let Some(user) = next.get_mut(&username) {
        user.allow_ipv4_global = input.allow_ipv4_global;
        user.allow_ipv6_global = input.allow_ipv6_global;
        user.allow_ipv4_split = input.allow_ipv4_split;
        user.allow_ipv6_split = input.allow_ipv6_split;
    } else {
        return StatusCode::NOT_FOUND;
    }
    // Capability changes revoke incompatible live devices. Build and persist
    // the complete device generation before publishing the reduced user
    // capabilities. If either later user write fails, the safer state is kept:
    // the old capabilities remain, but incompatible devices stay revoked.
    let (next_devices, revoked) = {
        let mut next = devices.clone();
        let mut revoked = Vec::new();
        for device in next.values_mut().filter(|device| {
            device.user_id == user_id
                && device.status == "active"
                && ((device.ipv4_mode == "global" && !input.allow_ipv4_global)
                    || (device.ipv4_mode == "split" && !input.allow_ipv4_split)
                    || (device.ipv6_mode == "global" && !input.allow_ipv6_global)
                    || (device.ipv6_mode == "split" && !input.allow_ipv6_split))
        }) {
            device.status = "revoked".into();
            device.last_seen_at = Utc::now();
            revoked.push((device.device_id, device.public_key.clone()));
        }
        (next, revoked)
    };
    if persist_devices(&state, &next_devices).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *devices = next_devices;
    if persist_users(&state, &next).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *users = next;
    drop(users);
    drop(devices);
    let mut result = StatusCode::NO_CONTENT;
    for (device_id, public_key) in revoked {
        if let Some(interface) = &state.gateway.interface {
            if remove_gateway_peer(&state.gateway, interface, &public_key).is_err() {
                result = StatusCode::BAD_GATEWAY;
            }
        }
        let _ = state.port_forward_store.disconnect_device(device_id);
    }
    result
}

async fn set_user_address_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(input): Json<UserAddressLeaseRequest>,
) -> Result<Json<UserSummary>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    if state
        .address_pools
        .read()
        .expect("address pools poisoned")
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "请使用双栈独立地址分配接口，旧接口不能修改多地址池策略".into(),
            }),
        ));
    }

    let address_mode = input.address_mode.trim().to_ascii_lowercase();
    let reserved_ipv4 = input
        .reserved_ipv4
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let reserved_ipv6 = input
        .reserved_ipv6
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if address_mode == "automatic" && (reserved_ipv4.is_some() || reserved_ipv6.is_some()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "自动分配模式不能同时填写固定地址".into(),
            }),
        ));
    }
    if address_mode != "automatic" && address_mode != "fixed" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "地址分配方式只能是 automatic 或 fixed".into(),
            }),
        ));
    }

    // A single lock order protects allocation, policy editing, and pool saves:
    // devices -> users -> pool. Hold the pool through persistence so a manual
    // reservation cannot be validated against a stale pool generation.
    let mut devices = state.devices.write().expect("device registry poisoned");
    let mut users = state.users.write().expect("user registry poisoned");
    let pool = state.pool.read().expect("address pool poisoned");
    let Some(existing) = users.get(&username).cloned() else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "用户不存在".into(),
            }),
        ));
    };
    let (used4, used6) = address_occupancy(&users, &devices, Some(&existing.user_id));
    let fixed_pair = if address_mode == "fixed" {
        let pair = if reserved_ipv4.is_some() || reserved_ipv6.is_some() {
            if reserved_ipv4.is_some() && reserved_ipv6.is_some() {
                validate_manual_pair(
                    reserved_ipv4.as_deref().unwrap(),
                    reserved_ipv6.as_deref().unwrap(),
                )
                .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?
            } else {
                // A single address can be selected from the pool and the
                // paired address is derived by offset. An arbitrary outside-
                // pool address must therefore be supplied as a complete pair.
                pool.complete_pair(reserved_ipv4.as_deref(), reserved_ipv6.as_deref())
                    .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?
            }
        } else {
            devices
                .values()
                .find(|device| device.user_id == existing.user_id)
                .map(|device| (device.ipv4.clone(), device.ipv6.clone()))
                .or_else(|| user_lease(&existing))
                .filter(|pair| {
                    pool.contains(&pair.0, &pair.1) && !address_conflict(pair, &used4, &used6)
                })
                .or_else(|| pool.allocate_dual(&used4, &used6))
                .ok_or_else(|| {
                    (
                        StatusCode::CONFLICT,
                        Json(ErrorResponse {
                            error: "地址池已耗尽，无法创建固定租约".into(),
                        }),
                    )
                })?
        };
        Some(pair)
    } else {
        None
    };

    if let Some(pair) = &fixed_pair {
        if address_conflict(pair, &used4, &used6) {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "固定地址已被其他用户或设备占用".into(),
                }),
            ));
        }
    }

    if let Some((ipv4, ipv6)) = &fixed_pair {
        if devices.values().any(|device| {
            device.user_id == existing.user_id
                && device.status == "active"
                && (device.ipv4 != *ipv4 || device.ipv6 != *ipv6)
        }) {
            return Err((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "用户存在活动连接，不能在线变更固定地址；请先断开连接".into(),
                }),
            ));
        }
    }

    let mut next = users.clone();
    let user = next.get_mut(&username).expect("user disappeared");
    user.address_mode = address_mode;
    user.reserved_ipv4 = fixed_pair.as_ref().map(|pair| pair.0.clone());
    user.reserved_ipv6 = fixed_pair.as_ref().map(|pair| pair.1.clone());
    let retained_pair = fixed_pair.clone().or_else(|| user_lease(&existing));
    user.leased_ipv4 = retained_pair.as_ref().map(|pair| pair.0.clone());
    user.leased_ipv6 = retained_pair.as_ref().map(|pair| pair.1.clone());
    let summary = user_summary(user);
    persist_users(&state, &next).map_err(server_error)?;
    *users = next;
    drop(users);

    if let Some((ipv4, ipv6)) = fixed_pair {
        let mut next_devices = devices.clone();
        for device in next_devices
            .values_mut()
            .filter(|device| device.user_id == existing.user_id && device.status != "active")
        {
            if device.ipv4 != ipv4 || device.ipv6 != ipv6 {
                device.ipv4 = ipv4.clone();
                device.ipv6 = ipv6.clone();
                device.config_version += 1;
            }
        }
        persist_devices(&state, &next_devices).map_err(server_error)?;
        *devices = next_devices;
    }
    Ok(Json(summary))
}

fn revoke_devices_for_user(state: &AppState, user_id: &str) -> StatusCode {
    let revoked = {
        let mut devices = state.devices.write().expect("device registry poisoned");
        let mut next = devices.clone();
        let mut revoked = Vec::new();
        for device in next
            .values_mut()
            .filter(|device| device.user_id == user_id && device.status == "active")
        {
            device.status = "revoked".into();
            device.last_seen_at = Utc::now();
            revoked.push((device.device_id, device.public_key.clone()));
        }
        if persist_devices(state, &next).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        *devices = next;
        revoked
    };
    let mut result = StatusCode::NO_CONTENT;
    for (device_id, public_key) in revoked {
        if let Some(interface) = &state.gateway.interface {
            if remove_gateway_peer(&state.gateway, interface, &public_key).is_err() {
                result = StatusCode::BAD_GATEWAY;
            }
        }
        let _ = state.port_forward_store.disconnect_device(device_id);
    }
    result
}

async fn get_pool_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PoolSettings>, StatusCode> {
    require_admin(&state, &headers)?;
    Ok(Json(pool_settings(&state)))
}

async fn update_pool_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PoolSettingsRequest>,
) -> Result<Json<PoolSettings>, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "需要管理员权限".into(),
            }),
        )
    })?;
    if state
        .address_pools
        .read()
        .expect("address pools poisoned")
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "请使用地址池管理接口，旧接口不能覆盖多地址池配置".into(),
            }),
        ));
    }
    let pool = AddressPool::from_cidrs(&input.ipv4_pool, &input.ipv6_pool)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?;
    let devices = state.devices.read().expect("device registry poisoned");
    let users = state.users.read().expect("user registry poisoned");
    let mut current_pool = state.pool.write().expect("address pool poisoned");
    let mut current_cidrs = state
        .internal_cidrs
        .write()
        .expect("internal route registry poisoned");
    // Omitted CIDRs in older callers preserve the current list. An explicit
    // empty list is invalid, not an instruction to silently install defaults.
    let internal_cidrs = match input.internal_cidrs {
        Some(values) => normalize_internal_cidrs(values.iter().map(String::as_str))
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error })))?,
        None => current_cidrs.clone(),
    };
    for user in users.values() {
        user_fixed_pair(user).map_err(|error| {
            (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!("用户 {} 的固定租约无效：{error}", user.username),
                }),
            )
        })?;
    }
    for device in devices.values() {
        if !pool.contains(&device.ipv4, &device.ipv6) {
            let automatic = users
                .values()
                .find(|user| user.user_id == device.user_id)
                .map(|user| user.address_mode == "automatic")
                .unwrap_or(true);
            if automatic && device.status == "active" {
                return Err((
                    StatusCode::CONFLICT,
                    Json(ErrorResponse {
                        error: format!(
                            "新地址池不包含用户 {} 的活动自动租约 {} / {}；请先断开连接",
                            device.username, device.ipv4, device.ipv6
                        ),
                    }),
                ));
            }
        }
    }
    // Persist the entire generation before publishing it in memory or
    // returning success. Serialize concurrent saves under these same locks.
    persist_network_settings(
        &state,
        &NetworkSettings {
            ipv4_pool: pool.ipv4_cidr(),
            ipv6_pool: pool.ipv6_cidr(),
            internal_cidrs: internal_cidrs.clone(),
        },
    )
    .map_err(server_error)?;
    *current_pool = pool;
    *current_cidrs = internal_cidrs;
    drop(current_cidrs);
    drop(current_pool);
    drop(users);
    drop(devices);
    Ok(Json(pool_settings(&state)))
}

#[derive(Serialize)]
struct DiagnosticUploadResponse {
    report_id: String,
    received_bytes: usize,
}

async fn upload_client_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<DiagnosticUploadResponse>, (StatusCode, Json<ErrorResponse>)> {
    let principal = authenticate(&state, &headers).map_err(|status| {
        (
            status,
            Json(ErrorResponse {
                error: "客户端会话无效".into(),
            }),
        )
    })?;
    if body.is_empty() || body.len() > MAX_CLIENT_DIAGNOSTIC_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(ErrorResponse {
                error: format!(
                    "诊断日志大小必须为 1 字节至 {} KiB",
                    MAX_CLIENT_DIAGNOSTIC_BYTES / 1024
                ),
            }),
        ));
    }
    let now = Utc::now();
    let uploads = DIAGNOSTIC_UPLOADS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let mut recent = uploads.lock().expect("diagnostic upload registry poisoned");
        recent.retain(|_, timestamp| {
            now.signed_duration_since(*timestamp).num_seconds() < DIAGNOSTIC_UPLOAD_INTERVAL_SECONDS
        });
        if recent.contains_key(&principal.user_id) {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: "同一账户每 60 秒只能上报一次诊断日志，请稍后重试".into(),
                }),
            ));
        }
        recent.insert(principal.user_id.clone(), now);
    }

    let base = state
        .diagnostic_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("diagnostics"));
    let user_dir = base.join(&principal.user_id);
    if fs::create_dir_all(&user_dir).is_err() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "主控无法创建诊断日志目录".into(),
            }),
        ));
    }
    let report_id = format!("diag_{}", Uuid::new_v4());
    let path = user_dir.join(format!("{report_id}.log"));
    if fs::write(&path, &body).is_err() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "主控无法保存诊断日志".into(),
            }),
        ));
    }

    let mut reports: Vec<_> = fs::read_dir(&user_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|value| value == "log"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    reports.sort_by_key(|(modified, _)| *modified);
    while reports.len() > MAX_DIAGNOSTIC_REPORTS_PER_USER {
        if let Some((_, old_path)) = reports.first() {
            let _ = fs::remove_file(old_path);
        }
        reports.remove(0);
    }
    Ok(Json(DiagnosticUploadResponse {
        report_id,
        received_bytes: body.len(),
    }))
}

fn app(state: AppState) -> Router {
    Router::new()
        .route(
            "/v1/admin/address-pools",
            get(address_pools::list).post(address_pools::create),
        )
        .route(
            "/v1/admin/address-pools/{id}",
            post(address_pools::edit).delete(address_pools::remove),
        )
        .route(
            "/v1/admin/address-pools/{id}/addresses",
            get(address_pools::addresses),
        )
        .route(
            "/v1/admin/address-pool-defaults",
            get(address_pools::list).post(address_pools::defaults),
        )
        .route(
            "/v1/admin/users/{username}/address-policy",
            get(address_pools::get_policy).post(address_pools::set_policy),
        )
        .route(
            "/v1/admin/settings/internal-routes",
            post(address_pools::save_routes),
        )
        .route("/healthz", get(healthz))
        .route("/v1/version", get(release_version))
        .route("/admin", get(admin_page))
        .route("/admin/", get(admin_page))
        .route("/admin/{*path}", get(admin_asset))
        .route("/v1/auth/login", post(login))
        .route("/register", get(invitations::public_page))
        .route("/register/", get(invitations::public_page))
        .route(
            "/v1/auth/register",
            post(invitations::register)
                .layer(DefaultBodyLimit::max(8 * 1024))
                .layer(axum::middleware::from_fn(invitations::rate_limit)),
        )
        .route(
            "/v1/client/invitations/policy",
            get(invitations::client_policy),
        )
        .route(
            "/v1/client/invitations",
            get(invitations::client_list)
                .post(invitations::client_create)
                .layer(DefaultBodyLimit::max(8 * 1024)),
        )
        .route(
            "/v1/client/invitations/{id}/revoke",
            post(invitations::client_revoke),
        )
        .route(
            "/v1/client/invitation-registrations",
            get(invitations::client_registrations),
        )
        .route(
            "/v1/admin/invitations",
            get(invitations::admin_list).post(invitations::admin_create),
        )
        .route("/v1/admin/invitations/{id}", post(invitations::edit))
        .route(
            "/v1/admin/invitations/{id}/revoke",
            post(invitations::admin_revoke),
        )
        .route(
            "/v1/admin/invitation-registrations",
            get(invitations::admin_registrations),
        )
        .route(
            "/v1/admin/users/{username}/invitation-policy",
            get(invitations::admin_policy).post(invitations::save_policy),
        )
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/password", post(change_password))
        .route("/v1/admin/diagnostics", get(diagnostic_admin::list))
        .route(
            "/v1/admin/diagnostics/{user_id}/{report_id}",
            get(diagnostic_admin::download),
        )
        .route(
            "/v1/client/diagnostics",
            post(upload_client_diagnostics)
                .layer(DefaultBodyLimit::max(MAX_CLIENT_DIAGNOSTIC_BYTES)),
        )
        .route("/v1/client/nodes", get(list_client_nodes))
        .route("/v1/client/geosite", get(client_geosite_resource))
        .route("/v1/client/connections", post(client_connect))
        .route(
            "/v1/client/port-forwards/entries",
            get(list_client_forward_entries),
        )
        .route(
            "/v1/client/port-forwards/quota",
            get(get_client_forward_quota),
        )
        .route(
            "/v1/client/port-forwards",
            get(list_client_forwards).post(create_client_forward),
        )
        .route(
            "/v1/client/port-forwards/{id}",
            delete(delete_client_forward),
        )
        .route(
            "/v1/client/releases/{channel}/latest",
            get(client_release_latest),
        )
        .route(
            "/v1/client/releases/{channel}/{version}/download",
            get(client_release_download),
        )
        .route(
            "/v1/admin/client-releases/{channel}",
            get(list_admin_client_releases).post(create_admin_client_release),
        )
        .route(
            "/v1/admin/client-releases/{channel}/{version}",
            post(edit_admin_client_release).delete(delete_admin_client_release),
        )
        .route("/v1/nodes/register", post(register_node))
        .route("/v1/nodes/{node_id}/heartbeat", post(node_heartbeat))
        .route("/v1/nodes/{node_id}/resource", get(node_resource))
        .route("/v1/nodes/{node_id}/geosite", get(geosite_resource))
        .route(
            "/v1/admin/geosite",
            get(admin_geosite).post(save_admin_geosite),
        )
        .route("/v1/admin/devices", get(list_devices))
        .route("/v1/admin/nodes", get(list_nodes).post(enroll_node))
        .route("/v1/admin/nodes/{node_id}", post(update_node_settings))
        .route(
            "/v1/admin/nodes/{node_id}/update",
            post(update_node_release),
        )
        .route(
            "/v1/admin/nodes/{node_id}/bird-config",
            get(admin_node_bird_config),
        )
        .route("/v1/admin/node-release", get(admin_node_release))
        .route("/v1/admin/nodes/{node_id}/cancel", post(cancel_enrollment))
        .route("/v1/admin/devices/{id}/revoke", post(admin_revoke))
        .route("/v1/admin/users", get(list_users).post(create_user))
        .route(
            "/v1/admin/users/{username}",
            delete(user_deletion::delete_user),
        )
        .route("/v1/admin/users/{username}/status", post(set_user_status))
        .route("/v1/admin/users/{username}/password", post(reset_password))
        .route(
            "/v1/admin/users/{username}/capabilities",
            post(set_user_capabilities),
        )
        .route(
            "/v1/admin/users/{username}/address-lease",
            post(set_user_address_lease),
        )
        .route(
            "/v1/admin/port-forward/entries",
            get(list_admin_forward_entries).post(save_admin_forward_entry),
        )
        .route(
            "/v1/admin/port-forward/entries/{id}",
            post(save_admin_forward_entry).delete(delete_admin_forward_entry),
        )
        .route(
            "/v1/admin/port-forward/allocations",
            get(list_admin_forward_allocations),
        )
        .route(
            "/v1/admin/port-forward/allocations/{id}/revoke",
            post(revoke_admin_forward),
        )
        .route(
            "/v1/admin/users/{username}/port-forward-policy",
            get(get_user_forward_policy).post(set_user_forward_policy),
        )
        .route(
            "/v1/admin/settings/address-pool",
            get(get_pool_settings).post(update_pool_settings),
        )
        .route("/v1/devices/register", post(register))
        .route("/v1/devices/{id}/config", get(get_config))
        .route("/v1/devices/{id}/heartbeat", post(heartbeat))
        .route("/v1/devices/{id}/p2p", post(p2p::advertise))
        .route("/v1/devices/{id}/revoke", post(revoke))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(axum::middleware::from_fn(invitations::sensitive_responses))
        .with_state(state)
}

fn new_state_with_database(
    gateway: GatewayConfig,
    users: HashMap<String, UserAccount>,
    database: Option<Arc<db::PgStore>>,
) -> AppState {
    let port_forward_store = match env::var("PMMAN_DATABASE_URL") {
        Ok(url) => {
            port_forward::PortForwardStore::open(&url).expect("无法初始化 PostgreSQL 端口转发存储")
        }
        Err(_) => {
            #[cfg(test)]
            {
                port_forward::PortForwardStore::memory().expect("无法初始化测试端口转发存储")
            }
            #[cfg(not(test))]
            {
                panic!("PMMAN_DATABASE_URL must be set for PostgreSQL persistence")
            }
        }
    };
    AppState {
        database,
        address_pools: Arc::new(RwLock::new(None)),
        p2p: Arc::new(RwLock::new(HashMap::new())),
        devices: Arc::new(RwLock::new(HashMap::new())),
        users: Arc::new(RwLock::new(users)),
        user_store: env::var("PMMAN_USER_STORE").ok().map(PathBuf::from),
        sessions: Arc::new(RwLock::new(HashMap::new())),
        pool: Arc::new(RwLock::new(
            AddressPool::from_environment().expect("invalid address pool"),
        )),
        internal_cidrs: Arc::new(RwLock::new(
            configured_internal_cidrs().expect("invalid PMMAN_INTERNAL_CIDRS"),
        )),
        settings_store: Some(
            env::var("PMMAN_SETTINGS_STORE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    env::var("PMMAN_USER_STORE")
                        .or_else(|_| env::var("PMMAN_NODE_STORE"))
                        .map(PathBuf::from)
                        .map(|path| path.with_file_name("network-settings.json"))
                        .unwrap_or_else(|_| PathBuf::from("network-settings.json"))
                }),
        ),
        nodes: Arc::new(RwLock::new(HashMap::new())),
        node_store: env::var("PMMAN_NODE_STORE").ok().map(PathBuf::from),
        node_resource_dir: env::var("PMMAN_NODE_RESOURCE_DIR").ok().map(PathBuf::from),
        port_forward_store,
        geosite_dir: env::var("PMMAN_GEOSITE_DIR").ok().map(PathBuf::from),
        client_release_dir: env::var("PMMAN_CLIENT_RELEASE_DIR").ok().map(PathBuf::from),
        client_release_private_key: env::var("PMMAN_CLIENT_RELEASE_PRIVATE_KEY").ok().or_else(
            || {
                env::var("PMMAN_CLIENT_RELEASE_PRIVATE_KEY_FILE")
                    .ok()
                    .and_then(|path| fs::read_to_string(path).ok())
            },
        ),
        diagnostic_dir: env::var("PMMAN_DIAGNOSTIC_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from),
        gateway,
    }
}

fn load_user_registry(state: &AppState) -> Result<(), String> {
    let mut users: HashMap<String, UserAccount> = if let Some(database) = &state.database {
        match database.get_document("users")? {
            Some(payload) => serde_json::from_str(&payload).map_err(|_| "账户记录格式错误")?,
            None => {
                let users = state.users.read().expect("user registry poisoned").clone();
                persist_users(state, &users)?;
                users
            }
        }
    } else {
        let Some(path) = &state.user_store else {
            return Ok(());
        };
        if !path.exists() {
            let users = state.users.read().expect("user registry poisoned").clone();
            persist_users(state, &users)?;
            return Ok(());
        }
        serde_json::from_slice(&fs::read(path).map_err(|_| "无法读取账户记录")?)
            .map_err(|_| "账户记录格式错误")?
    };
    let mut reserved_ipv4 = HashSet::new();
    let mut reserved_ipv6 = HashSet::new();
    for user in users.values_mut() {
        user_fixed_pair(user)?;
        if user.leased_ipv4.is_some() != user.leased_ipv6.is_some() {
            return Err("账户记录中的双栈租约不完整".into());
        }
        if let Some((ipv4, ipv6)) = user_lease(user) {
            let ipv4 = ipv4
                .parse::<Ipv4Addr>()
                .map_err(|_| "账户记录中的固定 IPv4 地址无效")?;
            let ipv6 = ipv6
                .parse::<Ipv6Addr>()
                .map_err(|_| "账户记录中的固定 IPv6 地址无效")?;
            if !reserved_ipv4.insert(u32::from(ipv4)) || !reserved_ipv6.insert(u128::from(ipv6)) {
                return Err("账户记录中的地址存在重复保留".into());
            }
            if user.address_mode == "fixed" {
                user.reserved_ipv4 = Some(ipv4.to_string());
                user.reserved_ipv6 = Some(ipv6.to_string());
            } else {
                user.leased_ipv4 = Some(ipv4.to_string());
                user.leased_ipv6 = Some(ipv6.to_string());
            }
        }
    }
    *state.users.write().expect("user registry poisoned") = users;
    Ok(())
}

fn load_node_registry(state: &AppState) -> Result<(), String> {
    let mut nodes: HashMap<String, Node> = if let Some(database) = &state.database {
        database
            .get_document("nodes")?
            .map(|payload| serde_json::from_str(&payload).map_err(|_| "节点记录格式错误"))
            .transpose()?
            .unwrap_or_default()
    } else {
        match &state.node_store {
            Some(path) if path.exists() => {
                serde_json::from_slice(&fs::read(path).map_err(|_| "无法读取节点记录")?)
                    .map_err(|_| "节点记录格式错误")?
            }
            _ => HashMap::new(),
        }
    };
    // A stored timestamp is never proof that a process is online after restart.
    let published_version = current_node_release(state)
        .ok()
        .flatten()
        .map(|release| release.version);
    for node in nodes.values_mut() {
        let repaired = node.network.apply_managed_dns_defaults();
        let repaired_override = node
            .network_override
            .as_mut()
            .is_some_and(NodeNetworkConfig::apply_managed_dns_defaults);
        if repaired || repaired_override {
            println!(
                "node {}: generated missing managed DNS upstream defaults",
                node.node_id
            );
        }
        node.last_seen_at = DateTime::UNIX_EPOCH;
        // A deployment replaces the single active resource. Do not preserve
        // an update target that no longer exists in the master resource store.
        if published_version.is_some()
            && node
                .desired_resource_version
                .as_ref()
                .is_some_and(|version| Some(version) != published_version.as_ref())
        {
            node.desired_resource_version = None;
        }
    }
    if let Ok(path) = env::var("PMMAN_NODE_BOOTSTRAP_FILE") {
        #[derive(Deserialize)]
        struct Seed {
            #[serde(flatten)]
            input: NodeEnrollmentRequest,
            node_token: String,
        }
        let seeds: Vec<Seed> =
            serde_json::from_slice(&fs::read(path).map_err(|_| "无法读取节点初始化配置")?)
                .map_err(|_| "节点初始化配置格式错误")?;
        for seed in seeds {
            if seed.node_token.len() < 24 {
                return Err("节点初始化凭据至少 24 位".into());
            }
            if !nodes.contains_key(&seed.input.node_id) {
                let node = pending_node(seed.input, seed.node_token)?;
                nodes.insert(node.node_id.clone(), node);
            }
        }
    }
    persist_nodes(state, &nodes)?;
    *state.nodes.write().expect("node registry poisoned") = nodes;
    Ok(())
}

fn validate_production_storage_configuration() {
    let value = env::var("PMMAN_DATABASE_URL").unwrap_or_else(|_| {
        panic!("PMMAN_DATABASE_URL must be set; refusing to start without PostgreSQL")
    });
    if !value.starts_with("postgres") {
        panic!("PMMAN_DATABASE_URL must be a PostgreSQL connection string");
    }
    for name in [
        "PMMAN_CLIENT_RELEASE_DIR",
        "PMMAN_NODE_RESOURCE_DIR",
        "PMMAN_GEOSITE_DIR",
        "PMMAN_DIAGNOSTIC_DIR",
    ] {
        let value = env::var(name).unwrap_or_else(|_| panic!("{name} must be set"));
        let path = PathBuf::from(&value);
        if !path.is_absolute() {
            panic!("{name} must be an absolute path: {value}");
        }
        if !path.is_dir() {
            panic!("{name} directory does not exist: {}", path.display());
        }
    }
}

fn main() {
    validate_production_storage_configuration();
    let database_url = env::var("PMMAN_DATABASE_URL").expect("PMMAN_DATABASE_URL must be set");
    // The `postgres` crate is synchronous and starts its own runtime. Finish
    // all database-backed bootstrap work before creating Tokio for HTTP.
    let database =
        Arc::new(db::PgStore::connect(&database_url).expect("无法初始化 PostgreSQL 持久化存储"));
    let state = new_state_with_database(
        GatewayConfig::from_environment(),
        seed_users(),
        Some(database),
    );
    load_network_settings(&state).expect("invalid network settings storage");
    load_user_registry(&state).expect("invalid user storage");
    load_node_registry(&state).expect("invalid node enrollment storage/bootstrap");
    load_device_registry(&state).expect("invalid device storage");
    address_pools::initialize(&state).expect("invalid address pool storage/migration");
    let cleanup_state = state.clone();
    let app = app(state);
    let addr: SocketAddr = env::var("PMMAN_MASTER_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()
        .expect("invalid PMMAN_MASTER_LISTEN");
    println!("control plane listening on http://{addr}");
    println!("initial administrator account configured through PMMAN_ADMIN_PASSWORD; change it after first login");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("无法创建 Tokio runtime")
        .block_on(async move {
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    let state = cleanup_state.clone();
                    match tokio::task::spawn_blocking(move || {
                        user_deletion::retry_legacy_cleanup(&state)
                    })
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => eprintln!("reason=legacy_cleanup_retry_failed"),
                        Err(_) => eprintln!("reason=legacy_cleanup_worker_failed"),
                    }
                }
            });
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn password_hashes_are_salted_and_legacy_hashes_remain_verifiable() {
        let first = hash_password("correct horse battery staple");
        let second = hash_password("correct horse battery staple");
        assert_ne!(first, second);
        assert!(verify_password(&first, "correct horse battery staple").0);
        assert!(!verify_password(&first, "wrong password").0);
        assert_eq!(
            verify_password(
                &legacy_hash_password("correct horse battery staple"),
                "correct horse battery staple"
            ),
            (true, true)
        );
    }

    fn bird_test_node(route_table: Option<&str>, interface: Option<&str>) -> Node {
        Node {
            rate_limit: NodeRateLimit::default(),
            resource_version: None,
            desired_resource_version: None,
            node_id: "bird-test".into(),
            name: "BIRD test".into(),
            wireguard_endpoint: "node.example.com:51820".into(),
            probe_endpoint: "node.example.com:51840".into(),
            network: NodeNetworkConfig::default(),
            network_override: None,
            applied: Vec::new(),
            sync_errors: Vec::new(),
            applied_route_table: None,
            cleanup_ok: false,
            port_forward_applied: Vec::new(),
            port_forward_errors: Vec::new(),
            region: None,
            server_public_key: String::new(),
            wireguard_interface: interface.map(str::to_owned),
            route_table: route_table.map(str::to_owned),
            node_token: "test-token".into(),
            last_seen_at: DateTime::UNIX_EPOCH,
            enabled: true,
            connectable: true,
            registered: true,
            wireguard_endpoint_override: None,
            probe_endpoint_override: None,
            region_override: None,
            server_public_key_override: None,
        }
    }

    #[test]
    fn bird_template_uses_node_interface_and_numeric_route_table() {
        let generated = node_bird_config(&bird_test_node(Some("52001"), Some("wgnetport")));
        assert!(generated.ready);
        assert!(generated.warnings.is_empty());
        assert!(generated
            .config
            .contains("define PMMAN_NETWORK_KERNEL_TABLE = 52001;"));
        assert!(generated
            .config
            .contains("define PMMAN_NETWORK_INTERFACE = \"wgnetport\";"));
        assert!(generated
            .config
            .contains("protocol direct pmman_network_direct"));
        assert!(generated
            .config
            .contains("protocol pipe pmman_network_pipe_v4"));
        assert!(generated
            .config
            .contains("protocol pipe pmman_network_pipe_v6"));
        assert!(!generated.config.contains("export all"));
    }

    #[test]
    fn bird_template_warns_for_named_table_or_missing_interface() {
        let generated = node_bird_config(&bird_test_node(Some("ospf_return"), None));
        assert!(!generated.ready);
        assert_eq!(generated.warnings.len(), 2);
        assert!(
            generated.config.contains("replace the placeholder")
                || generated.config.contains("未完成模板")
        );
        assert!(generated
            .config
            .contains("define PMMAN_NETWORK_KERNEL_TABLE = 0;"));
        assert!(generated
            .config
            .contains("REPLACE_WITH_WIREGUARD_INTERFACE"));
    }

    #[test]
    fn linux_interface_validation_rejects_shell_metacharacters() {
        assert!(validate_optional_linux_interface(Some("wgnetport")).is_ok());
        assert!(validate_optional_linux_interface(Some("wg;rm -rf")).is_err());
        assert!(validate_optional_linux_interface(Some("abcdefghijklmnop")).is_err());
    }

    fn test_users() -> HashMap<String, UserAccount> {
        let now = Utc::now();
        [
            (
                "admin".into(),
                UserAccount {
                    user_id: "admin".into(),
                    username: "admin".into(),
                    password_hash: hash_password("adminpw"),
                    role: "admin".into(),
                    enabled: true,
                    allow_ipv4_global: true,
                    allow_ipv6_global: true,
                    allow_ipv4_split: true,
                    allow_ipv6_split: true,
                    created_at: now,
                    must_change_password: false,
                    address_mode: default_address_mode(),
                    reserved_ipv4: None,
                    reserved_ipv6: None,
                    leased_ipv4: None,
                    leased_ipv6: None,
                },
            ),
            (
                "u1".into(),
                UserAccount {
                    user_id: "u1".into(),
                    username: "u1".into(),
                    password_hash: hash_password("userpw"),
                    role: "user".into(),
                    enabled: true,
                    allow_ipv4_global: false,
                    allow_ipv6_global: false,
                    allow_ipv4_split: false,
                    allow_ipv6_split: false,
                    created_at: now,
                    must_change_password: false,
                    address_mode: default_address_mode(),
                    reserved_ipv4: None,
                    reserved_ipv6: None,
                    leased_ipv4: None,
                    leased_ipv6: None,
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    pub(crate) fn test_state() -> AppState {
        let state = AppState {
            database: None,
            address_pools: Arc::new(RwLock::new(None)),
            p2p: Arc::new(RwLock::new(HashMap::new())),
            devices: Arc::new(RwLock::new(HashMap::new())),
            users: Arc::new(RwLock::new(test_users())),
            user_store: None,
            settings_store: None,
            diagnostic_dir: None,
            port_forward_store: port_forward::PortForwardStore::memory().unwrap(),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pool: Arc::new(RwLock::new(
                AddressPool::from_cidrs("10.88.0.0/24", "fd88:1000::/64").unwrap(),
            )),
            internal_cidrs: Arc::new(RwLock::new(
                DEFAULT_INTERNAL_CIDRS
                    .iter()
                    .map(|route| (*route).to_string())
                    .collect(),
            )),
            nodes: Arc::new(RwLock::new(
                [(
                    "node-a".into(),
                    pending_node(
                        NodeEnrollmentRequest {
                            node_id: "node-a".into(),
                            name: "Test node".into(),
                            region: Some("test".into()),
                            route_table: None,
                            network: NodeNetworkConfig::default(),
                            connectable: true,
                            wireguard_interface: None,
                        },
                        "node-secret".into(),
                    )
                    .unwrap(),
                )]
                .into_iter()
                .collect(),
            )),
            node_store: None,
            node_resource_dir: None,
            geosite_dir: None,
            client_release_dir: None,
            client_release_private_key: None,
            gateway: GatewayConfig {
                interface: None,
                server_public_key: "test-server-public-key".into(),
                endpoint: "127.0.0.1:51820".into(),
                wg_binary: "wg".into(),
            },
        };
        state
    }

    fn test_app() -> Router {
        app(test_state())
    }

    #[tokio::test]
    async fn node_resource_is_bound_to_node_token_and_manifest() {
        let directory = env::temp_dir().join(format!("pmman-node-resource-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let bytes = b"test-node-binary";
        let sha256 = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let release = NodeRelease {
            version: sha256.clone(),
            file_name: format!("pmman-node-{sha256}"),
            sha256,
            size: bytes.len() as u64,
            published_at: Utc::now(),
        };
        fs::write(directory.join(&release.file_name), bytes).unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&release).unwrap(),
        )
        .unwrap();
        let mut state = test_state();
        state.node_resource_dir = Some(directory.clone());
        let router = app(state);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/nodes/node-a/resource")
                    .header("X-PMMAN-NODE-TOKEN", "node-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-pmman-node-sha256"], release.sha256);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .as_ref(),
            bytes.as_slice()
        );
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/v1/nodes/node-a/resource")
                    .header("X-PMMAN-NODE-TOKEN", "wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn client_release_is_public_versioned_and_path_safe() {
        let directory = env::temp_dir().join(format!("pmman-client-release-{}", Uuid::new_v4()));
        let version_dir = directory.join("stable").join("0.2.0");
        fs::create_dir_all(&version_dir).unwrap();
        let bytes = b"lean-updater-installer";
        let sha256 = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let release = ClientReleaseManifest {
            product: "pmman-network-client".into(),
            channel: "stable".into(),
            platform: "windows".into(),
            arch: "x64".into(),
            version: "0.2.0".into(),
            min_supported_version: "0.0.1-alpha".into(),
            mandatory_after: None,
            installer_file: "0.2.0/installer.exe".into(),
            size: bytes.len() as u64,
            sha256: sha256.clone(),
            signature: "test-signature".into(),
            published_at: Utc::now().to_rfc3339(),
            release_notes: vec!["test".into()],
        };
        fs::write(version_dir.join("installer.exe"), bytes).unwrap();
        fs::write(
            directory.join("stable").join("manifest.json"),
            serde_json::to_vec(&release).unwrap(),
        )
        .unwrap();
        let mut state = test_state();
        state.client_release_dir = Some(directory.clone());
        let router = app(state);
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/client/releases/stable/latest?platform=windows&arch=x64")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let latest: ClientReleaseResponse = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(latest.available);
        assert_eq!(latest.manifest.unwrap().sha256, sha256);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/client/releases/stable/0.2.0/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .as_ref(),
            bytes.as_slice()
        );

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/v1/client/releases/stable/../secret/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn admin_entry_and_embedded_assets_have_correct_cache_and_content_types() {
        let router = test_app();
        for uri in ["/admin", "/admin/"] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let html = std::str::from_utf8(&body).unwrap();
            assert!(html.contains("/admin/assets/"));
            assert!(!html.contains("localhost:"));
        }
        let asset = ADMIN_ASSETS
            .get_dir("assets")
            .unwrap()
            .files()
            .find(|file| {
                file.path()
                    .extension()
                    .is_some_and(|extension| extension == "js")
            })
            .unwrap();
        let uri = format!("/admin/{}", asset.path().to_str().unwrap());
        let response = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
    }

    #[tokio::test]
    async fn missing_admin_assets_do_not_return_html_or_expose_project_files() {
        for uri in ["/admin/assets/missing.js", "/admin/../Cargo.toml"] {
            let response = test_app()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    pub(crate) async fn login_token(app: &Router, username: &str, password: &str) -> String {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                "{{\"username\":\"{username}\",\"password\":\"{password}\"}}"
            )))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<LoginResponse>(&body)
            .unwrap()
            .token
    }

    #[tokio::test]
    async fn failed_password_save_preserves_credentials_and_sessions() {
        let directory = env::temp_dir().join(format!("pmman-password-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let blocked = directory.join("blocked-parent");
        fs::write(&blocked, b"not a directory").unwrap();
        let mut state = test_state();
        state.user_store = Some(blocked.join("users.json"));
        let app = app(state.clone());
        let current = login_token(&app, "u1", "userpw").await;
        let other = login_token(&app, "u1", "userpw").await;
        let hash = state.users.read().unwrap()["u1"].password_hash.clone();
        let response = app
            .oneshot(json_post(
                "/v1/auth/password",
                &current,
                serde_json::json!({"current_password":"userpw", "new_password":"newpassword"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(state.users.read().unwrap()["u1"].password_hash, hash);
        assert!(state.sessions.read().unwrap().contains_key(&other));
        assert_eq!(fs::read(blocked).unwrap(), b"not a directory");
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn logout_revokes_the_bearer_token_immediately() {
        let app = test_app();
        let token = login_token(&app, "u1", "userpw").await;

        let before = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/client/nodes")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(before.status(), StatusCode::OK);

        let logout = Request::builder()
            .method("POST")
            .uri("/v1/auth/logout")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(logout).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let after = app
            .oneshot(
                Request::builder()
                    .uri("/v1/client/nodes")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn client_geosite_download_requires_a_valid_user_session() {
        let app = test_app();
        let anonymous = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/client/geosite")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let token = login_token(&app, "u1", "userpw").await;
        let authenticated = app
            .oneshot(
                Request::builder()
                    .uri("/v1/client/geosite")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authenticated.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn admin_password_reset_revokes_the_target_users_old_token() {
        let app = test_app();
        let admin = login_token(&app, "admin", "adminpw").await;
        let old_user_token = login_token(&app, "u1", "userpw").await;

        let reset = json_post(
            "/v1/admin/users/u1/password",
            &admin,
            serde_json::json!({"password":"new-user-password"}),
        );
        assert_eq!(
            app.clone().oneshot(reset).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let old_session = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/client/nodes")
                    .header("authorization", format!("Bearer {old_user_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(old_session.status(), StatusCode::UNAUTHORIZED);

        let new_token = login_token(&app, "u1", "new-user-password").await;
        let new_session = app
            .oneshot(
                Request::builder()
                    .uri("/v1/client/nodes")
                    .header("authorization", format!("Bearer {new_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(new_session.status(), StatusCode::OK);
    }

    async fn register(app: &Router, token: &str, public_key: &str) -> DeviceConfig {
        register_with_modes(app, token, public_key, "internal", "internal").await
    }

    async fn register_with_modes(
        app: &Router,
        token: &str,
        public_key: &str,
        ipv4_mode: &str,
        ipv6_mode: &str,
    ) -> DeviceConfig {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/devices/register")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(format!(
                "{{\"public_key\":\"{public_key}\",\"ipv4_mode\":\"{ipv4_mode}\",\"ipv6_mode\":\"{ipv6_mode}\"}}"
            )))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn register_node(app: &Router, node_id: &str, node_token: &str) {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/nodes/register")
            .header("content-type", "application/json")
            .header("x-pmman-node-token", node_token)
            .body(Body::from(format!(
                "{{\"node_id\":\"{node_id}\",\"name\":\"Test node\",\"probe_endpoint\":\"127.0.0.1:51840\",\"wireguard_endpoint\":\"127.0.0.1:51820\",\"region\":\"test\",\"server_public_key\":\"Egh8MitVQY/209St4o0Jg5/V7XtmNOWM/3B1zAVdPXQ=\"}}"
            )))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
        let request = Request::builder()
            .method("POST")
            .uri(format!("/v1/nodes/{node_id}/heartbeat"))
            .header("x-pmman-node-token", node_token)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn unauthenticated_registration_is_rejected() {
        let app = test_app();
        let request = Request::builder()
            .method("POST")
            .uri("/v1/devices/register")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"public_key":"k1"}"#))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn login_register_revoke_and_reuse_lease() {
        let app = test_app();
        let token = login_token(&app, "u1", "userpw").await;
        let first = register(&app, &token, "k1").await;
        assert_eq!(first.ipv4, "10.88.0.10/32");
        assert_eq!(first.ipv6, "fd88:1000::a/128");

        let revoke = Request::builder()
            .method("POST")
            .uri(format!("/v1/devices/{}/revoke", first.device_id))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(r#"{"public_key":"k1"}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(revoke).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let second = register(&app, &token, "k2").await;
        assert_eq!(second.device_id, first.device_id);
        assert_eq!(second.ipv4, first.ipv4);
        assert_eq!(second.ipv6, first.ipv6);
        assert_eq!(second.config_version, 2);
    }

    #[tokio::test]
    async fn crashed_client_lease_expires_in_node_snapshot_and_cannot_be_renewed_late() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let token = login_token(&app, "u1", "userpw").await;
        let original = register(&app, &token, "crashed-key").await;
        let now = Utc::now();
        {
            let mut devices = state.devices.write().unwrap();
            let device = devices.get_mut(&original.device_id).unwrap();
            device.node_id = "node-a".into();
            device.last_seen_at = now - Duration::seconds(CLIENT_AUTH_LEASE_SECONDS - 1);
        }
        expire_client_leases(&state, now).unwrap();
        assert_eq!(
            state.devices.read().unwrap()[&original.device_id].status,
            "active"
        );
        state
            .devices
            .write()
            .unwrap()
            .get_mut(&original.device_id)
            .unwrap()
            .last_seen_at = now - Duration::seconds(CLIENT_AUTH_LEASE_SECONDS);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/nodes/node-a/heartbeat")
                    .header("x-pmman-node-token", "node-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let snapshot: NodeHeartbeatResponse = serde_json::from_slice(&body).unwrap();
        assert!(snapshot.active.is_empty());
        assert_eq!(snapshot.revoked[0].public_key, "crashed-key");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/devices/{}/heartbeat", original.device_id))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        // This fixture reconnects through the legacy registration endpoint,
        // i.e. changes from node-a to another gateway. Expiry only revokes
        // authorization; it is not proof that node-a has removed its peer.
        let blocked = app
            .clone()
            .oneshot(json_post(
                "/v1/devices/register",
                &token,
                serde_json::json!({"public_key":"replacement-key"}),
            ))
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::CONFLICT);
        let cleaned = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/nodes/node-a/heartbeat")
                    .header("x-pmman-node-token", "node-secret")
                    .body(Body::from(r#"{"cleanup_ok":true,"applied":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cleaned.status(), StatusCode::OK);
        let replacement = register(&app, &token, "replacement-key").await;
        assert_eq!(replacement.device_id, original.device_id);
        assert_eq!(replacement.ipv4, original.ipv4);
        assert_eq!(replacement.ipv6, original.ipv6);
        assert_eq!(replacement.config_version, original.config_version + 1);
    }

    #[tokio::test]
    async fn mode_registration_requires_authorization_and_returns_modes() {
        let app = test_app();
        let user = login_token(&app, "u1", "userpw").await;
        let denied = Request::builder()
            .method("POST")
            .uri("/v1/devices/register")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {user}"))
            .body(Body::from(
                r#"{"public_key":"mode-denied","ipv4_mode":"global","ipv6_mode":"split"}"#,
            ))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(denied).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let admin = login_token(&app, "admin", "adminpw").await;
        let grant = Request::builder()
            .method("POST")
            .uri("/v1/admin/users/u1/capabilities")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(
                r#"{"allow_ipv4_global":true,"allow_ipv6_global":true,"allow_ipv4_split":true,"allow_ipv6_split":true}"#,
            ))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(grant).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let config = register_with_modes(&app, &user, "mode-allowed", "global", "split").await;
        assert_eq!(config.ipv4_mode, "global");
        assert_eq!(config.ipv6_mode, "split");
        assert!(config.dns_servers.is_empty());
        assert_eq!(config.routes, vec!["10.20.0.0/16", "fd20:1000::/48"]);
    }

    #[tokio::test]
    async fn master_connection_waits_for_exact_node_install_ack_and_no_user_token_reaches_node() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let user = login_token(&app, "u1", "userpw").await;
        let public_key = "CESWYapyJVz0oRvKAfRmYvkyj0mAGOJTdOtQkM9JBWs=";
        let request = json_post(
            "/v1/client/connections",
            &user,
            serde_json::json!({"node_id":"node-a","public_key":public_key}),
        );
        let task = tokio::spawn(app.clone().oneshot(request));
        let device = loop {
            let lease = state.devices.read().unwrap().values().next().cloned();
            if let Some(lease) = lease {
                break lease;
            }
            tokio::task::yield_now().await;
        };
        let sync_request = |version| {
            Request::builder()
                .method("POST")
                .uri("/v1/nodes/node-a/heartbeat")
                .header("x-pmman-node-token", "node-secret")
                .body(Body::from(
                    serde_json::json!({"applied":[{"device_id":device.device_id,
                "public_key":public_key,"config_version":version}]})
                    .to_string(),
                ))
                .unwrap()
        };
        app.clone()
            .oneshot(sync_request(device.config_version + 1))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            !task.is_finished(),
            "old/wrong acknowledgments must not release a config"
        );
        let response = app
            .clone()
            .oneshot(sync_request(device.config_version))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let sync: NodeHeartbeatResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(sync.active[0].config_version, device.config_version);
        let response = task.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let config: DeviceConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(config.ipv4, "10.88.0.10/32");
        assert_eq!(config.endpoint, "127.0.0.1:51820");
        // Nodes authenticate only with their own token; user credentials alone
        // cannot read peer reconciliation, and removed proxy routes are gone.
        let request = json_post("/v1/nodes/node-a/heartbeat", &user, serde_json::json!({}));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let old_proxy = json_post(
            "/v1/nodes/node-a/devices/register",
            &user,
            serde_json::json!({"public_key":public_key}),
        );
        assert_eq!(
            app.clone().oneshot(old_proxy).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        let revoke = json_post(
            &format!("/v1/devices/{}/revoke", device.device_id),
            &user,
            serde_json::json!({"public_key":public_key}),
        );
        let revoke_task = tokio::spawn(app.clone().oneshot(revoke));
        loop {
            if state
                .devices
                .read()
                .unwrap()
                .get(&device.device_id)
                .unwrap()
                .status
                == "revoked"
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let cleaned = Request::builder()
            .method("POST")
            .uri("/v1/nodes/node-a/heartbeat")
            .header("x-pmman-node-token", "node-secret")
            .body(Body::from(r#"{"applied":[],"cleanup_ok":true}"#))
            .unwrap();
        app.clone().oneshot(cleaned).await.unwrap();
        assert_eq!(
            revoke_task.await.unwrap().unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let response = app
            .oneshot(sync_request(device.config_version))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let sync: NodeHeartbeatResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(sync.active.is_empty());
        assert_eq!(sync.revoked[0].public_key, public_key);
    }

    #[tokio::test(start_paused = true)]
    async fn missing_install_ack_times_out_and_revokes_lease() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let user = login_token(&app, "u1", "userpw").await;
        let response = app
            .oneshot(json_post(
                "/v1/client/connections",
                &user,
                serde_json::json!({
                    "node_id":"node-a", "public_key":"CESWYapyJVz0oRvKAfRmYvkyj0mAGOJTdOtQkM9JBWs="
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            state
                .devices
                .read()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .status,
            "revoked"
        );
    }

    #[tokio::test]
    async fn node_install_error_is_reported_and_failed_connection_is_revoked() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let user = login_token(&app, "u1", "userpw").await;
        let public_key = "CESWYapyJVz0oRvKAfRmYvkyj0mAGOJTdOtQkM9JBWs=";
        let task = tokio::spawn(app.clone().oneshot(json_post(
            "/v1/client/connections",
            &user,
            serde_json::json!({"node_id":"node-a","public_key":public_key}),
        )));
        let lease = loop {
            let lease = state.devices.read().unwrap().values().next().cloned();
            if let Some(lease) = lease {
                break lease;
            }
            tokio::task::yield_now().await;
        };
        let error = Request::builder().method("POST").uri("/v1/nodes/node-a/heartbeat")
            .header("x-pmman-node-token","node-secret").body(Body::from(serde_json::json!({
                "errors":[{"device_id":lease.device_id,"public_key":public_key,
                    "config_version":lease.config_version,"error":"return route install denied"}]
            }).to_string())).unwrap();
        app.clone().oneshot(error).await.unwrap();
        let response = task.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8(bytes.to_vec())
            .unwrap()
            .contains("return route install denied"));
        assert_eq!(
            state
                .devices
                .read()
                .unwrap()
                .get(&lease.device_id)
                .unwrap()
                .status,
            "revoked"
        );
    }

    #[test]
    fn master_owns_route_and_dns_configuration_for_each_address_family() {
        let state = test_state();
        let mut node = state.nodes.read().unwrap().get("node-a").unwrap().clone();
        node.network = NodeNetworkConfig {
            ipv4_split_routes: Some(vec!["1.1.1.0/24".into()]),
            ipv6_split_routes: Some(vec!["2606:4700::/32".into()]),
            dns_servers: vec!["10.88.0.1".into(), "fd88:1000::1".into()],
            pmmandns_upstreams: vec!["1.1.1.1:53".into()],
            pmmandns_domestic_upstreams: vec!["114.114.114.114:53".into()],
            pmmandns_foreign_upstreams: vec!["1.1.1.1:53".into()],
            pmmandns_domestic_rules: vec!["cn.example".into()],
            pmmandns_foreign_rules: vec!["example.com".into()],
            pmmandns_default_group: "foreign".into(),
            pmmandns_rules: vec![pmman_protocol::dns::RouteRule {
                suffix: "special.example".into(),
                upstreams: vec!["8.8.8.8:53".into()],
            }],
            pmmandns_geosite: None,
            pmmandns_geosite_enabled: false,
            gateway_ipv4: Some("10.88.0.1/24".into()),
            gateway_ipv6: Some("fd88:1000::1/64".into()),
        };
        assert!(validate_node_network(&node.network).is_ok());
        let device = Device {
            device_id: Uuid::new_v4(),
            user_id: "u1".into(),
            username: "u1".into(),
            public_key: "key".into(),
            node_id: "node-a".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88:1000::a".into(),
            ipv4_mode: "split".into(),
            ipv6_mode: "global".into(),
            status: "active".into(),
            config_version: 1,
            last_seen_at: Utc::now(),
            traffic: pmman_protocol::TrafficStats::default(),
            session_traffic: pmman_protocol::TrafficStats::default(),
        };
        let result = node_device_config(&node, config(&device, &gateway_for_node(&node))).unwrap();
        assert!(result.routes.contains(&"1.1.1.0/24".into()));
        assert!(result.routes.contains(&"::/1".into()));
        assert!(!result.routes.contains(&"2606:4700::/32".into()));
        assert_eq!(result.dns_servers, node.network.dns_servers);
        let dns_policy = result.dns_policy.as_ref().unwrap();
        assert_eq!(dns_policy.domestic_rules, vec!["cn.example"]);
        assert!(dns_policy.foreign_rules.contains(&"example.com".into()));
        assert!(dns_policy.foreign_rules.contains(&"special.example".into()));
        node.network.ipv4_split_routes = None;
        assert!(node_device_config(&node, config(&device, &gateway_for_node(&node))).is_err());
    }

    #[test]
    fn built_in_split_capability_does_not_require_node_split_tables() {
        let state = test_state();
        let mut node = state.nodes.read().unwrap().get("node-a").unwrap().clone();
        node.network.ipv4_split_routes = None;
        node.network.ipv6_split_routes = None;
        let device = Device {
            device_id: Uuid::new_v4(),
            user_id: "u1".into(),
            username: "u1".into(),
            public_key: "key".into(),
            node_id: "node-a".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88:1000::a".into(),
            ipv4_mode: "split".into(),
            ipv6_mode: "split".into(),
            status: "active".into(),
            config_version: 1,
            last_seen_at: Utc::now(),
            traffic: pmman_protocol::TrafficStats::default(),
            session_traffic: pmman_protocol::TrafficStats::default(),
        };
        let result = node_device_config_with_capability(
            &node,
            config(&device, &gateway_for_node(&node)),
            &DEFAULT_INTERNAL_CIDRS
                .iter()
                .map(|route| (*route).to_string())
                .collect::<Vec<_>>(),
            Some("client_builtin_v1"),
            None,
        )
        .unwrap();
        assert_eq!(
            result.split_route_source.as_deref(),
            Some("client_builtin_v1")
        );
        assert!(result
            .routes
            .iter()
            .all(|route| !route.starts_with("1.1.1.")));
        assert!(result
            .routes
            .iter()
            .all(|route| !route.starts_with("2606:")));
    }

    #[tokio::test]
    async fn node_registration_reports_old_key_for_local_cleanup() {
        let app = test_app();
        register_node(&app, "node-a", "node-secret").await;
        let user = login_token(&app, "u1", "userpw").await;
        let first = register(&app, &user, "node-key-1").await;

        let revoke = Request::builder()
            .method("POST")
            .uri(format!("/v1/devices/{}/revoke", first.device_id))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {user}"))
            .body(Body::from(r#"{"public_key":"node-key-1"}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(revoke).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let second = register(&app, &user, "node-key-2").await;
        assert_eq!(second.previous_public_key.as_deref(), Some("node-key-1"));
    }

    #[tokio::test]
    async fn admin_can_manage_users_and_pool() {
        let app = test_app();
        let token = login_token(&app, "admin", "adminpw").await;
        let create = Request::builder()
            .method("POST")
            .uri("/v1/admin/users")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(
                r#"{"username":"new-user","password":"newpass","role":"user"}"#,
            ))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(create).await.unwrap().status(),
            StatusCode::OK
        );

        let update = Request::builder()
            .method("POST")
            .uri("/v1/admin/settings/address-pool")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(
                r#"{"ipv4_pool":"10.99.0.0/30","ipv6_pool":"fd99:1000::/126","internal_cidrs":["10.20.0.0/16","172.16.30.0/24","fd20:1000::/48","10.20.0.0/16"]}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(update).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let settings: PoolSettings = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            settings.internal_cidrs,
            vec![
                "10.20.0.0/16".to_string(),
                "172.16.30.0/24".to_string(),
                "fd20:1000::/48".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn bird_config_is_admin_only_and_contains_no_node_credentials() {
        let state = test_state();
        {
            let mut nodes = state.nodes.write().unwrap();
            let node = nodes.get_mut("node-a").unwrap();
            node.wireguard_interface = Some("wgnetport".into());
            node.route_table = Some("52001".into());
        }
        let app = app(state);
        let user_token = login_token(&app, "u1", "userpw").await;
        let forbidden = Request::builder()
            .uri("/v1/admin/nodes/node-a/bird-config")
            .header("authorization", format!("Bearer {user_token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(forbidden).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let admin_token = login_token(&app, "admin", "adminpw").await;
        let allowed = Request::builder()
            .uri("/v1/admin/nodes/node-a/bird-config")
            .header("authorization", format!("Bearer {admin_token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(allowed).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_text = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_text.contains("pmman_network_direct"));
        assert!(!body_text.contains("node-secret"));
        assert!(!body_text.contains("node_token"));
        assert!(!body_text.contains("DATABASE_URL"));
    }

    #[tokio::test]
    async fn network_settings_survive_restart_and_partial_updates() {
        let directory = env::temp_dir().join(format!("pmman-settings-{}", Uuid::new_v4()));
        let mut state = test_state();
        state.settings_store = Some(directory.join("network-settings.json"));
        load_network_settings(&state).unwrap();
        let app = app(state.clone());
        let token = login_token(&app, "admin", "adminpw").await;
        let uri = "/v1/admin/settings/address-pool";
        let response = app
            .clone()
            .oneshot(json_post(
                uri,
                &token,
                serde_json::json!({
                    "ipv4_pool":"172.29.60.5/24", "ipv6_pool":"fdab:12::/64",
                    "internal_cidrs":["172.16.5.3/16","fdab:55::/48","172.16.0.0/16"]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let saved: NetworkSettings =
            serde_json::from_slice(&fs::read(state.settings_store.as_ref().unwrap()).unwrap())
                .unwrap();
        assert_eq!(saved.ipv4_pool, "172.29.60.0/24");
        assert_eq!(saved.internal_cidrs, vec!["172.16.0.0/16", "fdab:55::/48"]);
        let mut restarted = test_state();
        restarted.settings_store = state.settings_store.clone();
        load_network_settings(&restarted).unwrap();
        assert_eq!(pool_settings(&restarted).ipv4_pool, saved.ipv4_pool);
        assert_eq!(pool_settings(&restarted).ipv6_pool, saved.ipv6_pool);
        assert_eq!(
            pool_settings(&restarted).internal_cidrs,
            saved.internal_cidrs
        );
        // Older callers without internal_cidrs must not reset custom routes.
        let response = app
            .clone()
            .oneshot(json_post(
                uri,
                &token,
                serde_json::json!({
                    "ipv4_pool":"172.29.60.0/24", "ipv6_pool":"fdab:12::/64"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(pool_settings(&state).internal_cidrs, saved.internal_cidrs);
        let bytes = fs::read(state.settings_store.as_ref().unwrap()).unwrap();
        let response = app
            .oneshot(json_post(
                uri,
                &token,
                serde_json::json!({
                    "ipv4_pool":"172.29.60.0/24", "ipv6_pool":"fdab:12::/64", "internal_cidrs":[]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            fs::read(state.settings_store.as_ref().unwrap()).unwrap(),
            bytes
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failed_network_settings_save_keeps_memory_and_disk_unchanged() {
        let directory = env::temp_dir().join(format!("pmman-settings-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let blocked = directory.join("blocked-parent");
        fs::write(&blocked, b"not a directory").unwrap();
        let mut state = test_state();
        state.settings_store = Some(blocked.join("network-settings.json"));
        let app = app(state.clone());
        let token = login_token(&app, "admin", "adminpw").await;
        let before = pool_settings(&state);
        let response = app.oneshot(json_post("/v1/admin/settings/address-pool", &token, serde_json::json!({
            "ipv4_pool":"172.29.60.0/24", "ipv6_pool":"fdab:12::/64", "internal_cidrs":["172.16.0.0/16"]
        }))).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(pool_settings(&state).ipv4_pool, before.ipv4_pool);
        assert_eq!(pool_settings(&state).ipv6_pool, before.ipv6_pool);
        assert_eq!(pool_settings(&state).internal_cidrs, before.internal_cidrs);
        assert_eq!(fs::read(blocked).unwrap(), b"not a directory");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_network_settings_never_silently_reset_or_overwrite_storage() {
        let directory = env::temp_dir().join(format!("pmman-settings-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let mut state = test_state();
        state.settings_store = Some(directory.join("network-settings.json"));
        for invalid in [b"broken json".as_slice(), br#"{"ipv4_pool":"10.0.0.0/31","ipv6_pool":"fd00::/64","internal_cidrs":["10.0.0.0/8"]}"#.as_slice(), br#"{"ipv4_pool":"10.0.0.0/24","ipv6_pool":"fd00::/64","internal_cidrs":[]}"#.as_slice()] {
            fs::write(state.settings_store.as_ref().unwrap(), invalid).unwrap();
            assert!(load_network_settings(&state).is_err());
            assert_eq!(fs::read(state.settings_store.as_ref().unwrap()).unwrap(), invalid);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn admin_node_route_table_setting_survives_node_reregistration_and_is_synced() {
        let app = test_app();
        register_node(&app, "node-a", "node-secret").await;
        let admin = login_token(&app, "admin", "adminpw").await;
        let update = Request::builder()
            .method("POST")
            .uri("/v1/admin/nodes/node-a")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(
                r#"{"route_table":"ospf_return","enabled":true}"#,
            ))
            .unwrap();
        let response = app.clone().oneshot(update).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let updated: NodeSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(updated.route_table.as_deref(), Some("ospf_return"));
        assert!(updated.enabled);

        register_node(&app, "node-a", "node-secret").await;
        let heartbeat = Request::builder()
            .method("POST")
            .uri("/v1/nodes/node-a/heartbeat")
            .header("x-pmman-node-token", "node-secret")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(heartbeat).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let sync: NodeHeartbeatResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(sync.route_table.as_deref(), Some("ospf_return"));
    }

    #[tokio::test]
    async fn admin_generates_missing_dns_defaults_and_rejects_incomplete_custom_groups() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let admin = login_token(&app, "admin", "adminpw").await;
        let response = app
            .clone()
            .oneshot(json_post(
                "/v1/admin/nodes/node-a",
                &admin,
                serde_json::json!({"enabled":true,"network":{
                    "gateway_ipv4":"10.88.0.1/24", "dns_servers":["10.88.0.1"]
                }}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let before = node_network(state.nodes.read().unwrap().get("node-a").unwrap());
        assert_eq!(
            before.pmmandns_domestic_upstreams,
            vec!["223.5.5.5", "114.114.114.114"]
        );
        assert_eq!(
            before.pmmandns_foreign_upstreams,
            vec!["8.8.8.8", "1.1.1.1"]
        );
        let response = app
            .oneshot(json_post(
                "/v1/admin/nodes/node-a",
                &admin,
                serde_json::json!({"enabled":true,"network":{
                    "gateway_ipv4":"10.88.0.1/24", "dns_servers":["10.88.0.1"],
                    "pmmandns_domestic_upstreams":["192.0.2.53"]
                }}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(error["error"]
            .as_str()
            .unwrap()
            .contains("同时配置国内和国外上游"));
        assert_eq!(
            node_network(state.nodes.read().unwrap().get("node-a").unwrap()),
            before
        );
    }

    #[test]
    fn legacy_gateway_dns_is_migrated_persistently_without_changing_custom_upstreams() {
        let mut state = test_state();
        let directory = env::temp_dir().join(format!("pmman-dns-migration-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        state.node_store = Some(directory.join("nodes.json"));
        let mut nodes = state.nodes.read().unwrap().clone();
        let node = nodes.get_mut("node-a").unwrap();
        node.network = NodeNetworkConfig {
            gateway_ipv4: Some("10.88.0.1/24".into()),
            dns_servers: vec!["10.88.0.1".into()],
            ..Default::default()
        };
        node.network_override = Some(node.network.clone());
        persist_nodes(&state, &nodes).unwrap();
        load_node_registry(&state).unwrap();
        let once = node_network(state.nodes.read().unwrap().get("node-a").unwrap());
        assert!(!once.pmmandns_domestic_upstreams.is_empty());
        let persisted: HashMap<String, Node> =
            serde_json::from_slice(&fs::read(state.node_store.as_ref().unwrap()).unwrap()).unwrap();
        assert_eq!(persisted["node-a"].network_override.as_ref(), Some(&once));
        load_node_registry(&state).unwrap();
        assert_eq!(
            node_network(state.nodes.read().unwrap().get("node-a").unwrap()),
            once
        );
        let mut nodes = state.nodes.read().unwrap().clone();
        let node = nodes.get_mut("node-a").unwrap();
        node.network_override
            .as_mut()
            .unwrap()
            .pmmandns_foreign_upstreams = vec!["192.0.2.53".into()];
        persist_nodes(&state, &nodes).unwrap();
        load_node_registry(&state).unwrap();
        assert_eq!(
            node_network(state.nodes.read().unwrap().get("node-a").unwrap())
                .pmmandns_foreign_upstreams,
            vec!["192.0.2.53"]
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn admin_node_network_setting_survives_reregistration_and_is_returned() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let admin = login_token(&app, "admin", "adminpw").await;
        let network = serde_json::json!({
            "ipv4_split_routes": ["10.20.0.0/16"],
            "ipv6_split_routes": ["fd20:1000::/48"],
            "dns_servers": ["192.0.2.53", "2001:db8::53"],
            "gateway_ipv4": "192.0.2.1/24",
            "gateway_ipv6": "2001:db8::1/64"
        });
        let response = app
            .clone()
            .oneshot(json_post(
                "/v1/admin/nodes/node-a",
                &admin,
                serde_json::json!({
                    "enabled": true,
                    "network": network
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let summary: NodeSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            summary.network.gateway_ipv4.as_deref(),
            Some("192.0.2.1/24")
        );
        assert_eq!(
            summary.network.dns_servers,
            vec!["192.0.2.53", "2001:db8::53"]
        );

        // A node may report its old local configuration during re-registration,
        // but the administrator-owned effective configuration must remain.
        register_node(&app, "node-a", "node-secret").await;
        let node = state.nodes.read().unwrap().get("node-a").unwrap().clone();
        assert_eq!(node.network.gateway_ipv6.as_deref(), Some("2001:db8::1/64"));
        assert_eq!(
            node.network_override,
            Some(NodeNetworkConfig {
                ipv4_split_routes: Some(vec!["10.20.0.0/16".into()]),
                ipv6_split_routes: Some(vec!["fd20:1000::/48".into()]),
                dns_servers: vec!["192.0.2.53".into(), "2001:db8::53".into()],
                pmmandns_upstreams: Vec::new(),
                pmmandns_domestic_upstreams: Vec::new(),
                pmmandns_foreign_upstreams: Vec::new(),
                pmmandns_domestic_rules: Vec::new(),
                pmmandns_foreign_rules: Vec::new(),
                pmmandns_default_group: String::new(),
                pmmandns_rules: Vec::new(),
                pmmandns_geosite: None,
                pmmandns_geosite_enabled: false,
                gateway_ipv4: Some("192.0.2.1/24".into()),
                gateway_ipv6: Some("2001:db8::1/64".into()),
            })
        );
    }

    #[tokio::test]
    async fn node_settings_require_admin_and_validate_route_table() {
        let app = test_app();
        register_node(&app, "node-a", "node-secret").await;
        let user = login_token(&app, "u1", "userpw").await;
        let denied = Request::builder()
            .method("POST")
            .uri("/v1/admin/nodes/node-a")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {user}"))
            .body(Body::from(r#"{"route_table":"51820","enabled":true}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(denied).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );

        let admin = login_token(&app, "admin", "adminpw").await;
        let invalid = Request::builder()
            .method("POST")
            .uri("/v1/admin/nodes/node-a")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(
                r#"{"route_table":"51820;touch /tmp/x","enabled":true}"#,
            ))
            .unwrap();
        assert_eq!(
            app.oneshot(invalid).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn node_rate_limit_is_validated_preserved_on_partial_updates_and_visible_to_clients() {
        let state = test_state();
        let app = app(state.clone());
        register_node(&app, "node-a", "node-secret").await;
        let admin = login_token(&app, "admin", "adminpw").await;
        let user = login_token(&app, "u1", "userpw").await;

        let update = json_post(
            "/v1/admin/nodes/node-a",
            &admin,
            serde_json::json!({
                "route_table": "51820",
                "enabled": true,
                "rate_limit": {"uploadMbps": 12, "downloadMbps": 34}
            }),
        );
        let response = app.clone().oneshot(update).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let summary: NodeSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary.rate_limit.upload_mbps, 12);
        assert_eq!(summary.rate_limit.download_mbps, 34);

        // A legacy caller that only changes routing must not erase the cap.
        let route_only = json_post(
            "/v1/admin/nodes/node-a",
            &admin,
            serde_json::json!({"route_table":"51821","enabled":true}),
        );
        let response = app.clone().oneshot(route_only).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let summary: NodeSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary.rate_limit.upload_mbps, 12);
        assert_eq!(summary.rate_limit.download_mbps, 34);

        let invalid = json_post(
            "/v1/admin/nodes/node-a",
            &admin,
            serde_json::json!({
                "route_table":"51821",
                "enabled":true,
                "rate_limit": {"uploadMbps": 100001, "downloadMbps": 0}
            }),
        );
        assert_eq!(
            app.clone().oneshot(invalid).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );

        // Re-registration must retain the administrator's persisted setting.
        register_node(&app, "node-a", "node-secret").await;
        let node = state.nodes.read().unwrap().get("node-a").unwrap().clone();
        assert_eq!(node.rate_limit.upload_mbps, 12);
        assert_eq!(node.rate_limit.download_mbps, 34);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/client/nodes")
                    .header("authorization", format!("Bearer {user}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let nodes: Vec<NodeSummary> = serde_json::from_slice(&body).unwrap();
        assert_eq!(nodes[0].rate_limit.upload_mbps, 12);
        assert_eq!(nodes[0].rate_limit.download_mbps, 34);
    }

    #[tokio::test]
    async fn disabling_account_revokes_heartbeat_and_reenabling_reuses_lease() {
        let app = test_app();
        let admin = login_token(&app, "admin", "adminpw").await;
        let user = login_token(&app, "u1", "userpw").await;
        let first = register(&app, &user, "k1").await;

        let disable = Request::builder()
            .method("POST")
            .uri("/v1/admin/users/u1/status")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(r#"{"enabled":false}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(disable).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );

        let heartbeat = Request::builder()
            .method("POST")
            .uri(format!("/v1/devices/{}/heartbeat", first.device_id))
            .header("authorization", format!("Bearer {user}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(heartbeat).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let enable = Request::builder()
            .method("POST")
            .uri("/v1/admin/users/u1/status")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(r#"{"enabled":true}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(enable).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        // Disabling the account invalidates its old bearer token; a real
        // client must authenticate again after an administrator re-enables it.
        let user = login_token(&app, "u1", "userpw").await;
        let second = register(&app, &user, "k2").await;
        assert_eq!(second.device_id, first.device_id);
        assert_eq!(second.ipv4, first.ipv4);
        assert_eq!(second.ipv6, first.ipv6);
    }

    #[tokio::test]
    async fn address_pool_exhaustion_is_reported() {
        let app = test_app();
        let admin = login_token(&app, "admin", "adminpw").await;
        let update = Request::builder()
            .method("POST")
            .uri("/v1/admin/settings/address-pool")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(
                r#"{"ipv4_pool":"10.99.0.0/30","ipv6_pool":"fd99:1000::/126"}"#,
            ))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(update).await.unwrap().status(),
            StatusCode::OK
        );
        let user = login_token(&app, "u1", "userpw").await;
        assert_eq!(register(&app, &user, "k1").await.ipv4, "10.99.0.2/32");
        let admin_user = login_token(&app, "admin", "adminpw").await;
        let create = Request::builder()
            .method("POST")
            .uri("/v1/admin/users")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {admin_user}"))
            .body(Body::from(
                r#"{"username":"full-pool","password":"fullpass","role":"user"}"#,
            ))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(create).await.unwrap().status(),
            StatusCode::OK
        );
        let full_user = login_token(&app, "full-pool", "fullpass").await;
        let request = Request::builder()
            .method("POST")
            .uri("/v1/devices/register")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {full_user}"))
            .body(Body::from(r#"{"public_key":"k3"}"#))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn admin_can_assign_reuse_and_change_a_fixed_address_lease() {
        let state = test_state();
        let app = app(state.clone());
        let admin = login_token(&app, "admin", "adminpw").await;
        let fixed = json_post(
            "/v1/admin/users/u1/address-lease",
            &admin,
            serde_json::json!({
                "address_mode":"fixed",
                "reserved_ipv4":"10.88.0.20",
                "reserved_ipv6":"fd88:1000::14"
            }),
        );
        let response = app.clone().oneshot(fixed).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let summary: UserSummary = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(summary.address_mode, "fixed");
        assert_eq!(summary.reserved_ipv4.as_deref(), Some("10.88.0.20"));
        assert_eq!(pool_settings(&state).allocated, 1);

        let user = login_token(&app, "u1", "userpw").await;
        let first = register(&app, &user, "fixed-key-1").await;
        assert_eq!(first.ipv4, "10.88.0.20/32");
        assert_eq!(first.ipv6, "fd88:1000::14/128");

        let create = json_post(
            "/v1/admin/users",
            &admin,
            serde_json::json!({"username":"automatic-user","password":"automatic-pass","role":"user"}),
        );
        assert_eq!(
            app.clone().oneshot(create).await.unwrap().status(),
            StatusCode::OK
        );
        let automatic = login_token(&app, "automatic-user", "automatic-pass").await;
        let automatic_config = register(&app, &automatic, "automatic-key").await;
        assert_eq!(automatic_config.ipv4, "10.88.0.10/32");

        let online_change = json_post(
            "/v1/admin/users/u1/address-lease",
            &admin,
            serde_json::json!({
                "address_mode":"fixed",
                "reserved_ipv4":"10.88.0.21",
                "reserved_ipv6":"fd88:1000::15"
            }),
        );
        assert_eq!(
            app.clone().oneshot(online_change).await.unwrap().status(),
            StatusCode::CONFLICT
        );

        let revoke = json_post(
            &format!("/v1/devices/{}/revoke", first.device_id),
            &user,
            serde_json::json!({"public_key":"fixed-key-1"}),
        );
        assert_eq!(
            app.clone().oneshot(revoke).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let offline_change = json_post(
            "/v1/admin/users/u1/address-lease",
            &admin,
            serde_json::json!({
                "address_mode":"fixed",
                "reserved_ipv4":"10.88.0.21",
                "reserved_ipv6":"fd88:1000::15"
            }),
        );
        assert_eq!(
            app.clone().oneshot(offline_change).await.unwrap().status(),
            StatusCode::OK
        );
        let second = register(&app, &user, "fixed-key-2").await;
        assert_eq!(second.device_id, first.device_id);
        assert_eq!(second.ipv4, "10.88.0.21/32");
        assert_eq!(second.ipv6, "fd88:1000::15/128");
        assert_eq!(second.config_version, first.config_version + 2);
    }

    #[tokio::test]
    async fn fixed_lease_rejects_reserved_addresses_conflicts_and_pool_shrink() {
        let app = test_app();
        let admin = login_token(&app, "admin", "adminpw").await;
        for (ipv4, ipv6) in [("10.88.0.0", "fd88:1000::"), ("10.88.0.1", "fd88:1000::1")] {
            let response = app
                .clone()
                .oneshot(json_post(
                    "/v1/admin/users/u1/address-lease",
                    &admin,
                    serde_json::json!({"address_mode":"fixed","reserved_ipv4":ipv4,"reserved_ipv6":ipv6}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        let mismatch = app
            .clone()
            .oneshot(json_post(
                "/v1/admin/users/u1/address-lease",
                &admin,
                serde_json::json!({"address_mode":"fixed","reserved_ipv4":"10.88.0.20","reserved_ipv6":"fd89:1000::15"}),
            ))
            .await
            .unwrap();
        assert_eq!(mismatch.status(), StatusCode::OK);

        let first = app
            .clone()
            .oneshot(json_post(
                "/v1/admin/users/u1/address-lease",
                &admin,
                serde_json::json!({"address_mode":"fixed","reserved_ipv4":"10.88.0.20","reserved_ipv6":"fd88:1000::14"}),
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let create = json_post(
            "/v1/admin/users",
            &admin,
            serde_json::json!({"username":"conflict-user","password":"conflict-pass","role":"user"}),
        );
        assert_eq!(
            app.clone().oneshot(create).await.unwrap().status(),
            StatusCode::OK
        );
        let conflict = app.clone()
            .oneshot(json_post(
                "/v1/admin/users/conflict-user/address-lease",
                &admin,
                serde_json::json!({"address_mode":"fixed","reserved_ipv4":"10.88.0.20","reserved_ipv6":"fd88:1000::14"}),
            ))
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);

        let create = json_post(
            "/v1/admin/users",
            &admin,
            serde_json::json!({"username":"outside-user","password":"outside-pass","role":"user"}),
        );
        assert_eq!(
            app.clone().oneshot(create).await.unwrap().status(),
            StatusCode::OK
        );
        let outside = json_post(
            "/v1/admin/users/outside-user/address-lease",
            &admin,
            serde_json::json!({"address_mode":"fixed","reserved_ipv4":"192.0.2.20","reserved_ipv6":"2001:db8::20"}),
        );
        assert_eq!(
            app.clone().oneshot(outside).await.unwrap().status(),
            StatusCode::OK
        );
        let outside_token = login_token(&app, "outside-user", "outside-pass").await;
        let outside_device = register(&app, &outside_token, "outside-key").await;
        assert_eq!(outside_device.ipv4, "192.0.2.20/32");
        assert_eq!(outside_device.ipv6, "2001:db8::20/128");
        let pool_update = json_post(
            "/v1/admin/settings/address-pool",
            &admin,
            serde_json::json!({"ipv4_pool":"10.99.0.0/30","ipv6_pool":"fd99:1000::/126"}),
        );
        assert_eq!(
            app.clone().oneshot(pool_update).await.unwrap().status(),
            StatusCode::OK
        );

        let shrink = test_app();
        let admin = login_token(&shrink, "admin", "adminpw").await;
        let user = login_token(&shrink, "u1", "userpw").await;
        let automatic = register(&shrink, &user, "automatic-key").await;
        let revoke = json_post(
            &format!("/v1/devices/{}/revoke", automatic.device_id),
            &user,
            serde_json::json!({"public_key":"automatic-key"}),
        );
        assert_eq!(
            shrink.clone().oneshot(revoke).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let pool = json_post(
            "/v1/admin/settings/address-pool",
            &admin,
            serde_json::json!({"ipv4_pool":"10.99.0.0/30","ipv6_pool":"fd99:1000::/126"}),
        );
        assert_eq!(
            shrink.clone().oneshot(pool).await.unwrap().status(),
            StatusCode::OK
        );
        let moved = register(&shrink, &user, "automatic-key-2").await;
        assert_eq!(moved.ipv4, "10.99.0.2/32");
    }

    #[tokio::test]
    async fn address_lease_and_automatic_assignment_survive_user_store_reload() {
        let directory = env::temp_dir().join(format!("pmman-address-lease-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let user_store = directory.join("users.json");
        let mut state = test_state();
        state.user_store = Some(user_store.clone());
        persist_users(&state, &state.users.read().unwrap()).unwrap();
        let app = app(state.clone());
        let admin = login_token(&app, "admin", "adminpw").await;
        let user = login_token(&app, "u1", "userpw").await;
        let assigned = register(&app, &user, "persisted-key").await;
        let fixed = json_post(
            "/v1/admin/users/u1/address-lease",
            &admin,
            serde_json::json!({"address_mode":"fixed","reserved_ipv4":"10.88.0.20","reserved_ipv6":"fd88:1000::14"}),
        );
        assert_eq!(
            app.clone().oneshot(fixed).await.unwrap().status(),
            StatusCode::CONFLICT
        );
        // The client is active, so first revoke it; the automatic lease is
        // then converted to a fixed reservation and written atomically.
        let revoke = json_post(
            &format!("/v1/devices/{}/revoke", assigned.device_id),
            &user,
            serde_json::json!({"public_key":"persisted-key"}),
        );
        assert_eq!(
            app.clone().oneshot(revoke).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        let fixed = json_post(
            "/v1/admin/users/u1/address-lease",
            &admin,
            serde_json::json!({"address_mode":"fixed","reserved_ipv4":"10.88.0.20","reserved_ipv6":"fd88:1000::14"}),
        );
        assert_eq!(
            app.clone().oneshot(fixed).await.unwrap().status(),
            StatusCode::OK
        );
        let stored: HashMap<String, UserAccount> =
            serde_json::from_slice(&fs::read(&user_store).unwrap()).unwrap();
        assert_eq!(stored["u1"].reserved_ipv4.as_deref(), Some("10.88.0.20"));
        state.users.write().unwrap().clear();
        load_user_registry(&state).unwrap();
        assert_eq!(state.users.read().unwrap()["u1"].address_mode, "fixed");
        assert_eq!(
            state.users.read().unwrap()["u1"].leased_ipv4.as_deref(),
            Some("10.88.0.20")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn allocator_excludes_reserved_addresses_even_at_exhaustion() {
        for (v4, v6, count) in [
            ("10.88.0.5/24", "fd88:1000::/64", 253),
            ("10.99.0.0/30", "fd99::/126", 1),
            ("0.0.0.0/0", "::/0", 4_294_967_293),
        ] {
            let pool = AddressPool::from_cidrs(v4, v6).unwrap();
            assert_eq!(pool.capacity(), count);
            let mut used = HashSet::new();
            for offset in [
                0,
                1,
                pool.total_addresses() - 1,
                pool.total_addresses(),
                u64::MAX,
            ] {
                assert!(pool.address_at(offset, &used).is_none());
            }
            assert!(!pool.contains(
                &Ipv4Addr::from(pool.ipv4_network).to_string(),
                &Ipv6Addr::from(pool.ipv6_network).to_string()
            ));
            if count > 1000 {
                continue;
            }
            while let Some((ipv4, ipv6)) = pool.allocate(&used) {
                assert!(pool.contains(&ipv4, &ipv6));
                assert!(used.insert(u32::from(ipv4.parse::<Ipv4Addr>().unwrap())));
            }
            assert_eq!(used.len() as u64, count);
            assert!(!used.contains(&pool.ipv4_network));
            assert!(!used.contains(&(pool.ipv4_network + 1)));
            assert!(!used.contains(&(pool.ipv4_network + pool.total_addresses() as u32 - 1)));
        }
        assert!(AddressPool::from_cidrs("10.0.0.0/31", "fd00::/64").is_err());
        assert!(AddressPool::from_cidrs("10.0.0.1/32", "fd00::/64").is_err());
        assert!(AddressPool::from_cidrs("10.0.0.0/24", "fd00::/126").is_err());
        let pool = AddressPool::from_cidrs("10.0.0.0/24", "fd00::/64").unwrap();
        for (v4, v6) in [
            ("10.0.0.0", "fd00::"),
            ("10.0.0.1", "fd00::1"),
            ("10.0.0.255", "fd00::ff"),
        ] {
            assert!(!pool.contains(v4, v6));
        }
        assert!(pool.contains("10.0.0.2", "fd00::3"));
    }

    fn json_post(path: &str, token: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn onboarding_requires_admin_and_binds_credentials_to_one_node() {
        let state = test_state();
        let app = app(state.clone());
        let user = login_token(&app, "u1", "userpw").await;
        let admin = login_token(&app, "admin", "adminpw").await;
        let body =
            serde_json::json!({"node_id":"new-node", "name":"New node", "route_table":"51832"});
        assert_eq!(
            app.clone()
                .oneshot(json_post("/v1/admin/nodes", &user, body.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = app
            .clone()
            .oneshot(json_post("/v1/admin/nodes", &admin, body.clone()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let enrolled: NodeEnrollmentResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(!enrolled.node.registered && !enrolled.node.healthy);
        assert_eq!(enrolled.node_token.len(), 64);
        assert_eq!(
            app.clone()
                .oneshot(json_post("/v1/admin/nodes", &admin, body))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let register = |id: &str, token: &str| {
            Request::builder().method("POST").uri("/v1/nodes/register")
            .header("content-type", "application/json").header("x-pmman-node-token", token)
            .body(Body::from(serde_json::json!({"node_id":id,"name":"Ignored", "probe_endpoint":"node.example.com:51840", "wireguard_endpoint":"node.example.com:51820", "server_public_key":"Egh8MitVQY/209St4o0Jg5/V7XtmNOWM/3B1zAVdPXQ="}).to_string())).unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(register("other-node", &enrolled.node_token))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(register("new-node", "wrong-token"))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(register("new-node", &enrolled.node_token))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let node = state.nodes.read().unwrap().get("new-node").unwrap().clone();
        assert!(node.registered && !node_is_healthy(&node));
        let heartbeat = Request::builder()
            .method("POST")
            .uri("/v1/nodes/new-node/heartbeat")
            .header("x-pmman-node-token", &enrolled.node_token)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(heartbeat).await.unwrap().status(),
            StatusCode::OK
        );
        assert!(node_is_healthy(
            state.nodes.read().unwrap().get("new-node").unwrap()
        ));
        assert_eq!(node.name, "New node");
        assert_eq!(node.route_table.as_deref(), Some("51832"));
        let response = app
            .clone()
            .oneshot(json_post(
                "/v1/admin/nodes/new-node/cancel",
                &admin,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/admin/nodes")
                    .header("authorization", format!("Bearer {admin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!String::from_utf8(bytes.to_vec())
            .unwrap()
            .contains(&enrolled.node_token));
        assert_eq!(
            app.clone()
                .oneshot(json_post(
                    "/v1/admin/nodes/node-a/cancel",
                    &admin,
                    serde_json::json!({})
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            app.oneshot(register("node-a", "node-secret"))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn onboarding_validation_and_storage_survive_restart() {
        assert!(!valid_node_id("../node"));
        assert!(valid_wireguard_endpoint("[2001:db8::1]:51820"));
        assert!(!valid_wireguard_endpoint("host:65536"));
        assert!(!valid_wireguard_key(&"中".repeat(14)));
        let mut state = test_state();
        let directory = env::temp_dir().join(format!("pmman-node-test-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("nodes.json");
        state.node_store = Some(path.clone());
        persist_nodes(&state, &state.nodes.read().unwrap()).unwrap();
        state.nodes.write().unwrap().clear();
        load_node_registry(&state).unwrap();
        let nodes = state.nodes.read().unwrap();
        let pending = nodes.get("node-a").unwrap();
        assert_eq!(pending.node_token, "node-secret");
        assert!(!node_is_healthy(pending));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn reserved_revoked_lease_is_reallocated_and_capacity_matches() {
        let state = test_state();
        let app = app(state.clone());
        let user = login_token(&app, "u1", "userpw").await;
        let first = register(&app, &user, "k1").await;
        {
            let mut devices = state.devices.write().unwrap();
            let lease = devices.get_mut(&first.device_id).unwrap();
            lease.ipv4 = "10.88.0.255".into();
            lease.ipv6 = "fd88:1000::ff".into();
            lease.status = "revoked".into();
        }
        let next = register(&app, &user, "k2").await;
        assert_eq!(next.device_id, first.device_id);
        assert_eq!(next.ipv4, "10.88.0.10/32");
        let metrics = pool_settings(&state);
        assert_eq!(
            (metrics.capacity, metrics.allocated, metrics.available),
            (253, 1, 252)
        );
    }

    #[tokio::test]
    async fn p2p_directory_binds_owner_key_and_revokes_disabled_accounts() {
        use pmman_protocol::p2p::{Directory, RelayPeer};
        let state = test_state();
        {
            let mut nodes = state.nodes.write().unwrap();
            let node = nodes.get_mut("node-a").unwrap();
            node.registered = true;
            node.last_seen_at = Utc::now();
            // Production nodes advertise DNS names rather than literal IPs.
            // The P2P directory must resolve this before serializing its
            // concrete relay SocketAddr.
            node.probe_endpoint = "localhost:51840".into();
        }
        let app = app(state.clone());
        let a = login_token(&app, "u1", "userpw").await;
        let b = login_token(&app, "admin", "adminpw").await;
        let da = register(&app, &a, "a-key").await;
        let db = register(&app, &b, "b-key").await;
        let ad = |id, key, enabled| serde_json::json!({"public_key":key,"enabled":enabled,"candidates":["192.0.2.20:3000"],"ignored_id":id});
        let uri = |id| format!("/v1/devices/{id}/p2p");
        assert_eq!(
            app.clone()
                .oneshot(json_post(
                    &uri(da.device_id),
                    &b,
                    ad(da.device_id, "a-key", true)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.clone()
                .oneshot(json_post(
                    &uri(da.device_id),
                    &a,
                    ad(da.device_id, "wrong", true)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        let response = app
            .clone()
            .oneshot(json_post(
                &uri(da.device_id),
                &a,
                ad(da.device_id, "a-key", true),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let first: Directory = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(first.peers.is_empty());
        assert_eq!(first.relay_endpoint.port(), 51840);
        assert!(first.relay_endpoint.ip().is_ipv4());
        let response = app
            .clone()
            .oneshot(json_post(
                &uri(db.device_id),
                &b,
                ad(db.device_id, "b-key", false),
            ))
            .await
            .unwrap();
        let second: Directory = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(second.peers.len(), 1);
        let response = app
            .clone()
            .oneshot(json_post(
                &uri(da.device_id),
                &a,
                ad(da.device_id, "a-key", true),
            ))
            .await
            .unwrap();
        let third: Directory = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(third.peers[0].secret, second.peers[0].secret);
        assert!(!third.peers[0].enabled);
        let leases: Vec<RelayPeer> = p2p::relay_peers(&state);
        assert_eq!(leases.len(), 2);
        state
            .users
            .write()
            .unwrap()
            .get_mut("admin")
            .unwrap()
            .enabled = false;
        assert_eq!(p2p::relay_peers(&state).len(), 1);
        state
            .devices
            .write()
            .unwrap()
            .get_mut(&da.device_id)
            .unwrap()
            .public_key = "rotated".into();
        assert!(p2p::relay_peers(&state).is_empty());
    }
}
