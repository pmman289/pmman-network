//! Authenticated transport for opaque WireGuard datagrams. No private keys or
//! user credentials enter the node protocol. HMAC verification is constant-time.
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn default_enabled() -> bool {
    true
}
pub const MAGIC: &[u8; 8] = b"PMMP2P02";
pub const REGISTER: u8 = 1;
pub const REGISTERED: u8 = 2;
pub const PROBE: u8 = 3;
pub const PROBED: u8 = 4;
pub const DATA: u8 = 5;
pub const RELAY: u8 = 6;
pub const ACK: u8 = 7;
pub const WAKE: u8 = 8;
pub const HEADER: usize = 49;
// Relay authentication reuses the source/destination in the inner signed
// frame. Omitting their duplicate UUIDs saves 32 bytes, so a full 1280-byte
// inner IPv6 packet fits the public 1500-byte MTU even on IPv6 UDP underlay.
pub const RELAY_HEADER: usize = 17;
pub const MAX_DATAGRAM: usize = 2048;
pub const IDLE: Duration = Duration::from_secs(30);
// A single missed UDP probe must not make the UI flap between direct and
// relay. Data repair still provides a fast failure path for an active flow;
// this lease is only the idle/no-probe safety bound.
pub const DIRECT_LEASE: Duration = Duration::from_secs(3);
pub const REPAIR_DELAY: Duration = Duration::from_millis(120);

