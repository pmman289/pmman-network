//! Routes owned by a Pmman WireGuard tunnel.
//!
//! `AllowedIPs` is the WireGuard cryptokey selector.  It is deliberately
//! separate from this module because the client must be able to receive a
//! global IPv6 source while keeping the user's existing IPv6 default route.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Mutex;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RouteSpec {
    pub destination: IpAddr,
    pub prefix_len: u8,
    pub metric: u32,
}

impl RouteSpec {
    pub fn parse(value: &str, metric: u32) -> Result<Self, String> {
        let (address, prefix) = value
            .trim()
            .split_once('/')
            .ok_or_else(|| format!("路由缺少前缀长度：{value}"))?;
        let destination = address
            .parse::<IpAddr>()
            .map_err(|error| format!("路由地址无效 {value}：{error}"))?;
        let prefix_len = prefix
            .parse::<u8>()
            .map_err(|error| format!("路由前缀无效 {value}：{error}"))?;
        let maximum = if destination.is_ipv4() { 32 } else { 128 };
        if prefix_len > maximum {
            return Err(format!("路由前缀超出范围：{value}"));
        }
        Ok(Self {
            destination,
            prefix_len,
            metric,
        })
    }

    pub fn cidr(&self) -> String {
        format!("{}/{}", self.destination, self.prefix_len)
    }
}

/// Build the host routes for one connection. The route list is already
/// authorized and assembled by the master; this function only adds the
/// gateway host routes. Internal and split modes retain a high-metric IPv6
/// default as a fallback for traffic sourced from the client's assigned GUA;
/// an available physical default should win by its lower effective metric.
pub fn plan(
    assigned_routes: &[String],
    gateway_ipv4: Option<&str>,
    gateway_ipv6: Option<&str>,
    ipv6_mode: &str,
) -> Result<Vec<RouteSpec>, String> {
    if !matches!(ipv6_mode, "internal" | "split" | "global") {
        return Err(format!("IPv6 流量模式无效：{ipv6_mode}"));
    }
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |value: &str, metric: u32| -> Result<(), String> {
        let route = RouteSpec::parse(value, metric)?;
        if seen.insert(route.clone()) {
            result.push(route);
        }
        Ok(())
    };

    if let Some(gateway) = gateway_ipv4.and_then(host_address) {
        add(&format!("{gateway}/32"), 5)?;
    }
    if let Some(gateway) = gateway_ipv6.and_then(host_address) {
        add(&format!("{gateway}/128"), 5)?;
    }
    for route in assigned_routes {
        add(route, 5)?;
    }

    if ipv6_mode != "global" {
        // This fallback is deliberately less preferred than a normal physical
        // IPv6 route. It keeps the tunnel usable for assigned-GUA replies when
        // no physical IPv6 default exists, at the cost of sending other IPv6
        // traffic through the tunnel on IPv6-less networks.
        add("::/0", 5000)?;
    }
    Ok(result)
}

fn host_address(value: &str) -> Option<String> {
    let (address, _) = value.split_once('/')?;
    address.parse::<IpAddr>().ok().map(|_| address.to_string())
}

pub struct RouteManager {
    tunnel: String,
    owned: Mutex<HashSet<RouteSpec>>,
}

impl RouteManager {
    pub fn new(tunnel: impl Into<String>) -> Self {
        Self {
            tunnel: tunnel.into(),
            owned: Mutex::new(HashSet::new()),
        }
    }

    #[cfg(not(windows))]
    pub fn install(&self, routes: &[RouteSpec]) -> Result<(), String> {
        self.install_with_timeout(routes, std::time::Duration::from_secs(120))
    }

    #[cfg(not(windows))]
    pub fn install_with_timeout(
        &self,
        routes: &[RouteSpec],
        _timeout: std::time::Duration,
    ) -> Result<(), String> {
        self.owned
            .lock()
            .expect("route state poisoned")
            .extend(routes.iter().cloned());
        Ok(())
    }

    #[cfg(windows)]
    pub fn install(&self, routes: &[RouteSpec]) -> Result<(), String> {
        self.install_with_timeout(routes, std::time::Duration::from_secs(120))
    }

