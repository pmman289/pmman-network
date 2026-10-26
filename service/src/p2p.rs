//! WireGuard remains the encryption engine. Loopback peer endpoints let this
//! transport detect demand without intercepting user IP packets or moving routes.
#[cfg(windows)]
#[path = "upnp.rs"]
mod upnp;

use crate::limiter::{Direction, RateLimiter};
use pmman_protocol::p2p::*;
use pmman_protocol::NodeRateLimit;
use std::{
    collections::{HashMap, VecDeque},
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

#[cfg(windows)]
use std::net::IpAddr;

pub struct Transport {
    pub enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    pub statuses: Arc<Mutex<Vec<PeerStatus>>>,
    pub traffic: Arc<Mutex<pmman_protocol::TrafficStats>>,
    pub message: Arc<Mutex<String>>,
    pub upnp_status: Arc<Mutex<String>>,
    rate_limiter: Arc<RateLimiter>,
    sender: mpsc::Sender<Update>,
    pub candidates: Arc<Mutex<Vec<SocketAddr>>>,
    underlay_v4: UdpSocket,
    underlay_v6: Option<UdpSocket>,
    upnp_candidate: Arc<Mutex<Option<SocketAddr>>>,
    #[cfg(windows)]
    upnp_worker: Option<std::thread::JoinHandle<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
struct Update {
    directory: Directory,
    bridges: Vec<(Peer, UdpSocket)>,
}
struct Pending {
    sequence: u64,
    packet: Vec<u8>,
    sent: Instant,
    direct: bool,
    attempts: u8,
}
struct Link {
    peer: Peer,
    bridge: UdpSocket,
    activity: Option<Instant>,
    direct: Option<(SocketAddr, Instant)>,
    // A probe only proves that a small authenticated UDP packet can travel.
    // Keep that candidate separate from a usable path until a real encrypted
    // datagram is acknowledged on the same endpoint.
    direct_confirmed: bool,
    direct_failures: u8,
    direct_probe_streak: u8,
    direct_probe_endpoint: Option<SocketAddr>,
    direct_blocked_until: Option<Instant>,
    probing: Option<Instant>,
    probe_at: Instant,
    probes: VecDeque<(u64, SocketAddr, Instant)>,
    pending: VecDeque<Pending>,
    status: PeerStatus,
    replay: ReplayWindow,
}
impl Link {
    fn new(peer: Peer, bridge: UdpSocket) -> Self {
        let status = PeerStatus {
            device_id: peer.device_id.to_string(),
            username: peer.username.clone(),
            public_key: peer.public_key.clone(),
            ipv4: peer.ipv4.clone(),
            ipv6: peer.ipv6.clone(),
            path: "idle".into(),
            ..Default::default()
        };
        Self {
            peer,
            bridge,
            activity: None,
            direct: None,
            direct_confirmed: false,
            direct_failures: 0,
            direct_probe_streak: 0,
            direct_probe_endpoint: None,
            direct_blocked_until: None,
            probing: None,
            probe_at: Instant::now() - Duration::from_secs(5),
            probes: VecDeque::new(),
            pending: VecDeque::new(),
            status,
            replay: ReplayWindow::default(),
        }
    }
    fn touch(&mut self) {
        if self.activity.is_none() {
            self.probing = Some(Instant::now());
        }
        self.activity = Some(Instant::now());
    }
    fn traffic(&mut self, bytes: &[u8]) {
        // WG type 4 with an empty authenticated payload is a 32-byte keepalive.
        // Rekey/probe/keepalive traffic must not keep idle application pairs alive.
        if self.activity.is_none() || (bytes.len() > 32 && bytes.get(..4) == Some(&[4, 0, 0, 0])) {
            self.touch();
        }
    }
    fn expire(&mut self, enabled: bool) {
        if self.activity.is_some_and(|t| t.elapsed() >= IDLE) {
            self.activity = None;
            self.direct = None;
            self.direct_confirmed = false;
            self.direct_probe_streak = 0;
            self.direct_probe_endpoint = None;
            self.direct_failures = 0;
            self.probing = None;
            self.probes.clear();
            self.pending.clear();
            self.status.path = "idle".into();
        } else if self
            .direct
            .is_some_and(|(_, t)| t.elapsed() > self.direct_lease())
            || !enabled
            || !self.peer.enabled
        {
            if self.direct.take().is_some() {
                self.status.fallback_count += 1;
            }
            self.direct_confirmed = false;
            self.direct_probe_streak = 0;
            self.direct_probe_endpoint = None;
            self.direct_failures = 0;
        }
    }
    fn repair_delay(&self) -> Duration {
        REPAIR_DELAY.max(Duration::from_millis(
            self.status
                .latency_ms
                .unwrap_or(0)
                .saturating_mul(3)
                .saturating_add(40)
                .min(1000),
        ))
    }
    fn direct_lease(&self) -> Duration {
        DIRECT_LEASE.max(Duration::from_millis(
            self.status
                .latency_ms
                .unwrap_or(0)
                .saturating_mul(6)
                .saturating_add(200),
        ))
    }
    fn direct_timeout(&mut self) {
        if self.direct.is_none() {
            return;
        }
        self.direct_failures = self.direct_failures.saturating_add(1);
        if self.direct_failures >= 3 {
            self.direct = None;
            self.direct_confirmed = false;
            self.direct_failures = 0;
            self.direct_probe_streak = 0;
            self.direct_probe_endpoint = None;
            // Do not immediately promote the same candidate on the next
            // probe. This is deliberately short enough to recover from a
            // real NAT mapping change, but long enough to prevent the UI and
            // traffic path from flapping on transient loss.
            self.direct_blocked_until = Some(Instant::now() + Duration::from_secs(5));
            self.status.fallback_count += 1;
            self.probing = Some(Instant::now());
        }
    }

    fn confirm_direct(&mut self, source: SocketAddr) {
        // A peer can complete hole punching first and send an authenticated
        // DATA/ACK before this side has accepted a PROBED candidate. The
        // packet already passed the pair HMAC and replay checks, so its
        // underlay source is stronger evidence than a probe. Adopt it here so
        // both sides converge on the same direct path instead of leaving the
        // receiver displayed as relay-only.
        self.direct = Some((source, Instant::now()));
        self.direct_confirmed = true;
        self.direct_failures = 0;
        self.direct_probe_streak = 0;
        self.direct_probe_endpoint = Some(source);
        self.direct_blocked_until = None;
        self.probing = None;
    }

    fn display_path(&self, enabled: bool) -> &'static str {
        if self.activity.is_none() {
            "idle"
        } else if self.direct_confirmed {
            "direct"
        } else if enabled
            && self.peer.enabled
            && (self.direct.is_some()
                || self
                    .probing
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(1500)))
        {
            "probing"
        } else {
            "relay"
        }
    }
}
impl Transport {
    pub fn start(
        enabled: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        wg_port: u16,
        rate_limit: NodeRateLimit,
    ) -> Result<Self, String> {
        let v4 = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("P2P UDP socket: {e}"))?;
        v4.set_nonblocking(true).map_err(|e| e.to_string())?;
        let v6 = UdpSocket::bind("[::]:0").ok();
        if let Some(socket) = &v6 {
            socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        }
        // Capture and pin the underlay before installing global WireGuard routes.
        let addresses = underlay(&v4, v6.as_ref()).unwrap_or_default();
        let underlay_v4 = v4.try_clone().map_err(|e| e.to_string())?;
        let underlay_v6 = v6
            .as_ref()
            .map(UdpSocket::try_clone)
            .transpose()
            .map_err(|e| e.to_string())?;
        let candidates = Arc::new(Mutex::new(addresses));
        let upnp_candidate = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&candidates);
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&statuses);
        let traffic = Arc::new(Mutex::new(pmman_protocol::TrafficStats::default()));
        let traffic_output = Arc::clone(&traffic);
        let rate_limiter = RateLimiter::new(rate_limit);
        let message = Arc::new(Mutex::new("正在同步互联目录".into()));
        let upnp_status = Arc::new(Mutex::new(String::new()));
        #[cfg(windows)]
        let upnp_worker = {
            let local = candidates
                .lock()
                .unwrap()
                .iter()
                .find_map(|candidate| match candidate {
                    SocketAddr::V4(address) => Some(*address),
                    SocketAddr::V6(_) => None,
                });
            upnp::start(
                Arc::clone(&enabled),
                Arc::clone(&stop),
                local,
                Arc::clone(&candidates),
                Arc::clone(&upnp_candidate),
                Arc::clone(&upnp_status),
            )
        };
        let (sender, receiver) = mpsc::channel();
        let stopped = Arc::clone(&stop);
        let feature = Arc::clone(&enabled);
        let worker_limiter = Arc::clone(&rate_limiter);
        let worker = std::thread::spawn(move || {
            run(
                v4,
                v6,
                wg_port,
                stopped,
                feature,
                receiver,
                output,
                observed,
                worker_limiter,
                traffic_output,
            )
        });
        Ok(Self {
            enabled,
            stop,
            statuses,
            traffic,
            message,
            upnp_status,
            rate_limiter,
            sender,
            candidates,
            upnp_candidate,
            #[cfg(windows)]
            upnp_worker,
            underlay_v4,
            underlay_v6,
            worker: Some(worker),
        })
    }
    pub fn update(
        &self,
        directory: Directory,
        bridges: Vec<(Peer, UdpSocket)>,
    ) -> Result<(), String> {
        self.sender
            .send(Update { directory, bridges })
            .map_err(|e| e.to_string())
    }

    pub fn set_rate_limit(&self, limit: NodeRateLimit) {
        self.rate_limiter.set_limits(limit);
    }
    pub fn refresh_underlay(&self) -> Result<(), String> {
        let addresses = underlay(&self.underlay_v4, self.underlay_v6.as_ref())?;
        // Replace stale LAN/NAT addresses. The next authenticated registration
        // repopulates the currently reflected address within two seconds.
        let mapped = *self.upnp_candidate.lock().unwrap();
        let mut addresses = addresses;
        if let Some(candidate) = mapped {
            if !addresses.contains(&candidate) && addresses.len() < 16 {
                addresses.push(candidate);
            }
        }
        *self.candidates.lock().unwrap() = addresses;
        Ok(())
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        #[cfg(windows)]
        if let Some(worker) = self.upnp_worker.take() {
            // IGD SOAP calls are intentionally isolated from the packet loop,
            // but the third-party sync client can wait up to its HTTP timeout.
            // Detach here so disconnect/revoke never blocks the UI for a
            // broken router; the worker observes `stop`, removes the mapping
            // when the in-flight request returns, and its short lease expires
            // if the process itself is terminated.
            drop(worker);
        }
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        self.stop();
    }
}

