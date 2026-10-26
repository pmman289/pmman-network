use pmman_protocol::{
    Action, ClientSnapshot, IpcRequest, IpcResponse, NodeInfo, NodeRateLimit, PortForwardEntryView,
    PortForwardProtocol, PortForwardQuota, PortForwardView, TrafficStats, UDP_PING_MAGIC,
    UDP_PING_SIZE, UNIX_SOCKET_PATH, WINDOWS_PIPE_NAME,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

const CLIENT_VERSION: &str = match option_env!("PMMAN_CLIENT_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
// Cargo/Tauri require SemVer for package metadata. The unreleased product
// The display label follows the release version unless an explicit build
// override is supplied.
const CLIENT_DISPLAY_VERSION: &str = match option_env!("PMMAN_CLIENT_DISPLAY_VERSION") {
    Some(version) => version,
    None => CLIENT_VERSION,
};
mod connection_retry;
mod diagnostics;
mod invitations;
mod ipc_transport;
mod limiter;
mod node_proxy;
mod p2p;
mod route_manager;
mod session;
mod split_routes;
#[cfg(any(windows, test))]
mod tunnel_service;
static UPDATE_BUSY: AtomicBool = AtomicBool::new(false);
static UPDATE_STATE: OnceLock<Mutex<pmman_protocol::ClientUpdate>> = OnceLock::new();
fn update_state() -> &'static Mutex<pmman_protocol::ClientUpdate> {
    UPDATE_STATE.get_or_init(|| {
        Mutex::new(pmman_protocol::ClientUpdate {
            current_version: CLIENT_DISPLAY_VERSION.into(),
            state: "idle".into(),
            ..Default::default()
        })
    })
}
fn publish_update(snapshot: &ClientSnapshot) {
    *update_state().lock().unwrap() = snapshot.client_update.clone();
}

fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

#[cfg(windows)]
fn command_output_timeout(
    command: &mut Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + timeout;
    // Drain both pipes while waiting. A split-mode `wg show dump` can exceed
    // the OS pipe buffer; polling exit before draining deadlocks that command.
    fn capture(
        pipe: impl io::Read + Send + 'static,
    ) -> std::sync::mpsc::Receiver<io::Result<Vec<u8>>> {
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            let mut oversized = false;
            let result = (|| {
                loop {
                    let count = pipe.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    if bytes.len() + count <= 4 * 1024 * 1024 {
                        bytes.extend_from_slice(&buffer[..count]);
                    } else {
                        oversized = true;
                    }
                }
                if oversized {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "command output exceeds limit",
                    ))
                } else {
                    Ok(bytes)
                }
            })();
            let _ = send.send(result);
        });
        receive
    }
    let stdout = capture(child.stdout.take().expect("stdout is piped"));
    let stderr = capture(child.stderr.take().expect("stderr is piped"));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("command exceeded {} ms", timeout.as_millis()),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let output = |receiver: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>| {
        receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "command output read timed out"))?
    };
    Ok(std::process::Output {
        status,
        stdout: output(stdout)?,
        stderr: output(stderr)?,
    })
}

const MIN_IPV6_TUNNEL_MTU: u16 = 1280;
const MAX_TUNNEL_MTU: u16 = 1420;

#[derive(Debug, Clone, PartialEq, Eq)]
struct MtuDecision {
    mtu: u16,
}

/// Convert an underlay path MTU to the inner WireGuard MTU. The lower bound is
/// the IPv6 minimum MTU; a smaller value would make an IPv6 tunnel invalid,
/// so it is reported as an error instead of silently creating a broken one.
fn tunnel_mtu_from_path(path_mtu: u16, outer_ipv6: bool) -> Result<u16, String> {
    let overhead = if outer_ipv6 { 80 } else { 60 };
    let inner = path_mtu
        .checked_sub(overhead)
        .ok_or_else(|| format!("节点公网路径 MTU {path_mtu} 小于 WireGuard 外层开销 {overhead}"))?;
    if inner < MIN_IPV6_TUNNEL_MTU {
        return Err(format!(
            "节点公网路径 MTU {path_mtu} 不足以承载 IPv6 隧道（需要至少 {}）",
            MIN_IPV6_TUNNEL_MTU + overhead
        ));
    }
    Ok(inner.min(MAX_TUNNEL_MTU))
}

#[cfg(windows)]
fn ping_payload_succeeds(endpoint: std::net::SocketAddr, payload: u16) -> bool {
    let mut command = hidden_command("ping.exe");
    if endpoint.is_ipv6() {
        command.arg("-6");
    } else {
        // IPv4 DF is required for the response to represent the path MTU.
        command.arg("-f");
    }
    command
        .args(["-n", "1", "-w", "300", "-l"])
        .arg(payload.to_string())
        .arg(endpoint.ip().to_string())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn tracepath_style_probe(endpoint: std::net::SocketAddr) -> Option<u16> {
    // Windows does not ship Linux tracepath. `ping -f -l` is its closest
    // inbox equivalent for a path-MTU search: DF makes an oversized packet
    // fail at the first constraining hop, and binary search minimizes probes.
    // IPv6 ping does not expose a portable DF switch, so the same bounded
    // search is used and the ICMPv6 result is treated as advisory.
    probe_path_mtu(endpoint)
}

#[cfg(windows)]
fn probe_path_mtu(endpoint: std::net::SocketAddr) -> Option<u16> {
    // The lower bound is deliberately above the smallest useful ICMP packet:
    // if it fails, ICMP is filtered or the path cannot carry a valid IPv6
    // tunnel, so use the interface-MTU fallback rather than trusting it.
    let (minimum_payload, maximum_payload, header) = if endpoint.is_ipv6() {
        (1200u16, 1452u16, 48u16)
    } else {
        (1200u16, 1472u16, 28u16)
    };
    if !ping_payload_succeeds(endpoint, minimum_payload) {
        return None;
    }
    let mut low = minimum_payload;
    let mut high = maximum_payload;
    while low < high {
        let candidate = low + (high - low).div_ceil(2);
        if ping_payload_succeeds(endpoint, candidate) {
            low = candidate;
        } else {
            high = candidate - 1;
        }
    }
    Some(low + header)
}

#[cfg(windows)]
fn physical_interface_mtu(endpoint: std::net::SocketAddr) -> Option<u16> {
    let family = if endpoint.is_ipv6() { 6 } else { 4 };
    let address = endpoint.ip().to_string();
    let script = format!(
        "$route=Find-NetRoute -RemoteIPAddress '{address}' -ErrorAction SilentlyContinue | Where-Object {{$_.InterfaceIndex}} | Select-Object -First 1; if($route){{(Get-NetIPInterface -AddressFamily {family} -InterfaceIndex $route.InterfaceIndex -ErrorAction SilentlyContinue | Select-Object -First 1).NlMtu}}"
    );
    let output = hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|value| value.parse::<u16>().ok())
}

#[cfg(windows)]
fn decide_tunnel_mtu(endpoint: std::net::SocketAddr) -> Result<MtuDecision, String> {
    let ipv6 = endpoint.is_ipv6();
    let (path_mtu, source) = if let Some(path_mtu) = tracepath_style_probe(endpoint) {
        (Some(path_mtu), "tracepath_style_df_probe".to_string())
    } else if let Some(path_mtu) = physical_interface_mtu(endpoint) {
        (Some(path_mtu), "physical_interface_mtu".to_string())
    } else {
        (None, "safe_fallback".to_string())
    };
    let mtu = match path_mtu {
        Some(path_mtu) => tunnel_mtu_from_path(path_mtu, ipv6)?,
        // A filtered ICMP endpoint must not prevent connection. 1280 is the
        // only valid IPv6 inner fallback and is recorded for diagnostics.
        None => MIN_IPV6_TUNNEL_MTU,
    };
    diagnostics::log(
        "mtu",
        &format!(
            "endpoint={} path_mtu={} source={} tunnel_mtu={}",
            endpoint,
            path_mtu
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into()),
            source,
            mtu
        ),
    );
    Ok(MtuDecision { mtu })
}

fn port_forward_firewall_rules(forwards: &[PortForwardView]) -> Vec<(String, u16)> {
    let mut rules = BTreeSet::new();
    for forward in forwards {
        if !matches!(forward.state.as_str(), "pending" | "applying" | "active") {
            continue;
        }
        for protocol in &forward.protocols {
            rules.insert((protocol.as_str().to_ascii_uppercase(), forward.target_port));
        }
    }
    rules.into_iter().collect()
}

#[cfg(windows)]
fn sync_windows_port_forward_firewall(
    tunnel_name: &str,
    forwards: &[PortForwardView],
) -> Result<(), String> {
    let rules = port_forward_firewall_rules(forwards);
    let mut desired = String::from("$desired=@(");
    for (protocol, port) in &rules {
        desired.push_str(&format!(
            "@{{Name='Pmman Network Port Forward {protocol} {port}'; Protocol='{protocol}'; Port={port}}};"
        ));
    }
    desired.push_str(")");
    // The service is elevated, so use the inbox NetSecurity cmdlets instead
    // of asking users to run a firewall command manually.  Rules are scoped to
    // the Pmman WireGuard interface and to the exact target port/protocol;
    // they never open all inbound traffic on the host.
    let script = format!(
        "$ErrorActionPreference='Stop'; $group='Pmman Network Port Forward'; $interface='{tunnel_name}'; {desired}; $wanted=@($desired | ForEach-Object {{ $_.Name }}); $existing=@(Get-NetFirewallRule -Group $group -ErrorAction SilentlyContinue); $existing | Where-Object {{ $wanted -notcontains $_.DisplayName }} | Remove-NetFirewallRule -Confirm:$false; $existingNames=@($existing | ForEach-Object {{ $_.DisplayName }}); foreach ($item in $desired) {{ if ($existingNames -notcontains $item.Name) {{ New-NetFirewallRule -DisplayName $item.Name -Group $group -Direction Inbound -Action Allow -Enabled True -Profile Any -InterfaceAlias $interface -Protocol $item.Protocol -LocalPort $item.Port -EdgeTraversalPolicy Block | Out-Null }} }}"
    );
    let output = command_output_timeout(
        hidden_command("powershell.exe").args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ]),
        std::time::Duration::from_secs(5),
    )
    .map_err(|error| format!("无法启动 Windows 防火墙配置：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            "Windows 防火墙规则配置失败".into()
        } else {
            format!("Windows 防火墙规则配置失败：{detail}")
        })
    }
}

#[cfg(not(windows))]
fn sync_windows_port_forward_firewall(
    _tunnel_name: &str,
    _forwards: &[PortForwardView],
) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn local_dns_servers() -> Vec<String> {
    let output = hidden_command("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-DnsClientServerAddress -AddressFamily IPv4,IPv6 -ErrorAction SilentlyContinue).ServerAddresses | Where-Object { $_ -and $_ -notmatch '^127\\.' -and $_ -ne '::1' } | Sort-Object -Unique | ConvertTo-Json -Compress",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let Ok(json) = serde_json::from_str::<serde_json::Value>(value.trim()) else {
        return Vec::new();
    };
    match json {
        serde_json::Value::String(server) => vec![server],
        serde_json::Value::Array(servers) => servers
            .into_iter()
            .filter_map(|server| server.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(not(windows))]
fn local_dns_servers() -> Vec<String> {
    Vec::new()
}

fn effective_dns_servers(node_servers: &[String]) -> Vec<String> {
    merge_dns_servers(node_servers, &local_dns_servers())
}

fn flush_system_dns_cache() {
    #[cfg(windows)]
    {
        match command_output_timeout(
            hidden_command("ipconfig.exe").arg("/flushdns"),
            std::time::Duration::from_secs(3),
        ) {
            Ok(output) if output.status.success() => {
                diagnostics::log("dns", "system_resolver_cache_flushed");
            }
            Ok(output) => diagnostics::log(
                "dns",
                &format!(
                    "system_resolver_cache_flush_failed={}",
                    TunnelManager::command_detail(&output)
                ),
            ),
            Err(error) => diagnostics::log(
                "dns",
                &format!("system_resolver_cache_flush_failed={error}"),
            ),
        }
    }
}

fn merge_dns_servers(node_servers: &[String], local_servers: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for server in node_servers.iter().chain(local_servers.iter()) {
        if server.parse::<std::net::IpAddr>().is_ok() && !result.contains(server) {
            result.push(server.clone());
        }
    }
    result
}

fn geosite_cache_path(spec: &pmman_protocol::GeositeConfig) -> PathBuf {
    diagnostics::log_directory().join(format!("geosite-{}.dat", spec.sha256.to_ascii_lowercase()))
}

fn load_geosite(spec: &pmman_protocol::GeositeConfig, token: &str) -> Result<Vec<u8>, String> {
    if spec.size == 0
        || spec.size > pmman_protocol::geosite::MAX_FILE_SIZE as u64
        || spec.sha256.len() != 64
        || !spec.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || spec.version.is_empty()
        || !spec
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("主控返回的 geosite 元数据无效".into());
    }
    let path = geosite_cache_path(spec);
    let valid = |bytes: &[u8]| {
        bytes.len() as u64 == spec.size
            && format!("{:x}", Sha256::digest(bytes)) == spec.sha256.to_ascii_lowercase()
    };
    if let Ok(bytes) = fs::read(&path) {
        if valid(&bytes) {
            return Ok(bytes);
        }
    }
    let url = format!(
        "{}/v1/client/geosite?version={}",
        control_plane_url()?.trim_end_matches('/'),
        spec.version
    );
    let response = resource_agent()
        .get(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| format!("从主控下载 geosite 失败：{error}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!("主控 geosite 下载失败：HTTP {}", response.status()));
    }
    let mut body = response.into_body();
    let bytes = body
        .with_config()
        .limit(pmman_protocol::geosite::MAX_FILE_SIZE as u64)
        .read_to_vec()
        .map_err(|error| format!("读取 geosite 文件失败：{error}"))?;
    if !valid(&bytes) {
        return Err("主控 geosite 文件大小或 SHA-256 校验失败".into());
    }
    for selectors in [&spec.domestic, &spec.foreign] {
        let selectors = selectors
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        pmman_protocol::geosite::rules(&bytes, &selectors)?;
    }
    fs::create_dir_all(diagnostics::log_directory()).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("dat-{}", uuid::Uuid::new_v4()));
    fs::write(&temporary, &bytes).map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        error.to_string()
    })?;
    Ok(bytes)
}

fn geosite_selectors(value: &str) -> Vec<&str> {
    value
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

fn client_dns_policy(
    config: &DeviceConfig,
    local_dns: &[String],
    geosite_bytes: Option<&[u8]>,
) -> Result<pmman_protocol::dns::Policy, String> {
    let node_dns = config
        .dns_servers
        .iter()
        .map(|value| pmman_protocol::dns::endpoint(value))
        .collect::<Result<Vec<_>, _>>()?;
    let domestic_dns = local_dns
        .iter()
        .map(|value| pmman_protocol::dns::endpoint(value))
        .collect::<Result<Vec<_>, _>>()?;
    let mut rules = Vec::new();
    if let Some(policy) = &config.dns_policy {
        for (suffixes, upstreams) in [
            (&policy.domestic_rules, &domestic_dns),
            (&policy.foreign_rules, &node_dns),
        ] {
            for value in suffixes {
                if value.trim().is_empty() || value.trim().starts_with('#') {
                    continue;
                }
                let suffix = pmman_protocol::dns::normalize_suffix(value)?;
                if rules.iter().any(|(known, _)| known == &suffix) {
                    return Err(format!("主控 DNS 分流规则重复：{suffix}"));
                }
                if upstreams.is_empty() {
                    return Err(format!("域名规则 {suffix} 对应的 DNS 上游不可用"));
                }
                rules.push((suffix, upstreams.clone()));
            }
        }
    }
    let geosite = match (&config.dns_policy, geosite_bytes) {
        (Some(policy), Some(bytes)) => {
            if domestic_dns.is_empty() {
                return Err("没有可用于国内域名的本机 DNS 服务器".into());
            }
            let spec = policy.geosite.as_ref().ok_or("缺少 geosite 配置")?;
            let domestic_rules =
                pmman_protocol::geosite::rules(bytes, &geosite_selectors(&spec.domestic))?;
            let foreign_rules =
                pmman_protocol::geosite::rules(bytes, &geosite_selectors(&spec.foreign))?;
            Some(Arc::new(pmman_protocol::geosite::Router::compile(&[
                (domestic_rules, domestic_dns.clone()),
                (foreign_rules, node_dns.clone()),
            ])?))
        }
        (Some(policy), None) if policy.geosite.is_some() => {
            return Err("缺少客户端 geosite 规则数据".into())
        }
        _ => None,
    };
    Ok(pmman_protocol::dns::Policy {
        primary: node_dns,
        fallback: Vec::new(),
        domestic_upstreams: domestic_dns,
        rules,
        allowed_sources: None,
        geosite,
    })
}

#[derive(Deserialize)]
struct ClientSettings {
    control_plane: String,
}

use pmman_protocol::release::Manifest as ClientReleaseManifest;

#[derive(Deserialize)]
struct ClientReleaseResponse {
    available: bool,
    manifest: Option<ClientReleaseManifest>,
    download: Option<String>,
}

fn control_plane_url() -> Result<String, String> {
    let value = if let Ok(value) = env::var("PMMAN_CONTROL_PLANE") {
        value
    } else {
        let executable = env::current_exe().map_err(|error| error.to_string())?;
        let sibling = executable.with_file_name("pmman-client.json");
        let resource = executable
            .parent()
            .map(|directory| directory.join("resources").join("pmman-client.json"));
        let configured = fs::read_to_string(&sibling).or_else(|error| {
            resource
                .as_ref()
                .map(fs::read_to_string)
                .unwrap_or(Err(error))
        });
        match configured {
            Ok(text) => {
                serde_json::from_str::<ClientSettings>(&text)
                    .map_err(|error| format!("客户端配置文件无效：{error}"))?
                    .control_plane
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(
                    "未找到主控配置 pmman-client.json，请将配置文件放在客户端旁边，或设置 PMMAN_CONTROL_PLANE"
                        .into(),
                )
            }
            Err(error) => return Err(format!("无法读取客户端配置：{error}")),
        }
    };
    let value = value.trim().trim_end_matches('/');
    validate_control_plane_url(value)?;
    Ok(value.to_string())
}

fn validate_control_plane_url(value: &str) -> Result<(), String> {
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = value.strip_prefix("http://") {
        ("http", rest)
    } else {
        return Err("主控地址必须使用 https://；仅允许本机回环地址使用 http://".into());
    };
    if value.chars().any(|character| character.is_whitespace()) || rest.is_empty() {
        return Err("主控地址格式无效".into());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("主控地址不得包含用户凭据或空主机名".into());
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
        Err("生产主控连接必须使用 HTTPS；HTTP 仅允许 localhost/回环地址开发使用".into())
    }
}

#[derive(Clone, Deserialize)]
struct DeviceConfig {
    #[serde(default)]
    rate_limit: NodeRateLimit,
    ipv4: String,
    ipv6: String,
    routes: Vec<String>,
    #[serde(default)]
    ipv4_mode: String,
    #[serde(default)]
    ipv6_mode: String,
    #[serde(default)]
    dns_servers: Vec<String>,
    #[serde(default)]
    dns_policy: Option<pmman_protocol::ClientDnsPolicy>,
    config_version: u64,
    device_id: String,
    server_public_key: String,
    endpoint: String,
    #[serde(default)]
    gateway_ipv4: Option<String>,
    #[serde(default)]
    gateway_ipv6: Option<String>,
    #[serde(default)]
    split_route_source: Option<String>,
}

struct ComposedRoutes {
    routes: Vec<String>,
    metadata: Option<split_routes::Metadata>,
}

#[derive(Clone, Debug, Default)]
struct SplitRouteDiagnostics {
    source: Option<String>,
    version: Option<String>,
    snapshot: Option<String>,
    ipv4_count: u32,
    ipv6_count: u32,
    install_ms: u64,
}

fn compose_routes(config: &DeviceConfig) -> Result<ComposedRoutes, String> {
    let Some(source) = config.split_route_source.as_deref() else {
        // A response from an older master already contains the complete route
        // policy. Preserve that wire-compatible behavior for old clients and
        // old servers while still enforcing the selected internal-mode
        // boundary: a stale default route must never capture public traffic.
        let mut routes = config.routes.clone();
        strip_unrequested_defaults(config, &mut routes);
        return Ok(ComposedRoutes {
            routes,
            metadata: None,
        });
    };
    if source != split_routes::SOURCE {
        return Err(format!("主控返回了不支持的分流规则来源：{source}"));
    }
    let snapshot = split_routes::snapshot()?;
    let uses_split = config.ipv4_mode == "split" || config.ipv6_mode == "split";
    let mut routes = config.routes.clone();
    if config.ipv4_mode == "global" {
        routes.extend(["0.0.0.0/1".into(), "128.0.0.0/1".into()]);
    } else if config.ipv4_mode == "split" {
        routes.extend(snapshot.ipv4.iter().cloned());
    } else if config.ipv4_mode != "internal" {
        return Err(format!("IPv4 流量模式无效：{}", config.ipv4_mode));
    }
    if config.ipv6_mode == "global" {
        routes.extend(["::/1".into(), "8000::/1".into()]);
    } else if config.ipv6_mode == "split" {
        routes.extend(snapshot.ipv6.iter().cloned());
    } else if config.ipv6_mode != "internal" {
        return Err(format!("IPv6 流量模式无效：{}", config.ipv6_mode));
    }
    strip_unrequested_defaults(config, &mut routes);
    routes.sort();
    routes.dedup();
    Ok(ComposedRoutes {
        routes,
        metadata: uses_split.then(|| snapshot.metadata.clone()),
    })
}

fn strip_unrequested_defaults(config: &DeviceConfig, routes: &mut Vec<String>) {
    routes.retain(|route| {
        let route = route.trim();
        if config.ipv4_mode == "internal"
            && matches!(route, "0.0.0.0/0" | "0.0.0.0/1" | "128.0.0.0/1")
        {
            return false;
        }
        if config.ipv6_mode == "internal" && matches!(route, "::/0" | "::/1" | "8000::/1") {
            return false;
        }
        true
    });
}

fn gateway_host_ip(value: &str, prefix: &str) -> Option<String> {
    let (address, supplied_prefix) = value.split_once('/')?;
    if supplied_prefix != prefix {
        return None;
    }
    address
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|_| address.to_string())
}

