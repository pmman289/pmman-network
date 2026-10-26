use pmman_protocol::p2p::{RelayPeer, RelayRegistry, MAX_DATAGRAM};
use pmman_protocol::{
    AppliedPortForward, NodeNetworkConfig, NodePortForward, PortForwardSyncError, UDP_PING_MAGIC,
    UDP_PING_SIZE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Read,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MASTER_AUTH_LEASE: Duration = Duration::from_secs(30);
const KERNEL_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const PMMAN_RETURN_RULE_PRIORITY: &str = "10000";

#[path = "../node_forward_routing.rs"]
mod node_forward_routing;

#[derive(Default)]
struct AuthorizationState {
    last_heartbeat: Option<Instant>,
    report: NodeHeartbeatRequest,
    // Retain orphan routes even after their kernel peer has been removed.
    pending_cleanup: HashMap<String, PeerState>,
}

impl AuthorizationState {
    fn expired(&self, now: Instant) -> bool {
        self.last_heartbeat
            .is_none_or(|seen| now.saturating_duration_since(seen) >= MASTER_AUTH_LEASE)
    }
}

#[derive(Clone)]
struct NodeConfig {
    node_id: String,
    name: String,
    probe_endpoint: String,
    wireguard_endpoint: String,
    region: Option<String>,
    master_url: String,
    node_token: String,
    probe_listen: SocketAddr,
    interface: String,
    private_key_path: String,
    ipv4_address: Option<String>,
    ipv6_address: Option<String>,
    listen_port: u16,
    server_public_key: String,
    wg_binary: String,
    nft_binary: String,
    // Used only on a reconciliation-local clone, never persisted or advertised.
    command_deadline: Option<Instant>,
    route_table: Option<String>,
    ip_binary: String,
    ipv4_split_routes_file: Option<String>,
    ipv6_split_routes_file: Option<String>,
    dns_ipv4: Option<String>,
    dns_ipv6: Option<String>,
    pmmandns_upstreams: Vec<String>,
    pmmandns_domestic_upstreams: Vec<String>,
    pmmandns_foreign_upstreams: Vec<String>,
    pmmandns_domestic_rules_file: Option<String>,
    pmmandns_foreign_rules_file: Option<String>,
    pmmandns_default_group: String,
    release_version: Option<String>,
    binary_path: String,
    version_path: String,
    geosite_cache_path: PathBuf,
    service_name: String,
}

#[derive(Clone)]
struct NodeState {
    config: Arc<RwLock<NodeConfig>>,
    peers: Arc<RwLock<HashMap<Uuid, PeerState>>>,
    pmmandns: Arc<PmmanDnsRuntime>,
    // Serialize renewal, reconciliation and expiry, including all kernel writes.
    authorization: Arc<Mutex<AuthorizationState>>,
}

#[derive(Clone)]
struct PeerState {
    public_key: String,
    ipv4: String,
    ipv6: String,
    config_version: u64,
}

/// The node delegates DNS classification and forwarding to the current Go
/// pmmandns build.  Keeping it as a child process makes the OpenWrt/mrouter
/// implementation the single DNS behavior used in production while the node
/// still owns its lifecycle and receives configuration from the master.
struct PmmanDnsRuntime {
    process: Mutex<Option<std::process::Child>>,
    geosite: Mutex<Option<pmman_protocol::GeositeConfig>>,
    config_hash: Mutex<Option<String>>,
    last_sync_error: Mutex<Option<String>>,
}

#[derive(Serialize)]
struct PmmanDnsConfig {
    listen: Vec<String>,
    domestic_upstreams: Vec<String>,
    foreign_upstreams: Vec<String>,
    domestic_rules: Vec<String>,
    foreign_rules: Vec<String>,
    geosite_file: String,
    geosite_domestic: String,
    geosite_foreign: String,
    default_group: String,
    timeout_ms: u32,
    cache_size: u32,
    max_ttl: u32,
    negative_ttl: u32,
    #[serde(rename = "pmman_generation")]
    generation: String,
}

const EMBEDDED_PMMANDNS: &[u8] = include_bytes!("../../../vendor/pmmandns/pmmandns-linux-amd64");

impl Default for PmmanDnsRuntime {
    fn default() -> Self {
        Self {
            process: Mutex::new(None),
            geosite: Mutex::new(None),
            config_hash: Mutex::new(None),
            last_sync_error: Mutex::new(None),
        }
    }
}

impl PmmanDnsRuntime {
    fn default_config_directory(config: &NodeConfig) -> PathBuf {
        PathBuf::from(&config.version_path)
            .parent()
            .map(|parent| parent.join("pmmandns-config"))
            .unwrap_or_else(|| PathBuf::from("/var/lib/pmman-node/pmmandns-config"))
    }

    fn new(_upstreams: Vec<String>) -> Result<Self, String> {
        Ok(Self::default())
    }

    fn stop(&self) {
        let mut process = self.process.lock().unwrap();
        if let Some(mut child) = process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        *self.config_hash.lock().unwrap() = None;
    }

    fn ensure_binary(&self, config: &NodeConfig) -> Result<PathBuf, String> {
        let path = PathBuf::from(env::var("PMMAN_PMMANDNS_BINARY").unwrap_or_else(|_| {
            PathBuf::from(&config.version_path)
                .parent()
                .map(|parent| parent.join("pmmandns-bin"))
                .unwrap_or_else(|| PathBuf::from("/var/lib/pmman-node/pmmandns-bin"))
                .to_string_lossy()
                .into_owned()
        }));
        let expected = sha256_hex(EMBEDDED_PMMANDNS);
        let current = fs::read(&path).ok().map(|bytes| sha256_hex(&bytes));
        if current.as_deref() != Some(expected.as_str()) {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("无法创建 pmmandns 目录：{error}"))?;
            }
            let temporary = path.with_extension("new");
            fs::write(&temporary, EMBEDDED_PMMANDNS)
                .map_err(|error| format!("无法写入 pmmandns：{error}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))
                    .map_err(|error| format!("无法设置 pmmandns 权限：{error}"))?;
            }
            fs::rename(&temporary, &path).map_err(|error| format!("无法激活 pmmandns：{error}"))?;
        }
        if !path.is_file() {
            return Err(format!("pmmandns 二进制不可用：{}", path.display()));
        }
        let _ = config;
        Ok(path)
    }

    fn update(
        &self,
        config: &NodeConfig,
        network: &NodeNetworkConfig,
        _peers: &[ActiveNodePeer],
    ) -> Result<(), String> {
        network.validate_managed_dns()?;
        if !network.managed_dns_enabled() {
            self.stop();
            return Ok(());
        }
        let gateway_ips: HashSet<_> = [
            config.ipv4_address.as_deref(),
            config.ipv6_address.as_deref(),
        ]
        .into_iter()
        .filter_map(|v| v.and_then(host_address))
        .collect();
        let mut listen = Vec::new();
        for value in &network.dns_servers {
            let ip = value
                .parse::<std::net::IpAddr>()
                .map_err(|_| "pmmandns 监听地址无效".to_string())?;
            if !gateway_ips.contains(&ip) {
                return Err("pmmandns 监听地址必须是节点 WireGuard 网关地址".into());
            }
            let address = SocketAddr::new(ip, 53);
            let value = if ip.is_ipv6() {
                format!("[{ip}]:53")
            } else {
                address.to_string()
            };
            if !listen.contains(&value) {
                listen.push(value);
            }
        }
        if listen.is_empty() {
            return Err("pmmandns 需要至少一个节点 WireGuard 网关地址".into());
        }
        // A WireGuard interface can lose its addresses when it is quarantined
        // during lease reconciliation. Re-apply them before binding the Go
        // daemon so an IPv6 listener never races the interface recovery.
        ensure_wireguard_addresses(config)?;

        let legacy = network.pmmandns_upstreams.clone();
        let domestic_upstreams = if network.pmmandns_domestic_upstreams.is_empty() {
            legacy.clone()
        } else {
            network.pmmandns_domestic_upstreams.clone()
        };
        let foreign_upstreams = if network.pmmandns_foreign_upstreams.is_empty() {
            legacy
        } else {
            network.pmmandns_foreign_upstreams.clone()
        };
        if domestic_upstreams.is_empty() || foreign_upstreams.is_empty() {
            return Err("pmmandns 必须同时配置国内和国外 DNS 上游".into());
        }
        for upstream in domestic_upstreams.iter().chain(foreign_upstreams.iter()) {
            let endpoint = pmman_protocol::dns::endpoint(upstream)
                .map_err(|_| format!("pmmandns 上游地址无效：{upstream}"))?;
            if gateway_ips.contains(&endpoint.ip()) {
                return Err("pmmandns 上游不能指向节点自身监听地址".into());
            }
        }

        let mut domestic_rules = network.pmmandns_domestic_rules.clone();
        let mut foreign_rules = network.pmmandns_foreign_rules.clone();
        for rule in &network.pmmandns_rules {
            let same_as = |group: &[String]| {
                !rule.upstreams.is_empty()
                    && rule.upstreams.len() == group.len()
                    && rule.upstreams.iter().all(|value| group.contains(value))
            };
            if same_as(&domestic_upstreams) {
                domestic_rules.push(rule.suffix.clone());
            } else if same_as(&foreign_upstreams) {
                foreign_rules.push(rule.suffix.clone());
            } else {
                return Err(format!(
                    "pmmandns 规则 {} 的上游必须归属国内或国外 DNS 组",
                    rule.suffix
                ));
            }
        }

        let (geosite_file, geosite_domestic, geosite_foreign, geosite_version) = self
            .geosite
            .lock()
            .unwrap()
            .as_ref()
            .map(|spec| {
                (
                    config.geosite_cache_path.to_string_lossy().into_owned(),
                    spec.domestic.clone(),
                    spec.foreign.clone(),
                    spec.version.clone(),
                )
            })
            .unwrap_or_default();
        if network.pmmandns_geosite.is_some() && geosite_file.is_empty() {
            return Err("geosite 尚未加载".into());
        }
        let directory = PathBuf::from(env::var("PMMAN_PMMANDNS_CONFIG_DIR").unwrap_or_else(|_| {
            Self::default_config_directory(config)
                .to_string_lossy()
                .into_owned()
        }));
        let domestic_rules_path = directory.join("domestic.txt");
        let foreign_rules_path = directory.join("foreign.txt");
        let default_group = match network
            .pmmandns_default_group
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "domestic" => "domestic",
            _ => "foreign",
        };
        let payload = PmmanDnsConfig {
            listen,
            domestic_upstreams,
            foreign_upstreams,
            domestic_rules: vec![domestic_rules_path.to_string_lossy().into_owned()],
            foreign_rules: vec![foreign_rules_path.to_string_lossy().into_owned()],
            geosite_file,
            geosite_domestic,
            geosite_foreign,
            default_group: default_group.into(),
            timeout_ms: 1200,
            cache_size: 4096,
            max_ttl: 86400,
            negative_ttl: 30,
            generation: geosite_version,
        };
        let bytes = serde_json::to_vec_pretty(&payload).map_err(|error| error.to_string())?;
        // The JSON contains rule-file paths, not their contents. Include both
        // generated rule files in the generation key so editing a master rule
        // actually restarts pmmandns instead of leaving the old matcher alive.
        let mut generation = bytes.clone();
        generation.extend_from_slice(domestic_rules.join("\n").as_bytes());
        generation.extend_from_slice(foreign_rules.join("\n").as_bytes());
        let digest = sha256_hex(&generation);
        if *self.config_hash.lock().unwrap() == Some(digest.clone()) {
            let mut process = self.process.lock().unwrap();
            if let Some(child) = process.as_mut() {
                if child
                    .try_wait()
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    return Ok(());
                }
            }
        }
        let binary = self.ensure_binary(config)?;
        fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建 pmmandns 配置目录：{error}"))?;
        let config_path = directory.join("config.json");
        let temporary = config_path.with_extension("new");
        fs::write(&temporary, &bytes)
            .map_err(|error| format!("无法写入 pmmandns 配置：{error}"))?;
        fs::rename(&temporary, &config_path)
            .map_err(|error| format!("无法激活 pmmandns 配置：{error}"))?;
        fs::write(&domestic_rules_path, domestic_rules.join("\n") + "\n")
            .map_err(|error| format!("无法写入 pmmandns 国内规则：{error}"))?;
        fs::write(&foreign_rules_path, foreign_rules.join("\n") + "\n")
            .map_err(|error| format!("无法写入 pmmandns 国外规则：{error}"))?;

        let mut process = self.process.lock().unwrap();
        if let Some(mut child) = process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let child = Command::new(&binary)
            .arg("-config")
            .arg(&config_path)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法启动 pmmandns：{error}"))?;
        *process = Some(child);
        *self.config_hash.lock().unwrap() = Some(digest);
        std::thread::sleep(Duration::from_millis(80));
        if let Some(child) = process.as_mut() {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                *self.config_hash.lock().unwrap() = None;
                return Err(format!("pmmandns 启动失败：{status}"));
            }
        }
        println!("pmmandns Go runtime configuration applied");
        Ok(())
    }

    fn prepare_geosite(
        &self,
        config: &NodeConfig,
        spec: Option<&pmman_protocol::GeositeConfig>,
    ) -> Result<(), String> {
        let Some(spec) = spec else {
            *self.geosite.lock().unwrap() = None;
            return Ok(());
        };
        if self
            .geosite
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|loaded| loaded == spec)
        {
            return Ok(());
        }
        if spec.size == 0 || spec.size > pmman_protocol::geosite::MAX_FILE_SIZE as u64 {
            return Err("geosite 文件大小无效".into());
        }
        let bytes = fs::read(&config.geosite_cache_path)
            .ok()
            .filter(|bytes| bytes.len() as u64 == spec.size)
            .filter(|bytes| sha256_hex(bytes) == spec.sha256.to_ascii_lowercase())
            .or_else(|| {
                let response = master_agent()
                    .get(&format!(
                        "{}/v1/nodes/{}/geosite",
                        config.master_url, config.node_id
                    ))
                    .query("version", &spec.version)
                    .header("X-PMMAN-NODE-TOKEN", &config.node_token)
                    .call()
                    .ok()?;
                let bytes = response
                    .into_body()
                    .with_config()
                    .limit(pmman_protocol::geosite::MAX_FILE_SIZE as u64)
                    .read_to_vec()
                    .ok()?;
                (bytes.len() as u64 == spec.size
                    && sha256_hex(&bytes) == spec.sha256.to_ascii_lowercase())
                .then_some(bytes)
            })
            .ok_or_else(|| "geosite 文件下载或校验失败".to_string())?;
        if let Some(parent) = config.geosite_cache_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建 geosite 缓存目录：{error}"))?;
        }
        let temporary = config.geosite_cache_path.with_extension("new");
        fs::write(&temporary, &bytes).map_err(|error| format!("无法写入 geosite 缓存：{error}"))?;
        fs::rename(&temporary, &config.geosite_cache_path)
            .map_err(|error| format!("无法替换 geosite 缓存：{error}"))?;
        *self.geosite.lock().unwrap() = Some(spec.clone());
        println!("geosite {} loaded for Go pmmandns", spec.version);
        Ok(())
    }
}

