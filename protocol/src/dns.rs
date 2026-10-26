//! Bounded DNS forwarding shared by the node and Windows session resolver.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

// Keep resolver failure bounded. A node may have an IPv6 upstream configured
// even when the current client network only has IPv4 (or vice versa). Waiting
// several seconds for each selected group makes a normal fallback lookup feel
// like the DNS service is frozen. Queries within a group are already issued
// concurrently, so a short per-upstream budget is enough to preserve a fast
// healthy path while still allowing the next group to answer.
const RESOLUTION_DEADLINE: Duration = Duration::from_secs(2);
const UPSTREAM_TIMEOUT: Duration = Duration::from_millis(450);
const NEGATIVE_GRACE: Duration = Duration::from_millis(60);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteRule {
    pub suffix: String,
    pub upstreams: Vec<String>,
}

#[derive(Clone, Default)]
pub struct Policy {
    pub primary: Vec<SocketAddr>,
    pub fallback: Vec<SocketAddr>,
    /// The domestic group performs mrouter-compatible A/AAAA joint lookup.
    /// An empty group keeps the generic forwarding behavior for callers that
    /// do not use domestic/foreign classification (for example the client
    /// side local fallback listener).
    pub domestic_upstreams: Vec<SocketAddr>,
    pub rules: Vec<(String, Vec<SocketAddr>)>,
    /// Rules compiled from the node's geosite.dat. These are evaluated after
    /// explicit administrator rules and before the default upstreams.
    pub geosite: Option<Arc<crate::geosite::Router>>,
    /// None is appropriate only for loopback-only client listeners.
    pub allowed_sources: Option<HashSet<IpAddr>>,
}

fn query_name(packet: &[u8]) -> Option<String> {
    if packet.len() < 13 {
        return None;
    }
    let mut offset = 12;
    let mut labels = Vec::new();
    loop {
        let length = *packet.get(offset)? as usize;
        offset += 1;
        if length == 0 {
            break;
        }
        if length > 63 || offset.checked_add(length)? > packet.len() {
            return None;
        }
        labels.push(String::from_utf8_lossy(&packet[offset..offset + length]).to_ascii_lowercase());
        offset += length;
    }
    (!labels.is_empty()).then(|| labels.join("."))
}

pub fn endpoint(value: &str) -> Result<SocketAddr, String> {
    let address = value
        .trim()
        .parse::<SocketAddr>()
        .or_else(|_| {
            value
                .trim()
                .parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, 53))
        })
        .map_err(|_| "DNS 地址必须是 IP、IP:端口或 [IPv6]:端口".to_string())?;
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err("DNS 地址或端口无效".into());
    }
    Ok(address)
}

pub fn normalize_suffix(value: &str) -> Result<String, String> {
    let suffix = value
        .trim()
        .strip_prefix("domain:")
        .or_else(|| value.trim().strip_prefix("DOMAIN:"))
        .unwrap_or(value.trim())
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if suffix.is_empty()
        || suffix.len() > 253
        || suffix.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(format!("DNS 域名后缀无效：{value}"));
    }
    Ok(suffix)
}

