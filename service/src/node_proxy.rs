use crate::limiter::{Direction, RateLimitCancellation, RateLimiter};
use std::{
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    sync::Arc,
    thread::JoinHandle,
    time::Duration,
};

/// Keep node transport diagnostics next to the route diagnostics. This file
/// is intentionally append-only and never records credentials or packet data.
pub(crate) fn proxy_log(message: &str) {
    crate::diagnostics::log("node-proxy", message);
}

/// A loopback UDP endpoint used only by the PMMan WireGuard peer. It limits
/// the selected node path in both directions; P2P peers use another endpoint
/// and never pass through this proxy.
pub struct NodeProxy {
    endpoint: SocketAddr,
    remote_endpoint: SocketAddr,
    remote_is_ipv4: bool,
    underlay_v4: UdpSocket,
    underlay_v6: Option<UdpSocket>,
    stop: Arc<RateLimitCancellation>,
    limits: Arc<RateLimiter>,
    worker: Option<JoinHandle<()>>,
}

impl NodeProxy {
    pub fn start(
        remote_endpoint: &str,
        wireguard_port: u16,
        limits: Arc<RateLimiter>,
    ) -> Result<Self, String> {
        let addresses = remote_endpoint
            .to_socket_addrs()
            .map_err(|error| format!("节点地址解析失败：{error}"))?
            .collect::<Vec<_>>();
        let remote = choose_remote_endpoint(addresses)
            .ok_or_else(|| "节点地址没有可用解析结果".to_string())?;
        let local = UdpSocket::bind("127.0.0.1:0")
            .map_err(|error| format!("节点限速代理启动失败：{error}"))?;
        local
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let endpoint = local.local_addr().map_err(|error| error.to_string())?;
        // The loopback socket is only the WireGuard-facing side. It must not
        // also be used for the public node path: packets sent from a socket
        // bound to 127.0.0.1 cannot reach the node's public endpoint.
        let underlay_v4 = UdpSocket::bind("0.0.0.0:0")
            .map_err(|error| format!("节点 IPv4 限速代理外联启动失败：{error}"))?;
        underlay_v4
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let v6 = if remote.is_ipv6() {
            let socket = UdpSocket::bind("[::]:0")
                .map_err(|error| format!("节点 IPv6 限速代理启动失败：{error}"))?;
            socket
                .set_nonblocking(true)
                .map_err(|error| error.to_string())?;
            Some(socket)
        } else {
            None
        };
        pin_underlay(&underlay_v4, v6.as_ref(), remote.is_ipv4())?;
        proxy_log(&format!(
            "start remote={} local={} wireguard_port={} underlay4={} underlay6={}",
            remote,
            endpoint,
            wireguard_port,
            underlay_v4
                .local_addr()
                .map(|address| address.to_string())
                .unwrap_or_else(|_| "unknown".into()),
            v6.as_ref()
                .and_then(|socket| socket.local_addr().ok())
                .map(|address| address.to_string())
                .unwrap_or_else(|| "none".into())
        ));
        let stop = Arc::new(RateLimitCancellation::default());
        let stopped = Arc::clone(&stop);
        let worker_limits = Arc::clone(&limits);
        let worker_v4 = underlay_v4.try_clone().map_err(|error| error.to_string())?;
        let worker_v6 = v6
            .as_ref()
            .map(UdpSocket::try_clone)
            .transpose()
            .map_err(|error| error.to_string())?;
        let worker = std::thread::spawn(move || {
            run(
                local,
                worker_v4,
                worker_v6,
                remote,
                SocketAddr::from(([127, 0, 0, 1], wireguard_port)),
                stopped,
                worker_limits,
            )
        });
        Ok(Self {
            endpoint,
            remote_endpoint: remote,
            remote_is_ipv4: remote.is_ipv4(),
            underlay_v4,
            underlay_v6: v6,
            stop,
            limits,
            worker: Some(worker),
        })
    }

    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    /// Return the resolved public node endpoint used by the underlay socket.
    /// MTU probing must target this address and must happen outside the new
    /// WireGuard interface so the probe cannot recurse into the tunnel.
    pub fn remote_endpoint(&self) -> SocketAddr {
        self.remote_endpoint
    }

    pub fn set_limits(&self, limits: pmman_protocol::NodeRateLimit) {
        self.limits.set_limits(limits);
    }

