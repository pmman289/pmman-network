use super::*;
use axum::http::Request;
use serde_json::{json, Value};
use tower::ServiceExt;

fn state() -> AppState {
    let state = crate::tests::test_state();
    for (token, name) in [("admin-session", "admin"), ("user-session", "u1")] {
        state.sessions.write().unwrap().insert(
            token.into(),
            Session {
                user_id: name.into(),
                username: name.into(),
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
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = app(state.clone())
        .oneshot(
            req.body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

async fn delete(state: &AppState, name: &str, token: Option<&str>) -> (StatusCode, Value) {
    request(
        state,
        "DELETE",
        &format!("/v1/admin/users/{name}"),
        token,
        Value::Null,
    )
    .await
}

#[tokio::test]
async fn authorization_precedes_lookup_and_protects_self_and_last_admin() {
    let state = state();
    let before = serde_json::to_value(&*state.users.read().unwrap()).unwrap();
    for name in ["u1", "missing", "admin"] {
        assert_eq!(delete(&state, name, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            delete(&state, name, Some("invalid")).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            delete(&state, name, Some("user-session")).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        delete(&state, "missing", Some("admin-session")).await.0,
        StatusCode::NOT_FOUND
    );
    let last = delete(&state, "admin", Some("admin-session")).await;
    assert_eq!(last.0, StatusCode::CONFLICT);
    assert!(last.1["error"].as_str().unwrap().contains("最后"));
    let mut second = state.users.read().unwrap()["admin"].clone();
    second.username = "admin2".into();
    second.user_id = "admin2-id".into();
    state.users.write().unwrap().insert("admin2".into(), second);
    let own = delete(&state, "admin", Some("admin-session")).await;
    assert_eq!(own.0, StatusCode::CONFLICT);
    assert!(own.1["error"].as_str().unwrap().contains("当前"));
    state.users.write().unwrap().remove("admin2");
    assert_eq!(
        serde_json::to_value(&*state.users.read().unwrap()).unwrap(),
        before
    );
    state
        .users
        .write()
        .unwrap()
        .get_mut("admin")
        .unwrap()
        .must_change_password = true;
    assert_eq!(
        delete(&state, "u1", Some("admin-session")).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn deletion_has_no_memory_or_file_fallback() {
    let state = state();
    let before = serde_json::to_value(&*state.users.read().unwrap()).unwrap();
    let result = delete(&state, "u1", Some("admin-session")).await;
    assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(result.1["error"].is_string());
    assert_eq!(
        serde_json::to_value(&*state.users.read().unwrap()).unwrap(),
        before
    );
    assert!(state.sessions.read().unwrap().contains_key("user-session"));
}

#[test]
fn usable_admin_guard_ignores_disabled_and_password_pending_admins() {
    let state = state();
    let mut users = state.users.read().unwrap().clone();
    for (name, enabled, pending) in [("disabled", false, false), ("pending", true, true)] {
        let mut user = users["admin"].clone();
        user.username = name.into();
        user.user_id = name.into();
        user.enabled = enabled;
        user.must_change_password = pending;
        users.insert(name.into(), user);
    }
    assert_eq!(
        guard(&users, "admin", "other-id").unwrap_err().0,
        StatusCode::CONFLICT
    );
    assert!(guard(&users, "disabled", "other-id").is_ok());
    assert!(guard(&users, "pending", "other-id").is_ok());
}

#[test]
fn session_insert_rechecks_identity_disable_and_password_after_verification() {
    let state = state();
    let verified = state.users.read().unwrap()["u1"].clone();
    assert!(issue_login_session(&state, &verified).is_ok());
    for mutation in [0, 1, 2, 3] {
        let mut current = verified.clone();
        match mutation {
            0 => current.enabled = false,
            1 => current.user_id = "replacement-id".into(),
            2 => current.password_hash = "changed-password-version".into(),
            _ => {}
        }
        state.users.write().unwrap().insert("u1".into(), current);
        if mutation == 3 {
            state.users.write().unwrap().remove("u1");
        }
        let count = state.sessions.read().unwrap().len();
        assert!(matches!(
            issue_login_session(&state, &verified),
            Err(StatusCode::UNAUTHORIZED)
        ));
        assert_eq!(state.sessions.read().unwrap().len(), count);
    }
}

// Opt-in local PostgreSQL only. Each test owns one random schema and never
// changes another test's tables, credentials, schemas or database lifecycle.
struct Database {
    root: String,
    schema: String,
    url: String,
    store: db::PgStore,
}
impl Database {
    fn new() -> Self {
        let root = env::var("PMMAN_USER_DELETION_TEST_DATABASE_URL")
            .expect("set dedicated deletion test database URL");
        let config: postgres::Config = root.parse().expect("invalid test database config");
        assert_eq!(
            config.get_dbname(),
            Some("pmman_user_deletion_test"),
            "refusing non-test database"
        );
        assert!(config.get_hosts().iter().all(|host| matches!(host, postgres::config::Host::Tcp(name) if name == "127.0.0.1" || name == "localhost" || name == "::1")), "test database must be local");
        let schema = format!("user_deletion_test_{}", Uuid::new_v4().simple());
        postgres::Client::connect(&root, postgres::NoTls)
            .expect("test database unavailable")
            .batch_execute(&format!("CREATE SCHEMA {schema}"))
            .unwrap();
        let sep = if root.contains('?') { '&' } else { '?' };
        let url = format!("{root}{sep}options=-csearch_path%3D{schema}");
        let store = db::PgStore::connect(&url).expect("test schema setup failed");
        Self {
            root,
            schema,
            url,
            store,
        }
    }
    fn client(&self) -> postgres::Client {
        postgres::Client::connect(&self.url, postgres::NoTls).expect("test connection failed")
    }
    fn state(&self) -> AppState {
        let mut state = state();
        state.database = Some(Arc::new(self.store.clone()));
        state.port_forward_store = port_forward::PortForwardStore::open(&self.url).unwrap();
        // Use an immutable ID different from the username, as real created
        // users do. No cleanup may confuse the two lookup keys.
        state.users.write().unwrap().get_mut("u1").unwrap().user_id = "deleted-user-id".into();
        state
            .sessions
            .write()
            .unwrap()
            .get_mut("user-session")
            .unwrap()
            .user_id = "deleted-user-id".into();
        persist_users(&state, &state.users.read().unwrap()).unwrap();
        state
    }
    fn execute(&self, sql: &str) {
        self.client().batch_execute(sql).unwrap();
    }
    fn count(&self, sql: &str) -> i64 {
        self.client().query_one(sql, &[]).unwrap().get(0)
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        assert!(
            self.schema.starts_with("user_deletion_test_")
                && self
                    .schema
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        let _ = postgres::Client::connect(&self.root, postgres::NoTls)
            .and_then(|mut c| c.batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema)));
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn activity_history_fixture() -> HashMap<Uuid, Device> {
    [
        ("active", "2026-10-20T01:02:03Z"),
        ("revoked", "2026-10-21T02:03:04Z"),
        ("released", "2026-10-22T03:04:05Z"),
        ("revoked", "1970-01-01T00:00:00Z"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (status, timestamp))| {
        let id = Uuid::new_v4();
        let device = serde_json::from_value(json!({
            "device_id": id, "user_id": "deleted-user-id", "username": "u1",
            "public_key": format!("history-key-{index}"), "node_id": "node-a",
            "ipv4": format!("10.88.0.{}", index + 10),
            "ipv6": format!("fd88:1000::{:x}", index + 10),
            "ipv4_mode": "internal", "ipv6_mode": "internal", "status": status,
            "config_version": 7, "last_seen_at": timestamp
        }))
        .unwrap();
        (id, device)
    })
    .collect()
}

#[test]
fn restarted_device_history_preserves_activity_and_release_quarantine() {
    let mut history = activity_history_fixture();
    let mut expected = history.clone();
    for device in expected.values_mut() {
        if device.status != "released" {
            device.status = "revoked".into();
        }
    }
    for _ in 0..3 {
        revoke_restored_devices(&mut history);
        assert_eq!(
            serde_json::to_value(&history).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        history = serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_restart_preserves_activity_through_snapshot_and_heartbeat() {
    let db = Database::new();
    let state = db.state();
    let rt = runtime();
    let history = activity_history_fixture();
    let active_id = history
        .values()
        .find(|d| d.status == "active")
        .unwrap()
        .device_id;
    persist_devices(&state, &history).unwrap();
    let original_document = db.store.get_document("devices").unwrap().unwrap();
    let mut expected = history.clone();
    revoke_restored_devices(&mut expected);
    for _ in 0..3 {
        load_device_registry(&state).unwrap();
        assert_eq!(
            serde_json::to_value(&*state.devices.read().unwrap()).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        // A late heartbeat cannot reactivate a restored authorization.
        assert_eq!(
            rt.block_on(request(
                &state,
                "POST",
                &format!("/v1/devices/{active_id}/heartbeat"),
                Some("user-session"),
                Value::Null
            ))
            .0,
            StatusCode::NOT_FOUND
        );
        persist_devices(&state, &state.devices.read().unwrap()).unwrap();
    }
    let stored: HashMap<Uuid, Device> =
        serde_json::from_str(&db.store.get_document("devices").unwrap().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&stored).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let before: HashMap<Uuid, Device> = serde_json::from_str(&original_document).unwrap();
    for (id, device) in &stored {
        assert_eq!(device.last_seen_at, before[id].last_seen_at);
    }
    // A newly authorized device still records and persists genuine activity.
    let started = Utc::now();
    {
        let mut devices = state.devices.write().unwrap();
        let device = devices.get_mut(&active_id).unwrap();
        device.status = "active".into();
        device.last_seen_at = started;
    }
    assert_eq!(
        rt.block_on(request(
            &state,
            "POST",
            &format!("/v1/devices/{active_id}/heartbeat"),
            Some("user-session"),
            Value::Null
        ))
        .0,
        StatusCode::NO_CONTENT
    );
    let stored: HashMap<Uuid, Device> =
        serde_json::from_str(&db.store.get_document("devices").unwrap().unwrap()).unwrap();
    assert!(stored[&active_id].last_seen_at >= started);
    for (id, device) in &stored {
        if *id != active_id {
            assert_eq!(device.last_seen_at, before[id].last_seen_at);
        }
    }
}

fn seed(db: &Database, state: &AppState) -> (Uuid, Uuid) {
    let id = Uuid::new_v4();
    let other = Uuid::new_v4();
    for (device_id, user_id, username, suffix) in [
        (id, "deleted-user-id", "u1", 10),
        (other, "admin", "admin", 11),
    ] {
        state.devices.write().unwrap().insert(
            device_id,
            Device {
                device_id,
                user_id: user_id.into(),
                username: username.into(),
                public_key: format!("key-{suffix}"),
                node_id: "node-a".into(),
                ipv4: format!("10.88.0.{suffix}"),
                ipv6: format!("fd88:1000::{suffix:x}"),
                ipv4_mode: "internal".into(),
                ipv6_mode: "internal".into(),
                status: "active".into(),
                config_version: 7,
                last_seen_at: Utc::now(),
                traffic: Default::default(),
                session_traffic: Default::default(),
            },
        );
    }
    persist_devices(state, &state.devices.read().unwrap()).unwrap();
    state
        .nodes
        .write()
        .unwrap()
        .get_mut("node-a")
        .unwrap()
        .registered = true;
    let mut client = db.client();
    client.batch_execute("INSERT INTO invitation_policies(user_id,payload) VALUES('deleted-user-id','{}'),('admin','{}');
        INSERT INTO invitations(id,code_hash,prefix,inviter_user_id,inviter_username,creator_username,source,kind,use_count)
        VALUES('unused','hash-unused','prefix','deleted-user-id','u1','u1','user','single',0),
        ('used','hash-used','prefix','deleted-user-id','u1','u1','user','single',1),
        ('unlimited','hash-unlimited','prefix','deleted-user-id','u1','u1','admin','unlimited',2),
        ('other','hash-other','prefix','admin','admin','admin','admin','single',0);
        INSERT INTO invitation_registrations(id,invitation_id,user_id,username,inviter_user_id,inviter_username,grant_payload,policy_revision)
        VALUES('reg-target','other','deleted-user-id','u1','admin','admin','{}',1),
        ('reg-invitee','used','invitee-id','invitee','deleted-user-id','u1','{}',3);
        INSERT INTO user_forward_policies(user_id,enabled,max_active_forwards,allowed_entry_ids,allowed_protocols,allowed_families,updated_at)
        VALUES('deleted-user-id',true,5,'[]','[]','[]',CURRENT_TIMESTAMP),('admin',true,5,'[]','[]','[]',CURRENT_TIMESTAMP);
        INSERT INTO forward_entries(entry_id,node_id,name,public_address,family,protocols,enabled) VALUES('entry','node-a','entry','203.0.113.1','ipv4','[\"tcp\"]',true);
        INSERT INTO pmman_audit_events(actor,event_type) VALUES('u1','historic-event');").unwrap();
    let policy =
        serde_json::to_string(&pmman_protocol::invitations::InvitationPolicy::default()).unwrap();
    client
        .execute("UPDATE invitation_policies SET payload=$1", &[&policy])
        .unwrap();
    for (forward, user_id, username, device, port, status) in [
        ("live", "deleted-user-id", "u1", id, 20001, "active"),
        ("grace", "deleted-user-id", "u1", id, 20002, "error"),
        ("pending", "deleted-user-id", "u1", id, 20003, "pending"),
        ("releasing", "deleted-user-id", "u1", id, 20004, "releasing"),
        ("released", "deleted-user-id", "u1", id, 20005, "released"),
        ("other", "admin", "admin", other, 20006, "active"),
    ] {
        client.execute("INSERT INTO port_forward_allocations(forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at) VALUES($1,$2,$3,$4,'entry','node-a','[\"tcp\"]',$5,'10.88.0.10', $5,$6,7,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)", &[&forward,&user_id,&username,&device,&(port as i32),&status]).unwrap();
    }
    (id, other)
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_delete_cleans_resources_preserves_history_and_node_reconciles() {
    let db = Database::new();
    let state = db.state();
    let rt = runtime();
    let (id, other) = seed(&db, &state);
    let admin = serde_json::to_value(&state.users.read().unwrap()["admin"]).unwrap();
    let other_device = serde_json::to_value(&state.devices.read().unwrap()[&other]).unwrap();
    let before_history: Vec<_> = db
        .client()
        .query(
            "SELECT row_to_json(r)::text FROM invitation_registrations r ORDER BY id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect();
    let result = rt.block_on(delete(&state, "u1", Some("admin-session")));
    assert_eq!(result, (StatusCode::NO_CONTENT, Value::Null));
    assert!(!state.users.read().unwrap().contains_key("u1"));
    assert_eq!(
        serde_json::to_value(&state.users.read().unwrap()["admin"]).unwrap(),
        admin
    );
    assert_eq!(
        serde_json::to_value(&state.devices.read().unwrap()[&other]).unwrap(),
        other_device
    );
    assert_eq!(state.devices.read().unwrap()[&id].status, "revoked");
    assert_eq!(pool_settings(&state).allocated, 2);
    assert!(!state.sessions.read().unwrap().contains_key("user-session"));
    assert!(state.sessions.read().unwrap().contains_key("admin-session"));
    assert!(
        serde_json::from_str::<Value>(&db.store.get_document("users").unwrap().unwrap())
            .unwrap()
            .get("u1")
            .is_none()
    );
    assert_eq!(
        db.count("SELECT count(*) FROM user_forward_policies WHERE user_id='deleted-user-id'"),
        0
    );
    assert_eq!(
        db.count("SELECT count(*) FROM invitation_policies WHERE user_id='deleted-user-id'"),
        0
    );
    assert_eq!(
        db.count("SELECT count(*) FROM user_forward_policies WHERE user_id='admin'"),
        1
    );
    assert_eq!(db.count("SELECT count(*) FROM invitations WHERE id IN ('unused','unlimited') AND revoked_at IS NOT NULL"), 2);
    assert_eq!(
        db.count(
            "SELECT count(*) FROM invitations WHERE id IN ('used','other') AND revoked_at IS NULL"
        ),
        2
    );
    let after_history: Vec<_> = db
        .client()
        .query(
            "SELECT row_to_json(r)::text FROM invitation_registrations r ORDER BY id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect();
    assert_eq!(before_history, after_history);
    assert_eq!(
        db.count("SELECT count(*) FROM pmman_audit_events WHERE event_type='historic-event'"),
        1
    );
    assert_eq!(db.count("SELECT count(*) FROM pmman_audit_events WHERE event_type='user.deleted' AND actor='admin' AND resource_id='deleted-user-id'"), 1);
    assert_eq!(db.count("SELECT count(*) FROM port_forward_allocations WHERE user_id='deleted-user-id' AND state='releasing'"), 4);
    assert_eq!(db.count("SELECT count(*) FROM port_forward_allocations WHERE forward_id='other' AND state='active'"), 1);
    assert_eq!(
        db.count("SELECT count(*) FROM port_forward_events WHERE event_type='user.deleted'"),
        3
    );
    assert!(state
        .port_forward_store
        .set_state("live", "active", None, None)
        .is_err());
    let heartbeat = |body: Value| {
        rt.block_on(async {
            let response = app(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/nodes/node-a/heartbeat")
                        .header("x-pmman-node-token", "node-secret")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice::<Value>(
                &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                    .await
                    .unwrap(),
            )
            .unwrap()
        })
    };
    let desired = heartbeat(json!({}));
    assert!(desired["revoked"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["device_id"] == id.to_string()));
    assert!(!desired["active"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["device_id"] == id.to_string()));
    assert!(!desired["port_forwards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["forward_id"] == "live"));
    assert!(desired["revoked_forward_ids"]
        .as_array()
        .unwrap()
        .contains(&json!("live")));
    heartbeat(json!({"released_forward_ids":["live","grace","pending","releasing"]}));
    assert_eq!(db.count("SELECT count(*) FROM port_forward_allocations WHERE user_id='deleted-user-id' AND state='released'"), 5);
    assert_eq!(
        rt.block_on(delete(&state, "u1", Some("admin-session"))).0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        rt.block_on(request(
            &state,
            "GET",
            "/v1/client/nodes",
            Some("user-session"),
            Value::Null
        ))
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        rt.block_on(request(
            &state,
            "POST",
            "/v1/auth/login",
            None,
            json!({"username":"u1","password":"userpw"})
        ))
        .0,
        StatusCode::UNAUTHORIZED
    );
    load_user_registry(&state).unwrap();
    load_device_registry(&state).unwrap();
    assert!(!state.users.read().unwrap().contains_key("u1"));
    assert_eq!(state.devices.read().unwrap()[&id].status, "revoked");
}

#[test]
fn late_p2p_publication_revalidates_user_and_active_device_key() {
    let state = state();
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer user-session".parse().unwrap());
    let principal = authenticate(&state, &headers).unwrap();
    let id = Uuid::new_v4();
    let original: Device = serde_json::from_value(json!({
        "device_id":id,"user_id":"u1","username":"u1","public_key":"original-key","node_id":"node-a",
        "ipv4":"10.88.0.10","ipv6":"fd88:1000::a","ipv4_mode":"internal","ipv6_mode":"internal",
        "status":"active","config_version":1,"last_seen_at":Utc::now()
    })).unwrap();
    let check = |devices: &HashMap<Uuid, Device>, users: &HashMap<String, UserAccount>| {
        p2p::revalidate_advertiser(devices, users, &principal, id, "original-key")
    };
    let mut devices = HashMap::from([(id, original.clone())]);
    let mut users = state.users.read().unwrap().clone();
    assert!(check(&devices, &users).is_ok());
    for (field, value) in [
        ("status", "revoked"),
        ("public_key", "rotated"),
        ("user_id", "other-user"),
    ] {
        let mut device = serde_json::to_value(&original).unwrap();
        device[field] = json!(value);
        devices.insert(id, serde_json::from_value(device).unwrap());
        assert_eq!(check(&devices, &users), Err(StatusCode::NOT_FOUND));
    }
    devices.insert(id, original);
    users.get_mut("u1").unwrap().user_id = "new-same-name-user".into();
    assert_eq!(check(&devices, &users), Err(StatusCode::UNAUTHORIZED));
    users.remove("u1");
    assert_eq!(check(&devices, &users), Err(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn admin_revoke_of_released_history_is_idempotent_without_storage_or_pool_changes() {
    let state = state();
    let id = Uuid::new_v4();
    let history: Device = serde_json::from_value(json!({
        "device_id":id,"user_id":"old-deleted-user","username":"u1","public_key":"old-key","node_id":"node-a",
        "ipv4":"10.88.0.10","ipv6":"fd88:1000::a","ipv4_mode":"internal","ipv6_mode":"internal",
        "status":"released","config_version":1,"last_seen_at":Utc::now()
    })).unwrap();
    state.devices.write().unwrap().insert(id, history);
    let before = serde_json::to_value(&*state.devices.read().unwrap()).unwrap();
    let assigned = config(&state.devices.read().unwrap()[&id], &state.gateway);
    revoke_failed_connection(&state, &assigned, "old-key").unwrap();
    assert_eq!(
        serde_json::to_value(&*state.devices.read().unwrap()).unwrap(),
        before
    );
    for _ in 0..2 {
        let response = request(
            &state,
            "POST",
            &format!("/v1/admin/devices/{id}/revoke"),
            Some("admin-session"),
            Value::Null,
        )
        .await;
        assert_eq!(response.0, StatusCode::NO_CONTENT);
        assert_eq!(
            serde_json::to_value(&*state.devices.read().unwrap()).unwrap(),
            before
        );
        assert_eq!(pool_settings(&state).allocated, 0);
    }
}

#[test]
fn revoked_device_cross_node_switch_waits_for_old_cleanup_but_same_node_key_rotation_works() {
    let state = state();
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer user-session".parse().unwrap());
    let principal = authenticate(&state, &headers).unwrap();
    let connect = |node: &str, key: &str| {
        register_device(
            &state,
            principal.clone(),
            RegisterRequest {
                public_key: key.into(),
                ipv4_mode: "internal".into(),
                ipv6_mode: "internal".into(),
                split_route_source: None,
            },
            node,
            &state.gateway,
            false,
        )
    };
    let assigned = connect("node-a", "old-key").ok().unwrap().0;
    revoke_failed_connection(&state, &assigned, "old-key").unwrap();
    let before = serde_json::to_value(&*state.devices.read().unwrap()).unwrap();
    assert!(matches!(
        connect("node-b", "new-key"),
        Err((StatusCode::CONFLICT, _))
    ));
    assert_eq!(
        serde_json::to_value(&*state.devices.read().unwrap()).unwrap(),
        before
    );
    // A stale cleanup report and a fresh report still containing the old key
    // must both leave the old ownership record intact.
    {
        let mut nodes = state.nodes.write().unwrap();
        let node = nodes.get_mut("node-a").unwrap();
        node.cleanup_ok = true;
        node.last_seen_at = DateTime::UNIX_EPOCH;
    }
    assert!(matches!(
        connect("node-b", "new-key"),
        Err((StatusCode::CONFLICT, _))
    ));
    {
        let mut nodes = state.nodes.write().unwrap();
        let node = nodes.get_mut("node-a").unwrap();
        node.last_seen_at = Utc::now();
        node.applied = vec![AppliedNodePeer {
            device_id: assigned.device_id,
            public_key: "old-key".into(),
            config_version: assigned.config_version,
        }];
    }
    assert!(matches!(
        connect("node-b", "new-key"),
        Err((StatusCode::CONFLICT, _))
    ));
    // Same-node rotation preserves the existing node-local cleanup protocol.
    let rotated = connect("node-a", "rotated-key")
        .ok()
        .expect("same-node rotation")
        .0;
    revoke_failed_connection(&state, &rotated, "rotated-key").unwrap();
    {
        let mut nodes = state.nodes.write().unwrap();
        let node = nodes.get_mut("node-a").unwrap();
        node.last_seen_at = Utc::now();
        node.applied.clear();
    }
    let moved = connect("node-b", "new-key")
        .ok()
        .expect("fresh old-node cleanup allows switch")
        .0;
    assert_eq!(moved.ipv4, assigned.ipv4);
    assert_eq!(
        state.devices.read().unwrap()[&moved.device_id].node_id,
        "node-b"
    );
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_legacy_gateway_cleanup_is_after_commit_quarantined_on_failure_and_retryable() {
    let db = Database::new();
    let mut state = db.state();
    let rt = runtime();
    let (id, _) = seed(&db, &state);
    state.gateway.interface = Some("test-legacy".into());
    state.gateway.wg_binary = "pmman-deletion-test-no-such-wg".into();
    state.devices.write().unwrap().get_mut(&id).unwrap().node_id = "legacy".into();
    persist_devices(&state, &state.devices.read().unwrap()).unwrap();
    assert_eq!(
        rt.block_on(delete(&state, "u1", Some("admin-session"))).0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(state.devices.read().unwrap()[&id].status, "revoked");
    assert_eq!(pool_settings(&state).allocated, 2);
    let mut attempts = 0;
    let failure = retry_legacy_cleanup_with(&state, |interface, key| {
        assert_eq!(interface, "test-legacy");
        assert_eq!(key, "key-10");
        attempts += 1;
        let users: Value =
            serde_json::from_str(&db.store.get_document("users").unwrap().unwrap()).unwrap();
        let devices: Value =
            serde_json::from_str(&db.store.get_document("devices").unwrap().unwrap()).unwrap();
        assert!(users.get("u1").is_none());
        assert_eq!(devices[id.to_string()]["status"], "revoked");
        Err("injected gateway removal failure".into())
    });
    assert!(failure.is_err());
    assert_eq!(attempts, 1);
    load_device_registry(&state).unwrap();
    assert_eq!(state.devices.read().unwrap()[&id].status, "revoked");
    for forward in ["live", "grace", "pending", "releasing"] {
        state.port_forward_store.confirm_release(forward).unwrap();
    }
    retry_legacy_cleanup_with(&state, |_, _| {
        attempts += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(state.devices.read().unwrap()[&id].status, "released");
    assert_eq!(pool_settings(&state).allocated, 1);
    load_device_registry(&state).unwrap();
    assert_eq!(state.devices.read().unwrap()[&id].status, "released");
    retry_legacy_cleanup_with(&state, |_, _| {
        panic!("terminal history must not be retried")
    })
    .unwrap();
    // No configured gateway still needs later retries when forward cleanup is
    // the remaining quarantine condition. Never invoke a CLI in this case.
    state.gateway.interface = None;
    state.devices.write().unwrap().get_mut(&id).unwrap().status = "revoked".into();
    persist_devices(&state, &state.devices.read().unwrap()).unwrap();
    db.execute("UPDATE port_forward_allocations SET state='releasing' WHERE forward_id='live'");
    retry_legacy_cleanup_with(&state, |_, _| panic!("no configured interface")).unwrap();
    assert_eq!(state.devices.read().unwrap()[&id].status, "revoked");
    state.port_forward_store.confirm_release("live").unwrap();
    retry_legacy_cleanup_with(&state, |_, _| panic!("no configured interface")).unwrap();
    assert_eq!(state.devices.read().unwrap()[&id].status, "released");
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_address_is_quarantined_until_peer_and_forward_ack_then_reusable_after_restart() {
    let db = Database::new();
    let state = db.state();
    let rt = runtime();
    let (id, _) = seed(&db, &state);
    let before = pool_settings(&state);
    assert_eq!(before.allocated, 2);
    assert_eq!(
        rt.block_on(delete(&state, "u1", Some("admin-session"))).0,
        StatusCode::NO_CONTENT
    );
    let occupancy = || {
        let devices = state.devices.read().unwrap();
        let users = state.users.read().unwrap();
        address_occupancy(&users, &devices, None)
    };
    assert!(occupancy()
        .0
        .contains(&u32::from(Ipv4Addr::new(10, 88, 0, 10))));
    let clean = NodeHeartbeatRequest {
        cleanup_ok: true,
        ..Default::default()
    };
    confirm_device_cleanup(&state, "node-a", &clean).unwrap();
    assert_eq!(
        state.devices.read().unwrap()[&id].status,
        "revoked",
        "forward rules still pending"
    );
    for forward in ["live", "grace", "pending", "releasing"] {
        state.port_forward_store.confirm_release(forward).unwrap();
    }
    for report in [
        NodeHeartbeatRequest::default(),
        NodeHeartbeatRequest {
            cleanup_ok: true,
            applied: vec![AppliedNodePeer {
                device_id: id,
                public_key: "key-10".into(),
                config_version: 7,
            }],
            ..Default::default()
        },
        NodeHeartbeatRequest {
            cleanup_ok: true,
            errors: vec![NodeSyncError {
                device_id: id,
                public_key: "key-10".into(),
                config_version: 7,
                error: "cleanup pending".into(),
            }],
            ..Default::default()
        },
    ] {
        confirm_device_cleanup(&state, "node-a", &report).unwrap();
        assert_eq!(pool_settings(&state).allocated, before.allocated);
    }
    confirm_device_cleanup(&state, "other-node", &clean).unwrap();
    assert_eq!(pool_settings(&state).allocated, before.allocated);
    // A failed cleanup acknowledgment persistence cannot free an in-memory
    // address or publish a tombstone that disappears on restart.
    db.execute("CREATE FUNCTION reject_cleanup_test_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected cleanup failure'; END $$; CREATE TRIGGER fail_cleanup BEFORE UPDATE ON pmman_documents FOR EACH ROW EXECUTE FUNCTION reject_cleanup_test_write()");
    assert!(confirm_device_cleanup(&state, "node-a", &clean).is_err());
    assert_eq!(pool_settings(&state).allocated, before.allocated);
    db.execute("DROP TRIGGER fail_cleanup ON pmman_documents");
    // Exercise the actual heartbeat acknowledgment path, not just the helper.
    let response = rt.block_on(async {
        app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/nodes/node-a/heartbeat")
                    .header("x-pmman-node-token", "node-secret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"cleanup_ok":true,"applied":[]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(state.devices.read().unwrap()[&id].status, "released");
    let after = pool_settings(&state);
    assert_eq!(after.capacity, before.capacity);
    assert_eq!(after.allocated, before.allocated - 1);
    assert_eq!(after.available, before.available + 1);
    assert!(!occupancy()
        .0
        .contains(&u32::from(Ipv4Addr::new(10, 88, 0, 10))));
    load_device_registry(&state).unwrap();
    assert_eq!(state.devices.read().unwrap()[&id].status, "released");
    assert_eq!(pool_settings(&state).allocated, after.allocated);
    assert_eq!(
        rt.block_on(request(
            &state,
            "POST",
            "/v1/admin/users",
            Some("admin-session"),
            json!({"username":"replacement","password":"replacementpw"})
        ))
        .0,
        StatusCode::OK
    );
    let user = state.users.read().unwrap()["replacement"].clone();
    let principal = Principal {
        user_id: user.user_id,
        username: user.username,
        role: user.role,
        allow_ipv4_global: false,
        allow_ipv6_global: false,
        allow_ipv4_split: false,
        allow_ipv6_split: false,
        must_change_password: true,
    };
    let assigned = register_device(
        &state,
        principal,
        RegisterRequest {
            public_key: "new-key".into(),
            ipv4_mode: "internal".into(),
            ipv6_mode: "internal".into(),
            split_route_source: None,
        },
        "node-a",
        &state.gateway,
        false,
    )
    .ok()
    .expect("released address must be reusable")
    .0;
    assert_eq!(assigned.ipv4, "10.88.0.10/32");
    assert_eq!(assigned.ipv6, "fd88:1000::a/128");
    assert_eq!(pool_settings(&state).allocated, before.allocated);
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_migration_preserves_v3_history_and_allows_public_same_name_reregistration() {
    let db = Database::new();
    let state = db.state();
    let rt = runtime();
    seed(&db, &state);
    db.execute("ALTER TABLE invitation_registrations ADD CONSTRAINT invitation_registrations_username_key UNIQUE(username); DROP INDEX invitation_registrations_username; DELETE FROM schema_migrations WHERE version=4");
    let old_users = db.store.get_document("users").unwrap();
    let history: Vec<String> = db
        .client()
        .query(
            "SELECT row_to_json(r)::text FROM invitation_registrations r ORDER BY id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    db.store.migrate().unwrap();
    db.store.migrate().unwrap();
    assert_eq!(old_users, db.store.get_document("users").unwrap());
    let migrated: Vec<String> = db
        .client()
        .query(
            "SELECT row_to_json(r)::text FROM invitation_registrations r ORDER BY id",
            &[],
        )
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(history, migrated);
    assert_eq!(
        db.count("SELECT count(*) FROM schema_migrations WHERE version=4"),
        1
    );
    assert_eq!(db.count("SELECT count(*) FROM pg_constraint WHERE conrelid='invitation_registrations'::regclass AND conname='invitation_registrations_username_key'"),0);
    let invite = rt.block_on(request(&state,"POST","/v1/admin/invitations",Some("admin-session"),json!({"kind":"single","note":"same name regression","grant":pmman_protocol::invitations::InvitationGrant::default()})));
    assert_eq!(invite.0, StatusCode::OK);
    let code = invite.1["code"].as_str().expect("created invitation code");
    assert_eq!(
        rt.block_on(delete(&state, "u1", Some("admin-session"))).0,
        StatusCode::NO_CONTENT
    );
    let register = rt.block_on(request(
        &state,
        "POST",
        "/v1/auth/register",
        None,
        json!({"username":"u1","password":"reregisteredpw","code":code}),
    ));
    assert_eq!(register.0, StatusCode::CREATED);
    let new_id = state.users.read().unwrap()["u1"].user_id.clone();
    assert_ne!(new_id, "deleted-user-id");
    assert_eq!(
        db.count("SELECT count(*) FROM invitation_registrations WHERE username='u1'"),
        2
    );
    assert_eq!(db.count("SELECT count(*) FROM invitation_registrations WHERE username='u1' AND user_id='deleted-user-id'"),1);
    assert_eq!(
        db.count("SELECT count(*) FROM invitation_policies WHERE user_id='deleted-user-id'"),
        0
    );
    assert!(state
        .port_forward_store
        .allocations(Some(&new_id))
        .unwrap()
        .is_empty());
    let result = rt.block_on(request(
        &state,
        "POST",
        "/v1/auth/login",
        None,
        json!({"username":"u1","password":"reregisteredpw"}),
    ));
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["user_id"], new_id);
    assert_eq!(
        rt.block_on(request(
            &state,
            "GET",
            "/v1/client/nodes",
            Some("user-session"),
            Value::Null
        ))
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_failure_at_each_write_rolls_back_without_memory_changes() {
    let db = Database::new();
    let state = db.state();
    let rt = runtime();
    seed(&db, &state);
    db.execute("CREATE FUNCTION reject_delete_test_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected deletion test failure'; END $$;");
    let users = serde_json::to_value(&*state.users.read().unwrap()).unwrap();
    let devices = serde_json::to_value(&*state.devices.read().unwrap()).unwrap();
    let users_doc = db.store.get_document("users").unwrap();
    let devices_doc = db.store.get_document("devices").unwrap();
    for table in [
        "pmman_documents",
        "port_forward_allocations",
        "port_forward_events",
        "user_forward_policies",
        "invitations",
        "invitation_policies",
        "pmman_audit_events",
    ] {
        db.execute(&format!("CREATE TRIGGER fail_delete BEFORE INSERT OR UPDATE OR DELETE ON {table} FOR EACH ROW EXECUTE FUNCTION reject_delete_test_write()"));
        let result = rt.block_on(delete(&state, "u1", Some("admin-session")));
        assert_eq!(
            result.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "failure boundary: {table}"
        );
        assert_eq!(result.1["error"], "账户删除存储暂不可用，请稍后重试");
        assert_eq!(
            serde_json::to_value(&*state.users.read().unwrap()).unwrap(),
            users
        );
        assert_eq!(
            serde_json::to_value(&*state.devices.read().unwrap()).unwrap(),
            devices
        );
        assert!(state.sessions.read().unwrap().contains_key("user-session"));
        assert_eq!(db.store.get_document("users").unwrap(), users_doc);
        assert_eq!(db.store.get_document("devices").unwrap(), devices_doc);
        assert_eq!(
            db.count("SELECT count(*) FROM invitations WHERE revoked_at IS NOT NULL"),
            0
        );
        assert_eq!(db.count("SELECT count(*) FROM invitation_registrations"), 2);
        assert_eq!(db.count("SELECT count(*) FROM invitation_policies"), 2);
        assert_eq!(db.count("SELECT count(*) FROM user_forward_policies"), 2);
        assert_eq!(
            db.count("SELECT count(*) FROM port_forward_allocations WHERE state='releasing'"),
            1
        );
        assert_eq!(
            db.count("SELECT count(*) FROM pmman_audit_events WHERE event_type='user.deleted'"),
            0
        );
        assert_eq!(db.count("SELECT count(*) FROM port_forward_events"), 0);
        db.execute(&format!("DROP TRIGGER fail_delete ON {table}"));
    }
}

#[test]
#[ignore = "requires dedicated local PostgreSQL"]
fn postgres_login_paused_after_verification_cannot_outlive_delete_or_same_name_recreation() {
    let db = Database::new();
    let state = db.state();
    let rt = runtime();
    seed(&db, &state);
    let verified = state.users.read().unwrap()["u1"].clone();
    assert!(verify_password(&verified.password_hash, "userpw").0);
    let resume = Arc::new(std::sync::Barrier::new(2));
    let worker_state = state.clone();
    let worker_resume = resume.clone();
    let login = std::thread::spawn(move || {
        worker_resume.wait();
        matches!(
            issue_login_session(&worker_state, &verified),
            Err(StatusCode::UNAUTHORIZED)
        )
    });
    assert_eq!(
        rt.block_on(delete(&state, "u1", Some("admin-session"))).0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        rt.block_on(request(
            &state,
            "POST",
            "/v1/admin/users",
            Some("admin-session"),
            json!({"username":"u1","password":"newpassword"})
        ))
        .0,
        StatusCode::OK
    );
    resume.wait();
    assert!(login.join().unwrap());
    let new_id = state.users.read().unwrap()["u1"].user_id.clone();
    assert_ne!(new_id, "deleted-user-id");
    assert!(state
        .sessions
        .read()
        .unwrap()
        .values()
        .all(|s| s.user_id != "deleted-user-id"));
    assert!(!state.port_forward_store.policy(&new_id).unwrap().enabled);
    assert!(state
        .port_forward_store
        .allocations(Some(&new_id))
        .unwrap()
        .is_empty());
    assert_eq!(db.count("SELECT count(*) FROM invitation_registrations WHERE username='u1' AND user_id='deleted-user-id'"), 1);
    assert_eq!(db.count("SELECT count(*) FROM invitations WHERE inviter_user_id='deleted-user-id' AND revoked_at IS NOT NULL"), 2);
    let login = rt.block_on(request(
        &state,
        "POST",
        "/v1/auth/login",
        None,
        json!({"username":"u1","password":"newpassword"}),
    ));
    assert_eq!(login.0, StatusCode::OK);
    assert_eq!(login.1["user_id"], new_id);
    assert_eq!(
        rt.block_on(request(
            &state,
            "GET",
            "/v1/client/nodes",
            Some("user-session"),
            Value::Null
        ))
        .0,
        StatusCode::UNAUTHORIZED
    );
    // The historical name is searchable but not a uniqueness/ownership key.
    db.client().execute("INSERT INTO invitation_registrations(id,invitation_id,user_id,username,inviter_user_id,inviter_username,grant_payload,policy_revision) VALUES('reg-new','other',$1,'u1','admin','admin','{}',1)", &[&new_id]).unwrap();
    assert_eq!(
        db.count("SELECT count(*) FROM invitation_registrations WHERE username='u1'"),
        2
    );
}