/// Compile the mrouter-compatible resolver groups into the runtime policy.
/// `pmmandns_upstreams` remains the legacy default and takes precedence when
/// present, so existing node configurations keep their behavior.
pub fn compile_groups(
    legacy_upstreams: &[String],
    domestic_upstreams: &[String],
    foreign_upstreams: &[String],
    domestic_rules: &[String],
    foreign_rules: &[String],
    default_group: &str,
    explicit_rules: &[RouteRule],
) -> Result<(Vec<SocketAddr>, Vec<(String, Vec<SocketAddr>)>), String> {
    let parse_group = |name: &str, values: &[String]| {
        values
            .iter()
            .map(|value| endpoint(value).map_err(|_| format!("{name} DNS 地址无效：{value}")))
            .collect::<Result<Vec<_>, _>>()
    };
    let domestic = parse_group("国内", domestic_upstreams)?;
    let foreign = parse_group("国外", foreign_upstreams)?;
    let primary = if !legacy_upstreams.is_empty() {
        parse_group("默认", legacy_upstreams)?
    } else {
        match default_group.trim().to_ascii_lowercase().as_str() {
            "" | "foreign" => foreign.clone(),
            "domestic" => domestic.clone(),
            other => return Err(format!("pmmandns 默认分组无效：{other}")),
        }
    };
    let mut rules = Vec::new();
    let mut add_group =
        |group: &str, lines: &[String], upstreams: &[SocketAddr]| -> Result<(), String> {
            if lines
                .iter()
                .any(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
                && upstreams.is_empty()
            {
                return Err(format!("pmmandns {group} 分组存在域名规则但没有上游 DNS"));
            }
            for line in lines {
                if line.trim().is_empty() || line.trim().starts_with('#') {
                    continue;
                }
                let suffix = normalize_suffix(line)?;
                if rules.iter().any(|(current, _)| current == &suffix) {
                    return Err(format!("pmmandns 分流规则重复：{suffix}"));
                }
                rules.push((suffix, upstreams.to_vec()));
            }
            Ok(())
        };
    add_group("国内", domestic_rules, &domestic)?;
    add_group("国外", foreign_rules, &foreign)?;
    for rule in explicit_rules {
        let suffix = normalize_suffix(&rule.suffix)?;
        let upstreams = rule
            .upstreams
            .iter()
            .map(|value| endpoint(value))
            .collect::<Result<Vec<_>, _>>()?;
        if upstreams.is_empty() {
            return Err(format!("DNS 分流规则 {suffix} 未配置上游 DNS"));
        }
        if rules.iter().any(|(current, _)| current == &suffix) {
            return Err(format!("pmmandns 分流规则重复：{suffix}"));
        }
        rules.push((suffix, upstreams));
    }
    Ok((primary, rules))
}

fn question_end(packet: &[u8]) -> Option<usize> {
    if packet.len() < 12 || packet[4..6] != [0, 1] {
        return None;
    }
    let mut offset = 12;
    loop {
        let length = *packet.get(offset)? as usize;
        offset += 1;
        // Ordinary DNS queries have an uncompressed question. Reject pointers
        // rather than trusting loops or pointers into unvalidated data.
        if length > 63 {
            return None;
        }
        if length == 0 {
            break;
        }
        offset = offset.checked_add(length)?;
        if offset > packet.len() || offset > 267 {
            return None;
        }
    }
    (offset + 4 <= packet.len()).then_some(offset + 4)
}

fn matches_query(query: &[u8], response: &[u8]) -> bool {
    let Some(end) = question_end(query) else {
        return false;
    };
    response.len() >= end
        && query[..2] == response[..2]
        && response[2] & 0x80 != 0
        && query[2] & 0x78 == response[2] & 0x78
        && response[4..6] == [0, 1]
        && query[12..end].eq_ignore_ascii_case(&response[12..end])
}

fn has_answer(response: &[u8]) -> bool {
    response.len() >= 12
        && response[3] & 15 == 0
        && u16::from_be_bytes([response[6], response[7]]) > 0
}

pub fn failure(query: &[u8]) -> Vec<u8> {
    let end = question_end(query).unwrap_or(12).min(query.len());
    let mut response = query[..end].to_vec();
    response.resize(response.len().max(12), 0);
    response[2] = 0x80 | (response[2] & 0x79);
    response[3] = 0x82; // recursion available, SERVFAIL
    response[6..12].fill(0);
    if end == 12 {
        response[4..6].fill(0);
    }
    response
}

fn tcp_query(query: &[u8], upstream: SocketAddr, timeout: Duration) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&upstream, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(&(query.len() as u16).to_be_bytes())?;
    stream.write_all(query)?;
    let mut header = [0; 2];
    stream.read_exact(&mut header)?;
    let mut response = vec![0; u16::from_be_bytes(header) as usize];
    stream.read_exact(&mut response)?;
    Ok(response)
}

