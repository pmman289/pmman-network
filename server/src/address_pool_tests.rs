use super::*;
use serde_json::{json, Value};
use tower::ServiceExt;

fn pool(segments: &[&str]) -> Pool {
    let mut p = Pool {
        pool_id: Uuid::new_v4().to_string(),
        name: "测试池".into(),
        description: String::new(),
        segments: segments.iter().map(|s| s.to_string()).collect(),
        exclusions: vec![],
    };
    p.normalize().unwrap();
    p
}
fn count(p: &Pool, f: u8) -> u128 {
    p.ranges(f, &[]).iter().map(|r| r.end - r.start + 1).sum()
}
fn initialized() -> AppState {
    let state = crate::tests::test_state();
    initialize(&state).unwrap();
    for (token, user) in [("admin-token", "admin"), ("user-token", "u1")] {
        state.sessions.write().unwrap().insert(
            token.into(),
            Session {
                user_id: user.into(),
                username: user.into(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        );
    }
    state
}
async fn request(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}
fn revision(state: &AppState) -> u64 {
    state
        .address_pools
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .revision
}

#[tokio::test]
async fn automatic_pool_routes_follow_crud_and_preserve_manual_routes() {
    let state = initialized();
    let manual = vec!["172.16.0.0/16".to_string(), "192.0.2.7/32".to_string()];
    *state.internal_cidrs.write().unwrap() = manual.clone();
    let (status, list) = request(
        &state,
        "POST",
        "/v1/admin/address-pools",
        "admin-token",
        json!({"revision":revision(&state),"name":"自动路由池",
            "segments":["192.0.2.7/32","192.0.2.255/32","2001:db8:2::/128"],
            "exclusions":["192.0.2.255/32"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let added = list["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "自动路由池")
        .unwrap();
    let id = added["pool_id"].as_str().unwrap();
    assert_eq!(added["warnings"], json!([]));
    let routes = internal_routes(&state);
    for route in [
        "172.16.0.0/16",
        "192.0.2.7/32",
        "192.0.2.255/32",
        "2001:db8:2::/128",
        "10.88.0.0/24",
        "fd88:1000::/64",
    ] {
        assert!(routes.iter().any(|r| r == route), "missing {route}");
    }
    assert_eq!(routes.iter().filter(|r| *r == "192.0.2.7/32").count(), 1);
    // Allocation exclusions do not remove reachability to the pool subnet.
    assert!(routes.contains(&"192.0.2.255/32".into()));
    assert_eq!(*state.internal_cidrs.read().unwrap(), manual);
    assert_eq!(
        request(
            &state,
            "POST",
            &format!("/v1/admin/address-pools/{id}"),
            "admin-token",
            json!({"revision":revision(&state),"name":"自动路由池",
            "segments":["198.51.100.0/31","2001:db8:3::/64"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    let routes = internal_routes(&state);
    assert!(!routes.contains(&"192.0.2.255/32".into()));
    assert!(!routes.contains(&"2001:db8:2::/128".into()));
    assert!(routes.contains(&"198.51.100.0/31".into()));
    assert!(routes.contains(&"2001:db8:3::/64".into()));
    assert!(routes.contains(&"192.0.2.7/32".into()));
    assert_eq!(
        request(
            &state,
            "DELETE",
            &format!("/v1/admin/address-pools/{id}?revision={}", revision(&state)),
            "admin-token",
            json!(null)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(!internal_routes(&state).contains(&"198.51.100.0/31".into()));
    assert!(!internal_routes(&state).contains(&"2001:db8:3::/64".into()));
    assert_eq!(*state.internal_cidrs.read().unwrap(), manual);
}

#[tokio::test]
async fn automatic_pool_routes_reach_registration_config_refresh_and_all_modes() {
    let state = initialized();
    *state.internal_cidrs.write().unwrap() = vec!["172.16.0.0/16".into()];
    let (status, _) = request(
        &state,
        "POST",
        "/v1/admin/address-pools",
        "admin-token",
        json!({"revision":revision(&state),"name":"独立双栈池",
            "segments":["192.0.2.255/32","2001:db8:2::/128"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer user-token".parse().unwrap());
    let assigned = register_device(
        &state,
        authenticate(&state, &headers).unwrap(),
        RegisterRequest {
            public_key: "pool-route-key".into(),
            ipv4_mode: "internal".into(),
            ipv6_mode: "internal".into(),
            split_route_source: None,
        },
        "legacy",
        &state.gateway,
        false,
    )
    .unwrap()
    .0;
    let expected = internal_routes(&state);
    assert_eq!(assigned.routes, expected);
    let (status, refreshed) = request(
        &state,
        "GET",
        &format!("/v1/devices/{}/config", assigned.device_id),
        "user-token",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(refreshed["routes"], json!(expected));
    let node = state.nodes.read().unwrap()["node-a"].clone();
    for mode in ["internal", "split", "global"] {
        let mut config = assigned.clone();
        config.ipv4_mode = mode.into();
        config.ipv6_mode = mode.into();
        let config = node_device_config_with_capability(
            &node,
            config,
            &expected,
            Some("client_builtin_v1"),
            None,
        )
        .unwrap();
        for route in &expected {
            assert!(config.routes.contains(route), "{mode}: {route}");
        }
    }
    // Derived routes must not leak back into the manual configuration API.
    assert_eq!(pool_settings(&state).internal_cidrs, vec!["172.16.0.0/16"]);
}

#[test]
fn host_segments_and_subnet_boundaries_are_independent() {
    assert_eq!(count(&pool(&["192.0.2.0/30"]), 4), 2);
    assert_eq!(count(&pool(&["192.0.2.0/31"]), 4), 2);
    assert_eq!(count(&pool(&["192.0.2.0/32", "192.0.2.255/32"]), 4), 2);
    assert_eq!(count(&pool(&["2001:db8::/126"]), 6), 3);
    assert_eq!(count(&pool(&["2001:db8::/128"]), 6), 1);
    // Adjacent /32s must not become a /30 and lose two usable addresses.
    assert_eq!(
        count(
            &pool(&[
                "192.0.2.0/32",
                "192.0.2.1/32",
                "192.0.2.2/32",
                "192.0.2.3/32"
            ]),
            4
        ),
        4
    );
    assert_eq!(count(&pool(&["192.0.2.0/30", "192.0.2.0/32"]), 4), 3);
}
#[test]
fn duplicate_segments_reservations_and_special_addresses() {
    let mut p = pool(&["192.0.2.7/24", "192.0.2.0/24", "127.0.0.1/32", "::1/128"]);
    p.exclusions = vec!["192.0.2.2/31".into()];
    assert_eq!(p.segments.len(), 3);
    assert_eq!(count(&p, 4), 252);
    assert_eq!(count(&p, 6), 0);
    assert!(!in_ranges(
        "192.0.2.1".parse().unwrap(),
        &p.ranges(4, &[cidr("192.0.2.1/32").unwrap().0])
    ));
}
#[test]
fn huge_ipv6_capacity_has_no_overflow_or_enumeration() {
    let p = pool(&["::/0"]);
    let n = count(&p, 6);
    assert!(n > u64::MAX as u128);
    assert!(n < u128::MAX);
    let p = pool(&["2001:db8::/64"]);
    assert_eq!(count(&p, 6), u64::MAX as u128);
}
#[test]
fn shrinking_pool_still_reports_quarantined_addresses_outside_new_segments() {
    let p = pool(&["192.0.2.20/32"]);
    let mut r = Registry::default();
    r.leases.push(Lease {
        id: Uuid::new_v4(),
        user_id: "deleted".into(),
        address: "192.0.2.10".parse().unwrap(),
        pool_id: Some(p.pool_id.clone()),
        state: "releasing".into(),
    });
    let s = stats(&p, 4, &r, &[]);
    assert_eq!(
        (s.capacity, s.allocated, s.releasing, s.available),
        ("1".into(), "1".into(), "1".into(), "1".into())
    );
}
#[test]
fn thousands_of_host_routes_are_merged_without_losing_hosts() {
    let segments: Vec<String> = (0..4096u32)
        .map(|i| format!("{}/32", Ipv4Addr::from(0xc0000000 + i)))
        .collect();
    let mut p = pool(&["192.0.2.1/32"]);
    p.segments = segments;
    p.normalize().unwrap();
    assert_eq!(count(&p, 4), 4096);
    assert_eq!(p.ranges(4, &[]).len(), 1);
}
#[test]
fn migration_keeps_addresses_and_excludes_old_gateway() {
    let state = crate::tests::test_state();
    {
        let mut users = state.users.write().unwrap();
        let u = users.get_mut("u1").unwrap();
        u.leased_ipv4 = Some("10.88.0.10".into());
        u.leased_ipv6 = Some("fd88:1000::a".into());
    }
    initialize(&state).unwrap();
    let r = state.address_pools.read().unwrap();
    let r = r.as_ref().unwrap();
    let p = r.pools.values().next().unwrap();
    assert_eq!(count(p, 4), 253);
    assert_eq!(r.leases.len(), 2);
    assert_eq!(
        state.users.read().unwrap()["u1"].leased_ipv4.as_deref(),
        Some("10.88.0.10")
    );
}
#[test]
fn independent_families_reuse_and_exhaustion() {
    let state = initialized();
    let mut guard = state.address_pools.write().unwrap();
    let r = guard.as_mut().unwrap();
    let mut p = pool(&["192.0.2.10/32", "2001:db8:1::/128"]);
    p.name = "独立池".into();
    let id = p.pool_id.clone();
    r.pools.insert(id.clone(), p);
    r.default4 = id.clone();
    r.default6 = id;
    let users = state.users.read().unwrap();
    let u = &users["u1"];
    let devices = state.devices.read().unwrap();
    let pair = r.allocate(&state, u, &users, &devices, None).unwrap();
    assert_eq!(pair, ("192.0.2.10".into(), "2001:db8:1::".into()));
    let mut users = users.clone();
    users.get_mut("admin").unwrap().leased_ipv4 = Some(pair.0.clone());
    users.get_mut("admin").unwrap().leased_ipv6 = Some(pair.1.clone());
    assert!(r
        .allocate(&state, &users["u1"], &users, &devices, None)
        .unwrap_err()
        .contains("耗尽"));
}
#[tokio::test]
async fn pool_crud_overlap_stale_version_and_access_control() {
    let state = initialized();
    assert_eq!(
        request(
            &state,
            "GET",
            "/v1/admin/address-pools",
            "user-token",
            json!(null)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let body = json!({"revision":revision(&state),"name":"离散池","segments":["192.0.2.0/32","192.0.2.255/32","2001:db8:2::/128"]});
    let (s, b) = request(
        &state,
        "POST",
        "/v1/admin/address-pools",
        "admin-token",
        body.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        request(
            &state,
            "POST",
            "/v1/admin/address-pools",
            "admin-token",
            body
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (s, b) = request(
        &state,
        "POST",
        "/v1/admin/address-pools",
        "admin-token",
        json!({"revision":revision(&state),"name":"重复池","segments":["192.0.2.0/24"]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    assert!(b["error"].as_str().unwrap().contains("重叠"));
    let id = state
        .address_pools
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .default4
        .clone();
    assert_eq!(
        request(
            &state,
            "DELETE",
            &format!("/v1/admin/address-pools/{id}?revision={}", revision(&state)),
            "admin-token",
            json!(null)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn fixed_pool_requires_selected_address_and_mixed_policy_works() {
    let state = initialized();
    let rev = revision(&state);
    let (s,_)=request(&state,"POST","/v1/admin/users/u1/address-policy","admin-token",json!({"revision":rev,"ipv4":{"mode":"pool_fixed","pool_id":null,"address":null},"ipv6":{"mode":"automatic","pool_id":null,"address":null}})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(revision(&state), rev);
    let (s,b)=request(&state,"POST","/v1/admin/users/u1/address-policy","admin-token",json!({"revision":rev,"ipv4":{"mode":"pool_fixed","pool_id":null,"address":"10.88.0.25"},"ipv6":{"mode":"automatic","pool_id":null,"address":null}})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let guard = state.address_pools.read().unwrap();
    let r = guard.as_ref().unwrap();
    let users = state.users.read().unwrap();
    let devices = state.devices.read().unwrap();
    assert_eq!(
        r.allocate(&state, &users["u1"], &users, &devices, None)
            .unwrap(),
        ("10.88.0.25".into(), "fd88:1000::2".into())
    );
}
#[tokio::test]
async fn fixed_address_is_global_reservation_and_conflicting_save_fails() {
    let state = initialized();
    let body = |revision| json!({"revision":revision,"ipv4":{"mode":"manual","pool_id":null,"address":"192.0.2.12"},"ipv6":{"mode":"automatic","pool_id":null,"address":null}});
    assert_eq!(
        request(
            &state,
            "POST",
            "/v1/admin/users/u1/address-policy",
            "admin-token",
            body(revision(&state))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (s, b) = request(
        &state,
        "POST",
        "/v1/admin/users/admin/address-policy",
        "admin-token",
        body(revision(&state)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    // Manual assignment is not a bypass for an administrator's exclusions.
    let body = json!({"revision":revision(&state),"ipv4":{"mode":"manual","address":"10.88.0.1"},"ipv6":{"mode":"automatic"}});
    assert_eq!(
        request(
            &state,
            "POST",
            "/v1/admin/users/admin/address-policy",
            "admin-token",
            body
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn address_picker_has_cursor_exact_search_and_own_reservation() {
    let state = initialized();
    let id = state
        .address_pools
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .default6
        .clone();
    let (s, b) = request(
        &state,
        "GET",
        &format!("/v1/admin/address-pools/{id}/addresses?family=6&limit=2"),
        "admin-token",
        json!(null),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["items"].as_array().unwrap().len(), 2);
    assert!(b["next_cursor"].is_string());
    let cursor = b["next_cursor"].as_str().unwrap();
    let (_, b) = request(
        &state,
        "GET",
        &format!("/v1/admin/address-pools/{id}/addresses?family=6&limit=2&cursor={cursor}"),
        "admin-token",
        json!(null),
    )
    .await;
    assert_eq!(b["items"][0]["address"], "fd88:1000::4");
    let (_, b) = request(
        &state,
        "GET",
        &format!("/v1/admin/address-pools/{id}/addresses?family=6&address=fd88:1000::ffff"),
        "admin-token",
        json!(null),
    )
    .await;
    assert_eq!(b["items"][0]["address"], "fd88:1000::ffff");
}
#[tokio::test]
async fn new_register_uses_separate_families_and_legacy_writes_are_blocked() {
    let state = initialized();
    let principal = authenticate(&state, &{
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer user-token".parse().unwrap());
        h
    })
    .unwrap();
    let result = register_device(
        &state,
        principal,
        RegisterRequest {
            public_key: "test-public".into(),
            ipv4_mode: "internal".into(),
            ipv6_mode: "internal".into(),
            split_route_source: None,
        },
        "legacy",
        &state.gateway,
        false,
    )
    .unwrap()
    .0;
    assert_eq!(result.ipv4, "10.88.0.2/32");
    assert_eq!(result.ipv6, "fd88:1000::2/128");
    assert_eq!(
        state
            .address_pools
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .leases
            .iter()
            .filter(|l| l.state != "released")
            .count(),
        2
    );
    let (s, _) = request(
        &state,
        "POST",
        "/v1/admin/settings/address-pool",
        "admin-token",
        json!({"ipv4_pool":"10.90.0.0/24","ipv6_pool":"fd90::/64"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

// Dedicated local PostgreSQL only; never point these tests at production.
#[tokio::test]
async fn pool_switch_keeps_active_connection_and_requires_cleanup_before_renumber() {
    let state = initialized();
    let principal = || {
        authenticate(&state, &{
            let mut h = HeaderMap::new();
            h.insert("authorization", "Bearer user-token".parse().unwrap());
            h
        })
        .unwrap()
    };
    let input = || RegisterRequest {
        public_key: "pool-switch-key".into(),
        ipv4_mode: "internal".into(),
        ipv6_mode: "internal".into(),
        split_route_source: None,
    };
    let original = register_device(
        &state,
        principal(),
        input(),
        "node-a",
        &state.gateway,
        false,
    )
    .unwrap()
    .0;
    let (_, list) = request(
        &state,
        "POST",
        "/v1/admin/address-pools",
        "admin-token",
        json!({"revision":revision(&state),"name":"新 IPv4 池","segments":["192.0.2.200/32"]}),
    )
    .await;
    let id = list["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "新 IPv4 池")
        .unwrap()["pool_id"]
        .as_str()
        .unwrap();
    let (s,b)=request(&state,"POST","/v1/admin/users/u1/address-policy","admin-token",json!({"revision":revision(&state),"ipv4":{"mode":"automatic","pool_id":id,"address":null},"ipv6":{"mode":"automatic","pool_id":null,"address":null}})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        state.devices.read().unwrap()[&original.device_id].ipv4,
        "10.88.0.2"
    );
    assert_eq!(
        register_device(
            &state,
            principal(),
            input(),
            "node-a",
            &state.gateway,
            false
        )
        .err()
        .unwrap()
        .0,
        StatusCode::CONFLICT
    );
    {
        let mut devices = state.devices.write().unwrap();
        let d = devices.get_mut(&original.device_id).unwrap();
        d.status = "revoked".into();
        d.last_seen_at = Utc::now();
    }
    assert_eq!(
        register_device(
            &state,
            principal(),
            input(),
            "node-a",
            &state.gateway,
            false
        )
        .err()
        .unwrap()
        .0,
        StatusCode::CONFLICT
    );
    {
        let mut nodes = state.nodes.write().unwrap();
        let n = nodes.get_mut("node-a").unwrap();
        n.cleanup_ok = true;
        n.last_seen_at = Utc::now();
        n.applied.clear();
        n.sync_errors.clear();
    }
    let new = register_device(
        &state,
        principal(),
        input(),
        "node-a",
        &state.gateway,
        false,
    )
    .unwrap()
    .0;
    assert_eq!(new.ipv4, "192.0.2.200/32");
    assert_eq!(new.ipv6, original.ipv6);
    let r = state.address_pools.read().unwrap();
    assert!(r
        .as_ref()
        .unwrap()
        .leases
        .iter()
        .any(|l| l.address.to_string() == "10.88.0.2" && l.state == "released"));
}

#[tokio::test]
async fn fixed_reservation_blocks_shrink_and_primary_switch() {
    let state = initialized();
    let original = state
        .address_pools
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .default4
        .clone();
    let (s,b)=request(&state,"POST","/v1/admin/users/u1/address-policy","admin-token",json!({"revision":revision(&state),"ipv4":{"mode":"pool_fixed","pool_id":null,"address":"10.88.0.50"},"ipv6":{"mode":"automatic"}})).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let before = revision(&state);
    let (s, _) = request(
        &state,
        "POST",
        &format!("/v1/admin/address-pools/{original}"),
        "admin-token",
        json!({"revision":before,"name":"缩小","segments":["10.88.0.0/27","fd88:1000::/64"]}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(revision(&state), before);
    let (s, b) = request(
        &state,
        "POST",
        "/v1/admin/address-pools",
        "admin-token",
        json!({"revision":before,"name":"新主池","segments":["192.0.2.0/24"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let id = b["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "新主池")
        .unwrap()["pool_id"]
        .as_str()
        .unwrap();
    let (s, _) = request(
        &state,
        "POST",
        "/v1/admin/address-pool-defaults",
        "admin-token",
        json!({"revision":revision(&state),"ipv4_pool_id":id,"ipv6_pool_id":original}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        request(
            &state,
            "DELETE",
            &format!("/v1/admin/address-pools/{id}?revision={}", revision(&state)),
            "admin-token",
            json!(null)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

fn forwarding_fixture(
    state: &AppState,
    device: Uuid,
    target: &str,
) -> port_forward::AllocationRecord {
    use crate::port_forward::PortRange;
    use pmman_protocol::PortForwardProtocol;
    let entry = port_forward::EntryRecord {
        entry_id: Uuid::new_v4().to_string(),
        node_id: "entry-node".into(),
        name: "test".into(),
        public_address: "192.0.2.1".into(),
        hostname: None,
        family: "ipv4".into(),
        protocols: vec![PortForwardProtocol::Tcp],
        ranges: vec![PortRange {
            start: 30000,
            end: 30100,
        }],
        reserved_ports: vec![],
        enabled: true,
    };
    state.port_forward_store.save_entry(&entry).unwrap();
    state
        .port_forward_store
        .save_policy(
            "u1",
            &port_forward::PolicyRecord {
                enabled: true,
                max_active_forwards: 10,
                allowed_entry_ids: vec![entry.entry_id.clone()],
                allowed_protocols: entry.protocols.clone(),
                allowed_target_ports: None,
                allowed_families: vec!["ipv4".into()],
            },
        )
        .unwrap();
    state
        .port_forward_store
        .allocate("u1", "u1", device, &entry, &entry.protocols, target, 22)
        .unwrap()
}

#[test]
fn expired_forward_requires_owning_nodes_explicit_removal_ack() {
    let state = initialized();
    let device = cleanup_device();
    let mapping = forwarding_fixture(&state, device.device_id, &device.ipv4);
    state
        .port_forward_store
        .set_state(&mapping.forward_id, "error", None, None)
        .unwrap();
    state
        .port_forward_store
        .set_state(&mapping.forward_id, "expired", None, None)
        .unwrap();
    state
        .devices
        .write()
        .unwrap()
        .insert(device.device_id, device.clone());
    let prepare = || {
        let devices = state.devices.read().unwrap();
        let users = state.users.read().unwrap();
        let mut registry = state.address_pools.write().unwrap();
        prepare_forward_cleanup(
            &state,
            &mut registry,
            &users,
            &devices,
            &device,
            true,
            false,
        )
        .unwrap()
    };
    assert!(!prepare());
    assert_eq!(
        pending_forward_cleanup(&state, "entry-node"),
        vec![mapping.forward_id.clone()]
    );
    acknowledge_forward_cleanup(
        &state,
        "other-node",
        std::slice::from_ref(&mapping.forward_id),
    )
    .unwrap();
    assert!(!prepare());
    acknowledge_forward_cleanup(
        &state,
        "entry-node",
        std::slice::from_ref(&mapping.forward_id),
    )
    .unwrap();
    assert!(prepare());
    assert!(pending_forward_cleanup(&state, "entry-node").is_empty());
}

fn cleanup_device() -> Device {
    Device {
        device_id: Uuid::new_v4(),
        user_id: "u1".into(),
        username: "u1".into(),
        public_key: "cleanup-key".into(),
        node_id: "node-a".into(),
        ipv4: "10.88.0.10".into(),
        ipv6: "fd88:1000::a".into(),
        ipv4_mode: "internal".into(),
        ipv6_mode: "internal".into(),
        status: "revoked".into(),
        config_version: 1,
        last_seen_at: Utc::now(),
        traffic: Default::default(),
        session_traffic: Default::default(),
    }
}

struct Database {
    root: String,
    schema: String,
    store: db::PgStore,
    url: String,
}
impl Database {
    fn new() -> Self {
        let root = env::var("PMMAN_ADDRESS_POOL_TEST_DATABASE_URL")
            .expect("set dedicated pmman_address_pool_test database URL");
        assert!(root.contains("pmman_address_pool_test"));
        let schema = format!("address_pool_test_{}", Uuid::new_v4().simple());
        postgres::Client::connect(&root, postgres::NoTls)
            .unwrap()
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .unwrap();
        let url = format!("{root}?options=-csearch_path%3D{schema}");
        let store = db::PgStore::connect(&url).unwrap();
        Self {
            root,
            schema,
            store,
            url,
        }
    }
    fn state(&self) -> AppState {
        let mut s = crate::tests::test_state();
        s.database = Some(Arc::new(self.store.clone()));
        s.port_forward_store = port_forward::PortForwardStore::open(&self.url).unwrap();
        persist_users(&s, &s.users.read().unwrap()).unwrap();
        initialize(&s).unwrap();
        s.sessions.write().unwrap().insert(
            "admin-token".into(),
            Session {
                user_id: "admin".into(),
                username: "admin".into(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        );
        s
    }
    fn execute(&self, sql: &str) {
        postgres::Client::connect(&self.url, postgres::NoTls)
            .unwrap()
            .batch_execute(sql)
            .unwrap();
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        assert!(
            self.schema.starts_with("address_pool_test_")
                && self
                    .schema
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        let _ = postgres::Client::connect(&self.root, postgres::NoTls)
            .and_then(|mut c| c.batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema)));
    }
}
#[test]
#[ignore = "dedicated PostgreSQL"]
fn postgres_migration_restart_and_rollback() {
    let db = Database::new();
    let state = db.state();
    let before = revision(&state);
    let guard = state.address_pools.read().unwrap();
    let current = guard.as_ref().unwrap().clone();
    drop(guard);
    let users = state.users.read().unwrap().clone();
    let devices = state.devices.read().unwrap().clone();
    let mut next = current.clone();
    next.pools.get_mut(&next.default4.clone()).unwrap().name = "更新后".into();
    save(&state, before, &mut next, &users, &devices, true).unwrap();
    *state.address_pools.write().unwrap() = Some(next);
    let mut restarted = crate::tests::test_state();
    restarted.database = Some(Arc::new(db.store.clone()));
    load_user_registry(&restarted).unwrap();
    initialize(&restarted).unwrap();
    assert_eq!(revision(&restarted), before + 1);
    assert_eq!(
        restarted
            .address_pools
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .pools[&current.default4]
            .name,
        "更新后"
    );
    db.execute("CREATE FUNCTION fail_address_save() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'injected'; END$$; CREATE TRIGGER fail_address_save BEFORE INSERT OR UPDATE ON address_leases FOR EACH ROW EXECUTE FUNCTION fail_address_save();");
    let mut users = users;
    users.get_mut("u1").unwrap().leased_ipv4 = Some("10.88.0.20".into());
    users.get_mut("u1").unwrap().leased_ipv6 = Some("fd88:1000::20".into());
    let mut next = state
        .address_pools
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    assert!(save(&state, before + 1, &mut next, &users, &devices, true).is_err());
    assert_eq!(revision(&state), before + 1);
    assert!(!db
        .store
        .get_document("users")
        .unwrap()
        .unwrap()
        .contains("10.88.0.20"));
    db.execute("DELETE FROM address_pool_defaults");
    assert!(initialize(&state)
        .unwrap_err()
        .contains("拒绝重新生成默认池"));
}
#[test]
#[ignore = "dedicated PostgreSQL"]
fn postgres_concurrent_selected_address_has_one_winner() {
    let db = Database::new();
    let state = db.state();
    let revision = revision(&state);
    let mut workers = Vec::new();
    for username in ["u1", "admin"] {
        let state = state.clone();
        workers.push(std::thread::spawn(move|| {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(request(&state,"POST",&format!("/v1/admin/users/{username}/address-policy"),"admin-token",json!({"revision":revision,"ipv4":{"mode":"pool_fixed","pool_id":null,"address":"10.88.0.50"},"ipv6":{"mode":"automatic","pool_id":null,"address":null}}))).0
        }));
    }
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|s| **s == StatusCode::OK).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let mut c = postgres::Client::connect(&db.url, postgres::NoTls).unwrap();
    let n:i64=c.query_one("SELECT count(*) FROM address_leases WHERE address='10.88.0.50'::inet AND state<>'released'",&[]).unwrap().get(0);
    assert_eq!(n, 1);
}

#[test]
#[ignore = "dedicated PostgreSQL"]
fn postgres_forward_cleanup_survives_restart_and_account_deletion() {
    let db = Database::new();
    let state = db.state();
    let device = cleanup_device();
    state
        .devices
        .write()
        .unwrap()
        .insert(device.device_id, device.clone());
    let mapping = forwarding_fixture(&state, device.device_id, &device.ipv4);
    state
        .port_forward_store
        .set_state(&mapping.forward_id, "error", None, None)
        .unwrap();
    state
        .port_forward_store
        .set_state(&mapping.forward_id, "expired", None, None)
        .unwrap();
    let (s, b) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(request(
            &state,
            "DELETE",
            "/v1/admin/users/u1",
            "admin-token",
            json!(null),
        ));
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    let report = NodeHeartbeatRequest {
        cleanup_ok: true,
        ..Default::default()
    };
    user_deletion::confirm_device_cleanup(&state, "node-a", &report).unwrap();
    assert_eq!(
        state.devices.read().unwrap()[&device.device_id].status,
        "revoked"
    );
    assert_eq!(
        pending_forward_cleanup(&state, "entry-node"),
        vec![mapping.forward_id.clone()]
    );
    // Reload the durable ledger, not an in-memory copy of its pending ACKs.
    *state.address_pools.write().unwrap() = None;
    initialize(&state).unwrap();
    assert_eq!(
        pending_forward_cleanup(&state, "entry-node"),
        vec![mapping.forward_id.clone()]
    );
    acknowledge_forward_cleanup(&state, "entry-node", &[mapping.forward_id]).unwrap();
    user_deletion::confirm_device_cleanup(&state, "node-a", &report).unwrap();
    assert_eq!(
        state.devices.read().unwrap()[&device.device_id].status,
        "released"
    );
    let result=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(request(&state,"POST","/v1/admin/users/admin/address-policy","admin-token",json!({"revision":revision(&state),"ipv4":{"mode":"pool_fixed","address":device.ipv4},"ipv6":{"mode":"automatic"}})));
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
}

#[test]
#[ignore = "dedicated PostgreSQL"]
fn postgres_failed_second_family_does_not_commit_first_family() {
    let db = Database::new();
    let state = db.state();
    let (old_users, before) = (
        serde_json::to_value(&*state.users.read().unwrap()).unwrap(),
        revision(&state),
    );
    let mut registry = state.address_pools.write().unwrap();
    let r = registry.as_mut().unwrap();
    let mut p = pool(&["192.0.2.100/32", "::1/128"]);
    p.name = "第二族耗尽".into();
    let id = p.pool_id.clone();
    r.pools.insert(id.clone(), p);
    r.default4 = id.clone();
    r.default6 = id;
    drop(registry);
    let mut h = HeaderMap::new();
    h.insert("authorization", "Bearer admin-token".parse().unwrap());
    let result = register_device(
        &state,
        authenticate(&state, &h).unwrap(),
        RegisterRequest {
            public_key: "dual-failure".into(),
            ipv4_mode: "internal".into(),
            ipv6_mode: "internal".into(),
            split_route_source: None,
        },
        "node-a",
        &state.gateway,
        false,
    );
    assert!(result.is_err());
    assert_eq!(revision(&state), before);
    assert_eq!(
        serde_json::to_value(&*state.users.read().unwrap()).unwrap(),
        old_users
    );
    assert!(state.devices.read().unwrap().is_empty());
    let mut c = postgres::Client::connect(&db.url, postgres::NoTls).unwrap();
    let n:i64=c.query_one("SELECT count(*) FROM address_leases WHERE address='192.0.2.100'::inet AND state<>'released'",&[]).unwrap().get(0);
    assert_eq!(n, 0);
}

#[test]
#[ignore = "dedicated PostgreSQL"]
fn postgres_concurrent_automatic_allocations_are_unique() {
    let db = Database::new();
    let state = db.state();
    state.sessions.write().unwrap().insert(
        "user-token".into(),
        Session {
            user_id: "u1".into(),
            username: "u1".into(),
            expires_at: Utc::now() + Duration::hours(1),
        },
    );
    let mut workers = Vec::new();
    for token in ["admin-token", "user-token"] {
        let state = state.clone();
        workers.push(std::thread::spawn(move || {
            let mut h = HeaderMap::new();
            h.insert("authorization", format!("Bearer {token}").parse().unwrap());
            register_device(
                &state,
                authenticate(&state, &h).unwrap(),
                RegisterRequest {
                    public_key: format!("key-{token}"),
                    ipv4_mode: "internal".into(),
                    ipv6_mode: "internal".into(),
                    split_route_source: None,
                },
                "node-a",
                &state.gateway,
                false,
            )
            .unwrap()
            .0
        }));
    }
    let pairs: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_ne!(pairs[0].ipv4, pairs[1].ipv4);
    assert_ne!(pairs[0].ipv6, pairs[1].ipv6);
    let mut c = postgres::Client::connect(&db.url, postgres::NoTls).unwrap();
    let n: i64 = c
        .query_one(
            "SELECT count(*) FROM address_leases WHERE state<>'released'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(n, 4);
}