fn gateway_allowed_ips(config: &DeviceConfig) -> Vec<String> {
    let mut result = Vec::new();
    if let Some(address) = config.gateway_ipv4.as_deref().and_then(|value| {
        gateway_host_ip(value, "32").or_else(|| {
            value.split_once('/').and_then(|(ip, _)| {
                ip.parse::<std::net::Ipv4Addr>()
                    .ok()
                    .map(|_| ip.to_string())
            })
        })
    }) {
        result.push(format!("{address}/32"));
    }
    if let Some(address) = config.gateway_ipv6.as_deref().and_then(|value| {
        gateway_host_ip(value, "128").or_else(|| {
            value.split_once('/').and_then(|(ip, _)| {
                ip.parse::<std::net::Ipv6Addr>()
                    .ok()
                    .map(|_| ip.to_string())
            })
        })
    }) {
        result.push(format!("{address}/128"));
    }
    result
}

fn wireguard_allowed_ips(config: &DeviceConfig, routes: &[String]) -> Vec<String> {
    let mut allowed = gateway_allowed_ips(config);
    if matches!(config.ipv6_mode.as_str(), "internal" | "split") {
        // Keep the peer eligible to carry replies sourced from the assigned
        // global IPv6 address. The Windows route is installed with a high
        // metric, so an existing physical default remains preferred.
        allowed.push("::/0".into());
    }
    allowed.extend(routes.iter().cloned());
    allowed.sort();
    allowed.dedup();
    allowed
}

#[derive(serde::Serialize)]
struct RegisterRequest<'a> {
    node_id: &'a str,
    public_key: &'a str,
    ipv4_mode: &'a str,
    ipv6_mode: &'a str,
    split_route_source: &'static str,
}

#[derive(Deserialize)]
struct ControlPlaneError {
    error: String,
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct LoginResponse {
    token: String,
    user_id: String,
    username: String,
    role: String,
    #[serde(default)]
    allow_ipv4_global: bool,
    #[serde(default)]
    allow_ipv6_global: bool,
    #[serde(default)]
    allow_ipv4_split: bool,
    #[serde(default)]
    allow_ipv6_split: bool,
}

#[derive(Clone)]
struct AuthSession {
    token: String,
    user_id: String,
    username: String,
    role: String,
    allow_ipv4_global: bool,
    allow_ipv6_global: bool,
    allow_ipv4_split: bool,
    allow_ipv6_split: bool,
}

#[derive(Deserialize, Serialize)]
struct StoredLogin {
    username: String,
    encrypted_password: Vec<u8>,
}

#[derive(Clone)]
struct ServiceState {
    command_gate: Arc<Mutex<()>>,
    snapshot: Arc<Mutex<ClientSnapshot>>,
    tunnel: Arc<TunnelManager>,
    auth: Arc<Mutex<Option<AuthSession>>>,
    auth_generation: Arc<AtomicU64>,
    auth_refresh_gate: Arc<Mutex<()>>,
    selected_node: Arc<Mutex<Option<NodeInfo>>>,
}

type ActiveConfig = (
    PathBuf,
    String,
    String,
    session::ConnectionAuth,
    String,
    String,
    u64,
);

impl ServiceState {
    fn connection_auth(&self, session: &AuthSession) -> session::ConnectionAuth {
        session::ConnectionAuth::new(
            session.clone(),
            Arc::clone(&self.auth),
            Arc::clone(&self.auth_generation),
            Arc::clone(&self.auth_refresh_gate),
        )
    }

    fn take_auth_for_logout(&self) -> Option<AuthSession> {
        let mut auth = self.auth.lock().expect("auth state poisoned");
        self.auth_generation.fetch_add(1, Ordering::AcqRel);
        auth.take()
    }
}

struct TunnelManager {
    connect_cancelled: AtomicBool,
    session_gate: Arc<Mutex<()>>,
    p2p: Arc<Mutex<Option<Arc<p2p::Transport>>>>,
    p2p_enabled: Arc<AtomicBool>,
    tunnel_name: String,
    active_config: Arc<Mutex<Option<ActiveConfig>>>,
    heartbeat_stop: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    node_proxy: Arc<Mutex<Option<node_proxy::NodeProxy>>>,
    dns_forwarder: Arc<Mutex<Option<pmman_protocol::dns::Forwarder>>>,
    routes: Arc<route_manager::RouteManager>,
    remote_revoked: Arc<AtomicBool>,
    auth_expired: Arc<AtomicBool>,
    pending_revokes: Mutex<Vec<std::thread::JoinHandle<Result<(), String>>>>,
    traffic: Arc<Mutex<TrafficStats>>,
}

enum HeartbeatResult {
    Active(NodeRateLimit),
    Revoked,
}

impl TunnelManager {
    fn from_environment() -> Self {
        let tunnel_name = env::var("PMMAN_WG_TUNNEL_NAME")
            .ok()
            .filter(|value| {
                value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            })
            .unwrap_or_else(|| "pmman-client".into());
        let manager = Self {
            connect_cancelled: AtomicBool::new(false),
            session_gate: Arc::new(Mutex::new(())),
            p2p: Arc::new(Mutex::new(None)),
            p2p_enabled: Arc::new(AtomicBool::new(load_p2p_preference())),
            tunnel_name: tunnel_name.clone(),
            active_config: Arc::new(Mutex::new(None)),
            heartbeat_stop: Arc::new(Mutex::new(None)),
            node_proxy: Arc::new(Mutex::new(None)),
            dns_forwarder: Arc::new(Mutex::new(None)),
            routes: Arc::new(route_manager::RouteManager::new(tunnel_name.clone())),
            remote_revoked: Arc::new(AtomicBool::new(false)),
            auth_expired: Arc::new(AtomicBool::new(false)),
            pending_revokes: Mutex::new(Vec::new()),
            traffic: Arc::new(Mutex::new(TrafficStats::default())),
        };
        manager
    }

    fn sync_port_forward_firewall(&self, forwards: &[PortForwardView]) -> Result<(), String> {
        let device_id = self
            .active_config
            .lock()
            .expect("tunnel state poisoned")
            .as_ref()
            .map(|config| config.2.clone());
        let owned = device_id
            .as_deref()
            .map(|device_id| {
                forwards
                    .iter()
                    .filter(|forward| forward.device_id == device_id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        sync_windows_port_forward_firewall(&self.tunnel_name, &owned)
    }

    fn wireguard_cli() -> PathBuf {
        env::var_os("PMMAN_WG_CLI")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                [
                    PathBuf::from(r"C:\Program Files\WireGuard\wg.exe"),
                    PathBuf::from(r"C:\Program Files (x86)\WireGuard\wg.exe"),
                ]
                .into_iter()
                .find(|path| path.is_file())
            })
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\WireGuard\wg.exe"))
    }

    fn wireguard_exe() -> PathBuf {
        env::var_os("PMMAN_WG_EXE")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                [
                    PathBuf::from(r"C:\Program Files\WireGuard\wireguard.exe"),
                    PathBuf::from(r"C:\Program Files (x86)\WireGuard\wireguard.exe"),
                ]
                .into_iter()
                .find(|path| path.is_file())
            })
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\WireGuard\wireguard.exe"))
    }