fn skip_name(packet: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        let length = *packet.get(offset)?;
        if length == 0 {
            return Some(offset + 1);
        }
        if length & 0xc0 == 0xc0 {
            return (offset + 2 <= packet.len()).then_some(offset + 2);
        }
        if length > 63 {
            return None;
        }
        offset = offset.checked_add(1 + length as usize)?;
        if offset > packet.len() {
            return None;
        }
    }
}

fn response_ttl(response: &[u8]) -> Option<Duration> {
    if response.len() < 12 {
        return None;
    }
    let questions = u16::from_be_bytes([response[4], response[5]]) as usize;
    let answers = u16::from_be_bytes([response[6], response[7]]) as usize;
    let authorities = u16::from_be_bytes([response[8], response[9]]) as usize;
    let mut offset = 12;
    for _ in 0..questions {
        offset = skip_name(response, offset)?.checked_add(4)?;
        if offset > response.len() {
            return None;
        }
    }
    let mut minimum = u32::MAX;
    for _ in 0..(answers + authorities) {
        offset = skip_name(response, offset)?;
        if offset.checked_add(10)? > response.len() {
            return None;
        }
        let ttl = u32::from_be_bytes([
            response[offset + 4],
            response[offset + 5],
            response[offset + 6],
            response[offset + 7],
        ]);
        let length = u16::from_be_bytes([response[offset + 8], response[offset + 9]]) as usize;
        offset = offset.checked_add(10 + length)?;
        if offset > response.len() {
            return None;
        }
        minimum = minimum.min(ttl);
    }
    (minimum != u32::MAX).then(|| Duration::from_secs(u64::from(minimum.min(300))))
}

fn query_upstream(query: &[u8], upstream: SocketAddr, deadline: Instant) -> Option<Vec<u8>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    let timeout = remaining.min(UPSTREAM_TIMEOUT);
    let bind = if upstream.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(upstream).ok()?;
    let mut request = query.to_vec();
    let id: u16 = rand::random();
    request[..2].copy_from_slice(&id.to_be_bytes());
    socket.set_read_timeout(Some(timeout)).ok()?;
    socket.send(&request).ok()?;
    let mut buffer = [0; 65535];
    let length = socket.recv(&mut buffer).ok()?;
    let mut response = buffer[..length].to_vec();
    if !matches_query(&request, &response) {
        return None;
    }
    if response[2] & 2 != 0 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        response = tcp_query(&request, upstream, remaining.min(timeout)).ok()?;
        if !matches_query(&request, &response) || response[2] & 2 != 0 {
            return None;
        }
    }
    response[..2].copy_from_slice(&query[..2]);
    Some(response)
}

/// Query one resolver group concurrently. A dead IPv6 resolver must not block
/// a healthy IPv4 resolver for every request, while fallback resolvers remain
/// isolated from a successful primary answer.
fn query_group(query: &[u8], servers: &[SocketAddr], deadline: Instant) -> Option<Vec<u8>> {
    let servers = servers
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if servers.is_empty() {
        return None;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    for server in servers.iter().copied() {
        let sender = sender.clone();
        let query = query.to_vec();
        thread::spawn(move || {
            let _ = sender.send(query_upstream(&query, server, deadline));
        });
    }
    drop(sender);
    let mut negative = None;
    let mut negative_deadline: Option<Instant> = None;
    for _ in 0..servers.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        // Once one resolver has returned a genuine negative answer, give
        // sibling resolvers a short grace period for a positive split-DNS
        // answer, but do not wait the full dead-upstream timeout forever.
        let wait = if let Some(grace_deadline) = negative_deadline {
            remaining.min(grace_deadline.saturating_duration_since(Instant::now()))
        } else {
            remaining
        };
        if wait.is_zero() {
            break;
        }
        let Ok(Some(response)) = receiver.recv_timeout(wait) else {
            continue;
        };
        if has_answer(&response) {
            return Some(response);
        }
        if matches!(response[3] & 15, 0 | 3) {
            negative = Some(response);
            negative_deadline.get_or_insert_with(|| Instant::now() + NEGATIVE_GRACE);
        }
    }
    negative
}

