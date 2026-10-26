//! Best-effort UPnP IGD support for the P2P UDP socket.
//!
//! UPnP is only an acceleration path. The service never maps the WireGuard
//! tunnel port, never exposes a control endpoint, and continues to use the
//! node relay when an IGD is absent, disabled, or rejects the request.

use igd::{search_gateway, PortMappingProtocol, SearchOptions};
use pmman_protocol::p2p::valid_candidate;
use std::{
    net::{SocketAddr, SocketAddrV4},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

const LEASE_SECONDS: u32 = 300;
const REFRESH_AFTER: Duration = Duration::from_secs(120);
const RETRY_AFTER: Duration = Duration::from_secs(30);
const DESCRIPTION: &str = "Pmman Network P2P";

struct Mapping {
    gateway: igd::Gateway,
    external: SocketAddrV4,
}

/// Starts the optional IGD worker. The worker is deliberately independent of
/// the packet loop because discovery and SOAP requests can be slow on broken
/// home routers. `local` is the address of the already-bound UDP socket.
pub fn start(
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    local: Option<SocketAddrV4>,
    candidates: Arc<Mutex<Vec<SocketAddr>>>,
    mapped_candidate: Arc<Mutex<Option<SocketAddr>>>,
    status: Arc<Mutex<String>>,
) -> Option<JoinHandle<()>> {
    let local = local?;
    Some(std::thread::spawn(move || {
        run(enabled, stop, local, candidates, mapped_candidate, status)
    }))
}

fn run(
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    local: SocketAddrV4,
    candidates: Arc<Mutex<Vec<SocketAddr>>>,
    mapped_candidate: Arc<Mutex<Option<SocketAddr>>>,
    status: Arc<Mutex<String>>,
) {
    let mut mapping = None;
    let mut retry_at = std::time::Instant::now();

    while !stop.load(Ordering::Acquire) {
        if !enabled.load(Ordering::Acquire) {
            if let Some(old) = mapping.take() {
                remove_mapping(&old);
                remove_candidate(&candidates, &mapped_candidate, old.external.into());
            }
            set_status(&status, "");
            sleep_or_stop(&stop, Duration::from_secs(1));
            continue;
        }

        if let Some(current) = mapping.as_ref() {
            if current.external.ip().is_unspecified() {
                mapping = None;
            } else if retry_at.elapsed() >= REFRESH_AFTER {
                if current
                    .gateway
                    .add_port(
                        PortMappingProtocol::UDP,
                        current.external.port(),
                        local,
                        LEASE_SECONDS,
                        DESCRIPTION,
                    )
                    .is_err()
                {
                    let old = mapping.take().expect("mapping exists");
                    remove_candidate(&candidates, &mapped_candidate, old.external.into());
                    mapping = None;
                    retry_at = std::time::Instant::now() + RETRY_AFTER;
                    set_status(&status, "UPnP 映射续租失败，继续使用 UDP 打洞和节点中转");
                } else {
                    retry_at = std::time::Instant::now();
                }
            }
        } else if std::time::Instant::now() >= retry_at {
            match discover_and_map(local) {
                Ok((gateway, external)) => {
                    let candidate: SocketAddr = external.into();
                    if valid_candidate(candidate) {
                        *mapped_candidate.lock().expect("UPnP candidate poisoned") =
                            Some(candidate);
                        let mut list = candidates.lock().expect("candidate list poisoned");
                        list.retain(|item| *item != candidate);
                        if list.len() >= 16 {
                            list.pop();
                        }
                        list.push(candidate);
                        drop(list);
                        mapping = Some(Mapping { gateway, external });
                        retry_at = std::time::Instant::now();
                        set_status(&status, "UPnP 已启用，正在进行 UDP 直连探测");
                    } else {
                        retry_at = std::time::Instant::now() + RETRY_AFTER;
                        set_status(&status, "UPnP 返回了无效地址，继续使用 UDP 打洞和节点中转");
                    }
                }
                Err(_) => {
                    retry_at = std::time::Instant::now() + RETRY_AFTER;
                    set_status(&status, "未发现可用 UPnP 网关，继续使用 UDP 打洞和节点中转");
                }
            }
        }

        sleep_or_stop(&stop, Duration::from_secs(1));
    }

    if let Some(old) = mapping {
        remove_mapping(&old);
        remove_candidate(&candidates, &mapped_candidate, old.external.into());
    }
}

fn discover_and_map(local: SocketAddrV4) -> Result<(igd::Gateway, SocketAddrV4), String> {
    // IGD discovery is interface-local.  Binding to 0.0.0.0 makes some
    // routers answer on a different interface (or not answer at all), even
    // though the host has a working default IPv4 route.  `local` is selected
    // from the same underlay socket that carries P2P traffic, so use its
    // address for SSDP as well.
    let gateway = search_gateway(SearchOptions {
        bind_addr: SocketAddr::V4(SocketAddrV4::new(*local.ip(), 0)),
        timeout: Some(Duration::from_secs(3)),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    let external = gateway
        .get_any_address(PortMappingProtocol::UDP, local, LEASE_SECONDS, DESCRIPTION)
        .map_err(|error| error.to_string())?;
    Ok((gateway, external))
}

fn remove_mapping(mapping: &Mapping) {
    let _ = mapping
        .gateway
        .remove_port(PortMappingProtocol::UDP, mapping.external.port());
}

fn remove_candidate(
    candidates: &Arc<Mutex<Vec<SocketAddr>>>,
    mapped_candidate: &Arc<Mutex<Option<SocketAddr>>>,
    candidate: SocketAddr,
) {
    candidates
        .lock()
        .expect("candidate list poisoned")
        .retain(|item| *item != candidate);
    let mut mapped = mapped_candidate.lock().expect("UPnP candidate poisoned");
    if *mapped == Some(candidate) {
        *mapped = None;
    }
}

fn set_status(status: &Arc<Mutex<String>>, value: &str) {
    *status.lock().expect("UPnP status poisoned") = value.to_string();
}

fn sleep_or_stop(stop: &AtomicBool, duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while !stop.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}