    pub fn limits(&self) -> pmman_protocol::NodeRateLimit {
        self.limits.limits()
    }

    /// Re-apply the physical-interface binding after a network change. The
    /// node endpoint must never follow the global WireGuard routes it helps
    /// establish, otherwise the outer UDP packet loops back into the tunnel.
    pub fn refresh_underlay(&self) -> Result<(), String> {
        let result = pin_underlay(
            &self.underlay_v4,
            self.underlay_v6.as_ref(),
            self.remote_is_ipv4,
        );
        if let Err(error) = &result {
            proxy_log(&format!("refresh_underlay_failed error={error}"));
        }
        result
    }
}

/// A node hostname can have both A and AAAA records.  The WireGuard tunnel
/// itself uses the loopback proxy endpoint, so the proxy is responsible for
/// choosing the real underlay.  Prefer IPv4 when both families are available:
/// on Windows an IPv6 default route may exist while the particular public
/// IPv6 path is filtered or asymmetric.  Selecting the first resolver result
/// made the connection outcome depend on DNS answer order.  IPv6-only hosts
/// still use the IPv6 result.
fn choose_remote_endpoint(mut addresses: Vec<SocketAddr>) -> Option<SocketAddr> {
    addresses.sort_by_key(|address| if address.is_ipv4() { 0 } else { 1 });
    addresses.into_iter().next()
}

impl Drop for NodeProxy {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(
    local: UdpSocket,
    underlay_v4: UdpSocket,
    underlay_v6: Option<UdpSocket>,
    mut remote: SocketAddr,
    wireguard: SocketAddr,
    stop: Arc<RateLimitCancellation>,
    limits: Arc<RateLimiter>,
) {
    let mut buffer = [0u8; 65535];
    let mut upload_packets = 0u64;
    let mut download_packets = 0u64;
    let mut upload_errors = 0u64;
    let mut last_error_log = std::time::Instant::now()
        .checked_sub(Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);
    while !stop.is_cancelled() {
        let mut did_work = false;
        if let Ok((size, source)) = local.recv_from(&mut buffer) {
            did_work = true;
            if source.ip().is_loopback() {
                if !limits.wait_take(Direction::Upload, size, &stop) || stop.is_cancelled() {
                    break;
                }
                let result = if remote.is_ipv4() {
                    underlay_v4.send_to(&buffer[..size], remote)
                } else if let Some(socket) = &underlay_v6 {
                    socket.send_to(&buffer[..size], remote)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::AddrNotAvailable,
                        "IPv6 socket unavailable",
                    ))
                };
                match result {
                    Ok(_) => upload_packets += 1,
                    Err(error) => {
                        upload_errors += 1;
                        if last_error_log.elapsed() >= Duration::from_secs(2) {
                            proxy_log(&format!(
                                "upload_failed remote={} error={} packets={} errors={}",
                                remote, error, upload_packets, upload_errors
                            ));
                            last_error_log = std::time::Instant::now();
                        }
                    }
                }
            }
        }
        if let Ok((size, source)) = underlay_v4.recv_from(&mut buffer) {
            did_work = true;
            if remote.is_ipv4() && source.ip() == remote.ip() {
                if !limits.wait_take(Direction::Download, size, &stop) || stop.is_cancelled() {
                    break;
                }
                if local.send_to(&buffer[..size], wireguard).is_ok() {
                    download_packets += 1;
                }
                // NATs can change the observed source port. Keep the node IP
                // trust boundary while accepting its fresh UDP source port.
                remote = source;
            }
        }
        if let Some(socket) = &underlay_v6 {
            if let Ok((size, source)) = socket.recv_from(&mut buffer) {
                did_work = true;
                if remote.is_ipv6() && source.ip() == remote.ip() {
                    if !limits.wait_take(Direction::Download, size, &stop) || stop.is_cancelled() {
                        break;
                    }
                    if local.send_to(&buffer[..size], wireguard).is_ok() {
                        download_packets += 1;
                    }
                    remote = source;
                }
            }
        }
        if !did_work {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    proxy_log(&format!(
        "stopped upload_packets={} download_packets={} upload_errors={}",
        upload_packets, download_packets, upload_errors
    ));
}

// Unit tests use loopback fake nodes: pinning those sockets to a physical
// Windows interface makes them unreachable. Real underlay binding is covered
// by the separate public-network acceptance, not the loopback fixture.
#[cfg(all(windows, not(test)))]
fn pin_underlay(v4: &UdpSocket, v6: Option<&UdpSocket>, require_v4: bool) -> Result<(), String> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        setsockopt, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, IP_UNICAST_IF,
    };

    #[derive(serde::Deserialize)]
    struct Interface {
        index: u32,
        family: u8,
    }

    let command = "$r=@();foreach($f in @(4,6)){$prefix=if($f -eq 4){'0.0.0.0/0'}else{'::/0'};$route=Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue | Where-Object {$_.InterfaceAlias -notlike '*pmman*' -and $_.State -eq 'Alive'} | Sort-Object RouteMetric | Select-Object -First 1;if($route){$r+=@{index=[uint32]$route.InterfaceIndex;family=$f}}};ConvertTo-Json -InputObject @($r) -Compress";
    let output = crate::hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .output()
        .map_err(|error| format!("读取物理出口失败：{error}"))?;
    if !output.status.success() {
        return Err("读取物理出口失败".into());
    }
    let interfaces: Vec<Interface> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("解析物理出口失败：{error}"))?;
    let mut v4_index = None;
    let mut v6_index = None;
    for interface in interfaces {
        match interface.family {
            4 => v4_index = Some(interface.index),
            6 => v6_index = Some(interface.index),
            _ => {}
        }
    }
    if require_v4 && v4_index.is_none() {
        return Err("没有可用的物理 IPv4 出口".into());
    }
    if v6.is_some() && v6_index.is_none() {
        return Err("没有可用的物理 IPv6 出口".into());
    }
    unsafe {
        if let Some(v4_index) = v4_index {
            let v4_index = v4_index.to_be();
            if setsockopt(
                v4.as_raw_socket() as usize,
                IPPROTO_IP,
                IP_UNICAST_IF,
                (&v4_index as *const u32).cast(),
                4,
            ) != 0
            {
                return Err("绑定节点 IPv4 外层 UDP 出口失败".into());
            }
        }
        if let (Some(socket), Some(index)) = (v6, v6_index) {
            if setsockopt(
                socket.as_raw_socket() as usize,
                IPPROTO_IPV6,
                IPV6_UNICAST_IF,
                (&index as *const u32).cast(),
                4,
            ) != 0
            {
                return Err("绑定节点 IPv6 外层 UDP 出口失败".into());
            }
        }
    }
    Ok(())
}