    #[cfg(windows)]
    pub fn install_with_timeout(
        &self,
        routes: &[RouteSpec],
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        route_log(&format!(
            "route_install_start tunnel={} route_count={}",
            self.tunnel,
            routes.len()
        ));
        let interface_index = wait_for_interface_index(&self.tunnel)?;
        let deadline = std::time::Instant::now() + timeout;
        route_log(&format!(
            "interface_ready tunnel={} if_index={}",
            self.tunnel, interface_index
        ));
        let mut installed: Vec<RouteSpec> = Vec::new();
        for route in routes {
            if std::time::Instant::now() >= deadline {
                route_log(&format!(
                    "route_install_timeout tunnel={} installed={} total={}",
                    self.tunnel,
                    installed.len(),
                    routes.len()
                ));
                for previous in installed.iter().rev() {
                    let _ = delete_route(interface_index, previous);
                }
                return Err(format!(
                    "Pmman 路由安装超时（已安装 {}/{} 条，限时 {} 秒）",
                    installed.len(),
                    routes.len(),
                    timeout.as_secs()
                ));
            }
            if let Err(error) = add_route(&self.tunnel, interface_index, route) {
                route_log(&format!(
                    "route_install_failed tunnel={} route={} error={}",
                    self.tunnel,
                    route.cidr(),
                    error
                ));
                for previous in installed.iter().rev() {
                    let _ = delete_route(interface_index, previous);
                }
                return Err(format!("Pmman 路由 {} 配置失败：{error}", route.cidr()));
            }
            installed.push(route.clone());
        }
        route_log(&format!(
            "route_install_complete tunnel={} installed={}",
            self.tunnel,
            installed.len()
        ));
        self.owned
            .lock()
            .expect("route state poisoned")
            .extend(installed);
        Ok(())
    }

    #[cfg(not(windows))]
    pub fn add(&self, route: RouteSpec) -> Result<(), String> {
        self.owned
            .lock()
            .expect("route state poisoned")
            .insert(route);
        Ok(())
    }

    #[cfg(windows)]
    pub fn add(&self, route: RouteSpec) -> Result<(), String> {
        let index = wait_for_interface_index(&self.tunnel)?;
        route_log(&format!(
            "route_add_start tunnel={} route={} if_index={}",
            self.tunnel,
            route.cidr(),
            index
        ));
        add_route(&self.tunnel, index, &route)?;
        route_log(&format!(
            "route_add_complete tunnel={} route={}",
            self.tunnel,
            route.cidr()
        ));
        self.owned
            .lock()
            .expect("route state poisoned")
            .insert(route);
        Ok(())
    }

    #[cfg(not(windows))]
    pub fn remove(&self, route: &RouteSpec) -> Result<(), String> {
        self.owned
            .lock()
            .expect("route state poisoned")
            .remove(route);
        Ok(())
    }

    #[cfg(windows)]
    pub fn remove(&self, route: &RouteSpec) -> Result<(), String> {
        if let Ok(index) = interface_index(&self.tunnel) {
            delete_route(index, route)?;
        }
        route_log(&format!(
            "route_remove_complete tunnel={} route={}",
            self.tunnel,
            route.cidr()
        ));
        self.owned
            .lock()
            .expect("route state poisoned")
            .remove(route);
        Ok(())
    }

    #[cfg(not(windows))]
    pub fn clear(&self) -> Result<(), String> {
        self.owned.lock().expect("route state poisoned").clear();
        Ok(())
    }

    #[cfg(windows)]
    pub fn clear(&self) -> Result<(), String> {
        let started = std::time::Instant::now();
        let routes = self
            .owned
            .lock()
            .expect("route state poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        route_log(&format!(
            "route_clear_start tunnel={} route_count={}",
            self.tunnel,
            routes.len()
        ));
        if let Ok(index) = interface_index(&self.tunnel) {
            for route in &routes {
                let _ = delete_route(index, route);
            }
        }
        route_log(&format!(
            "route_clear tunnel={} route_count={} elapsed_ms={}",
            self.tunnel,
            routes.len(),
            started.elapsed().as_millis()
        ));
        self.owned.lock().expect("route state poisoned").clear();
        Ok(())
    }
}

#[cfg(windows)]
fn interface_index(alias: &str) -> Result<u32, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceAliasToLuid, ConvertInterfaceLuidToIndex,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;

    let wide = std::ffi::OsStr::new(alias)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut luid = NET_LUID_LH { Value: 0 };
    let status = unsafe { ConvertInterfaceAliasToLuid(wide.as_ptr(), &mut luid) };
    if status != 0 {
        // Some Windows builds return ERROR_INVALID_PARAMETER (87) for a
        // freshly-created WireGuard alias even though Get-NetAdapter can
        // already resolve it. Keep the IP Helper route API, but use the
        // well-tested PowerShell adapter lookup as a compatibility fallback.
        if let Some(index) = powershell_interface_index(alias) {
            return Ok(index);
        }
        return Err(format!(
            "无法找到 WireGuard 接口 {alias}（Windows 错误 {status}）"
        ));
    }
    let mut index = 0;
    let status = unsafe { ConvertInterfaceLuidToIndex(&luid, &mut index) };
    if status != 0 {
        if let Some(index) = powershell_interface_index(alias) {
            return Ok(index);
        }
        return Err(format!(
            "无法读取 WireGuard 接口索引（Windows 错误 {status}）"
        ));
    }
    Ok(index)
}