    fn command_detail(output: &std::process::Output) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !stderr.is_empty() {
            stderr
        } else {
            stdout
        }
    }

    fn redact_dump(output: &str) -> String {
        output
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    let mut fields = line.split_whitespace();
                    let _private_key = fields.next();
                    std::iter::once("<redacted>")
                        .chain(fields)
                        .collect::<Vec<_>>()
                        .join("\t")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn run_wg(&self, args: &[&str]) -> Result<std::process::Output, String> {
        hidden_command(Self::wireguard_cli())
            .args(args)
            .output()
            .map_err(|error| {
                format!(
                    "无法启动 WireGuard 命令行工具，请确认已安装 WireGuard for Windows：{error}"
                )
            })
    }

    fn generate_keypair(&self) -> Result<(String, String), String> {
        let private_output = self.run_wg(&["genkey"])?;
        if !private_output.status.success() {
            return Err(format!(
                "WireGuard 私钥生成失败：{}",
                Self::command_detail(&private_output)
            ));
        }
        let private_key = String::from_utf8_lossy(&private_output.stdout)
            .trim()
            .to_string();
        if private_key.is_empty() {
            return Err("WireGuard 私钥生成结果为空".into());
        }

        let mut child = hidden_command(Self::wireguard_cli())
            .arg("pubkey")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("无法启动 WireGuard 公钥计算：{error}"))?;
        child
            .stdin
            .take()
            .ok_or("无法写入 WireGuard 私钥")?
            .write_all(private_key.as_bytes())
            .map_err(|error| error.to_string())?;
        let public_output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        if !public_output.status.success() {
            return Err(format!(
                "WireGuard 公钥计算失败：{}",
                Self::command_detail(&public_output)
            ));
        }
        let public_key = String::from_utf8_lossy(&public_output.stdout)
            .trim()
            .to_string();
        if public_key.is_empty() {
            return Err("WireGuard 公钥计算结果为空".into());
        }
        Ok((private_key, public_key))
    }

    fn apply(
        &self,
        session: &AuthSession,
        connection_auth: session::ConnectionAuth,
        node: &NodeInfo,
        ipv4_mode: &str,
        ipv6_mode: &str,
        progress: &Mutex<ClientSnapshot>,
    ) -> Result<(String, DeviceConfig, SplitRouteDiagnostics), String> {
        #[cfg(not(windows))]
        {
            return Err("真实 WireGuard 客户端仅支持 Windows；请在 Windows 上运行客户端".into());
        }
        #[cfg(windows)]
        {
            let _session = self.session_gate.lock().expect("session gate poisoned");
            self.remote_revoked.store(false, Ordering::Relaxed);
            self.auth_expired.store(false, Ordering::Release);
            if self
                .active_config
                .lock()
                .expect("tunnel state poisoned")
                .is_some()
            {
                return Err("WireGuard 隧道已经存在".into());
            }
            // Recover routes left by a previous failed replacement while the
            // old interface still exists, then remove the old tunnel.
            let _ = self.routes.clear();
            self.uninstall_and_wait()?;

            let reservation =
                std::net::UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
            let wg_port = reservation.local_addr().map_err(|e| e.to_string())?.port();
            drop(reservation);
            let (private_key, public_key) = self.generate_keypair()?;
            let master_url = control_plane_url()?;
            let assigned = connection_retry::run(
                std::time::Duration::from_secs(90),
                std::time::Duration::from_secs(2),
                &self.connect_cancelled,
                |remaining| {
                    control_plane_config(
                        &master_url,
                        &node.node_id,
                        &session.token,
                        &public_key,
                        ipv4_mode,
                        ipv6_mode,
                        remaining.min(std::time::Duration::from_secs(12)),
                    )
                },
                |attempt, elapsed| {
                    let mut shared = progress.lock().expect("service state poisoned");
                    shared.connection_waiting_cleanup = true;
                    if !self.connect_cancelled.load(Ordering::Acquire) {
                        shared.message = connection_retry::WAIT_MESSAGE.into();
                    }
                    diagnostics::log(
                        "connection",
                        &format!(
                            "address_cleanup_wait attempt={attempt} elapsed_ms={}",
                            elapsed.as_millis()
                        ),
                    );
                },
            );
            // Serialize the cancellation decision with CancelConnect. A successful
            // registration arriving after cancellation must be revoked, not installed.
            let cancelled = {
                let mut shared = progress.lock().expect("service state poisoned");
                shared.connection_waiting_cleanup = false;
                shared.message = "正在建立安全连接…".into();
                self.connect_cancelled.load(Ordering::Acquire)
            };
            let assigned = connection_retry::finish(assigned, cancelled, |config| {
                revoke_on_master(&master_url, &session.token, &config.device_id, &public_key)
            })?;
            if assigned.server_public_key.is_empty() || assigned.endpoint.is_empty() {
                let _ = revoke_on_master(
                    &master_url,
                    &session.token,
                    &assigned.device_id,
                    &public_key,
                );
                return Err(
                    "控制面未提供 WireGuard 网关公钥或 endpoint，拒绝建立非真实连接".into(),
                );
            }
            let composed = compose_routes(&assigned)
                .map_err(|error| format!("客户端内置分流规则校验失败：{error}"))?;
            // Do not start P2P workers while registration is still waiting/retrying.
            let stop = Arc::new(AtomicBool::new(false));
            let transport = Arc::new(p2p::Transport::start(
                Arc::clone(&self.p2p_enabled),
                Arc::clone(&stop),
                wg_port,
                NodeRateLimit::default(),
            )?);
            transport.set_rate_limit(assigned.rate_limit);
            let proxy = node_proxy::NodeProxy::start(
                &assigned.endpoint,
                wg_port,
                limiter::RateLimiter::new(assigned.rate_limit),
            )
            .inspect_err(|_| {
                let _ = revoke_on_master(
                    &master_url,
                    &session.token,
                    &assigned.device_id,
                    &public_key,
                );
            })?;
            let proxy_endpoint = proxy.endpoint().to_string();
            #[cfg(windows)]
            let mtu = match decide_tunnel_mtu(proxy.remote_endpoint()) {
                Ok(decision) => decision,
                Err(error) => {
                    diagnostics::log("mtu", &format!("rejected error={error}"));
                    let _ = revoke_on_master(
                        &master_url,
                        &session.token,
                        &assigned.device_id,
                        &public_key,
                    );
                    return Err(format!("当前节点公网链路无法承载 IPv6 隧道：{error}"));
                }
            };
            #[cfg(not(windows))]
            let mtu = MtuDecision {
                mtu: MIN_IPV6_TUNNEL_MTU,
            };
            let config_path = env::temp_dir().join(format!("{}.conf", self.tunnel_name));
            // AllowedIPs controls WireGuard cryptokey routing. Host routes are
            // installed explicitly by route_manager because Table=off is
            // required to preserve the user's existing IPv6 default route.
            let allowed_ips = wireguard_allowed_ips(&assigned, &composed.routes)
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ");
            let local_dns = local_dns_servers();
            diagnostics::log(
                "dns",
                &format!(
                    "resolver_start split={} local_upstreams={} node_upstreams={} geosite={}",
                    assigned.dns_policy.is_some(),
                    local_dns.len(),
                    assigned.dns_servers.len(),
                    assigned
                        .dns_policy
                        .as_ref()
                        .and_then(|policy| policy.geosite.as_ref())
                        .map(|spec| spec.version.as_str())
                        .unwrap_or("none")
                ),
            );
            let geosite_bytes = assigned
                .dns_policy
                .as_ref()
                .and_then(|policy| policy.geosite.as_ref())
                .map(|spec| load_geosite(spec, &session.token))
                .transpose();
            let geosite_bytes = match geosite_bytes {
                Ok(bytes) => bytes,
                Err(error) => {
                    let _ = revoke_on_master(
                        &master_url,
                        &session.token,
                        &assigned.device_id,
                        &public_key,
                    );
                    return Err(format!("无法准备客户端 DNS 分流规则：{error}"));
                }
            };
            let dns_forwarder = if assigned.dns_servers.is_empty() {
                Ok(None)
            } else if assigned.dns_policy.is_some() {
                client_dns_policy(&assigned, &local_dns, geosite_bytes.as_deref()).and_then(
                    |policy| {
                        pmman_protocol::dns::Forwarder::start(
                            &["127.0.0.2:53".parse().unwrap()],
                            policy,
                        )
                        .map(Some)
                    },
                )
            } else {
                (|| {
                    let primary = assigned
                        .dns_servers
                        .iter()
                        .map(|value| pmman_protocol::dns::endpoint(value))
                        .collect::<Result<Vec<_>, _>>()?;
                    let fallback = local_dns
                        .iter()
                        .map(|value| pmman_protocol::dns::endpoint(value))
                        .collect::<Result<Vec<_>, _>>()?;
                    pmman_protocol::dns::Forwarder::start(
                        &["127.0.0.2:53".parse().unwrap()],
                        pmman_protocol::dns::Policy {
                            primary,
                            fallback,
                            domestic_upstreams: Vec::new(),
                            rules: Vec::new(),
                            allowed_sources: None,
                            geosite: None,
                        },
                    )
                    .map(Some)
                })()
            };
            let dns_forwarder = match dns_forwarder {
                Ok(forwarder) => forwarder,
                Err(error) => {
                    let _ = revoke_on_master(
                        &master_url,
                        &session.token,
                        &assigned.device_id,
                        &public_key,
                    );
                    return Err(format!("无法启动客户端 DNS 分流：{error}"));
                }
            };
            let configured_dns = if dns_forwarder.is_some() {
                vec!["127.0.0.2"]
            } else {
                assigned.dns_servers.iter().map(String::as_str).collect()
            };
            let dns = if configured_dns.is_empty() {
                String::new()
            } else {
                format!("DNS = {}\n", configured_dns.join(", "))
            };
            let wireguard_config = format!(
                "[Interface]\nPrivateKey = {private_key}\nListenPort = {wg_port}\nMTU = {}\nTable = off\nAddress = {}, {}\n{dns}\n[Peer]\nPublicKey = {}\nEndpoint = {}\nAllowedIPs = {allowed_ips}\nPersistentKeepalive = 1\n",
                mtu.mtu,
                assigned.ipv4,
                assigned.ipv6,
                assigned.server_public_key,
                proxy_endpoint
            );
            if let Err(error) = fs::write(&config_path, wireguard_config) {
                let _ = revoke_on_master(
                    &master_url,
                    &session.token,
                    &assigned.device_id,
                    &public_key,
                );
                return Err(format!("无法写入临时 WireGuard 配置：{error}"));
            }

            let install = match command_output_timeout(
                hidden_command(Self::wireguard_exe()).args([
                    "/installtunnelservice",
                    config_path.to_string_lossy().as_ref(),
                ]),
                std::time::Duration::from_secs(8),
            ) {
                Ok(output) => output,
                Err(error) => {
                    let _ = revoke_on_master(
                        &master_url,
                        &session.token,
                        &assigned.device_id,
                        &public_key,
                    );
                    let _ = fs::remove_file(&config_path);
                    return Err(format!("无法启动 WireGuard 隧道安装器：{error}"));
                }
            };
            if !install.status.success() {
                let _ = fs::remove_file(&config_path);
                let _ = revoke_on_master(
                    &master_url,
                    &session.token,
                    &assigned.device_id,
                    &public_key,
                );
                let detail = Self::command_detail(&install);
                return Err(format!(
                    "WireGuard 隧道安装失败；请以管理员身份运行客户端{}",
                    if detail.is_empty() {
                        ".".into()
                    } else {
                        format!("：{detail}")
                    }
                ));
            }

            let managed_routes = match route_manager::plan(
                &composed.routes,
                assigned.gateway_ipv4.as_deref(),
                assigned.gateway_ipv6.as_deref(),
                &assigned.ipv6_mode,
            ) {
                Ok(routes) => routes,
                Err(error) => {
                    let _ = self.uninstall_and_wait();
                    let _ = revoke_on_master(
                        &master_url,
                        &session.token,
                        &assigned.device_id,
                        &public_key,
                    );
                    let _ = fs::remove_file(&config_path);
                    return Err(format!("服务端下发的路由无效：{error}"));
                }
            };
            let route_install_started = std::time::Instant::now();
            if let Err(error) = self
                .routes
                .install_with_timeout(&managed_routes, std::time::Duration::from_secs(120))
            {
                let _ = self.uninstall_and_wait();
                let _ = revoke_on_master(
                    &master_url,
                    &session.token,
                    &assigned.device_id,
                    &public_key,
                );
                let _ = fs::remove_file(&config_path);
                return Err(error);
            }
            let split_diagnostics = composed
                .metadata
                .as_ref()
                .map(|metadata| SplitRouteDiagnostics {
                    source: Some(split_routes::SOURCE.into()),
                    version: Some(metadata.version.into()),
                    snapshot: Some(metadata.generated_at.into()),
                    ipv4_count: metadata.ipv4_count as u32,
                    ipv6_count: metadata.ipv6_count as u32,
                    install_ms: route_install_started.elapsed().as_millis() as u64,
                })
                .unwrap_or_default();

            // The physical default route can change while WireGuard is being
            // installed (Wi-Fi roaming, VPN teardown, DHCP renewal). Keep
            // the node proxy pinned to the real underlay during the initial
            // handshake instead of relying on its pre-install binding.
            let gateway_result = self.wait_for_gateway(&assigned, &public_key, &proxy);
            let gateway_result = match gateway_result {
                Err(error) if error.contains("公钥与本次注册公钥不一致") => {
                    // WireGuard can briefly expose a stale service instance during replacement.
                    // Complete a second uninstall/install cycle before failing the connection.
                    let retry = self.uninstall_and_wait().and_then(|_| {
                        command_output_timeout(
                            hidden_command(Self::wireguard_exe()).args([
                                "/installtunnelservice",
                                config_path.to_string_lossy().as_ref(),
                            ]),
                            std::time::Duration::from_secs(8),
                        )
                        .map_err(|install_error| install_error.to_string())
                        .and_then(|output| {
                            if output.status.success() {
                                Ok(())
                            } else {
                                Err(Self::command_detail(&output))
                            }
                        })
                    });
                    match retry {
                        Ok(()) => self.wait_for_gateway(&assigned, &public_key, &proxy),
                        Err(retry_error) => Err(format!("{error}；重试安装失败：{retry_error}")),
                    }
                }
                result => result,
            };
            if let Err(error) = gateway_result {
                let _ = self.routes.clear();
                let _ = self.uninstall_and_wait();
                let _ = revoke_on_master(
                    &master_url,
                    &session.token,
                    &assigned.device_id,
                    &public_key,
                );
                let _ = fs::remove_file(&config_path);
                return Err(error);
            }
            flush_system_dns_cache();
            let device_id = assigned.device_id.clone();
            start_peer_sync(
                Arc::clone(&transport),
                self.tunnel_name.clone(),
                master_url.clone(),
                connection_auth.clone(),
                assigned.device_id.clone(),
                public_key.clone(),
                Arc::clone(&stop),
                Arc::clone(&self.session_gate),
                Arc::clone(&self.routes),
            );
            *self.p2p.lock().expect("p2p state poisoned") = Some(transport);
            *self.node_proxy.lock().expect("node proxy state poisoned") = Some(proxy);
            *self
                .dns_forwarder
                .lock()
                .expect("DNS forwarder state poisoned") = dns_forwarder;
            *self.active_config.lock().expect("tunnel state poisoned") = Some((
                config_path.clone(),
                public_key.clone(),
                device_id.clone(),
                connection_auth.clone(),
                master_url.clone(),
                assigned.server_public_key.clone(),
                assigned.config_version,
            ));
            *self.heartbeat_stop.lock().expect("tunnel state poisoned") = Some(Arc::clone(&stop));
            let heartbeat_stop = Arc::clone(&stop);
            let heartbeat_device_id = device_id.clone();
            let mut heartbeat_auth = connection_auth;
            let heartbeat_master_url = master_url.clone();
            let heartbeat_node_public_key = assigned.server_public_key.clone();
            let heartbeat_public_key = public_key.clone();
            let heartbeat_config_version = assigned.config_version;
            let remote_revoked = Arc::clone(&self.remote_revoked);
            let auth_expired = Arc::clone(&self.auth_expired);
            let active_config = Arc::clone(&self.active_config);
            let heartbeat_stop_state = Arc::clone(&self.heartbeat_stop);
            let tunnel_name = self.tunnel_name.clone();
            let p2p_state = Arc::clone(&self.p2p);
            let node_proxy_state = Arc::clone(&self.node_proxy);
            let dns_forwarder_state = Arc::clone(&self.dns_forwarder);
            let routes_state = Arc::clone(&self.routes);
            let session_gate = Arc::clone(&self.session_gate);
            *self.traffic.lock().unwrap() = TrafficStats::default();
            let traffic_state = Arc::clone(&self.traffic);
            std::thread::spawn(move || {
                let mut last_heartbeat = std::time::Instant::now();
                while !heartbeat_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let traffic = {
                        let Some(_session) = live_session(&session_gate, &heartbeat_stop) else {
                            break;
                        };
                        let sample = collect_traffic_stats(
                            &tunnel_name,
                            &heartbeat_node_public_key,
                            &p2p_state,
                        );
                        let mut traffic = traffic_state.lock().unwrap();
                        traffic.retain_max(sample);
                        *traffic
                    };
                    if last_heartbeat.elapsed() < std::time::Duration::from_secs(10) {
                        continue;
                    }
                    last_heartbeat = std::time::Instant::now();
                    let result = heartbeat_auth.call(|session| {
                        control_plane_heartbeat(
                            &heartbeat_master_url,
                            &session.token,
                            &heartbeat_device_id,
                            &heartbeat_public_key,
                            heartbeat_config_version,
                            traffic,
                        )
                    });
                    let result = match result {
                        Err(error) if session::is_permanent_auth_error(&error) => {
                            let Some(_session) = live_session(&session_gate, &heartbeat_stop)
                            else {
                                break;
                            };
                            diagnostics::log("auth", "heartbeat_credentials_rejected");
                            heartbeat_auth.invalidate();
                            auth_expired.store(true, Ordering::Release);
                            Ok(HeartbeatResult::Revoked)
                        }
                        result => result,
                    };
                    match result {
                        Ok(HeartbeatResult::Active(rate_limit)) => {
                            let Some(_session) = live_session(&session_gate, &heartbeat_stop)
                            else {
                                break;
                            };
                            if let Some(proxy) = node_proxy_state
                                .lock()
                                .expect("node proxy state poisoned")
                                .as_ref()
                            {
                                let _ = proxy.refresh_underlay();
                                proxy.set_limits(rate_limit);
                            }
                            if let Some(transport) =
                                p2p_state.lock().expect("p2p state poisoned").as_ref()
                            {
                                transport.set_rate_limit(rate_limit);
                            }
                        }
                        Ok(HeartbeatResult::Revoked) => {
                            let Some(_session) = live_session(&session_gate, &heartbeat_stop)
                            else {
                                break;
                            };
                            diagnostics::log("connection", "remote_revocation_cleanup_started");
                            remote_revoked.store(true, Ordering::Release);
                            heartbeat_stop.store(true, Ordering::Release);
                            // Removing the interface first lets Windows discard the
                            // large split-route set before the owned-route sweep.
                            let _ = tunnel_service::remove(&tunnel_name);
                            let _ = routes_state.clear();
                            if let Some((path, _, _, _, _, _, _)) =
                                active_config.lock().expect("tunnel state poisoned").take()
                            {
                                let _ = fs::remove_file(path);
                            }
                            let p2p = p2p_state.lock().expect("p2p state poisoned").take();
                            let proxy = node_proxy_state
                                .lock()
                                .expect("node proxy state poisoned")
                                .take();
                            let dns = dns_forwarder_state
                                .lock()
                                .expect("DNS forwarder state poisoned")
                                .take();
                            let _ = heartbeat_stop_state
                                .lock()
                                .expect("tunnel state poisoned")
                                .take();
                            // Destructors join workers; release container locks
                            // first so status queries never wait for a join.
                            drop((p2p, proxy, dns));
                            spawn_idle_firewall_cleanup(
                                tunnel_name.clone(),
                                Arc::clone(&session_gate),
                                Arc::clone(&active_config),
                            );
                            diagnostics::log("connection", "remote_revocation_cleanup_complete");
                            break;
                        }
                        Err(error) => {
                            diagnostics::log("heartbeat", &format!("request_failed error={error}"))
                        }
                    }
                }
            });
            Ok(("wireguard".into(), assigned, split_diagnostics))
        }
    }

    #[cfg(windows)]
    fn wait_for_gateway(
        &self,
        config: &DeviceConfig,
        client_public_key: &str,
        proxy: &node_proxy::NodeProxy,
    ) -> Result<(), String> {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(12);
        let mut last_detail = String::new();
        let mut keepalive_configured = false;
        let mut tunnel_public_key_verified = false;
        let mut underlay_refreshed = std::time::Instant::now();
        let mut last_diagnostic = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(3))
            .unwrap_or_else(std::time::Instant::now);
        while std::time::Instant::now() < deadline {
            // The tunnel service is installed asynchronously. Wait for its
            // interface before reading handshakes, and reject an old tunnel.
            if !tunnel_public_key_verified {
                match self.run_wg(&["show", &self.tunnel_name, "public-key"]) {
                    Ok(output) if output.status.success() => {
                        let observed_public_key =
                            String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if observed_public_key.is_empty() || observed_public_key == "(none)" {
                            last_detail = "WireGuard 隧道接口正在初始化".into();
                            std::thread::sleep(std::time::Duration::from_millis(250));
                            continue;
                        }
                        if observed_public_key != client_public_key {
                            return Err(format!(
                                "WireGuard 隧道公钥与本次注册公钥不一致，拒绝使用旧隧道（期望 {client_public_key}，实际 {observed_public_key}）"
                            ));
                        }
                        tunnel_public_key_verified = true;
                    }
                    Ok(output) => {
                        last_detail =
                            format!("等待 WireGuard 隧道接口：{}", Self::command_detail(&output));
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                    Err(error) => {
                        last_detail = format!("等待 WireGuard 隧道接口：{error}");
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    }
                }
            }
            if underlay_refreshed.elapsed() >= std::time::Duration::from_secs(2) {
                let _ = proxy.refresh_underlay();
                underlay_refreshed = std::time::Instant::now();
            }
            match self.run_wg(&["show", &self.tunnel_name, "latest-handshakes"]) {
                Ok(handshake) if handshake.status.success() => {
                    if !keepalive_configured {
                        match self.run_wg(&[
                            "set",
                            &self.tunnel_name,
                            "peer",
                            &config.server_public_key,
                            "persistent-keepalive",
                            "1",
                        ]) {
                            Ok(output) if output.status.success() => {
                                keepalive_configured = true;
                            }
                            Ok(_) | Err(_) => {}
                        }
                    }
                    let handshaken = has_peer_handshake(
                        &String::from_utf8_lossy(&handshake.stdout),
                        &config.server_public_key,
                    );
                    if last_diagnostic.elapsed() >= std::time::Duration::from_secs(3) {
                        node_proxy::proxy_log(&format!(
                            "handshake_poll tunnel={} peer={} present={} remaining_s={}",
                            self.tunnel_name,
                            config.server_public_key,
                            handshaken,
                            deadline
                                .saturating_duration_since(std::time::Instant::now())
                                .as_secs()
                        ));
                        last_diagnostic = std::time::Instant::now();
                    }
                    if handshaken {
                        let _ = self.run_wg(&[
                            "set",
                            &self.tunnel_name,
                            "peer",
                            &config.server_public_key,
                            "persistent-keepalive",
                            "25",
                        ]);
                        // The tunnel public key was generated for this connection and verified
                        // immediately after installation, so a non-zero handshake on this
                        // interface cannot be a stale peer from an earlier session. Do not
                        // compare wg.exe's Unix timestamp with the process wall clock: Windows
                        // time adjustments and service/driver startup races can make that
                        // comparison reject a real handshake. WireGuard handshake is
                        // authoritative; ICMP can be blocked independently.
                        return Ok(());
                    }
                    last_detail = "隧道已启动，等待本次客户端 peer 握手".into();
                }
                Ok(output) => {
                    last_detail = Self::command_detail(&output);
                }
                Err(error) => {
                    last_detail = error;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        diagnostics::log(
            "connection",
            &format!(
                "handshake_timeout_elapsed_ms={}",
                started.elapsed().as_millis()
            ),
        );
        let dump = self
            .run_wg(&["show", &self.tunnel_name, "dump"])
            .ok()
            .map(|output| Self::redact_dump(String::from_utf8_lossy(&output.stdout).trim()))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "无法读取隧道 dump".into());
        Err(format!(
            "WireGuard 隧道已安装但未完成握手（{}）；endpoint={}。隧道状态：{}。请检查 Windows 防火墙、所选节点的 WireGuard UDP 端口和回程路由。",
            last_detail, config.endpoint, dump,
        ))
    }

    #[cfg(windows)]
    fn uninstall_and_wait(&self) -> Result<(), String> {
        // The service can exist while its interface is temporarily absent. Always
        // request uninstall so a stopped old tunnel cannot win the next install.
        tunnel_service::remove(&self.tunnel_name)
    }

    fn remove(&self) -> Result<(), String> {
        #[cfg(not(windows))]
        {
            return Err("真实 WireGuard 客户端仅支持 Windows；没有可断开的本地隧道".into());
        }
        #[cfg(windows)]
        {
            let _session = self.session_gate.lock().expect("session gate poisoned");
            let disconnect_started = std::time::Instant::now();
            if let Some(stop) = self
                .heartbeat_stop
                .lock()
                .expect("tunnel state poisoned")
                .take()
            {
                stop.store(true, Ordering::Release);
            }
            let active = self
                .active_config
                .lock()
                .expect("tunnel state poisoned")
                .take();
            let tunnel_result = self.uninstall_and_wait();
            let route_result = self.routes.clear();
            let proxy = self
                .node_proxy
                .lock()
                .expect("node proxy state poisoned")
                .take();
            let dns = self
                .dns_forwarder
                .lock()
                .expect("DNS forwarder state poisoned")
                .take();
            let p2p = self.p2p.lock().expect("p2p state poisoned").take();
            drop((proxy, dns, p2p));
            if let Some((path, public_key, device_id, mut auth, master_url, _, _)) = active {
                let _ = fs::remove_file(path);
                let job = std::thread::spawn(move || {
                    let result = auth.call(|session| {
                        revoke_on_master(&master_url, &session.token, &device_id, &public_key)
                    });
                    diagnostics::log(
                        "connection",
                        &format!("disconnect_revoke_complete result={result:?}"),
                    );
                    result
                });
                let mut jobs = self.pending_revokes.lock().expect("revoke jobs poisoned");
                jobs.retain(|job| !job.is_finished());
                jobs.push(job);
            }
            spawn_idle_firewall_cleanup(
                self.tunnel_name.clone(),
                Arc::clone(&self.session_gate),
                Arc::clone(&self.active_config),
            );
            diagnostics::log(
                "connection",
                &format!(
                    "disconnect_local_teardown_ms={} routes_ok={} tunnel_ok={} firewall=async",
                    disconnect_started.elapsed().as_millis(),
                    route_result.is_ok(),
                    tunnel_result.is_ok()
                ),
            );
            match (route_result, tunnel_result) {
                (Ok(()), Ok(())) => Ok(()),
                (route, tunnel) => {
                    let mut errors = Vec::new();
                    if let Err(error) = route {
                        errors.push(error);
                    }
                    if let Err(error) = tunnel {
                        errors.push(error);
                    }
                    Err(errors.join("；"))
                }
            }
        }
    }

    fn take_remote_revocation(&self) -> bool {
        self.remote_revoked.load(Ordering::Acquire)
    }

    fn wait_for_revocations(&self) -> Option<String> {
        let jobs = std::mem::take(&mut *self.pending_revokes.lock().expect("revoke jobs poisoned"));
        let mut errors = Vec::new();
        for job in jobs {
            match job.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error),
                Err(_) => errors.push("设备撤销线程异常".into()),
            }
        }
        (!errors.is_empty()).then(|| errors.join("；"))
    }
}

#[cfg(windows)]
fn spawn_idle_firewall_cleanup(
    name: String,
    gate: Arc<Mutex<()>>,
    active: Arc<Mutex<Option<ActiveConfig>>>,
) {
    std::thread::spawn(move || {
        if !when_session_idle(&gate, &active, || {
            if let Err(error) = sync_windows_port_forward_firewall(&name, &[]) {
                diagnostics::log(
                    "firewall",
                    &format!("disconnect_cleanup_failed error={error}"),
                );
            }
        }) {
            diagnostics::log("firewall", "stale_disconnect_cleanup_skipped");
        }
    });
}

#[cfg(any(windows, test))]
fn when_session_idle<T>(
    gate: &Mutex<()>,
    active: &Mutex<Option<T>>,
    cleanup: impl FnOnce(),
) -> bool {
    let _session = gate.lock().expect("session gate poisoned");
    if active.lock().expect("tunnel state poisoned").is_some() {
        return false;
    }
    cleanup();
    true
}

fn has_peer_handshake(output: &str, expected_peer: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let Some(peer) = fields.next() else {
            return false;
        };
        let Some(timestamp) = fields.next().and_then(|value| value.parse::<u64>().ok()) else {
            return false;
        };
        peer == expected_peer && timestamp > 0
    })
}

fn parse_wireguard_peer_traffic(output: &str, expected_peer: &str) -> Option<(u64, u64)> {
    output.lines().skip(1).find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first().copied() != Some(expected_peer) || fields.len() < 8 {
            return None;
        }
        // wg dump peer fields: public key, preshared key, endpoint,
        // allowed IPs, handshake seconds, handshake nanoseconds, RX, TX.
        let rx = fields.get(6)?.parse().ok()?;
        let tx = fields.get(7)?.parse().ok()?;
        Some((rx, tx))
    })
}