struct CacheEntry {
    response: Vec<u8>,
    expires: Instant,
}

#[derive(Default)]
struct ResolverCache {
    entries: std::sync::Mutex<HashMap<Vec<u8>, CacheEntry>>,
}

fn cache_key(query: &[u8]) -> Vec<u8> {
    let mut key = query.to_vec();
    if key.len() >= 2 {
        key[..2].fill(0);
    }
    key
}

fn resolve_cached(query: &[u8], policy: &Policy, cache: &ResolverCache) -> Vec<u8> {
    let key = cache_key(query);
    if let Some(entry) = cache.entries.lock().unwrap().get(&key) {
        if entry.expires > Instant::now() {
            let mut response = entry.response.clone();
            response[..2].copy_from_slice(&query[..2]);
            return response;
        }
    }
    let response = resolve(query, policy);
    if response_ttl(&response).is_some() {
        let ttl = response_ttl(&response).unwrap_or_default();
        let mut stored = response.clone();
        stored[..2].fill(0);
        let mut entries = cache.entries.lock().unwrap();
        entries.retain(|_, entry| entry.expires > Instant::now());
        if entries.len() >= 2048 {
            entries.clear();
        }
        entries.insert(
            key,
            CacheEntry {
                response: stored,
                expires: Instant::now() + ttl,
            },
        );
    }
    response
}

pub fn resolve(query: &[u8], policy: &Policy) -> Vec<u8> {
    if question_end(query).is_none() || query[2] & 0xf8 != 0 || query.len() > 4096 {
        return failure(query);
    }
    let deadline = Instant::now() + RESOLUTION_DEADLINE;
    let selected = query_name(query).and_then(|name| {
        // Explicit rules are authoritative and use longest-suffix matching.
        if let Some((_, upstreams)) = policy
            .rules
            .iter()
            .filter(|(suffix, _)| name == *suffix || name.ends_with(&format!(".{suffix}")))
            .max_by_key(|(suffix, _)| suffix.len())
        {
            return Some(upstreams);
        }
        // geosite categories contain full domains, root domains, keywords and
        // regular expressions. Prefer the most specific match where several
        // forms cover the same query.
        policy
            .geosite
            .as_ref()
            .and_then(|router| router.select(&name))
    });
    // A classified group is authoritative for the query. In particular, a
    // domestic resolver may intentionally return NOERROR/NODATA for AAAA so
    // domestic names do not leak IPv6 answers from the foreign default group.
    // Fall back only when that selected group has no usable response at all.
    if let Some(upstreams) = selected {
        let servers = upstreams.iter().copied().collect::<Vec<_>>();
        if !policy.domestic_upstreams.is_empty() && *upstreams == policy.domestic_upstreams {
            return resolve_domestic(query, &servers, deadline);
        }
        if let Some(response) = query_group(query, &servers, deadline) {
            return response;
        }
    }
    for group in [&policy.primary, &policy.fallback] {
        if let Some(response) = query_group(query, group, deadline) {
            // A valid negative DNS response (NOERROR/NODATA or NXDOMAIN) is
            // still an authoritative response. Only transport failure should
            // fall through to the next resolver group; otherwise a domestic
            // AAAA filter can be undone by the local/foreign fallback DNS.
            return response;
        }
    }
    failure(query)
}