impl Drop for PmmanDnsRuntime {
    fn drop(&mut self) {
        if let Ok(process) = self.process.get_mut() {
            if let Some(mut child) = process.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn host_address(value: &str) -> Option<std::net::IpAddr> {
    value.split('/').next()?.parse().ok()
}

fn env_list(name: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_default()
        .split([',', ';', ' ', '\n', '\r', '\t'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn read_domain_rules(path: &str) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("无法读取 pmmandns 域名规则文件 {path}: {error}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}
#[derive(Serialize)]
struct NodeRegistration<'a> {
    node_id: &'a str,
    name: &'a str,
    probe_endpoint: &'a str,
    network: NodeNetworkConfig,
    wireguard_endpoint: &'a str,
    region: &'a Option<String>,
    server_public_key: &'a str,
    route_table: &'a Option<String>,
    wireguard_interface: &'a str,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeRelease {
    version: String,
    _file_name: String,
    sha256: String,
    size: u64,
    published_at: String,
}

#[derive(Deserialize, Serialize)]
struct UpdateRetryState {
    version: String,
    attempts: u32,
    next_retry_unix: i64,
}

#[derive(Deserialize)]
struct NodeHeartbeatResponse {
    #[serde(default)]
    relay_peers: Vec<RelayPeer>,
    active: Vec<ActiveNodePeer>,
    route_table: Option<String>,
    #[serde(default)]
    network: NodeNetworkConfig,
    #[serde(default)]
    release: Option<NodeRelease>,
    #[serde(default)]
    geosite: Option<pmman_protocol::GeositeConfig>,
    #[serde(default)]
    port_forwards: Vec<NodePortForward>,
    #[serde(default)]
    revoked_forward_ids: Vec<String>,
}
#[derive(Deserialize)]
struct ActiveNodePeer {
    device_id: Uuid,
    public_key: String,
    ipv4: String,
    ipv6: String,
    config_version: u64,
}
#[derive(Clone, Serialize, Default)]
struct NodeHeartbeatRequest {
    applied: Vec<AppliedPeer>,
    errors: Vec<SyncError>,
    route_table: Option<String>,
    cleanup_ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_version: Option<String>,
    #[serde(default)]
    applied_port_forwards: Vec<AppliedPortForward>,
    #[serde(default)]
    port_forward_errors: Vec<PortForwardSyncError>,
    #[serde(default)]
    released_forward_ids: Vec<String>,
}
#[derive(Clone, Serialize)]
struct AppliedPeer {
    device_id: Uuid,
    public_key: String,
    config_version: u64,
}
#[derive(Clone, Serialize)]
struct SyncError {
    device_id: Uuid,
    public_key: String,
    config_version: u64,
    error: String,
}

fn env_required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("missing required environment variable {name}"))
}

fn validate_master_url(value: &str) -> Result<(), String> {
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = value.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err(
            "PMMAN_MASTER_URL must use https://; http:// is only allowed for loopback development"
                .into(),
        );
    };
    if value.chars().any(|character| character.is_whitespace()) || rest.is_empty() {
        return Err("PMMAN_MASTER_URL has an invalid format".into());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("PMMAN_MASTER_URL must not contain credentials or an empty host".into());
    }
    if scheme == "https" {
        return Ok(());
    }
    let host = if let Some(host) = authority.strip_prefix('[') {
        host.split(']').next().unwrap_or_default()
    } else {
        authority
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(authority)
    };
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false);
    if loopback {
        Ok(())
    } else {
        Err("PMMAN_MASTER_URL must use HTTPS outside localhost/loopback development".into())
    }
}

fn validate_route_table(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!("invalid PMMAN_ROUTE_TABLE value: {value}"));
    }
    Ok(value.to_owned())
}

fn host_route(value: &str, prefix: u8) -> Result<String, String> {
    // Accept only one family for each route.  Older persisted heartbeat
    // snapshots could contain both serialized host routes in one whitespace
    // field; selecting the family token here keeps cleanup idempotent while
    // the next authenticated snapshot replaces the stale value.
    let value = value
        .split_whitespace()
        .find(|candidate| {
            let address = candidate.split('/').next().unwrap_or_default();
            if prefix == 32 {
                address.parse::<Ipv4Addr>().is_ok()
            } else {
                address.parse::<Ipv6Addr>().is_ok()
            }
        })
        .unwrap_or(value);
    let (address, supplied_prefix) = value
        .split_once('/')
        .map_or((value, None), |(address, prefix)| (address, Some(prefix)));
    if supplied_prefix.is_some_and(|value| value.parse::<u8>().ok() != Some(prefix)) {
        return Err(format!("invalid host route {value}"));
    }
    if prefix == 32 {
        Ok(format!(
            "{}/32",
            address
                .parse::<Ipv4Addr>()
                .map_err(|_| format!("invalid IPv4 address {address}"))?
        ))
    } else {
        Ok(format!(
            "{}/{}",
            address
                .parse::<Ipv6Addr>()
                .map_err(|_| format!("invalid IPv6 address {address}"))?,
            prefix
        ))
    }
}

fn split_routes_from_file(path: &str, ipv6: bool) -> Result<Vec<String>, String> {
    let content = fs::read_to_string(path)
        .map_err(|error| format!("cannot read split route file {path}: {error}"))?;
    let mut routes = Vec::new();
    let mut seen = HashSet::new();
    for line in content.lines() {
        let line = line.trim();
        let candidate = if let Some(rest) = line.strip_prefix("route ") {
            rest.split_whitespace().next().unwrap_or_default()
        } else {
            line.split_whitespace().next().unwrap_or_default()
        }
        .trim_end_matches(';')
        .trim();
        if candidate.is_empty() || candidate.starts_with('#') || candidate.starts_with("//") {
            continue;
        }
        let Some((address, prefix)) = candidate.split_once('/') else {
            continue;
        };
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| format!("invalid prefix in split route file {path}: {candidate}"))?;
        let normalized = if ipv6 {
            if prefix > 128 {
                return Err(format!("invalid IPv6 route: {candidate}"));
            }
            let address = address
                .parse::<Ipv6Addr>()
                .map_err(|_| format!("invalid IPv6 route: {candidate}"))?;
            format!("{address}/{prefix}")
        } else {
            if prefix > 32 {
                return Err(format!("invalid IPv4 route: {candidate}"));
            }
            let address = address
                .parse::<Ipv4Addr>()
                .map_err(|_| format!("invalid IPv4 route: {candidate}"))?;
            format!("{address}/{prefix}")
        };
        if seen.insert(normalized.clone()) {
            routes.push(normalized);
        }
        if routes.len() > 4096 {
            return Err(format!(
                "split route file {path} contains more than 4096 prefixes"
            ));
        }
    }
    Ok(routes)
}

fn optional_split_routes_from_file(
    path: Option<&str>,
    ipv6: bool,
) -> Result<Option<Vec<String>>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let routes = split_routes_from_file(path, ipv6)?;
    Ok((!routes.is_empty()).then_some(routes))
}

fn network_config(config: &NodeConfig) -> Result<NodeNetworkConfig, String> {
    let mut dns_servers: Vec<String> = [&config.dns_ipv4, &config.dns_ipv6]
        .into_iter()
        .filter_map(|dns| dns.clone())
        .collect();
    if dns_servers.is_empty()
        && (!config.pmmandns_upstreams.is_empty()
            || !config.pmmandns_domestic_upstreams.is_empty()
            || !config.pmmandns_foreign_upstreams.is_empty())
    {
        dns_servers = [
            config.ipv4_address.as_deref(),
            config.ipv6_address.as_deref(),
        ]
        .into_iter()
        .filter_map(|address| address.and_then(host_address))
        .map(|address| address.to_string())
        .collect();
    }
    Ok(NodeNetworkConfig {
        ipv4_split_routes: optional_split_routes_from_file(
            config.ipv4_split_routes_file.as_deref(),
            false,
        )?,
        ipv6_split_routes: optional_split_routes_from_file(
            config.ipv6_split_routes_file.as_deref(),
            true,
        )?,
        dns_servers,
        pmmandns_upstreams: config.pmmandns_upstreams.clone(),
        pmmandns_domestic_upstreams: config.pmmandns_domestic_upstreams.clone(),
        pmmandns_foreign_upstreams: config.pmmandns_foreign_upstreams.clone(),
        pmmandns_domestic_rules: config
            .pmmandns_domestic_rules_file
            .as_deref()
            .map(read_domain_rules)
            .transpose()?
            .unwrap_or_default(),
        pmmandns_foreign_rules: config
            .pmmandns_foreign_rules_file
            .as_deref()
            .map(read_domain_rules)
            .transpose()?
            .unwrap_or_default(),
        pmmandns_default_group: config.pmmandns_default_group.clone(),
        pmmandns_rules: Vec::new(),
        pmmandns_geosite: None,
        pmmandns_geosite_enabled: false,
        gateway_ipv4: config.ipv4_address.clone(),
        gateway_ipv6: config.ipv6_address.clone(),
    })
}

fn endpoint_port(value: &str) -> Result<u16, String> {
    let port = value
        .rsplit_once(':')
        .map(|(_, port)| port)
        .ok_or_else(|| format!("invalid WireGuard endpoint: {value}"))?;
    let port = port
        .parse::<u16>()
        .map_err(|_| format!("invalid WireGuard endpoint port: {value}"))?;
    if port == 0 {
        return Err(format!("invalid WireGuard endpoint port: {value}"));
    }
    Ok(port)
}