fn collect_traffic_stats(
    tunnel_name: &str,
    node_public_key: &str,
    p2p: &Arc<Mutex<Option<Arc<p2p::Transport>>>>,
) -> TrafficStats {
    let mut stats = TrafficStats::default();
    if let Some(transport) = p2p.lock().expect("p2p state poisoned").as_ref() {
        stats = *transport.traffic.lock().expect("p2p traffic poisoned");
    }
    #[cfg(windows)]
    if let Ok(output) = command_output_timeout(
        hidden_command(TunnelManager::wireguard_cli()).args(["show", tunnel_name, "dump"]),
        std::time::Duration::from_secs(3),
    ) {
        if output.status.success() {
            if let Some((rx, tx)) = parse_wireguard_peer_traffic(
                &String::from_utf8_lossy(&output.stdout),
                node_public_key,
            ) {
                stats.node_rx_bytes = rx;
                stats.node_tx_bytes = tx;
            }
        }
    }
    stats
}

// Serialize all fixed-name tunnel mutations with connection replacement. An
// old HTTP response must recheck cancellation *after* acquiring this gate.
fn live_session<'a>(
    gate: &'a Mutex<()>,
    stop: &AtomicBool,
) -> Option<std::sync::MutexGuard<'a, ()>> {
    let guard = gate.lock().expect("session gate poisoned");
    (!stop.load(Ordering::Acquire)).then_some(guard)
}

fn control_plane_login(username: &str, password: &str) -> Result<AuthSession, String> {
    diagnostics::log("auth", "login_request");
    let response = control_agent()
        .post(&format!("{}/v1/auth/login", control_plane_url()?))
        .send_json(LoginRequest { username, password })
        .map_err(|error| format!("登录失败：网络请求未完成：{error}"))?;
    let status = response.status();
    let mut body = response.into_body();
    if !(200..300).contains(&status.as_u16()) {
        let detail = body
            .read_json::<ControlPlaneError>()
            .map(|error| error.error)
            .unwrap_or_else(|_| "控制面未返回详细错误".into());
        return Err(format!("登录失败：HTTP {status}；{detail}"));
    }
    body.read_json::<LoginResponse>()
        .map(|response| AuthSession {
            token: response.token,
            user_id: response.user_id,
            username: response.username,
            role: response.role,
            allow_ipv4_global: response.allow_ipv4_global,
            allow_ipv6_global: response.allow_ipv6_global,
            allow_ipv4_split: response.allow_ipv4_split,
            allow_ipv6_split: response.allow_ipv6_split,
        })
        .map_err(|error| format!("登录响应无效：{error}"))
}

fn control_plane_logout(session: &AuthSession) -> Result<(), String> {
    let response = control_agent()
        .post(&format!("{}/v1/auth/logout", control_plane_url()?))
        .header("Authorization", &format!("Bearer {}", session.token))
        .send_empty()
        .map_err(|error| format!("退出控制面会话失败：{error}"))?;
    if response.status() == 204 || response.status() == 401 {
        Ok(())
    } else {
        Err(format!("退出控制面会话失败：HTTP {}", response.status()))
    }
}

fn control_plane_change_password(
    session: &AuthSession,
    current_password: &str,
    new_password: &str,
) -> Result<(), String> {
    let response = control_agent()
        .post(&format!("{}/v1/auth/password", control_plane_url()?))
        .header("Authorization", &format!("Bearer {}", session.token))
        .send_json(serde_json::json!({
            "current_password": current_password,
            "new_password": new_password,
        }))
        .map_err(|error| format!("修改密码失败：网络请求未完成：{error}"))?;
    let status = response.status();
    if status == 204 {
        return Ok(());
    }
    let detail = response
        .into_body()
        .read_json::<ControlPlaneError>()
        .map(|error| error.error)
        .unwrap_or_else(|_| "控制面未返回详细错误".into());
    Err(format!("修改密码失败：HTTP {status}；{detail}"))
}

fn control_plane_upload_diagnostics(
    session: &AuthSession,
    device_id: Option<&str>,
    bundle: Vec<u8>,
) -> Result<(String, usize), String> {
    let response = control_agent()
        .post(&format!("{}/v1/client/diagnostics", control_plane_url()?))
        .header("Authorization", &format!("Bearer {}", session.token))
        .header("Content-Type", "application/octet-stream")
        .header("X-Pmman-Client-Version", CLIENT_DISPLAY_VERSION)
        .header("X-Pmman-Device-Id", device_id.unwrap_or("unknown"))
        .send(bundle)
        .map_err(|error| format!("日志上报失败：网络请求未完成：{error}"))?;
    let status = response.status();
    let mut body = response.into_body();
    if !(200..300).contains(&status.as_u16()) {
        let detail = body
            .read_json::<ControlPlaneError>()
            .map(|error| error.error)
            .unwrap_or_else(|_| "主控未返回详细错误".into());
        return Err(format!("日志上报失败：HTTP {status}；{detail}"));
    }
    #[derive(Deserialize)]
    struct UploadResponse {
        report_id: String,
        received_bytes: usize,
    }
    body.read_json::<UploadResponse>()
        .map(|result| (result.report_id, result.received_bytes))
        .map_err(|error| format!("日志上报响应无效：{error}"))
}

fn login_state_path() -> PathBuf {
    env::var_os("PMMAN_AUTH_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(env::temp_dir)
                .join("Pmman Network")
                .join("login.json")
        })
}

#[cfg(windows)]
fn protect_login_password(password: &str) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let input = password.as_bytes();
    let input_len =
        u32::try_from(input.len()).map_err(|_| "登录凭据长度超出 Windows 限制".to_string())?;
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let protected = unsafe {
        CryptProtectData(
            &input_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output_blob,
        )
    };
    if protected == 0 {
        return Err(format!(
            "Windows 凭据保护失败：{}",
            io::Error::last_os_error()
        ));
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec()
    };
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(output_blob.pbData.cast());
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn protect_login_password(_password: &str) -> Result<Vec<u8>, String> {
    Err("登录凭据持久化仅支持 Windows DPAPI".into())
}

#[cfg(windows)]
fn unprotect_login_password(encrypted: &[u8]) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let input_len = u32::try_from(encrypted.len()).map_err(|_| "登录凭据长度无效".to_string())?;
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let unprotected = unsafe {
        CryptUnprotectData(
            &input_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output_blob,
        )
    };
    if unprotected == 0 {
        return Err(format!(
            "Windows 登录凭据已不可用：{}",
            io::Error::last_os_error()
        ));
    }
    let bytes = unsafe {
        std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec()
    };
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(output_blob.pbData.cast());
    }
    String::from_utf8(bytes).map_err(|_| "Windows 登录凭据格式无效".into())
}

#[cfg(not(windows))]
fn unprotect_login_password(_encrypted: &[u8]) -> Result<String, String> {
    Err("登录凭据持久化仅支持 Windows DPAPI".into())
}

fn save_login_credentials(username: &str, password: &str) -> Result<(), String> {
    let path = login_state_path();
    let encrypted_password = protect_login_password(password)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建登录状态目录失败：{error}"))?;
    }
    let data = serde_json::to_vec(&StoredLogin {
        username: username.to_string(),
        encrypted_password,
    })
    .map_err(|error| format!("编码登录状态失败：{error}"))?;
    atomic_write_file(&path, &data).map_err(|error| format!("保存登录状态失败：{error}"))
}

fn load_login_credentials() -> Result<Option<(String, String)>, String> {
    let path = login_state_path();
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取登录状态失败：{error}")),
    };
    let stored: StoredLogin =
        serde_json::from_slice(&data).map_err(|error| format!("登录状态文件无效：{error}"))?;
    let password = unprotect_login_password(&stored.encrypted_password)?;
    Ok(Some((stored.username, password)))
}

fn clear_login_credentials() -> Result<(), String> {
    match fs::remove_file(login_state_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("清除登录状态失败：{error}")),
    }
}

fn revoke_on_master(
    master_url: &str,
    token: &str,
    device_id: &str,
    public_key: &str,
) -> Result<(), String> {
    let response = control_agent()
        .post(&format!(
            "{}/v1/devices/{device_id}/revoke",
            master_url.trim_end_matches('/')
        ))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "public_key": public_key }))
        .map_err(|error| error.to_string())?;
    if (200..300).contains(&response.status().as_u16()) {
        Ok(())
    } else {
        Err(format!("控制面撤销设备失败：HTTP {}", response.status()))
    }
}

fn control_plane_heartbeat(
    master_url: &str,
    token: &str,
    device_id: &str,
    public_key: &str,
    config_version: u64,
    traffic: TrafficStats,
) -> Result<HeartbeatResult, String> {
    let response = control_agent()
        .post(&format!(
            "{}/v1/devices/{device_id}/heartbeat",
            master_url.trim_end_matches('/')
        ))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "traffic": traffic, "public_key": public_key, "config_version": config_version }))
        .map_err(|error| format!("心跳请求未完成：{error}"))?;
    match heartbeat_status(response.status().as_u16())? {
        HeartbeatResult::Active(_) => {
            let upload_mbps = response
                .headers()
                .get("x-pmman-upload-mbps")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok())
                .unwrap_or_default();
            let download_mbps = response
                .headers()
                .get("x-pmman-download-mbps")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok())
                .unwrap_or_default();
            Ok(HeartbeatResult::Active(NodeRateLimit {
                upload_mbps,
                download_mbps,
            }))
        }
        HeartbeatResult::Revoked => Ok(HeartbeatResult::Revoked),
    }
}

fn heartbeat_status(status: u16) -> Result<HeartbeatResult, String> {
    match status {
        204 => Ok(HeartbeatResult::Active(NodeRateLimit::default())),
        403 | 404 | 409 => Ok(HeartbeatResult::Revoked),
        401 => Err("主控 HTTP 401；登录会话已失效".into()),
        _ => Err(format!("主控 HTTP {status}；心跳暂不可用")),
    }
}

fn control_plane_config(
    master_url: &str,
    node_id: &str,
    token: &str,
    public_key: &str,
    ipv4_mode: &str,
    ipv6_mode: &str,
    timeout: std::time::Duration,
) -> Result<DeviceConfig, connection_retry::AttemptError> {
    use connection_retry::AttemptError;
    diagnostics::log(
        "connection",
        &format!(
            "config_request node={} ipv4_mode={} ipv6_mode={}",
            node_id, ipv4_mode, ipv6_mode
        ),
    );
    let response = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .new_agent()
        .post(&format!(
            "{}/v1/client/connections",
            master_url.trim_end_matches('/')
        ))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(RegisterRequest {
            node_id,
            public_key,
            ipv4_mode,
            ipv6_mode,
            split_route_source: split_routes::SOURCE,
        })
        .map_err(|error| {
            AttemptError::Failed(format!("控制面注册失败：网络请求未完成：{error}"))
        })?;
    let status = response.status();
    let mut body = response.into_body();
    if !(200..300).contains(&status.as_u16()) {
        let detail = body
            .read_json::<ControlPlaneError>()
            .map(|error| error.error)
            .unwrap_or_else(|_| "控制面未返回详细错误".into());
        if connection_retry::cleanup_pending(status.as_u16(), &detail) {
            return Err(AttemptError::CleanupPending);
        }
        return Err(AttemptError::Failed(format!(
            "控制面注册失败：HTTP {status}；{detail}"
        )));
    }
    body.read_json::<DeviceConfig>()
        .map_err(|error| AttemptError::Failed(format!("控制面返回的 WireGuard 配置无效：{error}")))
}

fn control_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(12)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn resource_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn is_newer_client_version(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(current),
    ) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

fn update_root() -> PathBuf {
    env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("PMMan")
        .join("updates")
}

fn update_manifest(channel: &str) -> Result<(ClientReleaseManifest, String), String> {
    if !matches!(channel, "stable" | "beta") {
        return Err("客户端更新渠道无效".into());
    }
    let response = control_agent()
        .get(&format!(
            "{}/v1/client/releases/{channel}/latest?platform=windows&arch=x64",
            control_plane_url()?
        ))
        .call()
        .map_err(|error| format!("更新检查失败：{error}"))?
        .into_body()
        .read_json::<ClientReleaseResponse>()
        .map_err(|error| format!("更新清单响应无效：{error}"))?;
    if !response.available {
        return Err("当前没有可用更新".into());
    }
    let manifest = response
        .manifest
        .ok_or_else(|| "更新清单缺少版本信息".to_string())?;
    let download = response
        .download
        .ok_or_else(|| "更新清单缺少下载地址".to_string())?;
    manifest.verify(
        include_str!("../../config/client-update-public-key.txt"),
        channel,
    )?;
    if manifest
        .mandatory_after
        .as_ref()
        .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_err())
    {
        return Err("强制更新日期无效".into());
    }
    if download
        != format!(
            "/v1/client/releases/{channel}/{}/download",
            manifest.version
        )
    {
        return Err("更新下载地址无效".into());
    }
    if !is_newer_client_version(&manifest.version, CLIENT_VERSION) {
        return Err("版本不是更新版本".into());
    }
    Ok((manifest, download))
}

fn atomic_write_file(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| "更新文件路径缺少文件名".to_string())?
        .to_string_lossy();
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));
    let result = (|| -> Result<(), String> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
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

fn check_client_update(snapshot: &mut ClientSnapshot, channel: &str) -> Result<(), String> {
    snapshot.client_update.current_version = CLIENT_DISPLAY_VERSION.into();
    snapshot.client_update.state = "checking".into();
    snapshot.client_update.message = "正在检查客户端更新".into();
    let result = update_manifest(channel);
    match result {
        Ok((manifest, _)) => {
            snapshot.client_update.release_notes = manifest.release_notes.clone();
            snapshot.client_update.available_version = Some(manifest.version);
            snapshot.client_update.mandatory = manifest
                .mandatory_after
                .as_ref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|t| {
                    t <= chrono::Utc::now()
                        && is_newer_client_version(&manifest.min_supported_version, CLIENT_VERSION)
                });
            snapshot.client_update.state = "available".into();
            snapshot.client_update.message = if manifest.release_notes.is_empty() {
                "发现新版本".into()
            } else {
                format!("发现新版本：{}", manifest.release_notes.join("；"))
            };
            Ok(())
        }
        Err(error) if error == "当前没有可用更新" => {
            snapshot.client_update.available_version = None;
            snapshot.client_update.mandatory = false;
            snapshot.client_update.state = "current".into();
            snapshot.client_update.message = "当前已是最新版本".into();
            Ok(())
        }
        Err(error) if error.contains("不是更新版本") => {
            snapshot.client_update.available_version = None;
            snapshot.client_update.mandatory = false;
            snapshot.client_update.state = "current".into();
            snapshot.client_update.message = "当前已是最新版本".into();
            Ok(())
        }
        Err(error) => {
            snapshot.client_update.state = "error".into();
            snapshot.client_update.message = error.clone();
            Err(error)
        }
    }
}