fn resolve_domestic(query: &[u8], servers: &[SocketAddr], deadline: Instant) -> Vec<u8> {
    let Some(end) = question_end(query) else {
        return failure(query);
    };
    if end < 4 {
        return failure(query);
    }
    let mut a_query = query.to_vec();
    let mut aaaa_query = query.to_vec();
    a_query[end - 4..end - 2].copy_from_slice(&1u16.to_be_bytes());
    aaaa_query[end - 4..end - 2].copy_from_slice(&28u16.to_be_bytes());
    let a_servers = servers.to_vec();
    let aaaa_servers = servers.to_vec();
    let (sender, receiver) = std::sync::mpsc::channel();
    let a_sender = sender.clone();
    thread::spawn(move || {
        let _ = a_sender.send((1u16, query_group(&a_query, &a_servers, deadline)));
    });
    thread::spawn(move || {
        let _ = sender.send((28u16, query_group(&aaaa_query, &aaaa_servers, deadline)));
    });
    let mut a_response = None;
    let mut aaaa_response = None;
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok((qtype, response)) = receiver.recv_timeout(remaining) else {
            break;
        };
        if qtype == 1 {
            a_response = response;
        } else {
            aaaa_response = response;
        }
    }
    let requested_type = u16::from_be_bytes([query[end - 4], query[end - 3]]);
    let a_has_answer = a_response.as_deref().is_some_and(has_answer);
    let aaaa_has_answer = aaaa_response.as_deref().is_some_and(has_answer);
    let chosen = match requested_type {
        1 => a_response.as_deref().or(aaaa_response.as_deref()),
        28 if a_has_answer => a_response.as_deref(),
        28 => aaaa_response.as_deref().or(a_response.as_deref()),
        _ => a_response.as_deref().or(aaaa_response.as_deref()),
    };
    let Some(response) = chosen else {
        return failure(query);
    };
    let output_type = if requested_type == 28 && a_has_answer {
        28
    } else if requested_type == 1 {
        1
    } else if aaaa_has_answer {
        28
    } else {
        requested_type
    };
    filter_response(response, query, output_type)
}