fn ensure_private_key(wg_binary: &str, path: &str) -> Result<String, String> {
    let private_key = if fs::metadata(path).is_ok() {
        fs::read_to_string(path)
            .map_err(|error| format!("cannot read WireGuard private key: {error}"))?
    } else {
        let output = Command::new(wg_binary)
            .arg("genkey")
            .output()
            .map_err(|error| format!("cannot generate WireGuard private key: {error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().into());
        }
        let generated = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if generated.is_empty() {
            return Err("wg genkey returned an empty key".into());
        }
        if let Some(parent) = std::path::Path::new(path).parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create WireGuard key directory: {error}"))?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("cannot persist WireGuard private key: {error}"))?;
        file.write_all(generated.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|error| format!("cannot write WireGuard private key: {error}"))?;
        generated
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot protect WireGuard private key: {error}"))?;
    }
    let mut child = Command::new(wg_binary)
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot derive WireGuard public key: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "cannot open wg pubkey stdin".to_owned())?
        .write_all(private_key.trim().as_bytes())
        .map_err(|error| format!("cannot provide WireGuard private key: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot derive WireGuard public key: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let public_key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if public_key.is_empty() {
        Err("wg pubkey returned an empty key".into())
    } else {
        Ok(public_key)
    }
}

fn ensure_wireguard_addresses(config: &NodeConfig) -> Result<(), String> {
    for (family, address) in [
        ("-4", config.ipv4_address.as_ref()),
        ("-6", config.ipv6_address.as_ref()),
    ] {
        let Some(address) = address else {
            continue;
        };
        let output = Command::new(&config.ip_binary)
            .args([
                family,
                "address",
                "replace",
                address,
                "dev",
                &config.interface,
            ])
            .output()
            .map_err(|error| format!("cannot assign WireGuard {family} address: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cannot assign WireGuard address {address}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

/// Apply the gateway addresses delivered by the authenticated master
/// snapshot.  The node environment remains the bootstrap fallback, but an
/// administrator override on the master must also reach the actual interface;
/// otherwise clients receive a DNS/gateway address that the node never owns.
fn apply_master_gateway_addresses(
    config: &mut NodeConfig,
    network: &NodeNetworkConfig,
) -> Result<(), String> {
    let mut changed = false;
    if let Some(address) = &network.gateway_ipv4 {
        let ip = host_address(address).ok_or("master returned an invalid IPv4 gateway")?;
        if !ip.is_ipv4() {
            return Err("master returned an IPv6 address as the IPv4 gateway".into());
        }
        if config.ipv4_address.as_deref() != Some(address.as_str()) {
            config.ipv4_address = Some(address.clone());
            changed = true;
        }
    }
    if let Some(address) = &network.gateway_ipv6 {
        let ip = host_address(address).ok_or("master returned an invalid IPv6 gateway")?;
        if !ip.is_ipv6() {
            return Err("master returned an IPv4 address as the IPv6 gateway".into());
        }
        if config.ipv6_address.as_deref() != Some(address.as_str()) {
            config.ipv6_address = Some(address.clone());
            changed = true;
        }
    }
    if changed {
        ensure_wireguard_addresses(config)?;
    }
    Ok(())
}

fn ensure_wireguard_interface(config: &NodeConfig) -> Result<(), String> {
    let exists = Command::new(&config.ip_binary)
        .args(["link", "show", "dev", &config.interface])
        .output()
        .map_err(|error| format!("cannot inspect WireGuard interface: {error}"))?
        .status
        .success();
    if !exists {
        let output = Command::new(&config.ip_binary)
            .args(["link", "add", "dev", &config.interface, "type", "wireguard"])
            .output()
            .map_err(|error| format!("cannot create WireGuard interface: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cannot create WireGuard interface {}: {}",
                config.interface,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    let output = Command::new(&config.wg_binary)
        .args([
            "set",
            &config.interface,
            "private-key",
            &config.private_key_path,
            "listen-port",
            &config.listen_port.to_string(),
        ])
        .output()
        .map_err(|error| format!("cannot configure WireGuard interface: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot configure WireGuard interface {}: {}",
            config.interface,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    ensure_wireguard_addresses(config)?;
    let output = Command::new(&config.ip_binary)
        .args(["link", "set", "dev", &config.interface, "mtu", "1380", "up"])
        .output()
        .map_err(|error| format!("cannot bring up WireGuard interface: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot bring up WireGuard interface {}: {}",
            config.interface,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn config_from_environment() -> NodeConfig {
    let probe_listen: SocketAddr = env_required("PMMAN_PROBE_LISTEN")
        .parse()
        .expect("invalid PMMAN_PROBE_LISTEN");
    let route_table = env::var("PMMAN_ROUTE_TABLE")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| validate_route_table(&value).expect("invalid PMMAN_ROUTE_TABLE"));
    let version_path = env::var("PMMAN_NODE_VERSION_FILE")
        .unwrap_or_else(|_| "/var/lib/pmman-node/version".into());
    // The version file is the authoritative runtime state. A binary may be
    // promoted to a node that already has a newer or older installed version,
    // and Cargo does not rebuild solely because a compile-time environment
    // value changed. Use the embedded version only for a first installation
    // with no local state yet.
    let release_version = fs::read_to_string(&version_path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| option_env!("PMMAN_SERVER_VERSION").map(str::to_owned))
        .or_else(|| env::var("PMMAN_NODE_VERSION").ok());
    let binary_path = env::var("PMMAN_NODE_BINARY")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            env::current_exe()
                .ok()
                .map(|path| path.display().to_string())
        })
        .unwrap_or_else(|| "/opt/pmman/pmman-node".into());
    let wg_binary = env::var("PMMAN_WG_CLI").unwrap_or_else(|_| "wg".into());
    let nft_binary = env::var("PMMAN_NFT_CLI").unwrap_or_else(|_| "nft".into());
    let private_key_path = env::var("PMMAN_WG_PRIVATE_KEY_FILE")
        .unwrap_or_else(|_| "/var/lib/pmman-node/wireguard-private.key".into());
    let server_public_key = ensure_private_key(&wg_binary, &private_key_path)
        .expect("cannot create or read the node WireGuard private key");
    let wireguard_endpoint = env_required("PMMAN_WG_ENDPOINT");
    let listen_port = endpoint_port(&wireguard_endpoint).expect("invalid PMMAN_WG_ENDPOINT port");
    let master_url = env_required("PMMAN_MASTER_URL")
        .trim_end_matches('/')
        .to_owned();
    validate_master_url(&master_url).expect("invalid PMMAN_MASTER_URL");
    NodeConfig {
        node_id: env_required("PMMAN_NODE_ID"),
        name: env_required("PMMAN_NODE_NAME"),
        probe_endpoint: env_required("PMMAN_PROBE_ENDPOINT"),
        wireguard_endpoint,
        region: env::var("PMMAN_NODE_REGION")
            .ok()
            .filter(|value| !value.is_empty()),
        master_url,
        node_token: env_required("PMMAN_NODE_TOKEN"),
        probe_listen,
        interface: env_required("PMMAN_WG_INTERFACE"),
        private_key_path,
        ipv4_address: env::var("PMMAN_WG_IPV4_ADDRESS")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        ipv6_address: env::var("PMMAN_WG_IPV6_ADDRESS")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        listen_port,
        server_public_key,
        wg_binary,
        nft_binary,
        command_deadline: None,
        route_table,
        ip_binary: env::var("PMMAN_IP_CLI").unwrap_or_else(|_| "ip".into()),
        ipv4_split_routes_file: env::var("PMMAN_IPV4_SPLIT_ROUTES_FILE")
            .ok()
            .filter(|value| !value.is_empty()),
        ipv6_split_routes_file: env::var("PMMAN_IPV6_SPLIT_ROUTES_FILE")
            .ok()
            .filter(|value| !value.is_empty()),
        dns_ipv4: env::var("PMMAN_DNS_IPV4")
            .ok()
            .filter(|value| !value.is_empty()),
        dns_ipv6: env::var("PMMAN_DNS_IPV6")
            .ok()
            .filter(|value| !value.is_empty()),
        pmmandns_upstreams: env::var("PMMAN_DNS_UPSTREAMS")
            .unwrap_or_default()
            .split([',', ';', ' ', '\n', '\r', '\t'])
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_owned())
            .collect(),
        pmmandns_domestic_upstreams: env_list("PMMAN_DNS_DOMESTIC_UPSTREAMS"),
        pmmandns_foreign_upstreams: env_list("PMMAN_DNS_FOREIGN_UPSTREAMS"),
        pmmandns_domestic_rules_file: env::var("PMMAN_DNS_DOMESTIC_RULES_FILE")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        pmmandns_foreign_rules_file: env::var("PMMAN_DNS_FOREIGN_RULES_FILE")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        pmmandns_default_group: env::var("PMMAN_DNS_DEFAULT_GROUP")
            .unwrap_or_else(|_| "foreign".into()),
        release_version,
        binary_path,
        version_path: version_path.clone(),
        geosite_cache_path: env::var("PMMAN_GEOSITE_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(&version_path).with_file_name("geosite.dat")),
        service_name: env_required("PMMAN_SERVICE_NAME"),
    }
}

fn register_with_master(config: &NodeConfig) -> Result<(), String> {
    master_agent()
        .post(&format!("{}/v1/nodes/register", config.master_url))
        .header("X-PMMAN-NODE-TOKEN", &config.node_token)
        .send_json(NodeRegistration {
            node_id: &config.node_id,
            name: &config.name,
            probe_endpoint: &config.probe_endpoint,
            network: network_config(config)?,
            wireguard_endpoint: &config.wireguard_endpoint,
            region: &config.region,
            server_public_key: &config.server_public_key,
            route_table: &config.route_table,
            wireguard_interface: &config.interface,
        })
        // Drain the response explicitly before dropping it.  Registration is
        // retried after a master outage, so the node must not leave a response
        // body half-consumed while the short-lived request agent is reused.
        .and_then(|response| response.into_body().read_to_string().map(|_| ()))
        .map_err(|error| format!("node registration failed: {error}"))
}

fn master_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // Keep the watchdog responsive when the master is SIGSTOPed or a
        // half-open loopback connection survives its process restart. Global
        // timeout alone does not bound every ureq body-read phase.
        .timeout_global(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(3)))
        .timeout_send_body(Some(Duration::from_secs(3)))
        .timeout_recv_response(Some(Duration::from_secs(3)))
        .timeout_recv_body(Some(Duration::from_secs(3)))
        .build()
        .into()
}

fn resource_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // Resource binaries are downloaded through the public reverse proxy
        // and can take longer than a heartbeat. Keep the short control-plane
        // watchdog separate so a slow download cannot be mistaken for a
        // registration failure, while still bounding a stalled connection.
        .timeout_global(Some(Duration::from_secs(300)))
        .timeout_send_request(Some(Duration::from_secs(10)))
        .timeout_send_body(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(180)))
        .build()
        .into()
}

fn update_retry_path(config: &NodeConfig) -> PathBuf {
    PathBuf::from(&config.version_path).with_extension("update.json")
}

fn update_staging_path(config: &NodeConfig) -> PathBuf {
    PathBuf::from(&config.binary_path).with_extension("update")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or_default()
}

fn record_update_failure(config: &NodeConfig, version: &str) {
    let path = update_retry_path(config);
    let previous = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<UpdateRetryState>(&bytes).ok())
        .filter(|state| state.version == version);
    let attempts = previous.map_or(1, |state| state.attempts.saturating_add(1));
    let delay = 5u64.saturating_mul(1u64 << attempts.min(6)).min(300) as i64;
    let state = UpdateRetryState {
        version: version.to_string(),
        attempts,
        next_retry_unix: unix_now().saturating_add(delay),
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temporary = path.with_extension("new");
    if let Ok(bytes) = serde_json::to_vec(&state) {
        if fs::write(&temporary, bytes).is_ok() {
            let _ = fs::rename(temporary, path);
        }
    }
}

fn update_retry_deferred(config: &NodeConfig, version: &str) -> bool {
    fs::read(update_retry_path(config))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<UpdateRetryState>(&bytes).ok())
        .is_some_and(|state| state.version == version && state.next_retry_unix > unix_now())
}

fn verify_staged_update(path: &std::path::Path, release: &NodeRelease) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    if bytes.len() as u64 != release.size {
        return false;
    }
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual == release.sha256.to_ascii_lowercase()
}

fn apply_release_update(config: &NodeConfig, release: &NodeRelease) -> Result<(), String> {
    if config.release_version.as_deref() == Some(release.version.as_str()) {
        let _ = fs::remove_file(update_retry_path(config));
        let _ = fs::remove_file(update_staging_path(config));
        return Ok(());
    }
    let binary = std::path::Path::new(&config.binary_path);
    if let Some(parent) = binary.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            record_update_failure(config, &release.version);
            format!("cannot create node resource directory: {error}")
        })?;
    }
    if update_retry_deferred(config, &release.version) {
        return Ok(());
    }
    let staged = update_staging_path(config);
    if !verify_staged_update(&staged, release) {
        let _ = fs::remove_file(&staged);
        let response = resource_agent()
            .get(&format!(
                "{}/v1/nodes/{}/resource",
                config.master_url, config.node_id
            ))
            .query("version", &release.version)
            .header("X-PMMAN-NODE-TOKEN", &config.node_token)
            .call()
            .map_err(|error| {
                record_update_failure(config, &release.version);
                format!("node resource download failed: {error}")
            })?;
        let bytes = response.into_body().read_to_vec().map_err(|error| {
            record_update_failure(config, &release.version);
            format!("node resource read failed: {error}")
        })?;
        if bytes.len() as u64 != release.size {
            record_update_failure(config, &release.version);
            return Err("node resource size does not match the master manifest".into());
        }
        let actual = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != release.sha256.to_ascii_lowercase() {
            record_update_failure(config, &release.version);
            return Err("node resource sha256 does not match the master manifest".into());
        }
        fs::write(&staged, bytes).map_err(|error| {
            record_update_failure(config, &release.version);
            format!("cannot stage node resource: {error}")
        })?;
    }
    let temporary = binary.with_extension("new");
    fs::copy(&staged, &temporary).map_err(|error| {
        record_update_failure(config, &release.version);
        format!("cannot prepare node resource: {error}")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).map_err(|error| {
            record_update_failure(config, &release.version);
            format!("cannot set node resource permissions: {error}")
        })?;
    }
    fs::rename(&temporary, binary).map_err(|error| {
        record_update_failure(config, &release.version);
        format!("cannot activate node resource: {error}")
    })?;
    if let Some(parent) = std::path::Path::new(&config.version_path).parent() {
        fs::create_dir_all(parent).map_err(|error| {
            record_update_failure(config, &release.version);
            format!("cannot create node version directory: {error}")
        })?;
    }
    fs::write(&config.version_path, format!("{}\n", release.version)).map_err(|error| {
        record_update_failure(config, &release.version);
        format!("cannot persist node resource version: {error}")
    })?;
    eprintln!(
        "node resource {} installed (published {}); restarting {}",
        release.version, release.published_at, config.service_name
    );
    let status = Command::new("systemctl")
        .args(["restart", &config.service_name])
        .status()
        .map_err(|error| {
            record_update_failure(config, &release.version);
            format!("cannot restart node service: {error}")
        })?;
    if !status.success() {
        record_update_failure(config, &release.version);
        return Err("node service restart failed after resource update".into());
    }
    std::process::exit(0);
}

// A stuck wg/ip process must not hold the authorization gate indefinitely.
// Drain both pipes while polling so a large peer dump cannot deadlock on stdout.
fn kernel_output(command: &mut Command, deadline: Option<Instant>) -> Result<Output, String> {
    let started = Instant::now();
    let budget = deadline.map_or(KERNEL_COMMAND_TIMEOUT, |deadline| {
        KERNEL_COMMAND_TIMEOUT.min(deadline.saturating_duration_since(started))
    });
    if budget.is_zero() {
        return Err("node reconciliation deadline expired".into());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let (out_sender, out) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out_sender.send(stdout.read_to_end(&mut bytes).map(|_| bytes));
    });
    let (err_sender, err) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err_sender.send(stderr.read_to_end(&mut bytes).map(|_| bytes));
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < budget => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error.to_string(),
                    _ => "node kernel command timed out".into(),
                });
            }
        }
    };
    Ok(Output {
        status,
        stdout: out
            .recv_timeout(budget.saturating_sub(started.elapsed()))
            .map_err(|_| "node kernel stdout timed out")?
            .map_err(|error| error.to_string())?,
        stderr: err
            .recv_timeout(budget.saturating_sub(started.elapsed()))
            .map_err(|_| "node kernel stderr timed out")?
            .map_err(|error| error.to_string())?,
    })
}