fn send(v4: &UdpSocket, v6: Option<&UdpSocket>, bytes: &[u8], target: SocketAddr) {
    let socket = if target.is_ipv4() { Some(v4) } else { v6 };
    if let Some(socket) = socket {
        let _ = socket.send_to(bytes, target);
    }
}
fn relay(
    v4: &UdpSocket,
    v6: Option<&UdpSocket>,
    directory: &Directory,
    link: &mut Link,
    bytes: &[u8],
    business: bool,
    rate_limiter: &RateLimiter,
) -> bool {
    if !rate_limiter.try_take(Direction::Upload, bytes.len()) {
        return false;
    }
    let packet = encode(
        RELAY,
        directory.device_id,
        link.peer.device_id,
        unix_ms(),
        bytes,
        &directory.relay_token,
    );
    send(v4, v6, &packet, directory.relay_endpoint);
    if business {
        link.status.relay_tx_bytes += Packet::parse(bytes).map_or(0, |p| p.payload.len() as u64);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn run(
    v4: UdpSocket,
    v6: Option<UdpSocket>,
    wg_port: u16,
    stop: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    updates: mpsc::Receiver<Update>,
    statuses: Arc<Mutex<Vec<PeerStatus>>>,
    candidates: Arc<Mutex<Vec<SocketAddr>>>,
    rate_limiter: Arc<RateLimiter>,
    traffic: Arc<Mutex<pmman_protocol::TrafficStats>>,
) {
    let mut directory: Option<Directory> = None;
    let mut links: HashMap<Uuid, Link> = HashMap::new();
    let mut retired = pmman_protocol::TrafficStats::default();
    let mut buffer = [0u8; MAX_DATAGRAM + 1];
    let mut sequence = unix_ms();
    let mut register_at = Instant::now() - Duration::from_secs(5);
    let mut register_sequence = 0;
    let mut publish_at = Instant::now();
    let wg_endpoint: SocketAddr = ([127, 0, 0, 1], wg_port).into();
    while !stop.load(Ordering::Acquire) {
        while let Ok(update) = updates.try_recv() {
            rate_limiter.set_limits(update.directory.relay_rate_limit);
            if directory.as_ref().is_none_or(|d| {
                d.relay_endpoint != update.directory.relay_endpoint
                    || d.relay_token != update.directory.relay_token
            }) {
                register_at = Instant::now() - Duration::from_secs(5);
            }
            links.retain(|id, link| {
                let keep = update.directory.peers.iter().any(|p| {
                    p.device_id == *id
                        && p.public_key == link.peer.public_key
                        && p.secret == link.peer.secret
                });
                if !keep {
                    retired.accumulate(link_traffic(&link.status));
                }
                keep
            });
            for peer in &update.directory.peers {
                if let Some(link) = links.get_mut(&peer.device_id) {
                    link.peer = peer.clone();
                }
            }
            for (peer, bridge) in update.bridges {
                if let Some(old) = links.insert(peer.device_id, Link::new(peer, bridge)) {
                    retired.accumulate(link_traffic(&old.status));
                }
            }
            directory = Some(update.directory);
        }
        let Some(d) = directory.as_ref() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if register_at.elapsed() >= Duration::from_secs(2) {
            register_sequence = unix_ms();
            send(
                &v4,
                v6.as_ref(),
                &encode(
                    REGISTER,
                    d.device_id,
                    Uuid::nil(),
                    register_sequence,
                    &[],
                    &d.relay_token,
                ),
                d.relay_endpoint,
            );
            register_at = Instant::now();
        }
        for socket in std::iter::once(&v4).chain(v6.iter()) {
            for _ in 0..256 {
                let Ok((n, source)) = socket.recv_from(&mut buffer) else {
                    break;
                };
                let bytes = &buffer[..n];
                let Some(header) = Packet::parse(bytes) else {
                    continue;
                };
                if header.kind == REGISTERED
                    && source == d.relay_endpoint
                    && header.dst == d.device_id
                {
                    if let Some(packet) = Packet::verify(bytes, &d.relay_token)
                        .filter(|p| p.sequence == register_sequence)
                    {
                        if let Ok(endpoint) = std::str::from_utf8(packet.payload)
                            .unwrap_or("")
                            .parse::<SocketAddr>()
                        {
                            let mut list = candidates.lock().unwrap();
                            if valid_candidate(endpoint) && !list.contains(&endpoint) {
                                if list.len() >= 16 {
                                    list.pop();
                                }
                                list.push(endpoint);
                            }
                        }
                    }
                    continue;
                }
                if header.dst != d.device_id {
                    continue;
                }
                let Some(link) = links.get_mut(&header.src) else {
                    continue;
                };
                let Some(packet) = Packet::verify(bytes, &link.peer.secret) else {
                    continue;
                };
                match packet.kind {
                    DATA => {
                        if packet.payload.len() < 4 || packet.payload.len() > 1500 {
                            continue;
                        }
                        let via_relay = source == d.relay_endpoint;
                        // Do not acknowledge a relay packet until its payload
                        // has received a download token. Otherwise the sender
                        // would remove its repair copy while the receiver
                        // silently dropped the payload under the cap.
                        if via_relay
                            && !rate_limiter.try_take(Direction::Download, packet.payload.len())
                        {
                            continue;
                        }
                        if link.replay.accept(packet.sequence) {
                            if !via_relay {
                                // A valid direct DATA frame proves that the
                                // path is usable even if this side had not
                                // selected the candidate from a probe yet.
                                link.confirm_direct(source);
                                link.status.direct_rx_bytes = link
                                    .status
                                    .direct_rx_bytes
                                    .saturating_add(packet.payload.len() as u64);
                            } else {
                                link.status.relay_rx_bytes = link
                                    .status
                                    .relay_rx_bytes
                                    .saturating_add(packet.payload.len() as u64);
                            }
                            link.traffic(packet.payload);
                            let _ = link.bridge.send_to(packet.payload, wg_endpoint);
                        }
                        let ack = encode(
                            ACK,
                            d.device_id,
                            link.peer.device_id,
                            packet.sequence,
                            &[],
                            &link.peer.secret,
                        );
                        if source == d.relay_endpoint {
                            relay(&v4, v6.as_ref(), d, link, &ack, false, &rate_limiter);
                        } else {
                            send(&v4, v6.as_ref(), &ack, source);
                        }
                    }
                    ACK => {
                        let direct_attempt = link
                            .pending
                            .iter()
                            .find(|pending| pending.sequence == packet.sequence)
                            .is_some_and(|pending| pending.direct);
                        link.pending.retain(|p| p.sequence != packet.sequence);
                        if source != d.relay_endpoint {
                            // Only a DATA/ACK exchange on the selected
                            // endpoint confirms a direct path. A PROBE alone
                            // must never make the UI report "direct". An ACK
                            // can also be the first authenticated evidence
                            // observed by this side, so adopt its source.
                            link.confirm_direct(source);
                        } else if source == d.relay_endpoint && direct_attempt {
                            // The relay ACK means the repair copy was the one
                            // that arrived. Count this as a failed direct
                            // attempt even when the relay was fast enough to
                            // beat the repair timer.
                            link.direct_timeout();
                        }
                    }
                    WAKE => {
                        if fresh(packet.sequence) {
                            link.touch();
                        }
                    }
                    PROBE
                        if enabled.load(Ordering::Relaxed)
                            && link.peer.enabled
                            && fresh(packet.sequence) =>
                    {
                        let response = encode(
                            PROBED,
                            d.device_id,
                            link.peer.device_id,
                            packet.sequence,
                            &[],
                            &link.peer.secret,
                        );
                        send(&v4, v6.as_ref(), &response, source);
                    }
                    PROBED
                        if source != d.relay_endpoint
                            && enabled.load(Ordering::Relaxed)
                            && link.peer.enabled =>
                    {
                        if let Some((_, _, sent)) = link
                            .probes
                            .iter()
                            .find(|(seq, ep, _)| {
                                *seq == packet.sequence && *ep == source && fresh(*seq)
                            })
                            .copied()
                        {
                            let latency = sent.elapsed().as_millis() as u64;
                            let blocked = link
                                .direct_blocked_until
                                .is_some_and(|until| until > Instant::now());
                            if blocked {
                                link.probes.retain(|(seq, ep, _)| {
                                    *seq != packet.sequence || *ep != source
                                });
                                continue;
                            }

                            // Require two consecutive replies from the same
                            // candidate before selecting it. NAT mappings can
                            // briefly answer a stale probe while the next
                            // packet is routed elsewhere.
                            if link.direct_probe_endpoint == Some(source) {
                                link.direct_probe_streak =
                                    link.direct_probe_streak.saturating_add(1);
                            } else {
                                link.direct_probe_endpoint = Some(source);
                                link.direct_probe_streak = 1;
                            }

                            let current = link.direct.map(|(endpoint, _)| endpoint);
                            let can_replace = current.is_none()
                                || current == Some(source)
                                || !link.direct_confirmed;
                            if can_replace && link.direct_probe_streak >= 2 {
                                let new_path = current != Some(source);
                                link.direct = Some((source, Instant::now()));
                                link.direct_failures = 0;
                                link.status.latency_ms = Some(latency);
                                link.probing = None;
                                link.direct_blocked_until = None;
                                if new_path {
                                    link.direct_confirmed = false;
                                    // The initial WG handshake may still be in
                                    // the relay queue. Deliver it immediately
                                    // on a candidate path; the real DATA ACK
                                    // below decides whether it is promoted to
                                    // the stable direct state.
                                    for pending in &mut link.pending {
                                        if !pending.direct {
                                            send(&v4, v6.as_ref(), &pending.packet, source);
                                            pending.direct = true;
                                            pending.sent = Instant::now();
                                            link.status.direct_tx_bytes +=
                                                Packet::parse(&pending.packet)
                                                    .map_or(0, |p| p.payload.len() as u64);
                                        }
                                    }
                                }
                            }
                            link.probes
                                .retain(|(seq, ep, _)| *seq != packet.sequence || *ep != source);
                        }
                    }
                    _ => {}
                }
            }
        }
        let feature = enabled.load(Ordering::Relaxed);
        for link in links.values_mut() {
            link.expire(feature);
            for _ in 0..64 {
                let Ok((n, source)) = link.bridge.recv_from(&mut buffer) else {
                    break;
                };
                if source != wg_endpoint || !(4..=1500).contains(&n) {
                    continue;
                }
                let first = link.activity.is_none();
                link.traffic(&buffer[..n]);
                sequence = sequence.wrapping_add(1);
                let packet = encode(
                    DATA,
                    d.device_id,
                    link.peer.device_id,
                    sequence,
                    &buffer[..n],
                    &link.peer.secret,
                );
                if let Some((endpoint, _)) = link.direct {
                    send(&v4, v6.as_ref(), &packet, endpoint);
                    link.status.direct_tx_bytes += n as u64;
                } else {
                    relay(&v4, v6.as_ref(), d, link, &packet, true, &rate_limiter);
                }
                if link.pending.len() >= 256 {
                    if let Some(old) = link.pending.pop_front() {
                        relay(&v4, v6.as_ref(), d, link, &old.packet, true, &rate_limiter);
                    }
                }
                link.pending.push_back(Pending {
                    sequence,
                    packet,
                    sent: Instant::now(),
                    direct: link.direct.is_some(),
                    attempts: 0,
                });
                if first {
                    let wake = encode(
                        WAKE,
                        d.device_id,
                        link.peer.device_id,
                        unix_ms(),
                        &[],
                        &link.peer.secret,
                    );
                    relay(&v4, v6.as_ref(), d, link, &wake, false, &rate_limiter);
                }
            }
            while link
                .pending
                .front()
                .is_some_and(|p| p.sent.elapsed() >= link.repair_delay())
            {
                let mut pending = link.pending.pop_front().unwrap();
                relay(
                    &v4,
                    v6.as_ref(),
                    d,
                    link,
                    &pending.packet,
                    true,
                    &rate_limiter,
                );
                if pending.direct && link.direct.is_some() {
                    // Do not tear down a path because of one lost data frame.
                    // Probes and successfully acknowledged frames reset this
                    // counter; three consecutive failures switch active
                    // traffic back to the authenticated relay.
                    link.direct_timeout();
                }
                pending.direct = false;
                pending.attempts += 1;
                pending.sent = Instant::now();
                if pending.attempts < 25 {
                    link.pending.push_back(pending);
                }
            }
            if link.activity.is_some() && feature && link.peer.enabled {
                let delay = if link.direct.is_some()
                    || link
                        .probing
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
                {
                    Duration::from_millis(200)
                } else {
                    Duration::from_secs(5)
                };
                if link.probe_at.elapsed() >= delay {
                    for endpoint in &link.peer.candidates {
                        let seq = unix_ms();
                        let probe = encode(
                            PROBE,
                            d.device_id,
                            link.peer.device_id,
                            seq,
                            &[],
                            &link.peer.secret,
                        );
                        send(&v4, v6.as_ref(), &probe, *endpoint);
                        link.probes.push_back((seq, *endpoint, Instant::now()));
                    }
                    link.probes
                        .retain(|(_, _, t)| t.elapsed() < Duration::from_secs(2));
                    link.probe_at = Instant::now();
                }
            }
            link.status.path = link.display_path(feature).into();
            link.status.direct_endpoint = link.direct.map(|(ep, _)| ep.to_string());
            if link.direct.is_none() {
                link.status.latency_ms = None;
            }
            link.status.last_activity_ms = link.activity.map(|t| t.elapsed().as_millis() as u64);
        }
        if publish_at.elapsed() >= Duration::from_millis(200) {
            let mut totals = retired;
            for link in links.values() {
                totals.accumulate(link_traffic(&link.status));
            }
            *traffic.lock().unwrap() = totals;
            let mut output: Vec<_> = links.values().map(|l| l.status.clone()).collect();
            output.sort_by(|a, b| a.device_id.cmp(&b.device_id));
            *statuses.lock().unwrap() = output;
            publish_at = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    statuses.lock().unwrap().clear();
}

fn link_traffic(status: &PeerStatus) -> pmman_protocol::TrafficStats {
    pmman_protocol::TrafficStats {
        p2p_direct_tx_bytes: status.direct_tx_bytes,
        p2p_direct_rx_bytes: status.direct_rx_bytes,
        p2p_relay_tx_bytes: status.relay_tx_bytes,
        p2p_relay_rx_bytes: status.relay_rx_bytes,
        ..Default::default()
    }
}

#[cfg(windows)]
fn underlay(v4: &UdpSocket, v6: Option<&UdpSocket>) -> Result<Vec<SocketAddr>, String> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        setsockopt, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, IP_UNICAST_IF,
    };
    #[derive(serde::Deserialize)]
    struct Interface {
        index: u32,
        address: String,
        family: u8,
    }
    let command="$r=@();foreach($f in @(4,6)){$prefix=if($f -eq 4){'0.0.0.0/0'}else{'::/0'};$route=Get-NetRoute -DestinationPrefix $prefix -ErrorAction SilentlyContinue | Where-Object {$_.InterfaceAlias -notlike '*pmman*'} | Sort-Object RouteMetric | Select-Object -First 1;if($route){Get-NetIPAddress -InterfaceIndex $route.InterfaceIndex -ErrorAction SilentlyContinue | Where-Object {$_.AddressState -eq 'Preferred' -and $_.IPAddress -notlike 'fe80:*'} | ForEach-Object {$r+=@{index=$route.InterfaceIndex;address=$_.IPAddress;family=$f}}}};ConvertTo-Json -InputObject @($r) -Compress";
    let output = super::hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .output()
        .map_err(|e| e.to_string())?;
    let interfaces: Vec<Interface> =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("读取公网接口失败：{e}"))?;
    let mut result = Vec::new();
    for item in interfaces {
        let Ok(ip) = item.address.parse::<IpAddr>() else {
            continue;
        };
        if (item.family == 4) != ip.is_ipv4() {
            continue;
        }
        let socket = if item.family == 4 { Some(v4) } else { v6 };
        let Some(socket) = socket else {
            continue;
        };
        let index = if item.family == 4 {
            item.index.to_be()
        } else {
            item.index
        };
        let (level, option) = if item.family == 4 {
            (IPPROTO_IP, IP_UNICAST_IF)
        } else {
            (IPPROTO_IPV6, IPV6_UNICAST_IF)
        };
        if unsafe {
            setsockopt(
                socket.as_raw_socket() as usize,
                level,
                option,
                (&index as *const u32).cast(),
                4,
            )
        } != 0
        {
            // P2P is an enhancement. A transient interface race (for example
            // while Windows is bringing up Wi-Fi) must not prevent the normal
            // WireGuard/relay connection; refresh_underlay will retry later.
            continue;
        }
        let endpoint = SocketAddr::new(ip, socket.local_addr().map_err(|e| e.to_string())?.port());
        if valid_candidate(endpoint) && result.len() < 15 {
            result.push(endpoint);
        }
    }
    Ok(result)
}
#[cfg(not(windows))]
fn underlay(_: &UdpSocket, _: Option<&UdpSocket>) -> Result<Vec<SocketAddr>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_links_survive_and_idle_links_expire() {
        let peer = Peer {
            device_id: Uuid::new_v4(),
            username: "a".into(),
            public_key: "key".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![],
            secret: "secret".into(),
        };
        let mut link = Link::new(peer, UdpSocket::bind("127.0.0.1:0").unwrap());
        assert!(link.activity.is_none());
        link.touch();
        link.direct = Some(("192.0.2.1:4000".parse().unwrap(), Instant::now()));
        link.expire(true);
        assert!(link.direct.is_some());
        link.expire(false);
        assert!(link.direct.is_none());
        assert_eq!(link.status.fallback_count, 1);
        assert!(link.activity.is_some());
        link.activity = Some(Instant::now() - IDLE);
        link.expire(true);
        assert!(link.activity.is_none());
        assert_eq!(link.status.path, "idle");
    }
    #[test]
    fn maintenance_packets_do_not_extend_business_idle_and_rtt_controls_repair() {
        let peer = Peer {
            device_id: Uuid::new_v4(),
            username: "a".into(),
            public_key: "key".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![],
            secret: "secret".into(),
        };
        let mut link = Link::new(peer, UdpSocket::bind("127.0.0.1:0").unwrap());
        let old = Instant::now() - IDLE;
        link.activity = Some(old);
        let mut keepalive = [0; 32];
        keepalive[0] = 4;
        link.traffic(&keepalive);
        link.traffic(&[1; 148]);
        assert_eq!(link.activity, Some(old));
        link.expire(true);
        assert!(link.activity.is_none());
        let mut data = [0; 64];
        data[0] = 4;
        link.traffic(&data);
        assert!(link.activity.is_some());
        link.status.latency_ms = Some(300);
        assert!(link.repair_delay() >= Duration::from_millis(940));
        assert!(link.direct_lease() >= Duration::from_secs(2));
        link.status.latency_ms = Some(u64::MAX);
        assert_eq!(link.repair_delay(), Duration::from_secs(1));
    }
    #[test]
    fn direct_path_has_loss_hysteresis() {
        let peer = Peer {
            device_id: Uuid::new_v4(),
            username: "loss-test".into(),
            public_key: "key".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![],
            secret: "secret".into(),
        };
        let mut link = Link::new(peer, UdpSocket::bind("127.0.0.1:0").unwrap());
        link.direct = Some(("198.51.100.10:4000".parse().unwrap(), Instant::now()));
        link.direct_timeout();
        link.direct_timeout();
        assert!(link.direct.is_some());
        assert_eq!(link.status.fallback_count, 0);
        link.direct_timeout();
        assert!(link.direct.is_none());
        assert_eq!(link.status.fallback_count, 1);
    }
    #[test]
    fn udp_probe_candidate_is_not_reported_as_direct_before_data_ack() {
        let peer = Peer {
            device_id: Uuid::new_v4(),
            username: "probe-only".into(),
            public_key: "key".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![],
            secret: "secret".into(),
        };
        let mut link = Link::new(peer, UdpSocket::bind("127.0.0.1:0").unwrap());
        link.touch();
        link.direct = Some(("198.51.100.10:4000".parse().unwrap(), Instant::now()));
        assert_eq!(link.display_path(true), "probing");
        link.confirm_direct("198.51.100.10:4000".parse().unwrap());
        assert_eq!(link.display_path(true), "direct");
    }

    #[test]
    fn authenticated_direct_packet_adopts_endpoint_before_local_probe() {
        let peer = Peer {
            device_id: Uuid::new_v4(),
            username: "remote-first".into(),
            public_key: "key".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![],
            secret: "secret".into(),
        };
        let mut link = Link::new(peer, UdpSocket::bind("127.0.0.1:0").unwrap());
        link.touch();
        let endpoint = "198.51.100.11:4001".parse().unwrap();
        assert!(link.direct.is_none());
        link.confirm_direct(endpoint);
        assert_eq!(link.direct, Some((endpoint, link.direct.unwrap().1)));
        assert_eq!(link.display_path(true), "direct");
    }

    #[test]
    fn three_relay_repairs_block_a_flapping_direct_candidate() {
        let peer = Peer {
            device_id: Uuid::new_v4(),
            username: "relay-repair".into(),
            public_key: "key".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![],
            secret: "secret".into(),
        };
        let mut link = Link::new(peer, UdpSocket::bind("127.0.0.1:0").unwrap());
        link.touch();
        link.direct = Some(("198.51.100.10:4000".parse().unwrap(), Instant::now()));
        link.direct_confirmed = true;
        link.direct_timeout();
        link.direct_timeout();
        assert!(link.direct.is_some());
        link.direct_timeout();
        assert!(link.direct.is_none());
        assert!(link.direct_blocked_until.is_some());
        assert_eq!(link.display_path(true), "probing");
    }

    #[test]
    fn actual_datagrams_start_on_demand_switch_direct_and_fall_back_without_reinstall() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let wg_a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let wg_b = UdpSocket::bind("127.0.0.1:0").unwrap();
        wg_a.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        wg_b.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let ua = UdpSocket::bind("127.0.0.1:0").unwrap();
        let ub = UdpSocket::bind("127.0.0.1:0").unwrap();
        ua.set_nonblocking(true).unwrap();
        ub.set_nonblocking(true).unwrap();
        let ea = ua.local_addr().unwrap();
        let eb = ub.local_addr().unwrap();
        let ba = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bb = UdpSocket::bind("127.0.0.1:0").unwrap();
        ba.set_nonblocking(true).unwrap();
        bb.set_nonblocking(true).unwrap();
        let bridge_a = ba.local_addr().unwrap();
        let pa = Peer {
            device_id: a,
            username: "a".into(),
            public_key: "ka".into(),
            ipv4: "10.88.0.10".into(),
            ipv6: "fd88::10".into(),
            enabled: true,
            candidates: vec![ea],
            secret: "pair".into(),
        };
        let pb = Peer {
            device_id: b,
            username: "b".into(),
            public_key: "kb".into(),
            ipv4: "10.88.0.11".into(),
            ipv6: "fd88::11".into(),
            enabled: true,
            candidates: vec![eb],
            secret: "pair".into(),
        };
        let relay_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay_socket.set_nonblocking(true).unwrap();
        let relay_ep = relay_socket.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let enabled_a = Arc::new(AtomicBool::new(true));
        let enabled_b = Arc::new(AtomicBool::new(true));
        let sa = Arc::new(Mutex::new(vec![]));
        let sb = Arc::new(Mutex::new(vec![]));
        let (ta, ra) = mpsc::channel();
        let (tb, rb) = mpsc::channel();
        ta.send(Update {
            directory: Directory {
                node_id: "gateway".into(),
                rate_limit: Default::default(),
                relay_node_id: "relay".into(),
                relay_rate_limit: Default::default(),
                device_id: a,
                relay_endpoint: relay_ep,
                relay_token: "a".into(),
                peers: vec![pb.clone()],
                refresh_ms: 1000,
            },
            bridges: vec![(pb, ba)],
        })
        .unwrap();
        tb.send(Update {
            directory: Directory {
                node_id: "gateway".into(),
                rate_limit: Default::default(),
                relay_node_id: "relay".into(),
                relay_rate_limit: Default::default(),
                device_id: b,
                relay_endpoint: relay_ep,
                relay_token: "b".into(),
                peers: vec![pa.clone()],
                refresh_ms: 1000,
            },
            bridges: vec![(pa, bb)],
        })
        .unwrap();
        let mut registry = RelayRegistry::default();
        registry.update(vec![
            RelayPeer {
                device_id: a,
                token: "a".into(),
            },
            RelayPeer {
                device_id: b,
                token: "b".into(),
            },
        ]);
        let sr = Arc::clone(&stop);
        let drop_relay_data = Arc::new(AtomicBool::new(true));
        let drop_data = Arc::clone(&drop_relay_data);
        let relay_worker = std::thread::spawn(move || {
            let mut buf = [0u8; MAX_DATAGRAM + 1];
            while !sr.load(Ordering::Relaxed) {
                if let Ok((n, source)) = relay_socket.recv_from(&mut buf) {
                    if let Some((data, dest)) = registry.handle(&buf[..n], source) {
                        if drop_data.load(Ordering::Relaxed)
                            && Packet::parse(&data).is_some_and(|p| p.kind == DATA)
                        {
                            continue;
                        }
                        relay_socket.send_to(&data, dest).unwrap();
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let s = Arc::clone(&stop);
        let e = Arc::clone(&enabled_a);
        let statuses = Arc::clone(&sa);
        let limited_a = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 1,
        });
        let limited_a_for_worker = Arc::clone(&limited_a);
        let port = wg_a.local_addr().unwrap().port();
        let wa = std::thread::spawn(move || {
            run(
                ua,
                None,
                port,
                s,
                e,
                ra,
                statuses,
                Arc::new(Mutex::new(vec![])),
                limited_a_for_worker,
                Arc::new(Mutex::new(pmman_protocol::TrafficStats::default())),
            )
        });
        let s = Arc::clone(&stop);
        let e = Arc::clone(&enabled_b);
        let statuses = Arc::clone(&sb);
        let limited_b = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 1,
        });
        let limited_b_for_worker = Arc::clone(&limited_b);
        let port = wg_b.local_addr().unwrap().port();
        let wb = std::thread::spawn(move || {
            run(
                ub,
                None,
                port,
                s,
                e,
                rb,
                statuses,
                Arc::new(Mutex::new(vec![])),
                limited_b_for_worker,
                Arc::new(Mutex::new(pmman_protocol::TrafficStats::default())),
            )
        });
        let outcome = std::panic::catch_unwind(|| {
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(sa.lock().unwrap()[0].path, "idle");
            let mut buf = [0u8; 128];
            let first_sent = Instant::now();
            wg_a.send_to(b"first encrypted WG datagram", bridge_a)
                .unwrap();
            let (n, _) = wg_b.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"first encrypted WG datagram");
            assert!(first_sent.elapsed() < Duration::from_secs(2));
            drop_relay_data.store(false, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while sa.lock().unwrap()[0].path != "direct" && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(sa.lock().unwrap()[0].path, "direct");
            // Exhaust the node relay budget. The following direct packet must
            // still arrive immediately because the direct branch never calls
            // the node limiter.
            assert!(limited_a.try_take(crate::limiter::Direction::Upload, 12_500));
            let direct_sent = Instant::now();
            wg_a.send_to(b"direct encrypted WG datagram", bridge_a)
                .unwrap();
            let (n, _) = wg_b.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"direct encrypted WG datagram");
            assert!(direct_sent.elapsed() < Duration::from_millis(500));
            enabled_a.store(false, Ordering::Release);
            std::thread::sleep(Duration::from_millis(20));
            wg_a.send_to(b"relay after toggle", bridge_a).unwrap();
            let (n, _) = wg_b.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"relay after toggle");
            std::thread::sleep(Duration::from_millis(250));
            let status = sa.lock().unwrap()[0].clone();
            assert_eq!(status.path, "relay");
            assert!(status.direct_tx_bytes > 0);
            assert!(status.relay_tx_bytes > 0);
        });
        stop.store(true, Ordering::Release);
        wa.join().unwrap();
        wb.join().unwrap();
        relay_worker.join().unwrap();
        outcome.unwrap();
    }
}