fn filter_response(response: &[u8], query: &[u8], qtype: u16) -> Vec<u8> {
    let Some(response_end) = question_end(response) else {
        return failure(query);
    };
    let Some(query_end) = question_end(query) else {
        return failure(query);
    };
    let mut output = response[..response_end].to_vec();
    output[..2].copy_from_slice(&query[..2]);
    output[query_end - 4..query_end - 2].copy_from_slice(&qtype.to_be_bytes());
    output[6..8].copy_from_slice(&0u16.to_be_bytes());
    output[8..10].copy_from_slice(&0u16.to_be_bytes());
    output[10..12].copy_from_slice(&0u16.to_be_bytes());
    let answer_count = u16::from_be_bytes([response[6], response[7]]);
    let mut offset = response_end;
    let mut answers = Vec::new();
    for _ in 0..answer_count {
        let Some((record_type, end)) = resource_end(response, offset) else {
            return failure(query);
        };
        if record_type == 5 || record_type == qtype {
            answers.push(response[offset..end].to_vec());
        }
        offset = end;
    }
    output[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
    for answer in answers {
        output.extend_from_slice(&answer);
    }
    output
}

fn resource_end(packet: &[u8], start: usize) -> Option<(u16, usize)> {
    let mut offset = start;
    loop {
        let length = *packet.get(offset)? as usize;
        if length & 0xc0 == 0xc0 {
            offset = offset.checked_add(2)?;
            break;
        }
        if length == 0 {
            offset += 1;
            break;
        }
        if length > 63 {
            return None;
        }
        offset = offset.checked_add(1 + length)?;
    }
    let record_type = u16::from_be_bytes([*packet.get(offset)?, *packet.get(offset + 1)?]);
    let rdlength =
        u16::from_be_bytes([*packet.get(offset + 8)?, *packet.get(offset + 9)?]) as usize;
    let end = offset.checked_add(10)?.checked_add(rdlength)?;
    (end <= packet.len()).then_some((record_type, end))
}

pub struct Forwarder {
    pub policy: Arc<RwLock<Policy>>,
    stop: Arc<AtomicBool>,
    listeners: Vec<thread::JoinHandle<()>>,
}

impl Forwarder {
    pub fn start(addresses: &[SocketAddr], policy: Policy) -> Result<Self, String> {
        let policy = Arc::new(RwLock::new(policy));
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let cache = Arc::new(ResolverCache::default());
        let mut sockets = Vec::new();
        // Bind all endpoints before spawning any worker: failures roll back.
        for address in addresses {
            let udp = UdpSocket::bind(address).map_err(|e| format!("DNS UDP {address}: {e}"))?;
            udp.set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(|e| e.to_string())?;
            let tcp = TcpListener::bind(address).map_err(|e| format!("DNS TCP {address}: {e}"))?;
            tcp.set_nonblocking(true).map_err(|e| e.to_string())?;
            sockets.push((udp, tcp));
        }
        let mut listeners = Vec::new();
        for (udp, tcp) in sockets {
            let udp = Arc::new(udp);
            let (p, s, a, c) = (policy.clone(), stop.clone(), active.clone(), cache.clone());
            listeners.push(thread::spawn(move || {
                let mut buffer = [0; 4097];
                while !s.load(Ordering::Acquire) {
                    let Ok((length, source)) = udp.recv_from(&mut buffer) else {
                        continue;
                    };
                    let current = p.read().unwrap().clone();
                    if !permitted(&current, source.ip()) || length > 4096 {
                        continue;
                    }
                    if a.fetch_add(1, Ordering::AcqRel) >= 32 {
                        a.fetch_sub(1, Ordering::AcqRel);
                        let _ = udp.send_to(&failure(&buffer[..length]), source);
                        continue;
                    }
                    let (socket, count, query) =
                        (udp.clone(), a.clone(), buffer[..length].to_vec());
                    let cache = c.clone();
                    thread::spawn(move || {
                        let _ = socket.send_to(&resolve_cached(&query, &current, &cache), source);
                        count.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            }));
            let (p, s, a, c) = (policy.clone(), stop.clone(), active.clone(), cache.clone());
            listeners.push(thread::spawn(move || {
                while !s.load(Ordering::Acquire) {
                    let Ok((mut stream, source)) = tcp.accept() else {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    };
                    let current = p.read().unwrap().clone();
                    if !permitted(&current, source.ip()) {
                        continue;
                    }
                    if a.fetch_add(1, Ordering::AcqRel) >= 32 {
                        a.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    let count = a.clone();
                    let cache = c.clone();
                    thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let mut header = [0; 2];
                        if stream.read_exact(&mut header).is_ok() {
                            let length = u16::from_be_bytes(header) as usize;
                            if (12..=4096).contains(&length) {
                                let mut query = vec![0; length];
                                if stream.read_exact(&mut query).is_ok() {
                                    let response = resolve_cached(&query, &current, &cache);
                                    let _ =
                                        stream.write_all(&(response.len() as u16).to_be_bytes());
                                    let _ = stream.write_all(&response);
                                }
                            }
                        }
                        count.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            }));
        }
        Ok(Self {
            policy,
            stop,
            listeners,
        })
    }
}

fn permitted(policy: &Policy, source: IpAddr) -> bool {
    policy
        .allowed_sources
        .as_ref()
        .map_or(source.is_loopback(), |ips| ips.contains(&source))
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for listener in self.listeners.drain(..) {
            let _ = listener.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query() -> Vec<u8> {
        let mut packet = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        packet.extend_from_slice(b"\x03www\x07example\x03com\x00\x00\x01\x00\x01");
        packet
    }
    fn query_for(name: &str) -> Vec<u8> {
        let mut packet = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            packet.push(label.len() as u8);
            packet.extend_from_slice(label.as_bytes());
        }
        packet.extend_from_slice(b"\x00\x00\x01\x00\x01");
        packet
    }
    fn responder(answer: bool) -> (SocketAddr, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        (
            address,
            thread::spawn(move || {
                let mut buffer = [0; 4096];
                let (len, source) = socket.recv_from(&mut buffer).unwrap();
                let mut response = buffer[..len].to_vec();
                response[2] = 0x81;
                response[3] = if answer { 0x80 } else { 0x83 };
                if answer {
                    response[7] = 1;
                    response.extend_from_slice(
                        b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x01\x00\x04\xc0\x00\x02\x01",
                    );
                }
                socket.send_to(&response, source).unwrap();
            }),
        )
    }
    fn responder_ip(ip: [u8; 4]) -> (SocketAddr, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        (
            address,
            thread::spawn(move || {
                let mut buffer = [0; 4096];
                let (len, source) = socket.recv_from(&mut buffer).unwrap();
                let mut response = buffer[..len].to_vec();
                response[2] = 0x81;
                response[3] = 0x80;
                response[7] = 1;
                response.extend_from_slice(b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x01\x00\x04");
                response.extend_from_slice(&ip);
                socket.send_to(&response, source).unwrap();
            }),
        )
    }
    fn responder_nodata() -> (SocketAddr, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        (
            address,
            thread::spawn(move || {
                let mut buffer = [0; 4096];
                let (len, source) = socket.recv_from(&mut buffer).unwrap();
                let mut response = buffer[..len].to_vec();
                response[2] = 0x81;
                response[3] = 0x80; // NOERROR, with zero answers.
                response[6] = 0;
                response[7] = 0;
                socket.send_to(&response, source).unwrap();
            }),
        )
    }
    #[test]
    fn node_negative_is_authoritative_and_preserves_original_transaction() {
        let (primary, p) = responder(false);
        let fallback = UdpSocket::bind("127.0.0.1:0").unwrap();
        fallback
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let fallback_address = fallback.local_addr().unwrap();
        let query = query();
        let response = resolve(
            &query,
            &Policy {
                primary: vec![primary],
                fallback: vec![fallback_address],
                ..Default::default()
            },
        );
        assert!(!has_answer(&response));
        assert_eq!(response[..2], query[..2]);
        let mut request = [0; 512];
        assert!(fallback.recv(&mut request).is_err());
        p.join().unwrap();
    }
    #[test]
    fn primary_timeout_falls_back_to_local_dns() {
        let primary = UdpSocket::bind("127.0.0.1:0").unwrap();
        primary
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let (fallback, worker) = responder(true);
        let response = resolve(
            &query(),
            &Policy {
                primary: vec![primary.local_addr().unwrap()],
                fallback: vec![fallback],
                ..Default::default()
            },
        );
        assert!(has_answer(&response));
        worker.join().unwrap();
    }
    #[test]
    fn primary_answer_never_queries_fallback() {
        let (primary, p) = responder(true);
        let fallback = UdpSocket::bind("127.0.0.1:0").unwrap();
        fallback
            .set_read_timeout(Some(Duration::from_millis(80)))
            .unwrap();
        assert!(has_answer(&resolve(
            &query(),
            &Policy {
                primary: vec![primary],
                fallback: vec![fallback.local_addr().unwrap()],
                ..Default::default()
            }
        )));
        assert!(fallback.recv(&mut [0; 64]).is_err());
        p.join().unwrap();
    }
    #[test]
    fn classified_nodata_does_not_fall_through_to_foreign_default() {
        let (domestic, domestic_worker) = responder_nodata();
        let foreign = UdpSocket::bind("127.0.0.1:0").unwrap();
        foreign
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let foreign_address = foreign.local_addr().unwrap();
        let response = resolve(
            &query_for("www.cn.example"),
            &Policy {
                primary: vec![foreign_address],
                rules: vec![("cn.example".into(), vec![domestic])],
                ..Default::default()
            },
        );
        assert!(!has_answer(&response));
        let mut request = [0; 512];
        assert!(foreign.recv(&mut request).is_err());
        domestic_worker.join().unwrap();
    }
    #[test]
    fn longest_domain_rule_is_tried_before_primary() {
        let (rule_server, worker) = responder(true);
        let primary = UdpSocket::bind("127.0.0.1:0").unwrap();
        primary
            .set_read_timeout(Some(Duration::from_millis(80)))
            .unwrap();
        let response = resolve(
            &query(),
            &Policy {
                primary: vec![primary.local_addr().unwrap()],
                rules: vec![("example.com".into(), vec![rule_server])],
                ..Default::default()
            },
        );
        assert!(has_answer(&response));
        assert!(primary.recv(&mut [0; 64]).is_err());
        worker.join().unwrap();
    }
    #[test]
    fn mrouter_groups_compile_with_foreign_default_and_domain_prefixes() {
        let (primary, rules) = compile_groups(
            &[],
            &["114.114.114.114:53".into()],
            &["1.1.1.1:53".into()],
            &["domain:cn.example".into()],
            &["example.com".into()],
            "foreign",
            &[],
        )
        .unwrap();
        assert_eq!(primary, vec!["1.1.1.1:53".parse().unwrap()]);
        assert!(rules.iter().any(|(suffix, _)| suffix == "cn.example"));
        assert!(rules.iter().any(|(suffix, _)| suffix == "example.com"));
    }
    #[test]
    fn forwarder_sends_domestic_and_foreign_queries_to_the_matching_group() {
        let (domestic, domestic_worker) = responder_ip([192, 0, 2, 11]);
        let (foreign, foreign_worker) = responder_ip([192, 0, 2, 22]);
        let (primary, rules) = compile_groups(
            &[],
            &[domestic.to_string()],
            &[foreign.to_string()],
            &["domain:cn.example".into()],
            &["domain:global.example".into()],
            "foreign",
            &[],
        )
        .unwrap();
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let forwarder = Forwarder::start(
            &[address],
            Policy {
                primary,
                rules,
                ..Default::default()
            },
        )
        .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        for (name, expected) in [
            ("www.cn.example", [192, 0, 2, 11]),
            ("www.global.example", [192, 0, 2, 22]),
        ] {
            client.send_to(&query_for(name), address).unwrap();
            let mut response = [0; 4096];
            let (length, _) = client.recv_from(&mut response).unwrap();
            assert!(has_answer(&response[..length]));
            assert_eq!(&response[length - 4..length], expected.as_slice());
        }
        drop(forwarder);
        domestic_worker.join().unwrap();
        foreign_worker.join().unwrap();
    }

    #[test]
    fn geosite_matchers_route_full_suffix_and_keyword_domains() {
        let (full, full_worker) = responder_ip([192, 0, 2, 31]);
        let (suffix, suffix_worker) = responder_ip([192, 0, 2, 32]);
        let (keyword, keyword_worker) = responder_ip([192, 0, 2, 33]);
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let forwarder = Forwarder::start(
            &[address],
            Policy {
                primary: vec![suffix],
                geosite: Some(Arc::new(
                    crate::geosite::Router::compile(&[
                        (
                            vec![crate::geosite::Rule {
                                matcher: crate::geosite::Matcher::Full("full.example".into()),
                            }],
                            vec![full],
                        ),
                        (
                            vec![crate::geosite::Rule {
                                matcher: crate::geosite::Matcher::Keyword("needle".into()),
                            }],
                            vec![keyword],
                        ),
                    ])
                    .unwrap(),
                )),
                ..Default::default()
            },
        )
        .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        for (name, expected) in [
            ("full.example", [192, 0, 2, 31]),
            ("www.needle.example", [192, 0, 2, 33]),
            ("www.example.com", [192, 0, 2, 32]),
        ] {
            client.send_to(&query_for(name), address).unwrap();
            let mut response = [0; 4096];
            let (length, _) = client.recv_from(&mut response).unwrap();
            assert_eq!(&response[length - 4..length], expected.as_slice());
        }
        drop(forwarder);
        full_worker.join().unwrap();
        suffix_worker.join().unwrap();
        keyword_worker.join().unwrap();
    }
    #[test]
    fn malformed_queries_and_wrong_questions_are_rejected() {
        assert_eq!(failure(&[]).len(), 12);
        let q = query();
        let mut r = q.clone();
        r[2] |= 0x80;
        assert!(matches_query(&q, &r));
        r[13] ^= 1;
        assert!(!matches_query(&q, &r));
        assert!(endpoint("0.0.0.0:53").is_err());
        assert!(endpoint("127.0.0.1:0").is_err());
    }
}