fn add_gateway_peer(
    config: &NodeConfig,
    public_key: &str,
    ipv4: &str,
    ipv6: &str,
) -> Result<(), String> {
    let allowed_ips = format!("{ipv4}, {ipv6}");
    let output = kernel_output(
        Command::new(&config.wg_binary).args([
            "set",
            &config.interface,
            "peer",
            public_key,
            "allowed-ips",
            &allowed_ips,
        ]),
        config.command_deadline,
    )
    .map_err(|error| format!("cannot execute WireGuard: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().into())
    }
}

fn remove_gateway_peer(config: &NodeConfig, public_key: &str) -> Result<(), String> {
    let output = kernel_output(
        Command::new(&config.wg_binary).args([
            "set",
            &config.interface,
            "peer",
            public_key,
            "remove",
        ]),
        config.command_deadline,
    )
    .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().into())
    }
}

fn run_route_command(
    config: &NodeConfig,
    family: &str,
    operation: &str,
    route: &str,
    tolerate_missing: bool,
) -> Result<(), String> {
    let Some(table) = &config.route_table else {
        return Ok(());
    };
    let mut command = Command::new(&config.ip_binary);
    command.args([
        family,
        "route",
        operation,
        route,
        "dev",
        &config.interface,
        "table",
        table,
    ]);
    // BIRD intentionally ignores Linux link-scope routes.  Return routes in
    // the dedicated table are meant to be advertised through OSPF, so install
    // them with global scope while keeping the table itself isolated.
    if operation == "replace" {
        command.args(["scope", "global"]);
    }
    let output = kernel_output(&mut command, config.command_deadline)
        .map_err(|error| format!("cannot execute ip route command: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if tolerate_missing {
        let lower = detail.to_ascii_lowercase();
        if lower.contains("no such file")
            || lower.contains("no such process")
            || lower.contains("not found")
        {
            return Ok(());
        }
    }
    Err(if detail.is_empty() {
        format!("ip route command failed for {route}")
    } else {
        detail
    })
}

fn run_return_rule_command(
    config: &NodeConfig,
    family: &str,
    operation: &str,
    route: &str,
    tolerate_missing: bool,
) -> Result<(), String> {
    let Some(table) = &config.route_table else {
        return Ok(());
    };
    let output = kernel_output(
        Command::new(&config.ip_binary).args([
            family,
            "rule",
            operation,
            "priority",
            PMMAN_RETURN_RULE_PRIORITY,
            "to",
            route,
            "table",
            table,
        ]),
        config.command_deadline,
    )?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let lower = detail.to_ascii_lowercase();
    if operation == "add" && lower.contains("file exists") {
        return Ok(());
    }
    if tolerate_missing
        && (lower.contains("no such file")
            || lower.contains("no such process")
            || lower.contains("not found"))
    {
        return Ok(());
    }
    Err(if detail.is_empty() {
        format!("ip rule command failed for {route}")
    } else {
        detail
    })
}

fn route_requires_link_up(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("device for nexthop is not up")
}

fn sync_routes(config: &NodeConfig, peer: &PeerState) -> Result<(), String> {
    if config.route_table.is_none() {
        return Ok(());
    }
    let ipv4 = host_route(&peer.ipv4, 32)?;
    let ipv6 = host_route(&peer.ipv6, 128)?;
    let mut raised_for_recovery = false;
    let mut install_route = |family: &str, route: &str| -> Result<(), String> {
        match run_route_command(config, family, "replace", route, false) {
            Ok(()) => Ok(()),
            Err(error) if route_requires_link_up(&error) && !raised_for_recovery => {
                // A fresh authorized snapshot may arrive while the node is
                // quarantined. Linux refuses a route whose device is down;
                // bring up only this dedicated interface, then retry the
                // exact route. Any later failure is rolled back by the
                // caller, so no stale authorization is exposed.
                set_gateway_up(config, true)?;
                raised_for_recovery = true;
                run_route_command(config, family, "replace", route, false)
            }
            Err(error) => Err(error),
        }
    };
    if let Err(error) = install_route("-4", &ipv4) {
        if raised_for_recovery {
            let _ = set_gateway_up(config, false);
        }
        return Err(format!(
            "failed to install IPv4 return route {ipv4}: {error}"
        ));
    }
    if let Err(error) = install_route("-6", &ipv6) {
        let _ = run_route_command(config, "-4", "del", &ipv4, true);
        if raised_for_recovery {
            let _ = set_gateway_up(config, false);
        }
        return Err(format!(
            "failed to install IPv6 return route {ipv6}: {error}"
        ));
    }
    if let Err(error) = run_return_rule_command(config, "-4", "add", &ipv4, false) {
        let _ = run_route_command(config, "-6", "del", &ipv6, true);
        let _ = run_route_command(config, "-4", "del", &ipv4, true);
        return Err(format!(
            "failed to install IPv4 return policy for {ipv4}: {error}"
        ));
    }
    if let Err(error) = run_return_rule_command(config, "-6", "add", &ipv6, false) {
        let _ = run_return_rule_command(config, "-4", "del", &ipv4, true);
        let _ = run_route_command(config, "-6", "del", &ipv6, true);
        let _ = run_route_command(config, "-4", "del", &ipv4, true);
        return Err(format!(
            "failed to install IPv6 return policy for {ipv6}: {error}"
        ));
    }
    Ok(())
}

fn remove_routes(config: &NodeConfig, peer: &PeerState) -> Result<(), String> {
    if config.route_table.is_none() {
        return Ok(());
    }
    let mut errors = Vec::new();
    if !peer.ipv4.is_empty() {
        let ipv4 = host_route(&peer.ipv4, 32)?;
        if let Err(error) = run_return_rule_command(config, "-4", "del", &ipv4, true) {
            errors.push(format!("IPv4 policy {ipv4}: {error}"));
        }
        if let Err(error) = run_route_command(config, "-4", "del", &ipv4, true) {
            errors.push(format!("IPv4 {ipv4}: {error}"));
        }
    }
    if !peer.ipv6.is_empty() {
        let ipv6 = host_route(&peer.ipv6, 128)?;
        if let Err(error) = run_return_rule_command(config, "-6", "del", &ipv6, true) {
            errors.push(format!("IPv6 policy {ipv6}: {error}"));
        }
        if let Err(error) = run_route_command(config, "-6", "del", &ipv6, true) {
            errors.push(format!("IPv6 {ipv6}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn nft_script(config: &NodeConfig, script: &str) -> Result<(), String> {
    let path = std::env::temp_dir().join(format!(
        "pmman-network-nft-{}-{}.conf",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::write(&path, script)
        .map_err(|error| format!("cannot write nftables transaction: {error}"))?;
    let result = kernel_output(
        Command::new(&config.nft_binary).args(["-f", path.to_string_lossy().as_ref()]),
        config.command_deadline,
    );
    let _ = fs::remove_file(&path);
    let output = result?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn ensure_nft_chain(config: &NodeConfig, family: &str) -> Result<(), String> {
    let table = format!("{family} pmman_network_port_forward");
    let probe = kernel_output(
        Command::new(&config.nft_binary).args([
            "list",
            "table",
            family,
            "pmman_network_port_forward",
        ]),
        config.command_deadline,
    )?;
    if !probe.status.success() {
        nft_script(config, &format!(
            "add table {table}\nadd chain {table} prerouting {{ type nat hook prerouting priority dstnat; policy accept; }}\nadd chain {table} postrouting {{ type nat hook postrouting priority srcnat; policy accept; }}\n"
        ))?;
    }
    let chain = kernel_output(
        Command::new(&config.nft_binary).args([
            "list",
            "chain",
            family,
            "pmman_network_port_forward",
            "return_mark",
        ]),
        config.command_deadline,
    )?;
    if !chain.status.success() {
        // conntrack (-200) has associated the reply; route lookup has not run yet.
        nft_script(config, &format!("add chain {table} return_mark {{ type filter hook prerouting priority mangle; policy accept; }}\n"))?;
    }
    Ok(())
}

/// Return the address that Linux sees on ingress for a port-forward entry.
///
/// A public cloud address is often translated to the instance's private
/// address before the packet reaches the guest.  Matching the configured
/// public EIP in nftables therefore never matches on those nodes.  Prefer the
/// configured address when it is actually assigned locally (routed/public
/// IPv6 is common), otherwise use the source address selected by the kernel's
/// default route (the private address after cloud DNAT).
#[cfg(test)]
fn nft_destination_address(config: &NodeConfig, listen_address: &str) -> Result<String, String> {
    let listen = listen_address
        .parse::<std::net::IpAddr>()
        .map_err(|_| format!("invalid listen address: {listen_address}"))?;
    let (family, route_destination, address_token) = if listen.is_ipv4() {
        ("-4", "1.1.1.1", "inet")
    } else {
        ("-6", "2001:4860:4860::8888", "inet6")
    };

    let addresses = kernel_output(
        Command::new(&config.ip_binary).args([family, "address", "show"]),
        config.command_deadline,
    )
    .map_err(|error| format!("cannot inspect local {family} addresses: {error}"))?;
    if addresses.status.success() {
        let address_text = String::from_utf8_lossy(&addresses.stdout);
        let local = address_text.split_whitespace().collect::<Vec<_>>();
        if local.windows(2).any(|pair| {
            pair[0] == address_token
                && pair[1]
                    .split('/')
                    .next()
                    .and_then(|value| value.parse::<std::net::IpAddr>().ok())
                    == Some(listen)
        }) {
            return Ok(listen.to_string());
        }
    }

    let route = kernel_output(
        Command::new(&config.ip_binary).args([family, "route", "get", route_destination]),
        config.command_deadline,
    )
    .map_err(|error| format!("cannot determine ingress {family} address: {error}"))?;
    if !route.status.success() {
        return Err(format!(
            "cannot determine ingress {family} address for {listen_address}"
        ));
    }
    let route_text = String::from_utf8_lossy(&route.stdout);
    let tokens = route_text.split_whitespace().collect::<Vec<_>>();
    let source = tokens
        .windows(2)
        .find(|pair| pair[0] == "src")
        .and_then(|pair| pair[1].parse::<std::net::IpAddr>().ok())
        .filter(|address| address.is_ipv4() == listen.is_ipv4())
        .ok_or_else(|| format!("kernel did not return an ingress {family} source address"))?;
    eprintln!(
        "port-forward listen address {listen_address} is not assigned locally; matching ingress address {source}"
    );
    Ok(source.to_string())
}

fn target_route(config: &NodeConfig, address: &str) -> Result<(), String> {
    let family = if address.contains(':') { "-6" } else { "-4" };
    let output = kernel_output(
        Command::new(&config.ip_binary).args([family, "route", "get", address]),
        config.command_deadline,
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("no route to client address {address}"))
    }
}

fn reconcile_port_forwards(
    config: &NodeConfig,
    desired: &[NodePortForward],
    report: &mut NodeHeartbeatRequest,
) -> bool {
    let mut routing = match node_forward_routing::Routing::load(config) {
        Ok(value) => value,
        Err(error) => {
            for forward in desired {
                report.port_forward_errors.push(PortForwardSyncError {
                    forward_id: forward.forward_id.clone(),
                    config_version: forward.config_version,
                    error: error.clone(),
                });
            }
            eprintln!("port-forward return routing unavailable: {error}");
            // Stop new mappings if ownership/options cannot be validated.
            for family in ["ip", "ip6"] {
                if ensure_nft_chain(config, family).is_ok() {
                    let _ = nft_script(
                        config,
                        &format!("flush chain {family} pmman_network_port_forward prerouting\n"),
                    );
                }
            }
            return false;
        }
    };
    let mut prepared = Vec::with_capacity(desired.len());
    for forward in desired {
        let result = (|| {
            let listen = forward
                .listen_address
                .parse::<std::net::IpAddr>()
                .map_err(|_| "invalid listen address".to_string())?;
            let target = forward
                .target_address
                .parse::<std::net::IpAddr>()
                .map_err(|_| "invalid target address".to_string())?;
            if listen.is_ipv4() != target.is_ipv4() {
                return Err("listen and target address families differ".into());
            }
            if forward.listen_port == 0 || forward.target_port == 0 {
                return Err("port must be between 1 and 65535".into());
            }
            if forward.protocols.is_empty() {
                return Err("protocol list is empty".into());
            }
            target_route(config, &forward.target_address)?;
            let ingress = node_forward_routing::discover(config, &forward.listen_address)?;
            let path = routing.prepare(config, ingress)?;
            Ok((forward, listen.is_ipv4(), path))
        })();
        match result {
            Ok(value) => prepared.push(value),
            Err(error) => report.port_forward_errors.push(PortForwardSyncError {
                forward_id: forward.forward_id.clone(),
                config_version: forward.config_version,
                error,
            }),
        }
    }
    let families = [true, false];
    let mut applied = Vec::new();
    let mut cleanup_ok = true;
    for ipv4 in families {
        let family = if ipv4 { "ip" } else { "ip6" };
        let family_forwards: Vec<_> = prepared
            .iter()
            .filter_map(|(forward, is_ipv4, path)| {
                (*is_ipv4 == ipv4).then_some((*forward, *is_ipv4, path))
            })
            .collect();
        // The desired snapshot is authoritative.  Even when one forward has
        // an invalid route, flush this node's project table so a released or
        // stale rule cannot continue accepting traffic.
        if let Err(error) = ensure_nft_chain(config, family) {
            cleanup_ok = false;
            for (forward, _, _) in family_forwards {
                report.port_forward_errors.push(PortForwardSyncError {
                    forward_id: forward.forward_id.clone(),
                    config_version: forward.config_version,
                    error: error.clone(),
                });
            }
            continue;
        }
        let table = format!("{family} pmman_network_port_forward");
        let mut script = format!("flush chain {table} prerouting\nflush chain {table} postrouting\nflush chain {table} return_mark\n");
        script.push_str(&routing.reply_rules(family));
        let mut masqueraded = HashSet::new();
        for (forward, _, path) in &family_forwards {
            let target = if ipv4 {
                format!("{}:{}", forward.target_address, forward.target_port)
            } else {
                format!("[{}]:{}", forward.target_address, forward.target_port)
            };
            for protocol in &forward.protocols {
                let protocol = protocol.as_str();
                script.push_str(&format!("add rule {table} prerouting iifname \"{}\" {family} daddr {} {protocol} dport {} {} dnat to {}\n",
                    path.ingress.interface, path.ingress.local_address, forward.listen_port, routing.dnat_mark(path), target));
            }
            if masqueraded.insert(routing.mark(path)) {
                script.push_str(&routing.masquerade_rule(path));
            }
        }
        if let Err(error) = nft_script(config, &script) {
            cleanup_ok = false;
            for (forward, _, _) in family_forwards {
                report.port_forward_errors.push(PortForwardSyncError {
                    forward_id: forward.forward_id.clone(),
                    config_version: forward.config_version,
                    error: error.clone(),
                });
            }
        } else {
            applied.extend(
                family_forwards
                    .into_iter()
                    .map(|(forward, _, _)| AppliedPortForward {
                        forward_id: forward.forward_id.clone(),
                        config_version: forward.config_version,
                    }),
            );
        }
    }
    let active_paths = prepared
        .iter()
        .map(|(_, _, path)| path.clone())
        .collect::<Vec<_>>();
    if cleanup_ok && routing.retained_rules() {
        if let Err(error) = routing.collect_unused(config, &active_paths) {
            eprintln!("port-forward return routing cleanup pending: {error}");
            cleanup_ok = false;
        }
    }
    report.applied_port_forwards = applied;
    if !desired.is_empty() || !report.port_forward_errors.is_empty() {
        eprintln!(
            "port-forward reconciliation: desired={} applied={} errors={}",
            desired.len(),
            report.applied_port_forwards.len(),
            report.port_forward_errors.len()
        );
        for error in &report.port_forward_errors {
            eprintln!(
                "port-forward {} v{} failed: {}",
                error.forward_id, error.config_version, error.error
            );
        }
    }
    cleanup_ok
}

fn install_peer(config: &NodeConfig, peer: &PeerState) -> Result<(), String> {
    add_gateway_peer(config, &peer.public_key, &peer.ipv4, &peer.ipv6)?;
    if let Err(error) = sync_routes(config, peer) {
        let _ = remove_gateway_peer(config, &peer.public_key);
        let _ = remove_routes(config, peer);
        return Err(error);
    }
    Ok(())
}

fn remove_peer(config: &NodeConfig, peer: &PeerState) -> Result<(), String> {
    let mut errors = Vec::new();
    if let Err(error) = remove_gateway_peer(config, &peer.public_key) {
        errors.push(format!("WireGuard peer {}: {error}", peer.public_key));
    }
    if let Err(error) = remove_routes(config, peer) {
        errors.push(format!("return routes: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn start_udp_probe(listen: SocketAddr, relay: Arc<RwLock<RelayRegistry>>) -> Result<(), String> {
    let socket = std::net::UdpSocket::bind(listen)
        .map_err(|error| format!("UDP probe bind failed: {error}"))?;
    std::thread::spawn(move || {
        let mut buffer = [0u8; MAX_DATAGRAM + 1];
        let mut window = std::time::Instant::now();
        let mut replies = 0;
        loop {
            let Ok((length, source)) = socket.recv_from(&mut buffer) else {
                continue;
            };
            if window.elapsed() >= Duration::from_secs(1) {
                window = std::time::Instant::now();
                replies = 0;
            }
            // Fixed-size challenge echo, no credentials, no amplification.
            if let Some(response) = probe_response(&buffer[..length]).filter(|_| replies < 200) {
                let _ = socket.send_to(response, source);
                replies += 1;
            } else if let Some((packet, destination)) = relay
                .write()
                .expect("relay registry poisoned")
                .handle(&buffer[..length], source)
            {
                let _ = socket.send_to(&packet, destination);
            }
        }
    });
    Ok(())
}

fn probe_response(packet: &[u8]) -> Option<&[u8]> {
    (packet.len() == UDP_PING_SIZE && &packet[..8] == UDP_PING_MAGIC).then_some(packet)
}

fn reconcile(
    state: &NodeState,
    heartbeat: NodeHeartbeatResponse,
    pending_cleanup: &mut HashMap<String, PeerState>,
    deadline: Instant,
) -> NodeHeartbeatRequest {
    let mut report = NodeHeartbeatRequest::default();
    let revoked_forward_ids: std::collections::HashSet<_> =
        heartbeat.revoked_forward_ids.iter().cloned().collect();
    let desired_port_forwards = heartbeat
        .port_forwards
        .iter()
        .filter(|forward| !revoked_forward_ids.contains(&forward.forward_id))
        .cloned()
        .collect::<Vec<_>>();
    let mut live_config = state.config.write().expect("node config poisoned");
    let mut config = live_config.clone();
    config.command_deadline = Some(deadline);
    let mut peers = state.peers.write().expect("node peer registry poisoned");
    if heartbeat.route_table != config.route_table {
        let mut failed = false;
        for peer in peers.values().chain(pending_cleanup.values()) {
            if remove_routes(&config, peer).is_err() {
                failed = true;
            }
        }
        if failed {
            report.route_table = config.route_table.clone();
            for desired in heartbeat.active {
                report.errors.push(SyncError {
                    device_id: desired.device_id,
                    public_key: desired.public_key,
                    config_version: desired.config_version,
                    error: "old return-route cleanup failed".into(),
                });
            }
            return report;
        }
        config.route_table = heartbeat.route_table;
        live_config.route_table = config.route_table.clone();
    }
    report.route_table = config.route_table.clone();
    let desired_keys: HashSet<_> = heartbeat
        .active
        .iter()
        .map(|peer| peer.public_key.clone())
        .collect();
    // The interface belongs exclusively to this node. Reconcile kernel peers as
    // well as memory so old leases cannot survive either process restarting.
    let mut cleanup_error = None;
    pending_cleanup.retain(|_, peer| match remove_peer(&config, peer) {
        Ok(()) => false,
        Err(error) => {
            cleanup_error = Some(error);
            true
        }
    });
    match kernel_output(
        Command::new(&config.wg_binary).args(["show", &config.interface, "allowed-ips"]),
        config.command_deadline,
    ) {
        Ok(output) if output.status.success() => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let Some((key, addresses)) = line.split_once('\t') else {
                    continue;
                };
                if desired_keys.contains(key) {
                    continue;
                }
                let addresses: Vec<_> = addresses
                    .split(',')
                    .map(str::trim)
                    .filter(|address| *address != "(none)")
                    .collect();
                let peer = PeerState {
                    public_key: key.into(),
                    ipv4: addresses
                        .iter()
                        .find(|address| !address.contains(':'))
                        .unwrap_or(&"")
                        .to_string(),
                    ipv6: addresses
                        .iter()
                        .find(|address| address.contains(':'))
                        .unwrap_or(&"")
                        .to_string(),
                    config_version: 0,
                };
                if let Err(error) = remove_peer(&config, &peer) {
                    cleanup_error = Some(error);
                    pending_cleanup.insert(key.into(), peer);
                }
            }
        }
        Ok(_) => cleanup_error = Some("cannot read WireGuard peer state".into()),
        Err(error) => cleanup_error = Some(error.to_string()),
    }
    let active_ids: HashSet<_> = heartbeat.active.iter().map(|peer| peer.device_id).collect();
    let stale: Vec<_> = peers
        .iter()
        .filter(|(id, _)| !active_ids.contains(id))
        .map(|(id, peer)| (*id, peer.clone()))
        .collect();
    for (id, peer) in stale {
        match remove_peer(&config, &peer) {
            Ok(()) => {
                peers.remove(&id);
            }
            Err(error) => cleanup_error = Some(error),
        }
    }
    for desired in heartbeat.active {
        let peer = PeerState {
            public_key: desired.public_key.clone(),
            ipv4: desired.ipv4,
            ipv6: desired.ipv6,
            config_version: desired.config_version,
        };
        let result = match &cleanup_error {
            Some(error) => Err(error.clone()),
            None => {
                let result = install_peer(&config, &peer);
                if result.is_err() {
                    // Rollback can fail too; retain both addresses for retry.
                    pending_cleanup.insert(peer.public_key.clone(), peer.clone());
                }
                result
            }
        };
        match result {
            Ok(()) => {
                report.applied.push(AppliedPeer {
                    device_id: desired.device_id,
                    public_key: peer.public_key.clone(),
                    config_version: peer.config_version,
                });
                peers.insert(desired.device_id, peer);
            }
            Err(error) => report.errors.push(SyncError {
                device_id: desired.device_id,
                public_key: peer.public_key,
                config_version: peer.config_version,
                error,
            }),
        }
    }
    if reconcile_port_forwards(&config, &desired_port_forwards, &mut report) {
        report.released_forward_ids = revoked_forward_ids.into_iter().collect();
    }
    report.cleanup_ok = cleanup_error.is_none() && pending_cleanup.is_empty();
    report
}

fn set_gateway_up(config: &NodeConfig, up: bool) -> Result<(), String> {
    let output = kernel_output(
        Command::new(&config.ip_binary).args([
            "link",
            "set",
            "dev",
            &config.interface,
            if up { "up" } else { "down" },
        ]),
        config.command_deadline,
    )
    .map_err(|error| format!("cannot change node interface state: {error}"))?;
    if output.status.success() {
        // Some Linux WireGuard setups remove IPv6 addresses when an interface
        // is administratively brought down. Re-apply both configured gateway
        // addresses after quarantine so the next pmmandns bind and heartbeat
        // recovery see the same addresses as the node environment.
        if !up {
            ensure_wireguard_addresses(config)?;
        }
        Ok(())
    } else {
        Err("cannot change node interface state".into())
    }
}

fn accept_heartbeat(
    state: &NodeState,
    relay: &RwLock<RelayRegistry>,
    heartbeat: NodeHeartbeatResponse,
) -> Option<NodeRelease> {
    let release = heartbeat.release.clone();
    accept_heartbeat_at(state, relay, heartbeat, Instant::now());
    release
}

fn accept_heartbeat_at(
    state: &NodeState,
    relay: &RwLock<RelayRegistry>,
    heartbeat: NodeHeartbeatResponse,
    received: Instant,
) {
    // The master is authoritative for node network overrides. Install those
    // addresses before pmmandns binds, so the daemon cannot report a healthy
    // DNS configuration on an interface that is missing the advertised IP.
    let gateway_error = {
        let mut config = state.config.write().expect("node config poisoned");
        apply_master_gateway_addresses(&mut config, &heartbeat.network).err()
    };
    let geosite_error = if heartbeat.network.pmmandns_geosite_enabled && heartbeat.geosite.is_none()
    {
        Some("主控没有可用 geosite 资源，保留旧规则".into())
    } else {
        state
            .pmmandns
            .prepare_geosite(
                &state.config.read().expect("node config poisoned"),
                heartbeat.geosite.as_ref(),
            )
            .err()
    };
    let mut authorization = state
        .authorization
        .lock()
        .expect("authorization gate poisoned");
    // A response delayed behind kernel cleanup must not resurrect an old lease.
    if received.elapsed() >= MASTER_AUTH_LEASE {
        return;
    }
    if let Some(error) = &geosite_error {
        eprintln!("geosite update failed; keeping previous policy: {error}");
    }
    let fatal_geosite_error =
        geosite_error.filter(|_| state.pmmandns.geosite.lock().unwrap().is_none());
    let sync_error =
        fatal_geosite_error.or(gateway_error.map(|error| format!("节点网关地址配置失败：{error}")));
    let sync_error = sync_error.or_else(|| {
        state
            .pmmandns
            .update(
                &state.config.read().unwrap(),
                &heartbeat.network,
                &heartbeat.active,
            )
            .err()
            .map(|error| format!("pmmandns 配置失败：{error}"))
    });
    // Configuration errors must be visible even with no active clients; the
    // peer error report below is empty in that case.
    {
        let mut previous = state.pmmandns.last_sync_error.lock().unwrap();
        if *previous != sync_error {
            if let Some(error) = &sync_error {
                eprintln!("node network configuration failed: {error}");
            } else if previous.is_some() {
                println!("node network configuration recovered");
            }
            *previous = sync_error.clone();
        }
    }
    let dns_peers: Vec<_> = heartbeat
        .active
        .iter()
        .map(|peer| SyncError {
            device_id: peer.device_id,
            public_key: peer.public_key.clone(),
            config_version: peer.config_version,
            error: sync_error.clone().unwrap_or_default(),
        })
        .collect();
    authorization.last_heartbeat = Some(received);
    relay
        .write()
        .expect("relay registry poisoned")
        .update(heartbeat.relay_peers.clone());
    authorization.report = reconcile(
        state,
        heartbeat,
        &mut authorization.pending_cleanup,
        received + MASTER_AUTH_LEASE,
    );
    if sync_error.is_some() {
        authorization.report.applied.clear();
        authorization.report.errors.extend(dns_peers);
    }
    if !authorization.report.errors.is_empty() {
        let detail = authorization
            .report
            .errors
            .first()
            .map(|error| error.error.as_str())
            .unwrap_or("unknown synchronization error");
        eprintln!(
            "master authorization snapshot sync failed: {} pending_cleanup={} detail={detail}",
            authorization.report.errors.len(),
            authorization.pending_cleanup.len()
        );
    }
    if received.elapsed() >= MASTER_AUTH_LEASE {
        let config = state.config.read().expect("node config poisoned");
        let _ = set_gateway_up(&config, false);
        relay
            .write()
            .expect("relay registry poisoned")
            .update(Vec::new());
        authorization.report.applied.clear();
        authorization.report.cleanup_ok = false;
        return;
    }
    // Reconcile while down first: never briefly expose stale keys on recovery.
    // Always restore after a fresh snapshot, including a process restart while
    // the dedicated interface was quarantined by the previous process.
    if authorization.report.cleanup_ok {
        let mut config = state.config.read().expect("node config poisoned").clone();
        config.command_deadline = Some(received + MASTER_AUTH_LEASE);
        match set_gateway_up(&config, true) {
            Ok(()) => {}
            Err(_) => {
                authorization.report.cleanup_ok = false;
                authorization.report.applied.clear();
                eprintln!("authorized node interface recovery failed; retrying on heartbeat");
            }
        }
    }
    // The final link-up command shares the same deadline too. If it completed
    // at the boundary, fence it back down before releasing the mutation gate.
    if received.elapsed() >= MASTER_AUTH_LEASE {
        let config = state.config.read().expect("node config poisoned");
        let _ = set_gateway_up(&config, false);
        relay
            .write()
            .expect("relay registry poisoned")
            .update(Vec::new());
        authorization.report.applied.clear();
        authorization.report.cleanup_ok = false;
    }
}

fn expire_authorization(state: &NodeState, relay: &RwLock<RelayRegistry>, now: Instant) {
    let mut authorization = state
        .authorization
        .lock()
        .expect("authorization gate poisoned");
    if !authorization.expired(now) {
        return;
    }
    // DNS is part of the node authorization boundary.  Do not leave the
    // resolver serving the internal interface after the master lease expires.
    state.pmmandns.stop();
    relay
        .write()
        .expect("relay registry poisoned")
        .update(Vec::new());
    let route_table = state
        .config
        .read()
        .expect("node config poisoned")
        .route_table
        .clone();
    // Stop traffic before potentially slow per-peer cleanup. This affects only
    // this node's dedicated interface, never host/default routes or other VPNs.
    let config = state.config.read().expect("node config poisoned");
    if set_gateway_up(&config, false).is_err() {
        eprintln!("expired authorization: node interface quarantine failed");
    }
    drop(config);
    // Also scan the dedicated kernel interface: after a restart memory is empty,
    // but old WireGuard peers can remain. Keep failed removals for the next tick.
    authorization.report = reconcile(
        state,
        NodeHeartbeatResponse {
            relay_peers: Vec::new(),
            active: Vec::new(),
            route_table,
            network: NodeNetworkConfig::default(),
            release: None,
            geosite: None,
            port_forwards: Vec::new(),
            revoked_forward_ids: Vec::new(),
        },
        &mut authorization.pending_cleanup,
        // Expired cleanup is incremental: don't monopolize the gate with an
        // unbounded batch. Failed/pending removals are retried on the next tick.
        Instant::now() + KERNEL_COMMAND_TIMEOUT,
    );
    if !authorization.report.cleanup_ok {
        eprintln!("expired authorization cleanup incomplete; retrying");
    }
}

fn main() {
    let config = config_from_environment();
    network_config(&config).expect("invalid node routing/DNS configuration");
    ensure_wireguard_interface(&config).expect("cannot initialize the PMMan WireGuard interface");
    let pmmandns = Arc::new(
        PmmanDnsRuntime::new(config.pmmandns_upstreams.clone())
            .expect("invalid pmmandns upstream configuration"),
    );
    // Keep the dedicated interface quarantined until the master grants a
    // fresh client snapshot. This also prevents peers left by an unclean
    // process exit from carrying traffic during startup.
    set_gateway_up(&config, false).expect("cannot quarantine the PMMan WireGuard interface");
    let relay = Arc::new(RwLock::new(RelayRegistry::default()));
    start_udp_probe(config.probe_listen, Arc::clone(&relay))
        .expect("cannot start UDP latency responder");
    let node_id = config.node_id.clone();
    let state = NodeState {
        config: Arc::new(RwLock::new(config)),
        peers: Arc::new(RwLock::new(HashMap::new())),
        pmmandns,
        authorization: Arc::new(Mutex::new(AuthorizationState::default())),
    };
    // HTTP registration and body reads can block. Lease expiry must keep running
    // independently, including before the first successful master connection.
    let expiry_state = state.clone();
    let expiry_relay = Arc::clone(&relay);
    std::thread::spawn(move || loop {
        expire_authorization(&expiry_state, &expiry_relay, Instant::now());
        std::thread::sleep(Duration::from_secs(1));
    });
    let mut registered = false;
    println!("PMMan node {node_id}: WireGuard + UDP probe, outbound master control only");
    loop {
        let config = state.config.read().expect("node config poisoned").clone();
        if !registered {
            registered = match register_with_master(&config) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("master registration failed; retrying: {error}");
                    false
                }
            };
            if !registered {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        }
        let report = state
            .authorization
            .lock()
            .expect("authorization gate poisoned")
            .report
            .clone();
        let mut report = report;
        report.resource_version = config.release_version.clone();
        match master_agent()
            .post(&format!(
                "{}/v1/nodes/{}/heartbeat",
                config.master_url, config.node_id
            ))
            .header("X-PMMAN-NODE-TOKEN", &config.node_token)
            .send_json(&report)
        {
            Ok(response) => match response.into_body().read_json::<NodeHeartbeatResponse>() {
                Ok(heartbeat) => {
                    if !heartbeat.port_forwards.is_empty()
                        || !heartbeat.revoked_forward_ids.is_empty()
                    {
                        eprintln!(
                            "master port-forward snapshot: desired={} revoked={}",
                            heartbeat.port_forwards.len(),
                            heartbeat.revoked_forward_ids.len()
                        );
                    }
                    if let Some(release) = accept_heartbeat(&state, &relay, heartbeat) {
                        if let Err(error) = apply_release_update(&config, &release) {
                            eprintln!("node resource update failed; retrying: {error}");
                        }
                    }
                }
                Err(error) => {
                    eprintln!("master heartbeat response invalid; re-registering: {error}");
                    registered = false;
                }
            },
            Err(error) => {
                eprintln!("master heartbeat request failed; re-registering: {error}");
                registered = false;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_http_is_limited_to_loopback_development_addresses() {
        assert!(validate_master_url("https://network.pmman.tech").is_ok());
        assert!(validate_master_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_master_url("http://[::1]:8080").is_ok());
        assert!(validate_master_url("http://network.pmman.tech").is_err());
        assert!(validate_master_url("http://user:pass@127.0.0.1:8080").is_err());
    }

    // These fake CLIs touch only temporary files, never a real interface/route.
    #[cfg(unix)]
    pub(super) struct KernelFixture {
        pub(super) path: std::path::PathBuf,
        pub(super) state: NodeState,
        relay: RwLock<RelayRegistry>,
        id: Uuid,
    }

    #[cfg(unix)]
    impl KernelFixture {
        pub(super) fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let path = env::temp_dir().join(format!("pmman-node-lease-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            fs::write(
                path.join("kernel"),
                "old-key\t10.88.0.10/32, fd88:1000::a/128\n",
            )
            .unwrap();
            let wg = path.join("wg");
            fs::write(
                &wg,
                r#"#!/bin/sh
d=${0%/*}
printf '%s\n' "$*" >> "$d/wg-log"
[ "$2" = pmman-test ] || exit 97
if [ "$5" = allowed-ips ] && [ -f "$d/slow-install" ]; then sleep 0.15; fi
if [ "$1" = show ]; then
    cat "$d/kernel"
elif [ "$5" = remove ]; then
    if [ -f "$d/fail-remove" ]; then echo 'mock removal failure' >&2; exit 1; fi
    awk -v key="$4" '$1 != key' "$d/kernel" > "$d/next"
    mv "$d/next" "$d/kernel"
else
    awk -v key="$4" '$1 != key' "$d/kernel" > "$d/next"
    printf '%s\t%s\n' "$4" "$6" >> "$d/next"
    mv "$d/next" "$d/kernel"
fi
"#,
            )
            .unwrap();
            fs::set_permissions(&wg, fs::Permissions::from_mode(0o700)).unwrap();
            let ip = path.join("ip");
            fs::write(
                &ip,
                r#"#!/bin/sh
d=${0%/*}
printf '%s\n' "$*" >> "$d/ip-log"
    if [ "$1" = -j ]; then
        if [ "$3" = address ]; then
            if [ "$2" = -4 ]; then echo '[{"ifname":"eth0","addr_info":[{"local":"192.0.2.10","scope":"global"}]}]'; else echo '[{"ifname":"eth0","addr_info":[{"local":"2001:db8::10","scope":"global"}]}]'; fi
        elif [ "$3" = rule ] && [ -f "$d/rule-json" ]; then cat "$d/rule-json"
        elif [ "$3" = route ] && [ "$6" = main ]; then
            if [ "$2" = -4 ]; then echo '[{"dst":"default","dev":"eth0","gateway":"192.0.2.1","prefsrc":"192.0.2.10"}]'; else echo '[{"dst":"default","dev":"eth0","gateway":"2001:db8::1","prefsrc":"2001:db8::10"}]'; fi
        elif [ "$3" = route ] && [ -f "$d/route-json" ]; then cat "$d/route-json"
        else echo '[]'; fi
    elif [ "$1" = link ]; then
        [ "$4" = pmman-test ] || exit 97
        printf '%s\n' "$5" > "$d/link-state"
    elif { [ "$1" = -4 ] || [ "$1" = -6 ]; } && [ "$2" = address ]; then
        # Address reconciliation is intentionally accepted by the fixture so
        # quarantine tests cover the real node's address re-application path.
        if [ "$3" = show ]; then
            if [ "$1" = -4 ]; then echo '2: eth0    inet 192.0.2.10/24 scope global'; else echo '2: eth0    inet6 2001:db8::10/64 scope global'; fi
        else
            exit 0
        fi
    elif { [ "$1" = -4 ] || [ "$1" = -6 ]; } && [ "$2" = route ] && [ "$3" = get ]; then
        if [ "$1" = -4 ]; then echo '1.1.1.1 via 192.0.2.1 dev eth0 src 192.0.2.10'; else echo '2001:4860:4860::8888 via 2001:db8::1 dev eth0 src 2001:db8::10'; fi
    elif { [ "$1" = -4 ] || [ "$1" = -6 ]; } && [ "$2" = rule ] && [ "$5" = 9000 ]; then
        [ ! -f "$d/fail-routing" ] || exit 98
    elif { [ "$1" = -4 ] || [ "$1" = -6 ]; } && [ "$2" = route ] && { [ "$4" = default ] || [ "$4" = unreachable ]; }; then
        [ ! -f "$d/fail-routing" ] || exit 98
    elif { [ "$1" = -4 ] || [ "$1" = -6 ]; } && [ "$2" = rule ]; then
        [ "$5" = 10000 ] && [ "$9" = 51820 ] || exit 97
    else
        [ "$6" = pmman-test ] && [ "$8" = 51820 ] || exit 97
        if [ "$3" = del ] && [ -f "$d/fail-route" ]; then echo 'mock route failure' >&2; exit 1; fi
        if [ "$3" = del ] && [ -f "$d/missing-route" ]; then echo 'RTNETLINK answers: No such process' >&2; exit 2; fi
        if [ "$3" = replace ] && [ "$(cat "$d/link-state" 2>/dev/null)" = down ]; then echo 'Error: Device for nexthop is not up.' >&2; exit 2; fi
    fi
"#,
            )
            .unwrap();
            fs::set_permissions(&ip, fs::Permissions::from_mode(0o700)).unwrap();
            let nft = path.join("nft");
            fs::write(
                &nft,
                r#"#!/bin/sh
d=${0%/*}
if [ "$1" = -f ]; then
    cat "$2" >> "$d/nft-log"
    [ ! -f "$d/fail-nft" ] || exit 98
elif [ "$1" = list ]; then
    exit 1
fi
"#,
            )
            .unwrap();
            fs::set_permissions(&nft, fs::Permissions::from_mode(0o700)).unwrap();
            let config = NodeConfig {
                node_id: "test".into(),
                name: "test".into(),
                probe_endpoint: "192.0.2.1:51840".into(),
                wireguard_endpoint: "192.0.2.1:51830".into(),
                region: None,
                master_url: "https://invalid.example".into(),
                node_token: "test-only".into(),
                probe_listen: "127.0.0.1:0".parse().unwrap(),
                interface: "pmman-test".into(),
                private_key_path: path.join("private.key").to_string_lossy().into_owned(),
                ipv4_address: Some("10.88.0.1/24".into()),
                ipv6_address: Some("fd88:1000::1/64".into()),
                listen_port: 51830,
                server_public_key: "test".into(),
                wg_binary: wg.to_str().unwrap().into(),
                nft_binary: nft.to_str().unwrap().into(),
                command_deadline: None,
                route_table: Some("51820".into()),
                ip_binary: ip.to_str().unwrap().into(),
                ipv4_split_routes_file: None,
                ipv6_split_routes_file: None,
                dns_ipv4: None,
                dns_ipv6: None,
                pmmandns_upstreams: Vec::new(),
                pmmandns_domestic_upstreams: Vec::new(),
                pmmandns_foreign_upstreams: Vec::new(),
                pmmandns_domestic_rules_file: None,
                pmmandns_foreign_rules_file: None,
                pmmandns_default_group: "foreign".into(),
                release_version: None,
                binary_path: path.join("pmman-node").to_string_lossy().into_owned(),
                version_path: path.join("version").to_string_lossy().into_owned(),
                geosite_cache_path: path.join("geosite.dat"),
                service_name: "pmman-node-test".into(),
            };
            Self {
                path,
                id: Uuid::new_v4(),
                relay: RwLock::new(RelayRegistry::default()),
                state: NodeState {
                    config: Arc::new(RwLock::new(config)),
                    peers: Arc::new(RwLock::new(HashMap::new())),
                    pmmandns: Arc::new(PmmanDnsRuntime::new(Vec::new()).unwrap()),
                    authorization: Arc::new(Mutex::new(AuthorizationState::default())),
                },
            }
        }
        fn heartbeat(&self, key: &str) -> NodeHeartbeatResponse {
            NodeHeartbeatResponse {
                route_table: Some("51820".into()),
                network: NodeNetworkConfig::default(),
                relay_peers: vec![RelayPeer {
                    device_id: self.id,
                    token: "test-relay".into(),
                }],
                active: vec![ActiveNodePeer {
                    device_id: self.id,
                    public_key: key.into(),
                    ipv4: "10.88.0.10/32".into(),
                    ipv6: "fd88:1000::a/128".into(),
                    config_version: 1,
                }],
                release: None,
                geosite: None,
                port_forwards: Vec::new(),
                revoked_forward_ids: Vec::new(),
            }
        }
        fn lease_start(&self) -> Instant {
            self.state
                .authorization
                .lock()
                .unwrap()
                .last_heartbeat
                .unwrap()
        }
        fn read(&self, file: &str) -> String {
            fs::read_to_string(self.path.join(file)).unwrap_or_default()
        }
    }
    #[cfg(unix)]
    impl Drop for KernelFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn dns_config_directory_does_not_collide_with_legacy_binary() {
        let f = KernelFixture::new();
        let legacy = f.path.join("pmmandns");
        fs::write(&legacy, b"old binary").unwrap();
        let directory = PmmanDnsRuntime::default_config_directory(&f.state.config.read().unwrap());
        fs::create_dir_all(&directory).unwrap();
        assert_eq!(directory, f.path.join("pmmandns-config"));
        assert_eq!(fs::read(&legacy).unwrap(), b"old binary");
    }

    #[cfg(unix)]
    #[test]
    fn gateway_dns_without_upstreams_is_not_silently_disabled() {
        let f = KernelFixture::new();
        let network = NodeNetworkConfig {
            gateway_ipv4: Some("10.88.0.1/24".into()),
            dns_servers: vec!["10.88.0.1".into()],
            ..Default::default()
        };
        let error = f
            .state
            .pmmandns
            .update(&f.state.config.read().unwrap(), &network, &[])
            .unwrap_err();
        assert!(error.contains("未配置 pmmandns 上游"));
    }

    #[cfg(unix)]
    #[test]
    fn master_loss_expires_kernel_peers_routes_and_relay_at_finite_deadline() {
        use pmman_protocol::p2p::{encode, unix_ms, REGISTER};
        let f = KernelFixture::new();
        accept_heartbeat(&f.state, &f.relay, f.heartbeat("old-key"));
        let seen = f.lease_start();
        let register = encode(REGISTER, f.id, Uuid::nil(), unix_ms(), &[], "test-relay");
        let source = "192.0.2.10:3000".parse().unwrap();
        assert!(f.relay.write().unwrap().handle(&register, source).is_some());
        expire_authorization(
            &f.state,
            &f.relay,
            seen + MASTER_AUTH_LEASE - Duration::from_millis(1),
        );
        assert!(f.read("kernel").contains("old-key"));
        assert_eq!(f.state.peers.read().unwrap().len(), 1);
        expire_authorization(&f.state, &f.relay, seen + MASTER_AUTH_LEASE);
        assert!(f.read("kernel").is_empty());
        assert!(f.state.peers.read().unwrap().is_empty());
        assert!(f.relay.write().unwrap().handle(&register, source).is_none());
        let auth = f.state.authorization.lock().unwrap();
        assert!(auth.report.applied.is_empty() && auth.report.cleanup_ok);
        assert_eq!(auth.last_heartbeat, Some(seen));
        let routes = f.read("ip-log");
        assert!(routes.contains("-4 route del 10.88.0.10/32 dev pmman-test table 51820"));
        assert!(routes.contains("-6 route del fd88:1000::a/128 dev pmman-test table 51820"));
        assert!(routes.contains("-4 rule del priority 10000 to 10.88.0.10/32 table 51820"));
        assert!(routes.contains("-6 rule del priority 10000 to fd88:1000::a/128 table 51820"));
    }

    #[cfg(unix)]
    #[test]
    fn startup_without_master_removes_kernel_peers_missing_from_memory() {
        let f = KernelFixture::new();
        assert!(f.state.peers.read().unwrap().is_empty());
        expire_authorization(&f.state, &f.relay, Instant::now());
        assert!(f.read("kernel").is_empty());
        assert!(f.state.authorization.lock().unwrap().report.cleanup_ok);
        assert!(f.read("ip-log").contains("route del"));
    }

    #[cfg(unix)]
    #[test]
    fn orphan_route_cleanup_is_retried_after_kernel_peer_is_already_gone() {
        let f = KernelFixture::new();
        fs::write(f.path.join("fail-route"), "fail").unwrap();
        expire_authorization(&f.state, &f.relay, Instant::now());
        assert!(f.read("kernel").is_empty());
        assert!(f.state.peers.read().unwrap().is_empty());
        assert_eq!(
            f.state.authorization.lock().unwrap().pending_cleanup.len(),
            1
        );
        assert_eq!(f.read("link-state").trim(), "down");
        let previous_attempts = f.read("ip-log").matches("route del").count();
        fs::remove_file(f.path.join("fail-route")).unwrap();
        expire_authorization(&f.state, &f.relay, Instant::now());
        let auth = f.state.authorization.lock().unwrap();
        assert!(auth.pending_cleanup.is_empty() && auth.report.cleanup_ok);
        assert!(f.read("ip-log").matches("route del").count() > previous_attempts);
    }

    #[cfg(unix)]
    #[test]
    fn missing_route_is_idempotent_during_cleanup() {
        let f = KernelFixture::new();
        accept_heartbeat(&f.state, &f.relay, f.heartbeat("gone-route-key"));
        fs::write(f.path.join("missing-route"), "missing").unwrap();
        let seen = f.lease_start();
        expire_authorization(&f.state, &f.relay, seen + MASTER_AUTH_LEASE);
        let auth = f.state.authorization.lock().unwrap();
        assert!(auth.pending_cleanup.is_empty() && auth.report.cleanup_ok);
        assert!(f.read("kernel").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn route_recovery_recognizes_only_the_linux_down_device_error() {
        assert!(route_requires_link_up(
            "Error: Device for nexthop is not up."
        ));
        assert!(!route_requires_link_up("device pmman-test is not up"));
        assert!(!route_requires_link_up("nexthop is not up"));
    }

    #[cfg(unix)]
    #[test]
    fn fresh_heartbeat_renews_lease_and_restores_interface_after_restart() {
        let f = KernelFixture::new();
        expire_authorization(&f.state, &f.relay, Instant::now());
        assert_eq!(f.read("link-state").trim(), "down");
        // A prior process's quarantine has no in-memory flag after a restart.
        *f.state.authorization.lock().unwrap() = AuthorizationState::default();
        accept_heartbeat(&f.state, &f.relay, f.heartbeat("current-key"));
        assert_eq!(f.read("link-state").trim(), "up");
        let log = f.read("ip-log");
        assert!(log.contains("link set dev pmman-test up"));
        assert!(log.contains("-4 address replace 10.88.0.1/24 dev pmman-test"));
        assert!(log.contains("-6 address replace fd88:1000::1/64 dev pmman-test"));
        assert!(
            log.contains("-4 route replace 10.88.0.10/32 dev pmman-test table 51820 scope global")
        );
        assert!(log
            .contains("-6 route replace fd88:1000::a/128 dev pmman-test table 51820 scope global"));
        assert!(log.contains("-4 rule add priority 10000 to 10.88.0.10/32 table 51820"));
        assert!(log.contains("-6 rule add priority 10000 to fd88:1000::a/128 table 51820"));
        let old_seen = Instant::now() - Duration::from_secs(20);
        f.state.authorization.lock().unwrap().last_heartbeat = Some(old_seen);
        accept_heartbeat(&f.state, &f.relay, f.heartbeat("current-key"));
        expire_authorization(&f.state, &f.relay, old_seen + MASTER_AUTH_LEASE);
        assert!(f.read("kernel").contains("current-key"));
        assert_eq!(f.read("link-state").trim(), "up");
    }

    #[cfg(unix)]
    #[test]
    fn stuck_kernel_command_has_a_bounded_wait() {
        let started = Instant::now();
        let error =
            kernel_output(Command::new("sh").args(["-c", "exec sleep 30"]), None).unwrap_err();
        assert_eq!(error, "node kernel command timed out");
        assert!(started.elapsed() < KERNEL_COMMAND_TIMEOUT + Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_expiring_during_slow_batch_never_restores_or_reports_authorization() {
        use pmman_protocol::p2p::{encode, unix_ms, REGISTER};
        let f = KernelFixture::new();
        fs::write(f.path.join("kernel"), "").unwrap();
        fs::write(f.path.join("slow-install"), "slow").unwrap();
        let mut heartbeat = f.heartbeat("key-0");
        for index in 1..8 {
            heartbeat.active.push(ActiveNodePeer {
                device_id: Uuid::new_v4(),
                public_key: format!("key-{index}"),
                ipv4: format!("10.88.0.{}/32", 10 + index),
                ipv6: format!("fd88:1000::{:x}/128", 10 + index),
                config_version: 1,
            });
        }
        // Leave a short *remaining* real lease, rather than sleeping 30 seconds.
        // Eight individually valid slow commands exceed that shared budget.
        let received = Instant::now() - MASTER_AUTH_LEASE + Duration::from_millis(400);
        let started = Instant::now();
        accept_heartbeat_at(&f.state, &f.relay, heartbeat, received);
        assert!(started.elapsed() < Duration::from_secs(2));
        let auth = f.state.authorization.lock().unwrap();
        assert_eq!(auth.last_heartbeat, Some(received));
        assert!(auth.expired(Instant::now()));
        assert!(auth.report.applied.is_empty() && !auth.report.cleanup_ok);
        assert_eq!(f.read("link-state").trim(), "down");
        assert!(!f.read("ip-log").contains("link set dev pmman-test up"));
        assert!(f.read("wg-log").matches("allowed-ips 10.").count() < 8);
        let register = encode(REGISTER, f.id, Uuid::nil(), unix_ms(), &[], "test-relay");
        assert!(f
            .relay
            .write()
            .unwrap()
            .handle(&register, "192.0.2.10:3000".parse().unwrap())
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn failed_expiry_quarantines_retries_and_recovers_only_after_fresh_heartbeat() {
        for failure in ["fail-remove", "fail-route"] {
            let f = KernelFixture::new();
            accept_heartbeat(&f.state, &f.relay, f.heartbeat("old-key"));
            fs::write(f.path.join(failure), "fail").unwrap();
            let expired = f.lease_start() + MASTER_AUTH_LEASE;
            expire_authorization(&f.state, &f.relay, expired);
            assert_eq!(f.read("link-state").trim(), "down");
            assert!(!f.state.authorization.lock().unwrap().report.cleanup_ok);
            assert_eq!(f.state.peers.read().unwrap().len(), 1);
            fs::remove_file(f.path.join(failure)).unwrap();
            expire_authorization(&f.state, &f.relay, expired + Duration::from_secs(1));
            assert!(f.read("kernel").is_empty());
            assert!(f.state.peers.read().unwrap().is_empty());
            assert!(f.state.authorization.lock().unwrap().report.cleanup_ok);
            assert_eq!(f.read("link-state").trim(), "down");
            accept_heartbeat(&f.state, &f.relay, f.heartbeat("replacement-key"));
            assert_eq!(f.read("link-state").trim(), "up");
            assert!(!f.read("kernel").contains("old-key"));
            assert!(f.read("kernel").contains("replacement-key"));
            let auth = f.state.authorization.lock().unwrap();
            assert_eq!(auth.report.applied[0].public_key, "replacement-key");
            let log = f.read("ip-log");
            assert!(
                log.rfind("route replace").unwrap()
                    < log.rfind("link set dev pmman-test up").unwrap()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn expiry_runs_while_master_request_is_still_blocked() {
        let f = KernelFixture::new();
        accept_heartbeat(&f.state, &f.relay, f.heartbeat("old-key"));
        f.state.authorization.lock().unwrap().last_heartbeat =
            Some(Instant::now() - MASTER_AUTH_LEASE);
        // A network wait has no authorization/kernel gate, just like main.
        let (release, pending) = std::sync::mpsc::channel::<()>();
        let blocked_request = std::thread::spawn(move || pending.recv().unwrap());
        let state = f.state.clone();
        let relay = RwLock::new(RelayRegistry::default());
        std::thread::spawn(move || expire_authorization(&state, &relay, Instant::now()))
            .join()
            .unwrap();
        assert!(!blocked_request.is_finished());
        assert!(f.read("kernel").is_empty());
        release.send(()).unwrap();
        blocked_request.join().unwrap();
    }

    #[test]
    fn udp_probe_echoes_only_fixed_size_protocol_packets() {
        let mut packet = [0u8; UDP_PING_SIZE];
        packet[..8].copy_from_slice(UDP_PING_MAGIC);
        packet[8..].copy_from_slice(Uuid::new_v4().as_bytes());
        assert_eq!(probe_response(&packet), Some(packet.as_slice()));
        assert!(probe_response(&packet[..8]).is_none());
        assert!(probe_response(&[0u8; UDP_PING_SIZE]).is_none());
        assert!(probe_response(&[0u8; UDP_PING_SIZE + 1]).is_none());
    }

    #[test]
    fn route_table_values_are_safe_for_ip_command_arguments() {
        assert_eq!(validate_route_table("51820").unwrap(), "51820");
        assert_eq!(validate_route_table("ospf_return").unwrap(), "ospf_return");
        assert!(validate_route_table("100; rm -rf /").is_err());
        assert!(validate_route_table("").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cloud_nat_port_forward_uses_kernel_ingress_address_when_eip_is_not_local() {
        let fixture = KernelFixture::new();
        let config = fixture.state.config.read().unwrap().clone();
        assert_eq!(
            nft_destination_address(&config, "198.51.100.10").unwrap(),
            "192.0.2.10"
        );
        assert_eq!(
            nft_destination_address(&config, "192.0.2.10").unwrap(),
            "192.0.2.10"
        );
    }

    fn fixture_forward(ipv6: bool) -> NodePortForward {
        NodePortForward {
            forward_id: if ipv6 { "test-v6" } else { "test-v4" }.into(),
            config_version: 1,
            protocols: vec![
                pmman_protocol::PortForwardProtocol::Tcp,
                pmman_protocol::PortForwardProtocol::Udp,
            ],
            listen_address: if ipv6 { "2001:db8:1::2" } else { "198.18.0.2" }.into(),
            listen_port: 50510,
            target_address: if ipv6 {
                "2001:db8:2::2"
            } else {
                "162.141.136.136"
            }
            .into(),
            target_port: 3000,
        }
    }

    #[cfg(unix)]
    #[test]
    fn forward_return_routing_precedes_nft_and_retains_stable_masked_marks() {
        let fixture = KernelFixture::new();
        let config = fixture.state.config.read().unwrap().clone();
        let forwards = vec![fixture_forward(false), fixture_forward(true)];
        let mut report = NodeHeartbeatRequest::default();
        assert!(reconcile_port_forwards(&config, &forwards, &mut report));
        assert!(
            report.port_forward_errors.is_empty(),
            "{:?}",
            report
                .port_forward_errors
                .iter()
                .map(|e| &e.error)
                .collect::<Vec<_>>()
        );
        assert_eq!(report.applied_port_forwards.len(), 2);
        let journal =
            fs::read(format!("{}.port-forward-routing.json", config.version_path)).unwrap();
        let commands = fixture.read("ip-log");
        assert!(commands.contains(
            "rule add priority 9000 fwmark 0x504d0000/0xffff0000 table 52100 protocol 242"
        ));
        assert!(commands.contains(
            "route replace default via 192.0.2.1 dev eth0 onlink table 52100 proto 242 metric 10"
        ));
        assert!(commands
            .contains("route replace unreachable default table 52100 proto 242 metric 32760"));
        let nft = fixture.read("nft-log");
        assert!(nft.contains("iifname \"eth0\" ip daddr 192.0.2.10 tcp dport 50510 ct mark set (ct mark & 0xffff) | 0x504d0000"));
        assert!(nft.contains("ct direction reply ct status dnat"));
        assert!(nft.contains("meta mark set (meta mark & 0xffff) | 0x504d0000"));
        assert!(nft.contains("udp dport 50510"));
        let mut second = NodeHeartbeatRequest::default();
        assert!(reconcile_port_forwards(&config, &forwards, &mut second));
        assert_eq!(
            journal,
            fs::read(format!("{}.port-forward-routing.json", config.version_path)).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn forward_routing_conflict_or_install_failure_does_not_activate_dnat() {
        for mode in ["conflict", "install", "occupied"] {
            let fixture = KernelFixture::new();
            let config = fixture.state.config.read().unwrap().clone();
            match mode {
                "conflict" => fs::write(
                    fixture.path.join("rule-json"),
                    r#"[{"priority":9000,"src":"all","table":"123"}]"#,
                )
                .unwrap(),
                "occupied" => fs::write(
                    fixture.path.join("route-json"),
                    r#"[{"dst":"default","dev":"eth1","protocol":"static"}]"#,
                )
                .unwrap(),
                _ => fs::write(fixture.path.join("fail-routing"), "").unwrap(),
            }
            let mut report = NodeHeartbeatRequest::default();
            reconcile_port_forwards(&config, &[fixture_forward(false)], &mut report);
            assert_eq!(report.port_forward_errors.len(), 1, "{mode}");
            assert!(report.applied_port_forwards.is_empty());
            assert!(!fixture.read("nft-log").contains("dnat to"), "{mode}");
            assert!(!fixture.read("ip-log").contains("route flush"));
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires disposable Linux network namespace; scripts/verify-port-forward-routing.py"]
    fn real_kernel_port_forward_return_routing() {
        assert_eq!(env::var("PMMAN_PF_KERNEL_FIXTURE").as_deref(), Ok("1"));
        let fixture = KernelFixture::new();
        let mut config = fixture.state.config.read().unwrap().clone();
        config.ip_binary = "ip".into();
        config.nft_binary = "nft".into();
        config.interface = "client0".into();
        config.version_path = env::var("PMMAN_PF_KERNEL_STATE").expect("isolated state path");
        let mut desired = if env::var("PMMAN_PF_KERNEL_STAGE").as_deref() == Ok("release") {
            vec![]
        } else {
            vec![fixture_forward(false), fixture_forward(true)]
        };
        if env::var("PMMAN_PF_KERNEL_STAGE").as_deref() == Ok("multi") {
            for (v6, address) in [(false, "198.18.1.2"), (true, "2001:db8:3::2")] {
                let mut forward = fixture_forward(v6);
                forward.forward_id.push_str("-second");
                forward.listen_address = address.into();
                desired.push(forward);
            }
        }
        let mut report = NodeHeartbeatRequest::default();
        assert!(reconcile_port_forwards(&config, &desired, &mut report));
        assert!(
            report.port_forward_errors.is_empty(),
            "{:?}",
            report
                .port_forward_errors
                .iter()
                .map(|e| &e.error)
                .collect::<Vec<_>>()
        );
        assert_eq!(report.applied_port_forwards.len(), desired.len());
    }

    #[test]
    fn host_routes_are_normalized_for_both_families() {
        assert_eq!(host_route("10.88.0.10", 32).unwrap(), "10.88.0.10/32");
        assert_eq!(host_route("10.88.0.10/32", 32).unwrap(), "10.88.0.10/32");
        assert_eq!(host_route("fd88:1000::a", 128).unwrap(), "fd88:1000::a/128");
        assert!(host_route("10.88.0.10/24", 32).is_err());
        assert!(host_route("not-an-ip", 128).is_err());
    }

    #[test]
    fn split_route_parser_accepts_cnipgen_bird_and_cidr_lines() {
        let path =
            std::env::temp_dir().join(format!("pmman-split-routes-{}.conf", std::process::id()));
        fs::write(
            &path,
            "# generated by cnipgen\nroute 1.1.1.0/24 via \"eth0\";\n8.8.8.0/24\n",
        )
        .unwrap();
        let routes = split_routes_from_file(path.to_str().unwrap(), false).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(routes, vec!["1.1.1.0/24", "8.8.8.0/24"]);
    }

    #[test]
    fn empty_optional_split_route_file_is_treated_as_unconfigured() {
        let path = std::env::temp_dir().join(format!(
            "pmman-empty-split-routes-{}.conf",
            std::process::id()
        ));
        fs::write(&path, "\n# optional\n").unwrap();
        let routes = optional_split_routes_from_file(Some(path.to_str().unwrap()), false).unwrap();
        let _ = fs::remove_file(&path);
        assert!(routes.is_none());
    }
}
