use super::*;
use pmman_protocol::p2p::{
    pair_secret, valid_candidate, Advertisement, Directory, Peer, RelayPeer,
};

#[derive(Clone)]
pub(super) struct Registration {
    advertisement: Advertisement,
    token: String,
    seen: DateTime<Utc>,
}

pub(super) fn relay_peers(state: &AppState) -> Vec<RelayPeer> {
    let devices = state.devices.read().expect("device registry poisoned");
    let users = state.users.read().expect("user registry poisoned");
    // Match advertise's lock order: device -> account -> transport registry.
    // Node heartbeats run concurrently with client directory refreshes.
    let registrations = state.p2p.read().expect("p2p registry poisoned");
    registrations
        .iter()
        .filter_map(|(id, entry)| {
            let device = devices.get(id)?;
            eligible(device, entry, &users).then(|| RelayPeer {
                device_id: *id,
                token: entry.token.clone(),
            })
        })
        .collect()
}

fn eligible(
    device: &Device,
    registration: &Registration,
    users: &HashMap<String, UserAccount>,
) -> bool {
    device.status == "active"
        && device.public_key == registration.advertisement.public_key
        && registration.seen > Utc::now() - Duration::seconds(30)
        && users
            .values()
            .any(|u| u.user_id == device.user_id && u.enabled)
}

pub(super) async fn advertise(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(mut input): Json<Advertisement>,
) -> Result<Json<Directory>, StatusCode> {
    let principal = authenticate(&state, &headers)?;
    expire_client_leases(&state, Utc::now()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if input.candidates.len() > 16 || input.candidates.iter().any(|c| !valid_candidate(*c)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    input.candidates.sort();
    input.candidates.dedup();
    let my_node_id = {
        let devices = state.devices.read().expect("device registry poisoned");
        devices
            .get(&id)
            .filter(|d| {
                d.user_id == principal.user_id
                    && d.status == "active"
                    && d.public_key == input.public_key
            })
            .map(|device| device.node_id.clone())
            .ok_or(StatusCode::NOT_FOUND)?
    };
    let (probe_endpoint, node_id, rate_limit, relay_node_id, relay_rate_limit) = {
        let nodes = state.nodes.read().expect("node registry poisoned");
        let relay_node = nodes
            .values()
            .filter(|n| node_is_connectable(n))
            .min_by(|a, b| a.node_id.cmp(&b.node_id))
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        // Older device records can outlive a removed node. Keep those clients
        // usable by assigning their directory to the same healthy relay
        // selected below; a missing node must not turn an otherwise valid P2P
        // advertisement into a misleading 503.
        let my_node = nodes.get(&my_node_id).unwrap_or(relay_node);
        (
            relay_node.probe_endpoint.clone(),
            my_node.node_id.clone(),
            my_node.rate_limit,
            relay_node.node_id.clone(),
            relay_node.rate_limit,
        )
    };
    // Node probe endpoints are intentionally stored as public hostnames in
    // production (for example `cn-gz02.pmman.tech:51840`). The directory
    // protocol carries a concrete SocketAddr, so resolve the endpoint at the
    // master instead of rejecting every hostname as an unhealthy relay. Keep
    // IPv4 first because most NATed clients publish an IPv4 underlay and the
    // node's UDP listener is dual-stack.
    let relay = resolve_relay_endpoint(&probe_endpoint).await?;
    let devices = state.devices.read().expect("device registry poisoned");
    let users = state.users.read().expect("user registry poisoned");
    // DNS resolution above yields to account/device lifecycle operations.
    // Recheck the exact identity and key under the publication locks: a late
    // advertisement must not recreate a deleted/revoked directory entry or
    // return other peers to a deleted or replaced account.
    revalidate_advertiser(&devices, &users, &principal, id, &input.public_key)?;
    let mut entries = state.p2p.write().expect("p2p registry poisoned");
    entries.retain(|device_id, entry| {
        devices
            .get(device_id)
            .is_some_and(|d| eligible(d, entry, &users))
    });
    let own = entries.entry(id).or_insert_with(|| Registration {
        advertisement: input.clone(),
        token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        seen: Utc::now(),
    });
    own.advertisement = input;
    own.seen = Utc::now();
    let token = own.token.clone();
    let peers = entries
        .iter()
        .filter_map(|(other_id, entry)| {
            let d = devices.get(other_id)?;
            if *other_id == id || !eligible(d, entry, &users) {
                return None;
            }
            Some(Peer {
                device_id: *other_id,
                username: d.username.clone(),
                public_key: d.public_key.clone(),
                ipv4: d.ipv4.clone(),
                ipv6: d.ipv6.clone(),
                enabled: entry.advertisement.enabled,
                candidates: entry.advertisement.candidates.clone(),
                secret: pair_secret(&token, &entry.token),
            })
        })
        .collect();
    Ok(Json(Directory {
        node_id,
        rate_limit,
        relay_node_id,
        relay_rate_limit,
        device_id: id,
        relay_endpoint: relay,
        relay_token: token,
        peers,
        refresh_ms: 1000,
    }))
}

pub(super) fn revalidate_advertiser(
    devices: &HashMap<Uuid, Device>,
    users: &HashMap<String, UserAccount>,
    principal: &Principal,
    id: Uuid,
    public_key: &str,
) -> Result<(), StatusCode> {
    if !users
        .get(&principal.username)
        .is_some_and(|user| user.enabled && user.user_id == principal.user_id)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !devices.get(&id).is_some_and(|device| {
        device.status == "active"
            && device.user_id == principal.user_id
            && device.public_key == public_key
    }) {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(())
}

async fn resolve_relay_endpoint(endpoint: &str) -> Result<SocketAddr, StatusCode> {
    if let Ok(address) = endpoint.parse::<SocketAddr>() {
        return Ok(address);
    }
    let mut addresses: Vec<_> = tokio::net::lookup_host(endpoint)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .collect();
    addresses.sort_by_key(|address| !address.is_ipv4());
    addresses
        .into_iter()
        .next()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}