/// Bounded out-of-order replay window. Repair via relay can deliver the same
/// transport frame twice; acknowledge both, but forward to WireGuard only once.
pub struct ReplayWindow {
    highest: u64,
    seen: [u64; 64],
}
impl Default for ReplayWindow {
    fn default() -> Self {
        Self {
            highest: 0,
            seen: [0; 64],
        }
    }
}
impl ReplayWindow {
    pub fn accept(&mut self, sequence: u64) -> bool {
        if self.highest.saturating_sub(sequence) >= 4096 {
            return false;
        }
        if sequence > self.highest {
            if sequence - self.highest >= 4096 {
                self.seen.fill(0);
            } else {
                for next in self.highest + 1..=sequence {
                    let bit = (next % 4096) as usize;
                    self.seen[bit / 64] &= !(1 << (bit % 64));
                }
            }
            self.highest = sequence;
        }
        let bit = (sequence % 4096) as usize;
        let mask = 1 << (bit % 64);
        if self.seen[bit / 64] & mask != 0 {
            return false;
        }
        self.seen[bit / 64] |= mask;
        true
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Advertisement {
    pub public_key: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub candidates: Vec<SocketAddr>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Peer {
    pub device_id: Uuid,
    pub username: String,
    pub public_key: String,
    pub ipv4: String,
    pub ipv6: String,
    pub enabled: bool,
    pub candidates: Vec<SocketAddr>,
    pub secret: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Directory {
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub rate_limit: crate::NodeRateLimit,
    #[serde(default)]
    pub relay_node_id: String,
    #[serde(default)]
    pub relay_rate_limit: crate::NodeRateLimit,
    pub device_id: Uuid,
    pub relay_endpoint: SocketAddr,
    pub relay_token: String,
    pub peers: Vec<Peer>,
    pub refresh_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelayPeer {
    pub device_id: Uuid,
    pub token: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatus {
    pub device_id: String,
    pub username: String,
    pub public_key: String,
    pub ipv4: String,
    pub ipv6: String,
    pub path: String,
    pub direct_endpoint: Option<String>,
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub direct_rx_bytes: u64,
    pub direct_tx_bytes: u64,
    #[serde(default)]
    pub relay_rx_bytes: u64,
    pub relay_tx_bytes: u64,
    pub fallback_count: u64,
    pub last_activity_ms: Option<u64>,
}

pub fn sign(secret: &str, bytes: &[u8]) -> [u8; 32] {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(bytes);
    mac.finalize().into_bytes().into()
}
pub fn pair_secret(left: &str, right: &str) -> String {
    let (left, right) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    sign(left, right.as_bytes())
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
pub fn encode(
    kind: u8,
    src: Uuid,
    dst: Uuid,
    sequence: u64,
    payload: &[u8],
    secret: &str,
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER + payload.len() + 32);
    packet.extend_from_slice(MAGIC);
    packet.push(kind);
    if kind == RELAY {
        if !Packet::parse(payload).is_some_and(|p| {
            p.src == src && p.dst == dst && matches!(p.kind, DATA | PROBE | PROBED | ACK | WAKE)
        }) {
            return Vec::new();
        }
    } else {
        packet.extend_from_slice(src.as_bytes());
        packet.extend_from_slice(dst.as_bytes());
    }
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(payload);
    let tag = sign(secret, &packet);
    packet.extend_from_slice(&tag);
    packet
}
pub struct Packet<'a> {
    pub kind: u8,
    pub src: Uuid,
    pub dst: Uuid,
    pub sequence: u64,
    pub payload: &'a [u8],
}
impl<'a> Packet<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < RELAY_HEADER + 32 || bytes.len() > MAX_DATAGRAM || &bytes[..8] != MAGIC {
            return None;
        }
        if bytes[8] == RELAY {
            let payload = &bytes[RELAY_HEADER..bytes.len() - 32];
            // Check kind before parsing the inner frame: nested relay frames
            // are invalid and must not permit unbounded recursive parsing.
            if !matches!(
                payload.get(8),
                Some(&DATA) | Some(&PROBE) | Some(&PROBED) | Some(&ACK) | Some(&WAKE)
            ) {
                return None;
            }
            let inner = Self::parse(payload)?;
            return Some(Self {
                kind: RELAY,
                src: inner.src,
                dst: inner.dst,
                sequence: u64::from_be_bytes(bytes[9..17].try_into().ok()?),
                payload,
            });
        }
        if bytes.len() < HEADER + 32 {
            return None;
        }
        Some(Self {
            kind: bytes[8],
            src: Uuid::from_slice(&bytes[9..25]).ok()?,
            dst: Uuid::from_slice(&bytes[25..41]).ok()?,
            sequence: u64::from_be_bytes(bytes[41..49].try_into().ok()?),
            payload: &bytes[49..bytes.len() - 32],
        })
    }
    pub fn verify(bytes: &'a [u8], secret: &str) -> Option<Self> {
        let packet = Self::parse(bytes)?;
        let n = bytes.len() - 32;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
        mac.update(&bytes[..n]);
        mac.verify_slice(&bytes[n..]).ok()?;
        Some(packet)
    }
}
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn fresh(sequence: u64) -> bool {
    unix_ms().abs_diff(sequence) <= 30_000
}
pub fn valid_candidate(address: SocketAddr) -> bool {
    let ip = address.ip();
    address.port() > 0
        && !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && match ip {
            std::net::IpAddr::V4(ip) => !ip.is_broadcast() && !ip.is_link_local(),
            std::net::IpAddr::V6(ip) => !ip.is_unicast_link_local(),
        }
}

/// Per-device leased relay authorization. Updates revoke endpoint registrations
/// immediately when a token changes or an account/device disappears.
#[derive(Default)]
pub struct RelayRegistry {
    peers: HashMap<Uuid, (String, Option<(SocketAddr, Instant)>)>,
    refreshed: Option<Instant>,
    limits: HashMap<Uuid, (Instant, u64)>,
}
impl RelayRegistry {
    pub fn update(&mut self, peers: Vec<RelayPeer>) {
        let mut next = HashMap::new();
        for peer in peers {
            let endpoint = self
                .peers
                .get(&peer.device_id)
                .filter(|(token, _)| token == &peer.token)
                .and_then(|(_, ep)| *ep);
            next.insert(peer.device_id, (peer.token, endpoint));
        }
        self.peers = next;
        self.limits.retain(|id, _| self.peers.contains_key(id));
        self.refreshed = Some(Instant::now());
    }
    pub fn handle(&mut self, bytes: &[u8], source: SocketAddr) -> Option<(Vec<u8>, SocketAddr)> {
        if self.refreshed?.elapsed() > Duration::from_secs(15) {
            return None;
        }
        let outer = Packet::parse(bytes)?;
        let (token, endpoint) = self.peers.get_mut(&outer.src)?;
        let packet = Packet::verify(bytes, token)?;
        if !fresh(packet.sequence) {
            return None;
        }
        // Bound authenticated relay load to 64 MiB/s per sender without an
        // unauthenticated global limit that one stranger could exhaust.
        let (window, used) = self.limits.entry(packet.src).or_insert((Instant::now(), 0));
        if window.elapsed() >= Duration::from_secs(1) {
            *window = Instant::now();
            *used = 0;
        }
        *used += bytes.len() as u64;
        if *used > 64 * 1024 * 1024 {
            return None;
        }
        if packet.kind == REGISTER && packet.dst.is_nil() && packet.payload.is_empty() {
            *endpoint = Some((source, Instant::now()));
            return Some((
                encode(
                    REGISTERED,
                    Uuid::nil(),
                    packet.src,
                    packet.sequence,
                    source.to_string().as_bytes(),
                    token,
                ),
                source,
            ));
        }
        if packet.kind != RELAY
            || endpoint.as_ref()?.0 != source
            || endpoint.as_ref()?.1.elapsed() > Duration::from_secs(15)
        {
            return None;
        }
        let inner = Packet::parse(packet.payload)?;
        if inner.src != packet.src
            || inner.dst != packet.dst
            || !matches!(inner.kind, DATA | PROBE | PROBED | ACK | WAKE)
        {
            return None;
        }
        let destination = self.peers.get(&packet.dst)?.1?;
        if destination.1.elapsed() > Duration::from_secs(15) {
            return None;
        }
        Some((packet.payload.to_vec(), destination.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maximum_ipv6_packet_fits_udp_relay_without_underlay_fragmentation() {
        let src = Uuid::new_v4();
        let dst = Uuid::new_v4();
        // WireGuard type-4 ciphertext: 1280-byte IP packet + 32-byte WG framing.
        let ciphertext = vec![0; 1280 + 32];
        let inner = encode(DATA, src, dst, 1, &ciphertext, "pair");
        let outer = encode(RELAY, src, dst, unix_ms(), &inner, "node-token");
        assert!(outer.len() + 40 + 8 <= 1500);
        let relay = Packet::verify(&outer, "node-token").unwrap();
        assert_eq!((relay.src, relay.dst), (src, dst));
        assert_eq!(
            Packet::verify(relay.payload, "pair").unwrap().payload,
            ciphertext
        );
        assert!(encode(RELAY, Uuid::new_v4(), dst, unix_ms(), &inner, "node-token").is_empty());
        assert!(encode(RELAY, src, dst, unix_ms(), &outer, "node-token").is_empty());
        for length in 0..RELAY_HEADER + 32 {
            assert!(Packet::parse(&outer[..length]).is_none());
        }
    }
    #[test]
    fn rejects_forged_and_truncated_packets() {
        let mut b = encode(
            DATA,
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            b"ciphertext",
            "secret",
        );
        assert!(Packet::verify(&b, "secret").is_some());
        assert!(Packet::verify(&b, "wrong").is_none());
        b[10] ^= 1;
        assert!(Packet::verify(&b, "secret").is_none());
        for n in 0..HEADER + 32 {
            assert!(Packet::parse(&b[..n]).is_none());
        }
    }
    #[test]
    fn relay_requires_authenticated_registration_and_revokes_immediately() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let sa = "192.0.2.1:3000".parse().unwrap();
        let sb = "192.0.2.2:4000".parse().unwrap();
        let mut r = RelayRegistry::default();
        r.update(vec![
            RelayPeer {
                device_id: a,
                token: "a".into(),
            },
            RelayPeer {
                device_id: b,
                token: "b".into(),
            },
        ]);
        let inner = encode(DATA, a, b, 1, b"encrypted", "pair");
        let outer = encode(RELAY, a, b, unix_ms(), &inner, "a");
        assert!(r.handle(&outer, sa).is_none());
        assert!(r
            .handle(&encode(REGISTER, a, Uuid::nil(), unix_ms(), &[], "a"), sa)
            .is_some());
        assert!(r
            .handle(
                &encode(REGISTER, b, Uuid::nil(), unix_ms(), &[], "wrong"),
                sb
            )
            .is_none());
        r.handle(&encode(REGISTER, b, Uuid::nil(), unix_ms(), &[], "b"), sb)
            .unwrap();
        assert_eq!(r.handle(&outer, sa).unwrap(), (inner, sb));
        assert!(r.handle(&outer, sb).is_none());
        assert!(r.handle(&encode(RELAY, a, b, 0, b"", "a"), sa).is_none());
        r.update(vec![]);
        assert!(r.handle(&outer, sa).is_none());
    }
    #[test]
    fn pair_key_is_symmetric_and_candidates_are_unicast() {
        assert_eq!(pair_secret("a", "b"), pair_secret("b", "a"));
        for a in [
            "127.0.0.1:1",
            "0.0.0.0:1",
            "255.255.255.255:1",
            "[ff02::1]:1",
            "169.254.0.1:1",
        ] {
            assert!(!valid_candidate(a.parse().unwrap()));
        }
        assert!(valid_candidate("192.168.1.1:4000".parse().unwrap()));
    }
    #[test]
    fn replay_window_allows_reordering_but_not_duplicates_or_old_frames() {
        let mut window = ReplayWindow::default();
        assert!(window.accept(5000));
        assert!(window.accept(4999));
        assert!(!window.accept(5000));
        assert!(!window.accept(1));
        assert!(window.accept(10_000));
        assert!(!window.accept(4999));
    }
}