fn download_client_update(snapshot: &mut ClientSnapshot, channel: &str) -> Result<(), String> {
    let (manifest, download) = update_manifest(channel)?;
    snapshot.client_update.current_version = CLIENT_DISPLAY_VERSION.into();
    snapshot.client_update.available_version = Some(manifest.version.clone());
    snapshot.client_update.state = "downloading".into();
    snapshot.client_update.progress = 0;
    snapshot.client_update.message = "正在下载客户端更新".into();
    let response = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(300)))
        .max_redirects(0)
        .build()
        .new_agent()
        .get(&format!("{}{}", control_plane_url()?, download))
        .call()
        .map_err(|error| format!("客户端更新下载失败：{error}"))?;
    let directory = update_root().join(&manifest.version);
    fs::create_dir_all(&directory).map_err(|error| format!("创建更新目录失败：{error}"))?;
    #[cfg(windows)]
    {
        let result = hidden_command("icacls.exe")
            .arg(&directory)
            .args([
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        if !result.status.success() {
            return Err("无法保护更新目录".into());
        }
    }
    let temporary = directory.join(format!("update.part-{}", uuid::Uuid::new_v4()));
    let ready = directory.join("ready.exe");
    let mut body = response.into_body();
    let mut reader = body.as_reader();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    use std::io::Read;
    let mut received = 0u64;
    let mut digest = Sha256::new();
    let transfer = (|| -> Result<(), String> {
        let mut buffer = [0u8; 65536];
        loop {
            let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            received += count as u64;
            if received > manifest.size {
                return Err("更新文件超过声明大小".into());
            }
            file.write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
            digest.update(&buffer[..count]);
            snapshot.client_update.progress = ((received * 100 / manifest.size).min(99)) as u8;
            publish_update(snapshot);
        }
        file.sync_all().map_err(|e| e.to_string())?;
        if received != manifest.size
            || format!("{:x}", digest.finalize()) != manifest.sha256.to_lowercase()
        {
            return Err("更新文件大小或 SHA-256 校验失败".into());
        }
        Ok(())
    })();
    drop(file);
    if let Err(error) = transfer {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    // Publish the manifest before the ready marker. A crash can therefore
    // leave an ignored manifest, but never a ready executable paired with a
    // manifest from another download generation.
    atomic_write_file(
        &directory.join("manifest.json"),
        &serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
    )?;
    if ready.exists() {
        fs::remove_file(&ready).map_err(|e| e.to_string())?;
    }
    fs::rename(&temporary, &ready).map_err(|error| format!("准备更新文件失败：{error}"))?;
    snapshot.client_update.state = "ready".into();
    snapshot.client_update.progress = 100;
    snapshot.client_update.ready = true;
    snapshot.client_update.installer_path = Some(ready.to_string_lossy().into_owned());
    snapshot.client_update.sha256 = Some(manifest.sha256);
    snapshot.client_update.message =
        format!("版本 {} 已下载完成，断开连接后即可安装", manifest.version);
    Ok(())
}

fn preference_path() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Pmman Network")
        .join("preferences.json")
}
fn load_p2p_preference() -> bool {
    fs::read(preference_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| v.get("p2pEnabled").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}
fn save_p2p_preference(enabled: bool) -> Result<(), String> {
    let path = preference_path();
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("保存 P2P 偏好失败：{e}"))?;
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({"p2pEnabled":enabled})).unwrap(),
    )
    .map_err(|e| format!("保存 P2P 偏好失败：{e}"))
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn start_peer_sync(
    transport: Arc<p2p::Transport>,
    tunnel: String,
    master: String,
    mut auth: session::ConnectionAuth,
    device: String,
    public_key: String,
    stop: Arc<AtomicBool>,
    session_gate: Arc<Mutex<()>>,
    routes: Arc<route_manager::RouteManager>,
) {
    use pmman_protocol::p2p::{Advertisement, Directory, Peer};
    use std::collections::HashMap;
    std::thread::spawn(move || {
        let mut installed: HashMap<uuid::Uuid, (Peer, u16)> = HashMap::new();
        let mut synced = std::time::Instant::now();
        let mut last_directory: Option<Directory> = None;
        let mut underlay_checked = std::time::Instant::now();
        let routes_state = routes;
        while !stop.load(Ordering::Acquire) {
            if underlay_checked.elapsed() >= std::time::Duration::from_secs(10) {
                if let Err(error) = transport.refresh_underlay() {
                    *transport.message.lock().unwrap() =
                        format!("网络接口刷新失败，正在重试：{error}");
                }
                underlay_checked = std::time::Instant::now();
            }
            let input = Advertisement {
                public_key: public_key.clone(),
                enabled: transport.enabled.load(Ordering::Acquire),
                candidates: transport.candidates.lock().unwrap().clone(),
            };
            let result = auth.call(|session| {
                control_agent()
                    .post(&format!("{master}/v1/devices/{device}/p2p"))
                    .header("Authorization", &format!("Bearer {}", session.token))
                    .send_json(&input)
                    .map_err(|e| e.to_string())
                    .and_then(|response| {
                        let status = response.status().as_u16();
                        let body = response
                            .into_body()
                            .read_to_string()
                            .map_err(|e| e.to_string())?;
                        parse_peer_directory_response(status, &body)
                    })
            });
            {
                let Some(_session) = live_session(&session_gate, &stop) else {
                    break;
                };
                match result {
                    Ok(directory) => {
                        last_directory = Some(directory.clone());
                        synced = std::time::Instant::now();
                        let mut bridges = Vec::new();
                        installed.retain(|id, (peer, _)| {
                            let keep = directory.peers.iter().any(|p| {
                                p.device_id == *id
                                    && p.public_key == peer.public_key
                                    && p.secret == peer.secret
                            });
                            if !keep {
                                let _ = command_output_timeout(
                                    hidden_command(TunnelManager::wireguard_cli()).args([
                                        "set",
                                        &tunnel,
                                        "peer",
                                        &peer.public_key,
                                        "remove",
                                    ]),
                                    std::time::Duration::from_secs(3),
                                );
                                let _ = peer_routes(&routes_state, peer, false);
                            }
                            keep
                        });
                        let mut error = None;
                        for peer in &directory.peers {
                            if installed.contains_key(&peer.device_id) {
                                continue;
                            }
                            let result = (|| -> Result<(std::net::UdpSocket, u16), String> {
                                let bridge = std::net::UdpSocket::bind("127.0.0.1:0")
                                    .map_err(|e| e.to_string())?;
                                bridge.set_nonblocking(true).map_err(|e| e.to_string())?;
                                let port = bridge.local_addr().map_err(|e| e.to_string())?.port();
                                let routes = format!(
                                    "{}/32,{}/128",
                                    peer.ipv4.split('/').next().unwrap(),
                                    peer.ipv6.split('/').next().unwrap()
                                );
                                let output = command_output_timeout(
                                    hidden_command(TunnelManager::wireguard_cli()).args([
                                        "set",
                                        &tunnel,
                                        "peer",
                                        &peer.public_key,
                                        "endpoint",
                                        &format!("127.0.0.1:{port}"),
                                        "allowed-ips",
                                        &routes,
                                        "persistent-keepalive",
                                        "0",
                                    ]),
                                    std::time::Duration::from_secs(3),
                                )
                                .map_err(|e| e.to_string())?;
                                if !output.status.success() {
                                    return Err(TunnelManager::command_detail(&output));
                                }
                                if let Err(error) = peer_routes(&routes_state, peer, true) {
                                    let _ = command_output_timeout(
                                        hidden_command(TunnelManager::wireguard_cli()).args([
                                            "set",
                                            &tunnel,
                                            "peer",
                                            &peer.public_key,
                                            "remove",
                                        ]),
                                        std::time::Duration::from_secs(3),
                                    );
                                    return Err(error);
                                }
                                Ok((bridge, port))
                            })();
                            match result {
                                Ok((bridge, port)) => {
                                    installed.insert(peer.device_id, (peer.clone(), port));
                                    bridges.push((peer.clone(), bridge));
                                }
                                Err(detail) => error = Some(detail),
                            }
                        }
                        let _ = transport.update(directory, bridges);
                        *transport.message.lock().unwrap() = error
                            .map(|e| format!("互联 peer 配置失败：{e}"))
                            .unwrap_or_else(|| {
                                if transport.enabled.load(Ordering::Acquire) {
                                    "设备互联已就绪，优先直连"
                                } else {
                                    "设备互联已关闭"
                                }
                                .into()
                            });
                    }
                    Err(error) => {
                        *transport.message.lock().unwrap() = format!("设备互联暂不可用：{error}");
                        if synced.elapsed() > std::time::Duration::from_secs(30) {
                            for (peer, _) in installed.values() {
                                let _ = command_output_timeout(
                                    hidden_command(TunnelManager::wireguard_cli()).args([
                                        "set",
                                        &tunnel,
                                        "peer",
                                        &peer.public_key,
                                        "remove",
                                    ]),
                                    std::time::Duration::from_secs(3),
                                );
                                let _ = peer_routes(&routes_state, peer, false);
                            }
                            installed.clear();
                            if let Some(mut directory) = last_directory.clone() {
                                directory.peers.clear();
                                let _ = transport.update(directory, Vec::new());
                            }
                        }
                    }
                }
            }
            for _ in 0..10 {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    });
}

/// Decode the P2P directory response without turning an empty error response
/// into the misleading `json: EOF` message.  Axum intentionally returns an
/// empty body for some status-only failures (notably 503), while proxies may
/// return an HTML or empty 502.  The client needs to preserve the distinction
/// because a directory failure is recoverable and does not invalidate an
/// already installed peer set.
fn parse_peer_directory_response(
    status: u16,
    body: &str,
) -> Result<pmman_protocol::p2p::Directory, String> {
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<ControlPlaneError>(body)
            .ok()
            .map(|error| error.error)
            .filter(|error| !error.trim().is_empty())
            .unwrap_or_else(|| match status {
                401 => "登录会话已失效".into(),
                403 => "当前账户无权使用客户端互联".into(),
                404 => "设备不存在或已被撤销".into(),
                502 => "主控暂时无法访问节点".into(),
                503 => "当前没有健康节点".into(),
                _ => "主控未返回详细错误".into(),
            });
        return Err(format!("主控 HTTP {status}；{detail}"));
    }
    serde_json::from_str(body).map_err(|error| format!("主控返回的互联目录无效：{error}"))
}

#[cfg(windows)]
fn peer_routes(
    routes: &route_manager::RouteManager,
    peer: &pmman_protocol::p2p::Peer,
    add: bool,
) -> Result<(), String> {
    // wg set changes cryptokey routing only. The shared route manager adds the
    // matching host routes and removes only routes owned by this tunnel.
    let ipv4 = peer
        .ipv4
        .split('/')
        .next()
        .unwrap_or("")
        .parse::<std::net::Ipv4Addr>()
        .map_err(|e| format!("互联 IPv4 地址无效：{e}"))?;
    let ipv6 = peer
        .ipv6
        .split('/')
        .next()
        .unwrap_or("")
        .parse::<std::net::Ipv6Addr>()
        .map_err(|e| format!("互联 IPv6 地址无效：{e}"))?;
    for (destination, prefix_len) in [
        (std::net::IpAddr::V4(ipv4), 32),
        (std::net::IpAddr::V6(ipv6), 128),
    ] {
        let route = route_manager::RouteSpec {
            destination,
            prefix_len,
            metric: 5,
        };
        if add {
            routes.add(route)?;
        } else {
            routes.remove(&route)?;
        }
    }
    Ok(())
}

fn node_latency(endpoint: &str) -> Option<u64> {
    udp_latency(endpoint).or_else(|| icmp_latency(endpoint))
}

fn apply_probe_health(nodes: &mut [NodeInfo], master_health: &[bool]) {
    let measured_any = nodes.iter().any(|node| node.latency_ms.is_some());
    for (node, control_plane_healthy) in nodes.iter_mut().zip(master_health.iter().copied()) {
        node.healthy = if measured_any {
            node.latency_ms.is_some()
        } else {
            control_plane_healthy
        };
    }
}

fn icmp_latency(endpoint: &str) -> Option<u64> {
    use std::net::ToSocketAddrs;
    let address = endpoint.to_socket_addrs().ok()?.next()?;
    let started = std::time::Instant::now();
    let mut command = hidden_command("ping.exe");
    if address.ip().is_ipv6() {
        command.arg("-6");
    }
    let output = command
        .args(["-n", "1", "-w", "700"])
        .arg(address.ip().to_string())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| started.elapsed().as_millis() as u64)
}

fn udp_latency(endpoint: &str) -> Option<u64> {
    use std::net::{ToSocketAddrs, UdpSocket};
    let mut addresses: Vec<_> = endpoint.to_socket_addrs().ok()?.collect();
    addresses.sort_by_key(|address| !address.is_ipv4());
    for address in addresses.into_iter().take(4) {
        let socket = UdpSocket::bind(if address.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })
        .ok()?;
        if socket.connect(address).is_err() {
            continue;
        }
        let mut samples = Vec::new();
        for _ in 0..3 {
            let mut packet = [0u8; UDP_PING_SIZE];
            packet[..8].copy_from_slice(UDP_PING_MAGIC);
            packet[8..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            let started = std::time::Instant::now();
            let deadline = started + std::time::Duration::from_millis(400);
            if socket.send(&packet).is_err() {
                continue;
            }
            while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                let _ = socket.set_read_timeout(Some(remaining));
                let mut response = [0u8; UDP_PING_SIZE + 1];
                match socket.recv(&mut response) {
                    Ok(length) if length == UDP_PING_SIZE && response[..length] == packet => {
                        samples.push(started.elapsed().as_micros() as u64);
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
        }
        if !samples.is_empty() {
            samples.sort_unstable();
            return Some(samples[samples.len() / 2].div_ceil(1000));
        }
    }
    None
}

fn fetch_nodes(token: &str) -> Result<Vec<NodeInfo>, String> {
    let response = control_agent()
        .get(&format!("{}/v1/client/nodes", control_plane_url()?))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| format!("节点列表获取失败：{error}"))?;
    let status = response.status();
    let mut body = response.into_body();
    let text = body
        .read_to_string()
        .map_err(|error| format!("节点列表响应读取失败：{error}"))?;
    if !(200..300).contains(&status.as_u16()) {
        let detail = serde_json::from_str::<ControlPlaneError>(&text)
            .ok()
            .map(|error| error.error)
            .filter(|error| !error.trim().is_empty())
            .unwrap_or_else(|| match status.as_u16() {
                401 => "登录会话已失效，请重新登录或等待自动恢复".into(),
                403 => "当前账户无权查看节点".into(),
                503 => "主控当前没有可用节点".into(),
                _ => "主控未返回详细错误".into(),
            });
        return Err(format!("主控 HTTP {}；{}", status.as_u16(), detail));
    }
    serde_json::from_str(&text).map_err(|error| format!("节点列表响应无效：{error}"))
}

fn control_plane_port_forward_entries(token: &str) -> Result<Vec<PortForwardEntryView>, String> {
    let response = control_agent()
        .get(&format!(
            "{}/v1/client/port-forwards/entries",
            control_plane_url()?
        ))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| format!("入口列表获取失败：{error}"))?;
    let status = response.status();
    let text = response
        .into_body()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    if !(200..300).contains(&status.as_u16()) {
        let detail = serde_json::from_str::<ControlPlaneError>(&text)
            .map(|error| error.error)
            .unwrap_or_else(|_| "主控未返回详细错误".into());
        return Err(format!("主控 HTTP {}；{}", status.as_u16(), detail));
    }
    serde_json::from_str(&text).map_err(|error| format!("入口列表响应无效：{error}"))
}

fn control_plane_port_forward_quota(token: &str) -> Result<PortForwardQuota, String> {
    let response = control_agent()
        .get(&format!(
            "{}/v1/client/port-forwards/quota",
            control_plane_url()?
        ))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| format!("端口转发配额获取失败：{error}"))?;
    let status = response.status();
    let mut body = response.into_body();
    if !(200..300).contains(&status.as_u16()) {
        let detail = body
            .read_json::<ControlPlaneError>()
            .map(|error| error.error)
            .unwrap_or_else(|_| "主控未返回详细错误".into());
        return Err(format!("端口转发配额获取失败：HTTP {}；{}", status, detail));
    }
    body.read_json()
        .map_err(|error| format!("端口转发配额响应无效：{error}"))
}

fn control_plane_port_forwards(token: &str) -> Result<Vec<PortForwardView>, String> {
    let response = control_agent()
        .get(&format!("{}/v1/client/port-forwards", control_plane_url()?))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| format!("端口转发列表获取失败：{error}"))?;
    let status = response.status();
    let text = response
        .into_body()
        .read_to_string()
        .map_err(|error| error.to_string())?;
    if !(200..300).contains(&status.as_u16()) {
        return Err(format!(
            "主控 HTTP {}；端口转发列表获取失败",
            status.as_u16()
        ));
    }
    serde_json::from_str(&text).map_err(|error| format!("端口转发列表响应无效：{error}"))
}

fn control_plane_create_port_forward(
    token: &str,
    device_id: &str,
    entry_id: Option<String>,
    protocols: Vec<PortForwardProtocol>,
    target_port: u16,
) -> Result<PortForwardView, String> {
    let response = control_agent()
        .post(&format!("{}/v1/client/port-forwards", control_plane_url()?))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "device_id": device_id, "entry_id": entry_id, "protocols": protocols, "target_port": target_port }))
        .map_err(|error| format!("创建端口转发失败：{error}"))?;
    let status = response.status();
    let mut body = response.into_body();
    if !(200..300).contains(&status.as_u16()) {
        let detail = body
            .read_json::<ControlPlaneError>()
            .map(|error| error.error)
            .unwrap_or_else(|_| "主控未返回详细错误".into());
        return Err(format!(
            "创建端口转发失败：HTTP {}；{}",
            status.as_u16(),
            detail
        ));
    }
    body.read_json()
        .map_err(|error| format!("端口转发响应无效：{error}"))
}

fn control_plane_release_port_forward(token: &str, forward_id: &str) -> Result<(), String> {
    let response = control_agent()
        .delete(&format!(
            "{}/v1/client/port-forwards/{forward_id}",
            control_plane_url()?
        ))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(|error| format!("释放端口转发失败：{error}"))?;
    if (200..300).contains(&response.status().as_u16()) {
        Ok(())
    } else {
        Err(format!("释放端口转发失败：HTTP {}", response.status()))
    }
}

/// Port-forward requests travel through the generic IPC error channel. Keep
/// the feature-specific message for the page, but also replace the generic
/// snapshot message so the UI does not display a stale connection success
/// message when the request fails.
fn set_port_forward_error(snapshot: &mut ClientSnapshot, error: impl Into<String>) {
    let error = error.into();
    snapshot.port_forward_message = error.clone();
    snapshot.message = error;
}

// The tunnel / master mutation has already succeeded. Local firewall sync is
// a separate, recoverable outcome, never a reason to report that operation failed.
fn firewall_sync_response(
    snapshot: &mut ClientSnapshot,
    operation: &str,
    result: Result<(), String>,
) -> IpcResponse {
    match result {
        Ok(()) => {
            if !snapshot.port_forward_firewall_warning.is_empty() {
                diagnostics::log("firewall", &format!("sync_recovered operation={operation}"));
            }
            snapshot.port_forward_firewall_warning.clear();
        }
        Err(error) => {
            diagnostics::log(
                "firewall",
                &format!("sync_failed operation={operation} error={error}"),
            );
            snapshot.port_forward_firewall_warning = format!(
                "Windows 防火墙规则同步失败：{error}；请手动检查 pmman-client 接口上目标端口和协议的入站规则。"
            );
        }
    }
    IpcResponse {
        ok: true,
        status: snapshot.clone(),
    }
}

fn is_session_expired_error(error: &str) -> bool {
    error.split('；').next().unwrap_or("").contains("HTTP 401")
}

fn update_authenticated_snapshot(snapshot: &mut ClientSnapshot, session: &AuthSession) {
    snapshot.auth_restore_pending = false;
    snapshot.authenticated = true;
    snapshot.user_id = Some(session.user_id.clone());
    snapshot.username = Some(session.username.clone());
    snapshot.role = Some(session.role.clone());
    snapshot.allow_ipv4_global = session.allow_ipv4_global;
    snapshot.allow_ipv6_global = session.allow_ipv6_global;
    snapshot.allow_ipv4_split = session.allow_ipv4_split;
    snapshot.allow_ipv6_split = session.allow_ipv6_split;

    // A permission change can revoke the mode that was selected before the
    // refresh.  Do not leave the UI showing an option that the next connect
    // request will reject.  An active tunnel keeps its actual mode until the
    // heartbeat/revocation path tears it down.
    if snapshot.state != "connected" {
        if snapshot.ipv4_mode == "global" && !session.allow_ipv4_global
            || snapshot.ipv4_mode == "split" && !session.allow_ipv4_split
        {
            snapshot.ipv4_mode = "internal".into();
        }
        if snapshot.ipv6_mode == "global" && !session.allow_ipv6_global
            || snapshot.ipv6_mode == "split" && !session.allow_ipv6_split
        {
            snapshot.ipv6_mode = "internal".into();
        }
    }
}

fn refresh_authenticated_session(
    state: &ServiceState,
    snapshot: &mut ClientSnapshot,
) -> Result<AuthSession, String> {
    let current = state
        .auth
        .lock()
        .expect("auth state poisoned")
        .clone()
        .ok_or_else(|| "登录会话已失效，请重新登录".to_string())?;
    let session = state.connection_auth(&current).refresh()?;
    update_authenticated_snapshot(snapshot, &session);
    Ok(session)
}

fn choose_node_with_session_refresh(
    state: &ServiceState,
    snapshot: &mut ClientSnapshot,
    session: AuthSession,
    selected_node_id: Option<&str>,
) -> Result<(AuthSession, NodeInfo, Vec<NodeInfo>), String> {
    match choose_node(&session.token, selected_node_id) {
        Ok((node, nodes)) => Ok((session, node, nodes)),
        Err(error) if is_session_expired_error(&error) => {
            // The master keeps sessions in memory.  A master restart therefore
            // invalidates the bearer token even though the user's credentials
            // are still valid.  Re-authenticate once and retry the exact
            // operation so refresh/select/connect all recover consistently.
            let refreshed = refresh_authenticated_session(state, snapshot)?;
            choose_node(&refreshed.token, selected_node_id)
                .map(|(node, nodes)| (refreshed, node, nodes))
        }
        Err(error) => Err(error),
    }
}

fn apply_authenticated_snapshot(
    snapshot: &mut ClientSnapshot,
    session: &AuthSession,
    nodes: Vec<NodeInfo>,
    message: String,
) {
    snapshot.auth_restore_pending = false;
    snapshot.state = "disconnected".into();
    snapshot.authenticated = true;
    snapshot.user_id = Some(session.user_id.clone());
    snapshot.username = Some(session.username.clone());
    snapshot.role = Some(session.role.clone());
    snapshot.allow_ipv4_global = session.allow_ipv4_global;
    snapshot.allow_ipv6_global = session.allow_ipv6_global;
    snapshot.allow_ipv4_split = session.allow_ipv4_split;
    snapshot.allow_ipv6_split = session.allow_ipv6_split;
    snapshot.ipv4_mode = "internal".into();
    snapshot.ipv6_mode = "internal".into();
    snapshot.nodes = nodes;
    snapshot.node_rate_limit = NodeRateLimit::default();
    snapshot.selected_node_id = None;
    snapshot.selected_node_name = None;
    snapshot.node_selection = "auto".into();
    snapshot.message = message;
}

fn commit_restored_login(
    state: &ServiceState,
    session: &AuthSession,
    nodes: Vec<NodeInfo>,
    generation: u64,
) -> bool {
    // Use handle_request's lock order, and commit auth/UI together so a
    // logout or replacement login cannot be overwritten by a late response.
    let _command = state.command_gate.lock().expect("command gate poisoned");
    let mut snapshot = state.snapshot.lock().expect("service state poisoned");
    let mut auth = state.auth.lock().expect("auth state poisoned");
    if auth.is_some() || state.auth_generation.load(Ordering::Acquire) != generation {
        return false;
    }
    *auth = Some(session.clone());
    apply_authenticated_snapshot(&mut snapshot, session, nodes, "已恢复登录状态".into());
    true
}

fn restore_login_in_background(state: ServiceState, credentials: Option<(String, String)>) {
    let Some((username, password)) = credentials else {
        return;
    };
    let restore_generation = state.auth_generation.load(Ordering::Acquire);
    std::thread::spawn(move || {
        let result = control_plane_login(&username, &password);
        match result {
            Ok(session) => {
                let nodes = fetch_nodes(&session.token).unwrap_or_default();
                if !commit_restored_login(&state, &session, nodes, restore_generation) {
                    let _ = control_plane_logout(&session);
                }
            }
            Err(_) => {
                let _command = state.command_gate.lock().expect("command gate poisoned");
                let mut snapshot = state.snapshot.lock().expect("service state poisoned");
                snapshot.auth_restore_pending = false;
                if !snapshot.authenticated
                    && state.auth_generation.load(Ordering::Acquire) == restore_generation
                {
                    snapshot.message = "登录状态已失效，请重新登录".into();
                }
            }
        }
    });
}