#[cfg(windows)]
fn wait_for_interface_index(alias: &str) -> Result<u32, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut last_error = String::new();
    while std::time::Instant::now() < deadline {
        match interface_index(alias) {
            Ok(index) => return Ok(index),
            Err(error) => last_error = error,
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let error = format!("无法找到 WireGuard 接口 {alias}（接口启动超时；{last_error}）");
    route_log(&format!(
        "interface_wait_timeout alias={} error={}",
        alias, error
    ));
    Err(error)
}

#[cfg(windows)]
fn powershell_interface_index(alias: &str) -> Option<u32> {
    // tunnel names are validated by the caller to [A-Za-z0-9_-], so this
    // single-quoted PowerShell literal cannot be used for command injection.
    let script = format!(
        "$ErrorActionPreference='Stop';(Get-NetAdapter -Name '{alias}' -ErrorAction Stop).ifIndex"
    );
    let mut command = std::process::Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

#[cfg(windows)]
fn sockaddr(address: IpAddr) -> windows_sys::Win32::Networking::WinSock::SOCKADDR_INET {
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, IN6_ADDR, IN6_ADDR_0, IN_ADDR, IN_ADDR_0, SOCKADDR_IN, SOCKADDR_IN6,
        SOCKADDR_IN6_0, SOCKADDR_INET,
    };
    match address {
        IpAddr::V4(address) => SOCKADDR_INET {
            Ipv4: SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: 0,
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes(address.octets()),
                    },
                },
                sin_zero: [0; 8],
            },
        },
        IpAddr::V6(address) => SOCKADDR_INET {
            Ipv6: SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: IN6_ADDR {
                    u: IN6_ADDR_0 {
                        Byte: address.octets(),
                    },
                },
                Anonymous: SOCKADDR_IN6_0 { sin6_scope_id: 0 },
            },
        },
    }
}

#[cfg(windows)]
fn empty_sockaddr(ipv6: bool) -> windows_sys::Win32::Networking::WinSock::SOCKADDR_INET {
    if ipv6 {
        sockaddr(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED))
    } else {
        sockaddr(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
    }
}

#[cfg(windows)]
fn make_row(
    interface_index: u32,
    route: &RouteSpec,
) -> windows_sys::Win32::NetworkManagement::IpHelper::MIB_IPFORWARD_ROW2 {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        InitializeIpForwardEntry, IP_ADDRESS_PREFIX, MIB_IPFORWARD_ROW2,
    };
    use windows_sys::Win32::Networking::WinSock::{NlroManual, MIB_IPPROTO_NETMGMT};

    let mut row = MaybeUninit::<MIB_IPFORWARD_ROW2>::uninit();
    unsafe { InitializeIpForwardEntry(row.as_mut_ptr()) };
    let mut row = unsafe { row.assume_init() };
    row.InterfaceIndex = interface_index;
    row.DestinationPrefix = IP_ADDRESS_PREFIX {
        Prefix: sockaddr(route.destination),
        PrefixLength: route.prefix_len,
    };
    row.NextHop = empty_sockaddr(route.destination.is_ipv6());
    // This is the site prefix, not the destination prefix. Zero means the
    // route is not tied to a site-prefix policy (the normal host-route case).
    row.SitePrefixLength = 0;
    row.ValidLifetime = u32::MAX;
    row.PreferredLifetime = u32::MAX;
    row.Metric = route.metric;
    row.Protocol = MIB_IPPROTO_NETMGMT;
    row.Loopback = 0;
    row.AutoconfigureAddress = 0;
    row.Publish = 0;
    row.Immortal = 1;
    row.Origin = NlroManual;
    row
}