#[cfg(any(not(windows), test))]
fn pin_underlay(_: &UdpSocket, _: Option<&UdpSocket>, _: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pmman_protocol::NodeRateLimit;
    use std::sync::{mpsc, Mutex};
    use std::time::Instant;

    #[test]
    fn proxy_drop_cancels_pending_upload_and_releases_status_lock() {
        assert_drop_cancels_pending_packet(Direction::Upload);
    }

    #[test]
    fn proxy_drop_cancels_pending_download_and_releases_status_lock() {
        assert_drop_cancels_pending_packet(Direction::Download);
    }

    fn assert_drop_cancels_pending_packet(direction: Direction) {
        let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
        let remote_endpoint = remote.local_addr().unwrap();
        let wireguard = UdpSocket::bind("127.0.0.1:0").unwrap();
        let wireguard_endpoint = wireguard.local_addr().unwrap();
        let local = UdpSocket::bind("127.0.0.1:0").unwrap();
        local.set_nonblocking(true).unwrap();
        let endpoint = local.local_addr().unwrap();
        let underlay_v4 = UdpSocket::bind("127.0.0.1:0").unwrap();
        underlay_v4.set_nonblocking(true).unwrap();
        // Keep a receive-side clone to observe that run consumed the queued
        // packet before Drop. This avoids relying on an arbitrary sleep.
        let observed = match direction {
            Direction::Upload => local.try_clone().unwrap(),
            Direction::Download => underlay_v4.try_clone().unwrap(),
        };
        let packet = vec![0u8; 65_507];
        match direction {
            Direction::Upload => wireguard.send_to(&packet, endpoint).unwrap(),
            Direction::Download => remote
                .send_to(&packet, underlay_v4.local_addr().unwrap())
                .unwrap(),
        };
        assert!(observed.peek_from(&mut [0u8; 65_535]).is_ok());
        let limits = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 1,
        });
        let stop = Arc::new(RateLimitCancellation::default());
        let worker_limits = Arc::clone(&limits);
        let worker_stop = Arc::clone(&stop);
        let worker_v4 = underlay_v4.try_clone().unwrap();
        let worker = std::thread::spawn(move || {
            run(
                local,
                worker_v4,
                None,
                remote_endpoint,
                wireguard_endpoint,
                worker_stop,
                worker_limits,
            );
        });
        let proxy = NodeProxy {
            endpoint,
            remote_endpoint,
            remote_is_ipv4: true,
            underlay_v4,
            underlay_v6: None,
            stop,
            limits,
            worker: Some(worker),
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match observed.peek_from(&mut [0u8; 65_535]) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Ok(_) => {
                    assert!(
                        Instant::now() < deadline,
                        "proxy did not receive the packet"
                    );
                    std::thread::yield_now();
                }
                Err(error) => panic!("observe proxy receive failed: {error}"),
            }
        }
        let state = Arc::new(Mutex::new(Some(proxy)));
        let drop_state = Arc::clone(&state);
        let (dropping, drop_started) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            let mut guard = drop_state.lock().unwrap();
            dropping.send(()).unwrap();
            // Match cleanup at the call site: Drop runs with state locked.
            drop(guard.take());
            drop(guard);
            let _ = done.send(());
        });
        drop_started.recv_timeout(Duration::from_secs(3)).unwrap();
        let (status_done, status_completed) = mpsc::channel();
        let status = std::thread::spawn(move || {
            let disconnected = state.lock().unwrap().is_none();
            let _ = status_done.send(disconnected);
        });
        completed.recv_timeout(Duration::from_millis(250)).unwrap();
        assert!(status_completed
            .recv_timeout(Duration::from_millis(250))
            .unwrap());
        dropper.join().unwrap();
        status.join().unwrap();
        let receiver = match direction {
            Direction::Upload => remote,
            Direction::Download => wireguard,
        };
        receiver
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let error = receiver.recv_from(&mut [0u8; 65_535]).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn proxy_drop_joins_an_idle_worker() {
        let proxy = NodeProxy::start(
            "127.0.0.1:9",
            51820,
            RateLimiter::new(NodeRateLimit::default()),
        )
        .unwrap();
        let (done, completed) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(proxy);
            let _ = done.send(());
        });
        completed.recv_timeout(Duration::from_millis(250)).unwrap();
        dropper.join().unwrap();
    }

    #[test]
    fn proxy_prefers_ipv4_for_dual_stack_node() {
        let selected = choose_remote_endpoint(vec![
            "[2001:db8::10]:51820".parse().unwrap(),
            "192.0.2.10:51820".parse().unwrap(),
        ])
        .unwrap();
        assert!(selected.is_ipv4());
    }

    #[test]
    fn proxy_keeps_ipv6_when_it_is_the_only_endpoint() {
        let selected =
            choose_remote_endpoint(vec!["[2001:db8::10]:51820".parse().unwrap()]).unwrap();
        assert!(selected.is_ipv6());
    }

    #[test]
    fn proxy_binds_loopback_and_preserves_limited_endpoint() {
        let limits = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 2,
        });
        let proxy = NodeProxy::start("127.0.0.1:9", 51820, limits).unwrap();
        assert!(proxy.endpoint().ip().is_loopback());
    }

    #[test]
    fn proxy_forwards_between_loopback_wireguard_and_public_underlay() {
        let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
        remote.set_nonblocking(true).unwrap();
        let remote_endpoint = remote.local_addr().unwrap();
        let wireguard = UdpSocket::bind("127.0.0.1:0").unwrap();
        let wireguard_port = wireguard.local_addr().unwrap().port();
        wireguard
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let proxy = NodeProxy::start(
            &remote_endpoint.to_string(),
            wireguard_port,
            RateLimiter::new(NodeRateLimit::default()),
        )
        .unwrap();
        wireguard
            .send_to(b"wireguard-to-node", proxy.endpoint())
            .unwrap();
        let mut packet = [0u8; 64];
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let (size, source) = loop {
            match remote.recv_from(&mut packet) {
                Ok(value) => break value,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "fake node receive timed out"
                    );
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("fake node receive failed: {error}"),
            }
        };
        assert_eq!(&packet[..size], b"wireguard-to-node");
        remote.send_to(b"node-to-wireguard", source).unwrap();
        let (size, _) = wireguard.recv_from(&mut packet).unwrap();
        assert_eq!(&packet[..size], b"node-to-wireguard");
    }
}