fn choose_node(
    token: &str,
    selected_node_id: Option<&str>,
) -> Result<(NodeInfo, Vec<NodeInfo>), String> {
    let mut nodes = fetch_nodes(token)?;
    if nodes.is_empty() {
        return Err("主控当前没有可用节点".into());
    }
    // The master health bit is a recent control-plane heartbeat.  UDP/ICMP
    // probes add latency information, but a network that drops both probe
    // protocols must not prevent us from attempting the authoritative
    // WireGuard connection.  Keep the original control-plane health so we
    // can fail open only when every probe was filtered, without ever
    // fabricating a latency value.
    let master_health: Vec<bool> = nodes.iter().map(|node| node.healthy).collect();
    for batch in nodes.chunks_mut(16) {
        std::thread::scope(|scope| {
            for node in batch {
                scope.spawn(move || {
                    node.latency_ms = node_latency(&node.probe_endpoint);
                });
            }
        });
    }
    apply_probe_health(&mut nodes, &master_health);
    let selected = if let Some(node_id) = selected_node_id {
        nodes
            .iter()
            .find(|node| node.node_id == node_id && node.healthy)
            .cloned()
            .ok_or_else(|| format!("指定节点不可用：{node_id}"))?
    } else {
        nodes
            .iter()
            .filter(|node| node.healthy)
            .min_by_key(|node| node.latency_ms.unwrap_or(u64::MAX))
            .cloned()
            .ok_or_else(|| "所有节点均不可达".to_string())?
    };
    Ok((selected, nodes))
}

fn connection_error_response(snapshot: &mut ClientSnapshot, error: String) -> IpcResponse {
    let cancelled = error == connection_retry::CANCELLED;
    diagnostics::log(
        "connection",
        &format!(
            "{} error={error}",
            if cancelled {
                "connect_cancelled"
            } else {
                "connect_failed"
            }
        ),
    );
    snapshot.state = if cancelled { "disconnected" } else { "error" }.into();
    snapshot.connection_waiting_cleanup = false;
    snapshot.tunnel_mode = None;
    snapshot.message = error;
    // Intentional cancellation must survive send_ipc as a successful outcome;
    // an IPC failure would be converted back to state=error by Tauri.
    IpcResponse {
        ok: cancelled,
        status: snapshot.clone(),
    }
}

fn handle_request(state: &ServiceState, request: IpcRequest) -> IpcResponse {
    if matches!(request.action, Action::Status) {
        return handle_request_inner(state, request);
    }
    if matches!(request.action, Action::CancelConnect) {
        let mut status = state.snapshot.lock().expect("service state poisoned");
        let accepted = status.state == "connecting" && status.connection_waiting_cleanup;
        if accepted {
            state
                .tunnel
                .connect_cancelled
                .store(true, Ordering::Release);
            status.message = "正在取消连接…".into();
            diagnostics::log("connection", "address_cleanup_cancel_requested");
        }
        let mut result = status.clone();
        if !accepted {
            result.message = "当前不在等待地址切换".into();
        }
        return IpcResponse {
            ok: accepted,
            status: result,
        };
    }
    // Serialize mutations, not the transport or status readers. Do not queue
    // unbounded blocking workers behind a slow connect/disconnect operation.
    let Ok(_command) = state.command_gate.try_lock() else {
        let mut status = state
            .snapshot
            .lock()
            .expect("service state poisoned")
            .clone();
        status.message = "正在处理其他操作，请稍后重试".into();
        return IpcResponse { ok: false, status };
    };
    let started = std::time::Instant::now();
    let action = format!("{:?}", request.action);
    let invitation = matches!(request.action, Action::Invitation);
    if matches!(request.action, Action::Connect) {
        state
            .tunnel
            .connect_cancelled
            .store(false, Ordering::Release);
    }
    let pending = match request.action {
        Action::Connect => Some(("connecting", "正在连接")),
        Action::Disconnect | Action::Logout => Some(("disconnecting", "正在断开连接")),
        _ => None,
    };
    // The handler works on a private snapshot. Network I/O, route teardown and
    // worker joins must never own the shared UI snapshot mutex.
    let mut response = handle_request_inner_with_pending(state, request, pending);
    response.status.connection_waiting_cleanup = false;
    if !invitation {
        apply_remote_revocation(state, &mut response.status);
        *state.snapshot.lock().expect("service state poisoned") = response.status.clone();
    }
    diagnostics::log(
        "ipc",
        &format!(
            "action={action} completed ok={} elapsed_ms={}",
            response.ok,
            started.elapsed().as_millis()
        ),
    );
    response
}

fn apply_remote_revocation(state: &ServiceState, snapshot: &mut ClientSnapshot) {
    if state.tunnel.auth_expired.load(Ordering::Acquire)
        && state.auth.try_lock().is_ok_and(|auth| auth.is_none())
        && snapshot.authenticated
    {
        snapshot.authenticated = false;
        snapshot.auth_restore_pending = false;
        snapshot.message = "登录凭据已失效，请重新登录".into();
    }
    if state.tunnel.take_remote_revocation() && snapshot.tunnel_mode.is_some() {
        snapshot.state = "disconnected".into();
        snapshot.ipv4 = None;
        snapshot.ipv6 = None;
        snapshot.allowed_ips.clear();
        snapshot.node_dns_servers.clear();
        snapshot.dns_servers.clear();
        snapshot.dns_split_enabled = false;
        snapshot.tunnel_mode = None;
        snapshot.split_route_source = None;
        snapshot.split_route_version = None;
        snapshot.split_route_snapshot = None;
        snapshot.split_ipv4_prefix_count = 0;
        snapshot.split_ipv6_prefix_count = 0;
        snapshot.split_route_install_ms = 0;
        snapshot.message = "连接已被服务端撤销".into();
        if state.tunnel.auth_expired.load(Ordering::Acquire) {
            snapshot.authenticated = false;
            snapshot.auth_restore_pending = false;
            snapshot.message = "登录凭据已失效，请重新登录".into();
        }
    }
}

fn handle_request_inner(state: &ServiceState, request: IpcRequest) -> IpcResponse {
    handle_request_inner_with_pending(state, request, None)
}