#[cfg(windows)]
fn add_route(alias: &str, initial_interface_index: u32, route: &RouteSpec) -> Result<(), String> {
    use windows_sys::Win32::Foundation::ERROR_OBJECT_ALREADY_EXISTS;
    use windows_sys::Win32::NetworkManagement::IpHelper::CreateIpForwardEntry2;

    // WireGuard's tunnel service reports a successful install before the
    // adapter is fully visible to the IP Helper route table.  During that
    // short window CreateIpForwardEntry2 commonly returns ERROR_NOT_FOUND
    // (1168), even though the adapter alias and the interface index can
    // already be queried.  Re-resolve the index while retrying so a stale
    // index from a just-replaced tunnel cannot poison the connection.
    const ERROR_NOT_FOUND: u32 = 1168;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(12);
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

    let deadline = std::time::Instant::now() + RETRY_WINDOW;
    let mut current_interface_index = initial_interface_index;
    let mut attempts = 0u32;

    loop {
        let row = make_row(current_interface_index, route);
        let status = unsafe { CreateIpForwardEntry2(&row) };
        // A split snapshot contains thousands of prefixes. Log failures and
        // recovered retries, not every successful insertion: otherwise one
        // connection rotates away the failure that prompted a report.
        if (status != 0 && status != ERROR_OBJECT_ALREADY_EXISTS) || attempts > 0 {
            route_log(&format!(
                "route_add_attempt alias={} route={} if_index={} attempt={} status={}",
                alias,
                route.cidr(),
                current_interface_index,
                attempts + 1,
                status
            ));
        }
        if status == 0 || status == ERROR_OBJECT_ALREADY_EXISTS {
            return Ok(());
        }
        attempts += 1;

        if status != ERROR_NOT_FOUND && status != ERROR_INVALID_PARAMETER {
            return Err(format!("Windows 错误 {status}"));
        }
        if std::time::Instant::now() >= deadline {
            route_log(&format!(
                "route_add_timeout alias={} route={} if_index={} attempts={} status={}",
                alias,
                route.cidr(),
                current_interface_index,
                attempts,
                status
            ));
            return Err(format!(
                "Windows 错误 {status}（WireGuard 接口尚未就绪，已重试 {attempts} 次）"
            ));
        }

        if let Ok(index) = interface_index(alias) {
            if index != current_interface_index {
                route_log(&format!(
                    "interface_index_refresh alias={} old_if_index={} new_if_index={}",
                    alias, current_interface_index, index
                ));
            }
            current_interface_index = index;
        }
        std::thread::sleep(RETRY_DELAY);
    }
}

#[cfg(windows)]
fn route_log(message: &str) {
    crate::diagnostics::log("route", message);
}

#[cfg(windows)]
fn delete_route(interface_index: u32, route: &RouteSpec) -> Result<(), String> {
    use windows_sys::Win32::NetworkManagement::IpHelper::DeleteIpForwardEntry2;
    let row = make_row(interface_index, route);
    let status = unsafe { DeleteIpForwardEntry2(&row) };
    if status == 0 || status == 1168 || status == 5010 {
        Ok(())
    } else {
        route_log(&format!(
            "route_delete_failed route={} if_index={} status={}",
            route.cidr(),
            interface_index,
            status
        ));
        Err(format!("Windows 错误 {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_adds_gateways_and_low_priority_ipv6_fallback() {
        let routes = plan(
            &["10.20.0.0/16".into(), "fd20:1000::/48".into()],
            Some("10.88.0.1/24"),
            Some("fd88:1000::1/64"),
            "internal",
        )
        .unwrap();
        assert!(routes
            .iter()
            .any(|r| r.cidr() == "10.88.0.1/32" && r.metric == 5));
        assert!(routes
            .iter()
            .any(|r| r.cidr() == "fd88:1000::1/128" && r.metric == 5));
        assert!(routes
            .iter()
            .any(|r| r.cidr() == "::/0" && r.metric == 5000));
    }

    #[test]
    fn plan_deduplicates_routes_but_preserves_metric_identity() {
        let routes = plan(
            &["fd20::/48".into(), "fd20::/48".into()],
            None,
            None,
            "internal",
        )
        .unwrap();
        assert_eq!(routes.iter().filter(|r| r.cidr() == "fd20::/48").count(), 1);
    }

    #[test]
    fn global_mode_does_not_add_fallback_default() {
        let routes = plan(&["::/1".into(), "8000::/1".into()], None, None, "global").unwrap();
        assert!(!routes.iter().any(|r| r.cidr() == "::/0"));
    }

    #[test]
    fn internal_and_split_modes_install_low_priority_ipv6_fallback() {
        for mode in ["internal", "split"] {
            let routes = plan(&["fd20:1000::/48".into()], None, None, mode).unwrap();
            assert!(routes
                .iter()
                .any(|route| route.cidr() == "::/0" && route.metric == 5000));
        }
    }
}