fn handle_request_inner_with_pending(
    state: &ServiceState,
    request: IpcRequest,
    pending: Option<(&str, &str)>,
) -> IpcResponse {
    if matches!(request.action, Action::Invitation) {
        diagnostics::log("invitation", "request");
        return invitations::handle(state, request.invitation_request);
    }
    let mut snapshot = state
        .snapshot
        .lock()
        .expect("service state poisoned")
        .clone();
    if let Some((phase, message)) = pending {
        let mut shared = state.snapshot.lock().expect("service state poisoned");
        shared.state = phase.into();
        shared.message = message.into();
        shared.connection_waiting_cleanup = false;
    }
    if !matches!(&request.action, Action::Status) {
        diagnostics::log("ipc", &format!("action={:?}", request.action));
    }
    match request.action {
        Action::CancelConnect => unreachable!("handled before the mutation gate"),
        Action::Invitation => unreachable!(),
        Action::Status => {
            snapshot.client_update = update_state().lock().unwrap().clone();
            snapshot.p2p_enabled = state.tunnel.p2p_enabled.load(Ordering::Relaxed);
            if let Ok(traffic) = state.tunnel.traffic.try_lock() {
                snapshot.traffic_stats = *traffic;
            }
            if let Ok(proxy) = state.tunnel.node_proxy.try_lock() {
                // Heartbeat updates the live limiter while a tunnel remains
                // connected. Reflect that value in the UI instead of showing
                // the limit captured at connect time indefinitely.
                if let Some(proxy) = proxy.as_ref() {
                    snapshot.node_rate_limit = proxy.limits();
                }
            }
            let transport = state.tunnel.p2p.try_lock().ok().map(|value| value.clone());
            if let Some(Some(transport)) = &transport {
                if let Ok(peers) = transport.statuses.try_lock() {
                    snapshot.p2p_peers = peers.clone();
                }
                if let Ok(message) = transport.message.try_lock() {
                    snapshot.p2p_message = message.clone();
                }
                if let Ok(upnp_status) = transport.upnp_status.try_lock() {
                    if !upnp_status.is_empty() {
                        snapshot.p2p_message = format!("{}；{}", snapshot.p2p_message, upnp_status);
                    }
                }
            } else if matches!(transport, Some(None)) {
                snapshot.p2p_peers.clear();
                snapshot.p2p_message.clear();
            }
            if let Ok(auth) = state.auth.try_lock() {
                if let Some(session) = auth
                    .as_ref()
                    .filter(|session| snapshot.user_id.as_deref() == Some(session.user_id.as_str()))
                {
                    update_authenticated_snapshot(&mut snapshot, session);
                }
            }
            apply_remote_revocation(state, &mut snapshot);
            IpcResponse {
                ok: true,
                status: snapshot.clone(),
            }
        }
        Action::CheckUpdate | Action::DownloadUpdate => {
            if !UPDATE_BUSY.swap(true, Ordering::AcqRel) {
                let download = matches!(request.action, Action::DownloadUpdate);
                let channel = request.update_channel.unwrap_or_else(|| "stable".into());
                let mut current = snapshot.clone();
                current.client_update = update_state().lock().unwrap().clone();
                current.client_update.state =
                    if download { "downloading" } else { "checking" }.into();
                current.client_update.message = if download {
                    "正在下载更新"
                } else {
                    "正在检查更新"
                }
                .into();
                if download {
                    current.client_update.ready = false;
                    current.client_update.installer_path = None;
                    current.client_update.sha256 = None;
                    current.client_update.progress = 0;
                }
                publish_update(&current);
                std::thread::spawn(move || {
                    let result = if download {
                        download_client_update(&mut current, &channel)
                    } else {
                        check_client_update(&mut current, &channel)
                    };
                    if let Err(error) = result {
                        current.client_update.state = "error".into();
                        current.client_update.message = error;
                        current.client_update.ready = false;
                    }
                    publish_update(&current);
                    UPDATE_BUSY.store(false, Ordering::Release);
                });
            }
            snapshot.client_update = update_state().lock().unwrap().clone();
            IpcResponse {
                ok: true,
                status: snapshot.clone(),
            }
        }
        Action::UploadDiagnostics => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                snapshot.diagnostic_upload_state = "error".into();
                snapshot.diagnostic_upload_message = "请先登录后上报日志".into();
                return IpcResponse {
                    ok: true,
                    status: snapshot.clone(),
                };
            };
            snapshot.diagnostic_upload_state = "uploading".into();
            snapshot.diagnostic_upload_message = "正在整理日志".into();
            let summary = format!(
                "version={} state={} node={} device={}",
                CLIENT_DISPLAY_VERSION,
                snapshot.state,
                snapshot.selected_node_id.as_deref().unwrap_or("none"),
                snapshot.device_id.as_deref().unwrap_or("none")
            );
            let result = diagnostics::bundle(&summary).and_then(|bundle| {
                state.connection_auth(&session).call(|session| {
                    control_plane_upload_diagnostics(
                        session,
                        snapshot.device_id.as_deref(),
                        bundle.clone(),
                    )
                })
            });
            match result {
                Ok((report_id, bytes)) => {
                    diagnostics::log(
                        "diagnostics",
                        &format!("upload_succeeded report_id={} bytes={bytes}", report_id),
                    );
                    snapshot.diagnostic_upload_state = "uploaded".into();
                    snapshot.diagnostic_upload_id = Some(report_id.clone());
                    snapshot.diagnostic_upload_message =
                        format!("日志已上报，报告编号：{report_id}");
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    diagnostics::log("diagnostics", &format!("upload_failed error={error}"));
                    snapshot.diagnostic_upload_state = "error".into();
                    snapshot.diagnostic_upload_message = error;
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::SetP2p => {
            let Some(enabled) = request.p2p_enabled else {
                return IpcResponse {
                    ok: false,
                    status: ClientSnapshot {
                        message: "缺少 P2P 开关值".into(),
                        ..snapshot.clone()
                    },
                };
            };
            if let Err(error) = save_p2p_preference(enabled) {
                return IpcResponse {
                    ok: false,
                    status: ClientSnapshot {
                        message: error,
                        ..snapshot.clone()
                    },
                };
            }
            state.tunnel.p2p_enabled.store(enabled, Ordering::Release);
            snapshot.p2p_enabled = enabled;
            snapshot.p2p_message = if enabled {
                "设备互联已开启"
            } else {
                "设备互联已关闭"
            }
            .into();
            IpcResponse {
                ok: true,
                status: snapshot.clone(),
            }
        }
        Action::ListPortForwardEntries => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                set_port_forward_error(&mut snapshot, "请先登录后查看端口转发入口");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            match control_plane_port_forward_entries(&session.token) {
                Ok(entries) => {
                    snapshot.port_forward_entries = entries;
                    snapshot.port_forward_message = "入口列表已更新".into();
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    set_port_forward_error(&mut snapshot, error);
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::ListPortForwardQuota => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                set_port_forward_error(&mut snapshot, "请先登录后查看端口转发配额");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            match control_plane_port_forward_quota(&session.token) {
                Ok(quota) => {
                    snapshot.port_forward_quota = quota;
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    set_port_forward_error(&mut snapshot, error);
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::ListPortForwards => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                set_port_forward_error(&mut snapshot, "请先登录后查看端口转发");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            match control_plane_port_forwards(&session.token) {
                Ok(forwards) => {
                    let firewall_result = if state
                        .tunnel
                        .active_config
                        .lock()
                        .expect("tunnel state poisoned")
                        .is_some()
                    {
                        state.tunnel.sync_port_forward_firewall(&forwards)
                    } else {
                        // A disconnected client cannot safely bind rules to a
                        // missing tunnel interface; also remove any stale
                        // rules left by a previous process.
                        state.tunnel.sync_port_forward_firewall(&[])
                    };
                    snapshot.port_forwards = forwards;
                    snapshot.port_forward_message = "端口转发列表已更新".into();
                    firewall_sync_response(&mut snapshot, "list", firewall_result)
                }
                Err(error) => {
                    set_port_forward_error(&mut snapshot, error);
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::CreatePortForward => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                set_port_forward_error(&mut snapshot, "请先登录后创建端口转发");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            let device_id = request.device_id.or_else(|| snapshot.device_id.clone());
            let Some(device_id) = device_id else {
                set_port_forward_error(&mut snapshot, "当前没有可用的已连接设备");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            let Some(protocols) = request.protocols else {
                set_port_forward_error(&mut snapshot, "请选择转发协议");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            let Some(target_port) = request.target_port else {
                set_port_forward_error(&mut snapshot, "请输入目标端口");
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            match control_plane_create_port_forward(
                &session.token,
                &device_id,
                request.entry_id,
                protocols,
                target_port,
            ) {
                Ok(forward) => {
                    snapshot
                        .port_forwards
                        .retain(|item| item.forward_id != forward.forward_id);
                    snapshot.port_forwards.push(forward);
                    let firewall_result = state
                        .tunnel
                        .sync_port_forward_firewall(&snapshot.port_forwards);
                    snapshot.port_forward_message = "端口转发已建立".into();
                    firewall_sync_response(&mut snapshot, "create", firewall_result)
                }
                Err(error) => {
                    set_port_forward_error(&mut snapshot, error);
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::ReleasePortForward => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                let mut status = snapshot.clone();
                set_port_forward_error(&mut status, "请先登录");
                return IpcResponse { ok: false, status };
            };
            let Some(forward_id) = request.forward_id else {
                let mut status = snapshot.clone();
                set_port_forward_error(&mut status, "缺少转发 ID");
                return IpcResponse { ok: false, status };
            };
            match control_plane_release_port_forward(&session.token, &forward_id) {
                Ok(()) => {
                    snapshot
                        .port_forwards
                        .retain(|item| item.forward_id != forward_id);
                    let firewall_result = state
                        .tunnel
                        .sync_port_forward_firewall(&snapshot.port_forwards);
                    snapshot.port_forward_message = "端口转发已释放".into();
                    firewall_sync_response(&mut snapshot, "release", firewall_result)
                }
                Err(error) => {
                    set_port_forward_error(&mut snapshot, error);
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::Login => {
            let username = request.username.unwrap_or_default();
            let password = request.password.unwrap_or_default();
            if username.is_empty() || password.is_empty() {
                snapshot.state = "error".into();
                snapshot.message = "请输入用户名和密码".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            }
            if state
                .tunnel
                .active_config
                .lock()
                .expect("tunnel state poisoned")
                .is_some()
            {
                snapshot.message = "请先断开当前隧道再切换账户".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            }
            state.auth_generation.fetch_add(1, Ordering::AcqRel);
            match control_plane_login(&username, &password) {
                Ok(session) => {
                    diagnostics::log("auth", "login_succeeded");
                    let nodes = fetch_nodes(&session.token).unwrap_or_default();
                    let message = if nodes.is_empty() {
                        "登录成功，暂无可用节点".into()
                    } else {
                        "登录成功".into()
                    };
                    apply_authenticated_snapshot(&mut snapshot, &session, nodes, message);
                    *state.auth.lock().expect("auth state poisoned") = Some(session);
                    if let Err(error) = save_login_credentials(&username, &password) {
                        snapshot.message = format!("登录成功，但无法保存登录状态：{error}");
                    }
                    *state
                        .selected_node
                        .lock()
                        .expect("selected node state poisoned") = None;
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    diagnostics::log("auth", &format!("login_failed error={error}"));
                    snapshot.authenticated = false;
                    snapshot.user_id = None;
                    snapshot.username = None;
                    snapshot.role = None;
                    snapshot.device_id = None;
                    snapshot.allow_ipv4_global = false;
                    snapshot.allow_ipv6_global = false;
                    snapshot.allow_ipv4_split = false;
                    snapshot.allow_ipv6_split = false;
                    snapshot.state = "error".into();
                    snapshot.message = error;
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::ChangePassword => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                snapshot.message = "请先登录后修改密码".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            let current_password = request.current_password.unwrap_or_default();
            let new_password = request.new_password.unwrap_or_default();
            if current_password.is_empty() || new_password.is_empty() {
                snapshot.message = "请输入当前密码和新密码".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            }
            if new_password.len() < 6 || new_password.len() > 256 {
                snapshot.message = "新密码长度应为 6 至 256 位".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            }
            match control_plane_change_password(&session, &current_password, &new_password) {
                Ok(()) => {
                    // The current bearer session remains valid on the master,
                    // so the user can keep an active tunnel. Update the
                    // encrypted local credential immediately for the next
                    // automatic login after restart.
                    snapshot.message =
                        match save_login_credentials(&session.username, &new_password) {
                            Ok(()) => "密码已修改，当前连接保持不变".into(),
                            Err(error) => format!(
                                "密码已修改，但无法保存本机登录状态；下次启动需要手动登录：{error}"
                            ),
                        };
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    snapshot.message = error;
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::SetTrafficMode => {
            let Some(session) = state.auth.lock().expect("auth state poisoned").clone() else {
                snapshot.message = "请先登录后设置流量模式".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            if state
                .tunnel
                .active_config
                .lock()
                .expect("tunnel state poisoned")
                .is_some()
            {
                snapshot.message = "请先断开隧道再修改流量模式".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            }
            let ipv4_mode = request.ipv4_mode.as_deref().unwrap_or("internal");
            let ipv6_mode = request.ipv6_mode.as_deref().unwrap_or("internal");
            let allowed = |mode: &str, global: bool, split: bool| match mode {
                "internal" => true,
                "global" => global,
                "split" => split,
                _ => false,
            };
            if !allowed(
                ipv4_mode,
                session.allow_ipv4_global,
                session.allow_ipv4_split,
            ) || !allowed(
                ipv6_mode,
                session.allow_ipv6_global,
                session.allow_ipv6_split,
            ) {
                snapshot.message = "所选流量模式未获主控授权".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            }
            snapshot.ipv4_mode = ipv4_mode.into();
            snapshot.ipv6_mode = ipv6_mode.into();
            snapshot.message = "网络模式已更新".into();
            IpcResponse {
                ok: true,
                status: snapshot.clone(),
            }
        }
        Action::ListNodes => {
            let session = state.auth.lock().expect("auth state poisoned").clone();
            let Some(session) = session else {
                snapshot.message = "请先登录后查看节点".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            match choose_node_with_session_refresh(state, &mut snapshot, session, None) {
                Ok((_, _, nodes)) => {
                    snapshot.nodes = nodes;
                    snapshot.node_rate_limit = NodeRateLimit::default();
                    snapshot.message = "节点列表和账户授权已刷新".into();
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    snapshot.message = error;
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::SelectNode => {
            let session = state.auth.lock().expect("auth state poisoned").clone();
            let Some(session) = session else {
                snapshot.message = "请先登录后选择节点".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            match choose_node_with_session_refresh(
                state,
                &mut snapshot,
                session,
                request.node_id.as_deref(),
            ) {
                Ok((_, node, nodes)) => {
                    snapshot.nodes = nodes;
                    snapshot.selected_node_id = request.node_id.clone();
                    snapshot.selected_node_name = request.node_id.as_ref().and_then(|_| {
                        snapshot
                            .nodes
                            .iter()
                            .find(|candidate| candidate.node_id == node.node_id)
                            .map(|candidate| candidate.name.clone())
                    });
                    snapshot.node_selection = if request.node_id.is_some() {
                        "manual".into()
                    } else {
                        "auto".into()
                    };
                    *state
                        .selected_node
                        .lock()
                        .expect("selected node state poisoned") = request.node_id.map(|_| node);
                    snapshot.message = if snapshot.node_selection == "auto" {
                        "已切换为自动选择最低延迟节点".into()
                    } else {
                        format!(
                            "已选择节点 {}",
                            snapshot.selected_node_name.as_deref().unwrap_or("")
                        )
                    };
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => {
                    snapshot.message = error;
                    IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    }
                }
            }
        }
        Action::Logout => {
            match state.tunnel.remove() {
                Ok(()) => {
                    // remove() revokes the device using this token. Only
                    // invalidate the token after that request has completed.
                    let revoke_error = state.tunnel.wait_for_revocations();
                    let logout_session = state.take_auth_for_logout();
                    let remote_logout_error = logout_session
                        .as_ref()
                        .and_then(|session| control_plane_logout(session).err());
                    let credential_error = clear_login_credentials()
                        .err()
                        .or(remote_logout_error)
                        .or(revoke_error);
                    snapshot.state = "disconnected".into();
                    snapshot.authenticated = false;
                    snapshot.user_id = None;
                    snapshot.username = None;
                    snapshot.role = None;
                    snapshot.device_id = None;
                    snapshot.allow_ipv4_global = false;
                    snapshot.allow_ipv6_global = false;
                    snapshot.allow_ipv4_split = false;
                    snapshot.allow_ipv6_split = false;
                    snapshot.ipv4_mode = "internal".into();
                    snapshot.ipv6_mode = "internal".into();
                    snapshot.ipv4 = None;
                    snapshot.ipv6 = None;
                    snapshot.allowed_ips.clear();
                    snapshot.node_dns_servers.clear();
                    snapshot.dns_servers.clear();
                    snapshot.dns_split_enabled = false;
                    snapshot.tunnel_mode = None;
                    snapshot.nodes.clear();
                    snapshot.port_forward_firewall_warning.clear();
                    snapshot.selected_node_id = None;
                    snapshot.selected_node_name = None;
                    snapshot.node_selection = "auto".into();
                    snapshot.node_rate_limit = NodeRateLimit::default();
                    snapshot.message = credential_error
                        .map(|error| format!("已退出登录，但会话清理未完全成功：{error}"))
                        .unwrap_or_else(|| "已退出登录".into());
                    *state
                        .selected_node
                        .lock()
                        .expect("selected node state poisoned") = None;
                    IpcResponse {
                        ok: true,
                        status: snapshot.clone(),
                    }
                }
                Err(error) => IpcResponse {
                    ok: false,
                    status: ClientSnapshot {
                        state: "error".into(),
                        message: error,
                        ..snapshot.clone()
                    },
                },
            }
        }
        Action::Connect => {
            diagnostics::log("connection", "connect_started");
            let session = state.auth.lock().expect("auth state poisoned").clone();
            let Some(session) = session else {
                snapshot.state = "error".into();
                snapshot.message = "请先登录后再连接内部网络".into();
                return IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                };
            };
            let selected_node_id = state
                .selected_node
                .lock()
                .expect("selected node state poisoned")
                .as_ref()
                .map(|node| node.node_id.clone());
            let (session, node, nodes) = match choose_node_with_session_refresh(
                state,
                &mut snapshot,
                session,
                selected_node_id.as_deref(),
            ) {
                Ok(result) => result,
                Err(error) => {
                    snapshot.state = "error".into();
                    snapshot.message = error;
                    return IpcResponse {
                        ok: false,
                        status: snapshot.clone(),
                    };
                }
            };
            snapshot.nodes = nodes;
            snapshot.selected_node_id = Some(node.node_id.clone());
            snapshot.selected_node_name = Some(node.name.clone());
            snapshot.node_rate_limit = node.rate_limit;
            let ipv4_mode = if snapshot.ipv4_mode.is_empty() {
                "internal".to_string()
            } else {
                snapshot.ipv4_mode.clone()
            };
            let ipv6_mode = if snapshot.ipv6_mode.is_empty() {
                "internal".to_string()
            } else {
                snapshot.ipv6_mode.clone()
            };
            match state
                .tunnel
                .apply(
                    &session,
                    state.connection_auth(&session),
                    &node,
                    &ipv4_mode,
                    &ipv6_mode,
                    &state.snapshot,
                )
                .map(|(mode, config, split_diagnostics)| {
                    let connected_routes = compose_routes(&config)
                        .map(|routes| routes.routes)
                        .unwrap_or_else(|_| config.routes.clone());
                    let allowed_ips = wireguard_allowed_ips(&config, &connected_routes);
                    let route_count = connected_routes.len();
                    snapshot.state = "connected".into();
                    snapshot.authenticated = true;
                    snapshot.user_id = Some(session.user_id.clone());
                    snapshot.username = Some(session.username.clone());
                    snapshot.role = Some(session.role.clone());
                    snapshot.device_id = Some(config.device_id.to_string());
                    snapshot.ipv4 = Some(config.ipv4);
                    snapshot.ipv6 = Some(config.ipv6);
                    snapshot.allowed_ips = allowed_ips;
                    snapshot.node_dns_servers = config.dns_servers.clone();
                    snapshot.dns_servers = effective_dns_servers(&config.dns_servers);
                    snapshot.dns_split_enabled = config.dns_policy.is_some();
                    snapshot.ipv4_mode = config.ipv4_mode;
                    snapshot.ipv6_mode = config.ipv6_mode;
                    snapshot.node_rate_limit = config.rate_limit;
                    snapshot.tunnel_mode = Some(mode);
                    snapshot.split_route_source = split_diagnostics.source;
                    snapshot.split_route_version = split_diagnostics.version;
                    snapshot.split_route_snapshot = split_diagnostics.snapshot;
                    snapshot.split_ipv4_prefix_count = split_diagnostics.ipv4_count;
                    snapshot.split_ipv6_prefix_count = split_diagnostics.ipv6_count;
                    snapshot.split_route_install_ms = split_diagnostics.install_ms;
                    snapshot.message = if snapshot.split_route_source.is_some() {
                        format!(
                            "已连接到内网；客户端分流规则 {} 已安装（IPv4 {} 条，IPv6 {} 条）",
                            snapshot.split_route_version.as_deref().unwrap_or("内置"),
                            snapshot.split_ipv4_prefix_count,
                            snapshot.split_ipv6_prefix_count
                        )
                    } else {
                        "已连接到内网".into()
                    };
                    diagnostics::log(
                        "connection",
                        &format!(
                            "connect_tunnel_ready node={} ipv4_mode={} ipv6_mode={} routes={}",
                            node.node_id, snapshot.ipv4_mode, snapshot.ipv6_mode, route_count
                        ),
                    );
                }) {
                Ok(()) => {
                    let firewall_result = state
                        .tunnel
                        .sync_port_forward_firewall(&snapshot.port_forwards);
                    firewall_sync_response(&mut snapshot, "connect", firewall_result)
                }
                Err(error) => connection_error_response(&mut snapshot, error),
            }
        }
        Action::Disconnect => match state.tunnel.remove() {
            Ok(()) => {
                diagnostics::log("connection", "disconnect_succeeded");
                snapshot.state = "disconnected".into();
                snapshot.ipv4 = None;
                snapshot.ipv6 = None;
                snapshot.allowed_ips.clear();
                snapshot.node_dns_servers.clear();
                snapshot.dns_servers.clear();
                snapshot.dns_split_enabled = false;
                snapshot.tunnel_mode = None;
                snapshot.split_route_source = None;
                snapshot.split_route_version = None;
                snapshot.split_route_snapshot = None;
                snapshot.split_ipv4_prefix_count = 0;
                snapshot.split_ipv6_prefix_count = 0;
                snapshot.split_route_install_ms = 0;
                snapshot.message = "已断开内网连接".into();
                IpcResponse {
                    ok: true,
                    status: snapshot.clone(),
                }
            }
            Err(error) => {
                diagnostics::log("connection", &format!("disconnect_failed error={error}"));
                snapshot.state = "error".into();
                snapshot.message = error;
                IpcResponse {
                    ok: false,
                    status: snapshot.clone(),
                }
            }
        },
    }
}

#[cfg(windows)]
fn current_user_pipe_sddl() -> io::Result<String> {
    use std::ffi::CStr;
    use windows_sys::core::PSTR;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidA;
    use windows_sys::Win32::Security::{
        GetTokenInformation, IsValidSid, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut length = 0u32;
        let first =
            unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut length) };
        if first != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; length as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "当前进程令牌没有有效用户 SID",
            ));
        }
        let mut sid_text: PSTR = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidA(sid, &mut sid_text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let value = unsafe { CStr::from_ptr(sid_text.cast()) }
            .to_str()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "用户 SID 不是有效文本"))?
            .to_owned();
        unsafe { windows_sys::Win32::Foundation::LocalFree(sid_text.cast()) };
        Ok(format!("D:P(A;;GA;;;{value})"))
    })();
    unsafe { CloseHandle(token) };
    result
}

#[cfg(windows)]
async fn run_listener(state: ServiceState) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use tokio::net::windows::named_pipe::ServerOptions;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    let sddl_text = current_user_pipe_sddl()?;
    let sddl: Vec<u16> = std::ffi::OsStr::new(&sddl_text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let valid = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if valid == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let result = async {
        let mut create = |first| {
            let mut options = ServerOptions::new();
            options
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .access_outbound(true)
                .access_inbound(true);
            unsafe {
                options.create_with_security_attributes_raw(
                    WINDOWS_PIPE_NAME,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
            }
        };
        let transport = new_ipc_transport();
        let mut pending = create(true)?;
        loop {
            let connected = pending.connect().await;
            // Keep ownership of the pipe name and an accepting instance even
            // while another client performs a slow tunnel operation.
            let next = create(false)?;
            let server = std::mem::replace(&mut pending, next);
            if let Err(error) = connected {
                diagnostics::log(
                    "ipc_transport",
                    &format!("pipe_connect_failed kind={:?}", error.kind()),
                );
                continue;
            }
            let state = state.clone();
            let _ = transport.try_spawn(server, move |request: IpcRequest| {
                handle_request(&state, request)
            });
        }
    }
    .await;
    unsafe { windows_sys::Win32::Foundation::LocalFree(descriptor.cast()) };
    result
}

#[cfg(not(windows))]
async fn run_listener(state: ServiceState) -> io::Result<()> {
    use tokio::net::UnixListener;
    let _ = fs::remove_file(UNIX_SOCKET_PATH);
    let listener = UnixListener::bind(UNIX_SOCKET_PATH)?;
    // The helper has permission to configure the local tunnel. Do not leave
    // the default umask-dependent socket mode in place, otherwise another
    // local account could issue IPC commands as this user.
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(UNIX_SOCKET_PATH, fs::Permissions::from_mode(0o600))?;
    let transport = new_ipc_transport();
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        let _ = transport.try_spawn(stream, move |request: IpcRequest| {
            handle_request(&state, request)
        });
    }
}

fn new_ipc_transport() -> ipc_transport::IpcTransport {
    ipc_transport::IpcTransport::new(ipc_transport::TransportConfig::default(), |error| {
        diagnostics::log("ipc_transport", &error.to_string());
    })
}

#[tokio::main]
async fn main() -> io::Result<()> {
    diagnostics::log("lifecycle", "service_start");
    let saved_credentials = load_login_credentials().unwrap_or_default();
    let tunnel = Arc::new(TunnelManager::from_environment());
    // Remove rules left by an interrupted client process before a new tunnel
    // is created. The group is owned exclusively by this client.
    let _ = tunnel.sync_port_forward_firewall(&[]);
    let state = ServiceState {
        command_gate: Arc::new(Mutex::new(())),
        auth_refresh_gate: Arc::new(Mutex::new(())),
        snapshot: Arc::new(Mutex::new(ClientSnapshot {
            p2p_enabled: load_p2p_preference(),
            auth_restore_pending: saved_credentials.is_some(),
            state: "disconnected".into(),
            message: "服务已启动，等待用户点击连接".into(),
            node_selection: "auto".into(),
            ipv4_mode: "internal".into(),
            ipv6_mode: "internal".into(),
            ..Default::default()
        })),
        tunnel,
        auth: Arc::new(Mutex::new(None)),
        auth_generation: Arc::new(AtomicU64::new(0)),
        selected_node: Arc::new(Mutex::new(None)),
    };
    println!(
        "Pmman Network service listening on {}",
        if cfg!(windows) {
            WINDOWS_PIPE_NAME
        } else {
            UNIX_SOCKET_PATH
        }
    );
    restore_login_in_background(state.clone(), saved_credentials);
    run_listener(state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn bounded_command_drains_output_larger_than_pipe_buffer() {
        for suffix in ["", " 1>&2"] {
            let output = command_output_timeout(
                hidden_command("cmd.exe").args([
                    "/d",
                    "/c",
                    &format!("for /L %n in (1,1,2048) do @echo fixture-output{suffix}"),
                ]),
                std::time::Duration::from_secs(5),
            )
            .unwrap();
            assert!(output.status.success());
            assert!(output.stdout.len() + output.stderr.len() > 16 * 1024);
        }
    }

    #[cfg(windows)]
    #[test]
    fn bounded_command_kills_only_its_own_child_on_timeout() {
        let started = std::time::Instant::now();
        let error = command_output_timeout(
            hidden_command("cmd.exe").args([
                "/d",
                "/c",
                "for /L %n in (1,1,2147483647) do @echo fixture-output",
            ]),
            std::time::Duration::from_millis(80),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn control_plane_http_is_limited_to_loopback_development_addresses() {
        assert!(validate_control_plane_url("https://network.pmman.tech").is_ok());
        assert!(validate_control_plane_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_control_plane_url("http://[::1]:8080").is_ok());
        assert!(validate_control_plane_url("http://network.pmman.tech").is_err());
        assert!(validate_control_plane_url("http://user:pass@127.0.0.1:8080").is_err());
    }

    #[test]
    fn port_forward_errors_replace_stale_generic_connection_message() {
        let mut snapshot = ClientSnapshot {
            message: "已连接到内网".into(),
            port_forward_message: "端口转发已建立".into(),
            ..Default::default()
        };

        set_port_forward_error(&mut snapshot, "创建端口转发失败：HTTP 502；节点应用超时");

        assert_eq!(snapshot.message, "创建端口转发失败：HTTP 502；节点应用超时");
        assert_eq!(snapshot.port_forward_message, snapshot.message);
    }

    #[test]
    fn firewall_failure_after_handshake_is_connected_success_with_warning() {
        let mut snapshot = ClientSnapshot {
            state: "connected".into(),
            tunnel_mode: Some("wireguard".into()),
            ipv4: Some("192.0.2.42".into()),
            message: "已连接到内网".into(),
            port_forwards: vec![PortForwardView {
                forward_id: "fixture-lease".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let result = firewall_sync_response(&mut snapshot, "connect", Err("Windows 错误 5".into()));
        assert!(
            result.ok,
            "Tauri must receive the successful connected snapshot"
        );
        assert_eq!(result.status.state, "connected");
        assert_eq!(result.status.tunnel_mode.as_deref(), Some("wireguard"));
        assert_eq!(result.status.ipv4.as_deref(), Some("192.0.2.42"));
        assert_eq!(result.status.message, "已连接到内网");
        assert_eq!(result.status.port_forwards[0].forward_id, "fixture-lease");
        assert!(result
            .status
            .port_forward_firewall_warning
            .contains("Windows 错误 5"));
    }

    #[test]
    fn firewall_sync_does_not_turn_completed_forward_mutations_into_errors() {
        for operation in ["list", "create", "release"] {
            let mut snapshot = ClientSnapshot {
                state: "connected".into(),
                message: "已连接到内网".into(),
                port_forward_message: "操作已完成".into(),
                ..Default::default()
            };
            let response =
                firewall_sync_response(&mut snapshot, operation, Err("系统策略拒绝".into()));
            assert!(response.ok);
            assert_eq!(response.status.port_forward_message, "操作已完成");
            assert_eq!(response.status.message, "已连接到内网");
            assert!(!response.status.port_forward_firewall_warning.is_empty());
        }
    }

    #[test]
    fn firewall_warning_survives_status_and_clears_only_after_successful_sync() {
        let state = ipc_test_state();
        {
            let mut snapshot = state.snapshot.lock().unwrap();
            firewall_sync_response(&mut snapshot, "connect", Err("fixture-denied".into()));
        }
        for _ in 0..2 {
            let response = handle_request(&state, IpcRequest::new(Action::Status));
            assert_eq!(response.status.state, "connected");
            assert!(!response.status.port_forward_firewall_warning.is_empty());
        }
        let mut snapshot = state.snapshot.lock().unwrap();
        let response = firewall_sync_response(&mut snapshot, "list", Ok(()));
        assert!(response.ok);
        assert!(response.status.port_forward_firewall_warning.is_empty());
        assert_eq!(response.status.state, "connected");
    }

    #[test]
    fn port_forward_firewall_rules_are_exact_ports_and_protocols() {
        let forward = |state: &str, port: u16, protocols| PortForwardView {
            state: state.into(),
            target_port: port,
            protocols,
            ..Default::default()
        };
        let rules = port_forward_firewall_rules(&[
            forward(
                "active",
                22,
                vec![PortForwardProtocol::Tcp, PortForwardProtocol::Udp],
            ),
            forward("applying", 22, vec![PortForwardProtocol::Tcp]),
            forward("released", 3389, vec![PortForwardProtocol::Tcp]),
        ]);
        assert_eq!(rules, vec![("TCP".into(), 22), ("UDP".into(), 22)]);
    }

    #[test]
    fn node_dns_is_first_and_local_dns_is_fallback() {
        let merged = merge_dns_servers(
            &["10.20.0.53".into(), "fd20:1000::53".into()],
            &["192.168.1.1".into(), "10.20.0.53".into(), "invalid".into()],
        );
        assert_eq!(
            merged,
            vec![
                "10.20.0.53".to_string(),
                "fd20:1000::53".to_string(),
                "192.168.1.1".to_string()
            ]
        );
    }

    #[test]
    fn client_dns_policy_keeps_domestic_local_and_foreign_on_node() {
        let config = DeviceConfig {
            dns_servers: vec!["10.88.0.1".into()],
            dns_policy: Some(pmman_protocol::ClientDnsPolicy {
                geosite: None,
                domestic_rules: vec!["cn.example".into()],
                foreign_rules: vec!["global.example".into()],
            }),
            ..serde_json::from_value(serde_json::json!({
                "rate_limit": {"uploadMbps": 0, "downloadMbps": 0},
                "ipv4": "10.88.0.2/32",
                "ipv6": "fd88::2/128",
                "routes": [],
                "config_version": 1,
                "device_id": "device",
                "server_public_key": "key",
                "endpoint": "node:51820"
            }))
            .unwrap()
        };
        let policy = client_dns_policy(&config, &["192.168.1.1".into()], None).unwrap();
        let node: std::net::SocketAddr = "10.88.0.1:53".parse().unwrap();
        let local: std::net::SocketAddr = "192.168.1.1:53".parse().unwrap();
        assert_eq!(policy.primary, vec![node]);
        assert!(policy.fallback.is_empty());
        assert_eq!(policy.domestic_upstreams, vec![local]);
        assert!(policy.rules.contains(&("cn.example".into(), vec![local])));
        assert!(policy
            .rules
            .contains(&("global.example".into(), vec![node])));
    }

    #[test]
    fn handshake_accepts_current_peer_without_wall_clock_comparison() {
        let peer = "BChmmv4Vny/ygqbk7ZXZ1K8gUjFKFceP6engsc2fpg0=";
        assert!(has_peer_handshake(&format!("{peer} 1"), peer));
        assert!(!has_peer_handshake(&format!("{peer} 0"), peer));
        assert!(!has_peer_handshake("other-peer 1791369520", peer));
    }

    #[test]
    fn refreshed_authorization_updates_modes_without_resetting_connection_state() {
        let session = AuthSession {
            token: "token".into(),
            user_id: "user".into(),
            username: "alice".into(),
            role: "user".into(),
            allow_ipv4_global: false,
            allow_ipv6_global: true,
            allow_ipv4_split: false,
            allow_ipv6_split: false,
        };
        let mut snapshot = ClientSnapshot {
            state: "disconnected".into(),
            ipv4_mode: "global".into(),
            ipv6_mode: "split".into(),
            ..Default::default()
        };
        update_authenticated_snapshot(&mut snapshot, &session);
        assert!(snapshot.authenticated);
        assert!(!snapshot.allow_ipv4_global);
        assert!(snapshot.allow_ipv6_global);
        assert_eq!(snapshot.ipv4_mode, "internal");
        assert_eq!(snapshot.ipv6_mode, "internal");

        snapshot.state = "connected".into();
        snapshot.ipv4_mode = "global".into();
        update_authenticated_snapshot(&mut snapshot, &session);
        assert_eq!(snapshot.ipv4_mode, "global");
    }

    #[test]
    fn only_master_unauthorized_responses_trigger_session_recovery() {
        assert!(is_session_expired_error("主控 HTTP 401；登录会话已失效"));
        assert!(!is_session_expired_error("主控 HTTP 503；当前没有可用节点"));
        assert!(!is_session_expired_error("节点列表获取失败：timeout"));
        assert!(is_session_expired_error(
            "日志上报失败：HTTP 401 Unauthorized；客户端会话无效"
        ));
        assert!(!is_session_expired_error(
            "主控 HTTP 503；upstream HTTP 401"
        ));
    }

    #[test]
    fn heartbeat_session_expiry_is_not_device_revocation() {
        assert!(is_session_expired_error(
            &heartbeat_status(401).err().unwrap()
        ));
        for status in [403, 404, 409] {
            assert!(matches!(
                heartbeat_status(status),
                Ok(HeartbeatResult::Revoked)
            ));
        }
        for status in [429, 500, 502, 503] {
            assert!(heartbeat_status(status).is_err());
        }
        assert!(matches!(
            heartbeat_status(204),
            Ok(HeartbeatResult::Active(_))
        ));
    }

    fn ipc_test_state() -> ServiceState {
        ServiceState {
            command_gate: Arc::new(Mutex::new(())),
            auth_refresh_gate: Arc::new(Mutex::new(())),
            snapshot: Arc::new(Mutex::new(ClientSnapshot {
                state: "connected".into(),
                tunnel_mode: Some("wireguard".into()),
                ..Default::default()
            })),
            tunnel: Arc::new(TunnelManager::from_environment()),
            auth: Arc::new(Mutex::new(None)),
            auth_generation: Arc::new(AtomicU64::new(0)),
            selected_node: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn status_stays_available_during_mutation_and_transport_cleanup() {
        let state = ipc_test_state();
        let command = state.command_gate.lock().unwrap();
        let proxy = state.tunnel.node_proxy.lock().unwrap();
        let p2p = state.tunnel.p2p.lock().unwrap();
        let traffic = state.tunnel.traffic.lock().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let other = state.clone();
        let worker = std::thread::spawn(move || {
            send.send(handle_request(&other, IpcRequest::new(Action::Status)))
                .unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(1));
        drop((command, proxy, p2p, traffic));
        worker.join().unwrap();
        assert!(result.unwrap().ok);
    }

    #[test]
    fn concurrent_mutations_reject_without_corrupting_connection_state() {
        let state = ipc_test_state();
        let _command = state.command_gate.lock().unwrap();
        let result = handle_request(&state, IpcRequest::new(Action::Disconnect));
        assert!(!result.ok);
        assert_eq!(result.status.state, "connected");
        assert_eq!(state.snapshot.lock().unwrap().state, "connected");
    }

    #[test]
    fn cleanup_cancel_bypasses_command_gate_but_cannot_cancel_installed_tunnel() {
        let state = ipc_test_state();
        let _command = state.command_gate.lock().unwrap();
        assert!(!handle_request(&state, IpcRequest::new(Action::CancelConnect)).ok);
        assert!(!state.tunnel.connect_cancelled.load(Ordering::Acquire));
        {
            let mut snapshot = state.snapshot.lock().unwrap();
            snapshot.state = "connecting".into();
            snapshot.connection_waiting_cleanup = true;
            snapshot.message = connection_retry::WAIT_MESSAGE.into();
        }
        assert!(
            handle_request(&state, IpcRequest::new(Action::Status))
                .status
                .connection_waiting_cleanup
        );
        let response = handle_request(&state, IpcRequest::new(Action::CancelConnect));
        assert!(response.ok);
        assert_eq!(response.status.state, "connecting");
        assert!(state.tunnel.connect_cancelled.load(Ordering::Acquire));
        state.snapshot.lock().unwrap().connection_waiting_cleanup = false;
        assert!(!handle_request(&state, IpcRequest::new(Action::CancelConnect)).ok);
    }

    #[test]
    fn intentional_cancellation_is_successful_ipc_not_connection_failure() {
        let mut snapshot = ClientSnapshot {
            state: "connecting".into(),
            connection_waiting_cleanup: true,
            ..Default::default()
        };
        let response = connection_error_response(&mut snapshot, connection_retry::CANCELLED.into());
        assert!(response.ok);
        assert_eq!(response.status.state, "disconnected");
        assert!(!response.status.connection_waiting_cleanup);
        let response = connection_error_response(&mut snapshot, "主控撤销未完成".into());
        assert!(!response.ok);
        assert_eq!(response.status.state, "error");
    }

    #[test]
    fn registration_retries_real_http_cleanup_response_with_same_request() {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                requests.push(body);
                let (status, json) = if index == 0 {
                    (
                        "409 Conflict",
                        serde_json::json!({"error":"地址策略已变更，旧连接及端口映射尚未清理，请断开后稍候重试"}),
                    )
                } else {
                    (
                        "200 OK",
                        serde_json::json!({"device_id":"fixture-device","ipv4":"192.0.2.1","ipv6":"2001:db8::1","routes":[],"config_version":7,"server_public_key":"fixture","endpoint":"127.0.0.1:51820"}),
                    )
                };
                let body = json.to_string();
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            assert_eq!(
                requests[0], requests[1],
                "key, node and modes must remain stable"
            );
        });
        let mut progress = 0;
        let config = connection_retry::run(
            std::time::Duration::from_secs(5),
            std::time::Duration::ZERO,
            &AtomicBool::new(false),
            |remaining| {
                control_plane_config(
                    &url,
                    "fixture-node",
                    "fixture-token",
                    "fixture-key",
                    "internal",
                    "internal",
                    remaining,
                )
            },
            |n, _| progress = n,
        )
        .unwrap();
        assert_eq!(config.device_id, "fixture-device");
        assert_eq!(progress, 1);
        server.join().unwrap();
    }

    #[test]
    fn revocation_remains_visible_to_all_readers_and_clears_split_details() {
        let state = ipc_test_state();
        state.tunnel.remote_revoked.store(true, Ordering::Release);
        for _ in 0..2 {
            let result = handle_request(&state, IpcRequest::new(Action::Status));
            assert_eq!(result.status.state, "disconnected");
            assert!(result.status.tunnel_mode.is_none());
            assert!(result.status.allowed_ips.is_empty());
        }
    }

    #[test]
    fn stale_firewall_cleanup_cannot_touch_replacement_session() {
        let gate = Mutex::new(());
        let active = Mutex::new(Some(()));
        assert!(!when_session_idle(&gate, &active, || panic!(
            "new connection rules deleted"
        )));
        active.lock().unwrap().take();
        let mut cleaned = false;
        assert!(when_session_idle(&gate, &active, || {
            assert!(gate.try_lock().is_err());
            cleaned = true;
        }));
        assert!(cleaned);
    }

    #[test]
    fn logout_waits_for_pending_remote_revoke_before_invalidating_auth() {
        let state = ipc_test_state();
        let finished = Arc::new(AtomicBool::new(false));
        let (release, wait) = std::sync::mpsc::channel();
        let job_finished = Arc::clone(&finished);
        state
            .tunnel
            .pending_revokes
            .lock()
            .unwrap()
            .push(std::thread::spawn(move || {
                wait.recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap();
                job_finished.store(true, Ordering::Release);
                Ok(())
            }));
        release.send(()).unwrap();
        assert!(state.tunnel.wait_for_revocations().is_none());
        assert!(finished.load(Ordering::Acquire));
        assert!(state.tunnel.pending_revokes.lock().unwrap().is_empty());
        let epoch = state.auth_generation.load(Ordering::Acquire);
        assert!(state.take_auth_for_logout().is_none());
        assert_eq!(state.auth_generation.load(Ordering::Acquire), epoch + 1);
    }

    #[test]
    fn permanent_auth_failure_is_visible_even_after_tunnel_cleanup() {
        let state = ipc_test_state();
        state.tunnel.remote_revoked.store(true, Ordering::Release);
        state.tunnel.auth_expired.store(true, Ordering::Release);
        state.snapshot.lock().unwrap().authenticated = true;
        let result = handle_request(&state, IpcRequest::new(Action::Status));
        assert!(!result.status.authenticated);
        assert_eq!(result.status.state, "disconnected");
        assert!(result.status.message.contains("重新登录"));
    }

    #[test]
    fn late_old_session_response_cannot_mutate_replacement_tunnel() {
        let gate = Arc::new(Mutex::new(()));
        let stopped = Arc::new(AtomicBool::new(false));
        let replacement = gate.lock().unwrap();
        let (ready, response) = std::sync::mpsc::channel();
        let old_gate = Arc::clone(&gate);
        let old_stop = Arc::clone(&stopped);
        let late_response = std::thread::spawn(move || {
            // Simulate an HTTP response arriving during disconnect/reconnect.
            ready.send(()).unwrap();
            live_session(&old_gate, &old_stop).is_some()
        });
        response.recv().unwrap();
        stopped.store(true, Ordering::Release);
        drop(replacement);
        assert!(!late_response.join().unwrap());
        let new_stop = AtomicBool::new(false);
        assert!(live_session(&gate, &new_stop).is_some());
    }

    fn test_node(id: &str, healthy: bool, latency_ms: Option<u64>) -> NodeInfo {
        NodeInfo {
            rate_limit: NodeRateLimit::default(),
            node_id: id.into(),
            name: id.into(),
            wireguard_endpoint: format!("{id}:51820"),
            probe_endpoint: format!("{id}:51840"),
            region: None,
            healthy,
            latency_ms,
        }
    }

    #[test]
    fn probe_health_uses_control_plane_when_every_probe_is_filtered() {
        let mut nodes = vec![test_node("a", true, None), test_node("b", false, None)];
        apply_probe_health(&mut nodes, &[true, false]);
        assert_eq!(
            nodes.iter().map(|node| node.healthy).collect::<Vec<_>>(),
            [true, false]
        );
    }

    #[test]
    fn probe_health_prefers_measured_candidates_when_any_probe_succeeds() {
        let mut nodes = vec![test_node("a", true, Some(12)), test_node("b", true, None)];
        apply_probe_health(&mut nodes, &[true, true]);
        assert_eq!(
            nodes.iter().map(|node| node.healthy).collect::<Vec<_>>(),
            [true, false]
        );
    }

    #[test]
    fn udp_latency_accepts_challenge_echo_and_ignores_wrong_or_oversized_replies() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let endpoint = socket.local_addr().unwrap().to_string();
        let responder = std::thread::spawn(move || {
            for _ in 0..3 {
                let mut packet = [0u8; UDP_PING_SIZE];
                let (length, source) = socket.recv_from(&mut packet).unwrap();
                assert_eq!(length, UDP_PING_SIZE);
                assert_eq!(&packet[..8], UDP_PING_MAGIC);
                let mut wrong = packet;
                wrong[8] ^= 1;
                socket.send_to(&wrong, source).unwrap();
                let mut oversized = packet.to_vec();
                oversized.push(0);
                socket.send_to(&oversized, source).unwrap();
                socket.send_to(&packet, source).unwrap();
            }
        });
        assert!(udp_latency(&endpoint).is_some());
        responder.join().unwrap();
    }

    #[test]
    fn udp_latency_rejects_nonmatching_nonce_with_bounded_wait() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let endpoint = socket.local_addr().unwrap().to_string();
        let responder = std::thread::spawn(move || {
            for _ in 0..3 {
                let mut packet = [0u8; UDP_PING_SIZE];
                let (_, source) = socket.recv_from(&mut packet).unwrap();
                packet[8] ^= 1;
                socket.send_to(&packet, source).unwrap();
            }
        });
        let started = std::time::Instant::now();
        assert_eq!(udp_latency(&endpoint), None);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        responder.join().unwrap();
    }
    #[test]
    fn stale_restore_cannot_reauthenticate_after_logout_or_replace_a_login() {
        let state = ServiceState {
            command_gate: Arc::new(Mutex::new(())),
            auth_refresh_gate: Arc::new(Mutex::new(())),
            snapshot: Arc::new(Mutex::new(ClientSnapshot {
                auth_restore_pending: true,
                ..Default::default()
            })),
            tunnel: Arc::new(TunnelManager::from_environment()),
            auth: Arc::new(Mutex::new(None)),
            auth_generation: Arc::new(AtomicU64::new(1)),
            selected_node: Arc::new(Mutex::new(None)),
        };
        let session = AuthSession {
            token: "test-token".into(),
            user_id: "test-id".into(),
            username: "test-user".into(),
            role: "user".into(),
            allow_ipv4_global: false,
            allow_ipv6_global: false,
            allow_ipv4_split: false,
            allow_ipv6_split: false,
        };
        assert!(!commit_restored_login(&state, &session, Vec::new(), 0));
        assert!(state.auth.lock().unwrap().is_none());
        assert!(!state.snapshot.lock().unwrap().authenticated);
        assert!(commit_restored_login(&state, &session, Vec::new(), 1));
        assert!(!state.snapshot.lock().unwrap().auth_restore_pending);
        let mut late = session.clone();
        late.username = "late-user".into();
        assert!(!commit_restored_login(&state, &late, Vec::new(), 1));
        assert_eq!(
            state.auth.lock().unwrap().as_ref().unwrap().username,
            "test-user"
        );
        assert_eq!(
            state.snapshot.lock().unwrap().username.as_deref(),
            Some("test-user")
        );
    }

    #[test]
    fn service_starts_disconnected() {
        let state = ServiceState {
            command_gate: Arc::new(Mutex::new(())),
            auth_refresh_gate: Arc::new(Mutex::new(())),
            snapshot: Arc::new(Mutex::new(ClientSnapshot {
                state: "disconnected".into(),
                message: String::new(),
                node_selection: "auto".into(),
                ipv4_mode: "internal".into(),
                ipv6_mode: "internal".into(),
                ..Default::default()
            })),
            tunnel: Arc::new(TunnelManager::from_environment()),
            auth: Arc::new(Mutex::new(None)),
            auth_generation: Arc::new(AtomicU64::new(0)),
            selected_node: Arc::new(Mutex::new(None)),
        };
        let response = handle_request(&state, IpcRequest::new(Action::Status));
        assert!(response.ok);
        assert_eq!(response.status.state, "disconnected");
    }

    #[test]
    fn built_in_split_routes_are_composed_by_family_and_mode() {
        let config = DeviceConfig {
            rate_limit: NodeRateLimit::default(),
            ipv4: "10.88.0.10/32".into(),
            ipv6: "fd88:1000::a/128".into(),
            routes: vec!["10.20.0.0/16".into(), "fd20:1000::/48".into()],
            ipv4_mode: "split".into(),
            ipv6_mode: "internal".into(),
            dns_servers: Vec::new(),
            dns_policy: None,
            config_version: 1,
            device_id: "device".into(),
            server_public_key: "server".into(),
            endpoint: "node:51820".into(),
            gateway_ipv4: None,
            gateway_ipv6: None,
            split_route_source: Some(split_routes::SOURCE.into()),
        };
        let composed = compose_routes(&config).unwrap();
        assert!(composed.routes.contains(&"10.20.0.0/16".into()));
        assert!(composed.routes.contains(&"1.0.0.0/24".into()));
        assert!(!composed.routes.iter().any(|route| route == "2000::/16"));
        assert!(composed.metadata.unwrap().ipv6_count > 1000);

        let mut legacy = config.clone();
        legacy.split_route_source = None;
        legacy.ipv6_mode = "internal".into();
        legacy
            .routes
            .extend(["::/0".into(), "::/1".into(), "8000::/1".into()]);
        let legacy_routes = compose_routes(&legacy).unwrap().routes;
        assert!(!legacy_routes
            .iter()
            .any(|route| { matches!(route.as_str(), "::/0" | "::/1" | "8000::/1") }));
        assert!(wireguard_allowed_ips(&legacy, &legacy_routes).contains(&"::/0".into()));

        let mut global = config;
        global.ipv4_mode = "global".into();
        let global_composed = compose_routes(&global).unwrap();
        assert!(global_composed.metadata.is_none());
        let global_routes = global_composed.routes;
        assert!(global_routes.contains(&"0.0.0.0/1".into()));
        assert!(global_routes.contains(&"128.0.0.0/1".into()));
    }

    #[test]
    fn tunnel_mtu_subtracts_family_specific_wireguard_overhead() {
        assert_eq!(tunnel_mtu_from_path(1500, false).unwrap(), 1420);
        assert_eq!(tunnel_mtu_from_path(1500, true).unwrap(), 1420);
        assert_eq!(tunnel_mtu_from_path(1400, false).unwrap(), 1340);
        assert_eq!(tunnel_mtu_from_path(1400, true).unwrap(), 1320);
    }

    #[test]
    fn tunnel_mtu_rejects_a_path_that_cannot_carry_ipv6() {
        assert!(tunnel_mtu_from_path(1339, false).is_err());
        assert!(tunnel_mtu_from_path(1359, true).is_err());
    }

    #[test]
    fn p2p_directory_errors_explain_empty_and_json_http_failures() {
        let error = parse_peer_directory_response(503, "").unwrap_err();
        assert_eq!(error, "主控 HTTP 503；当前没有健康节点");

        let error =
            parse_peer_directory_response(502, r#"{"error":"节点心跳暂不可用"}"#).unwrap_err();
        assert_eq!(error, "主控 HTTP 502；节点心跳暂不可用");
    }
}
