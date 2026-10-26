//! Run only against a dedicated local database with --ignored.
use super::*;
use axum::http::Request as HttpRequest;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Database {
    root: String,
    schema: String,
    store: db::PgStore,
    url: String,
}
impl Database {
    fn new() -> Self {
        let root = env::var("PMMAN_INVITATION_TEST_DATABASE_URL")
            .expect("set PMMAN_INVITATION_TEST_DATABASE_URL to a dedicated test database");
        assert!(
            root.contains("pmman_invitation_test"),
            "refusing non-test database"
        );
        let schema = format!("invitation_test_{}", Uuid::new_v4().simple());
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
        let mut state = crate::tests::test_state();
        state.database = Some(Arc::new(self.store.clone()));
        state.port_forward_store = port_forward::PortForwardStore::open(&self.url).unwrap();
        persist_users(&state, &state.users.read().unwrap()).unwrap();
        let mut sessions = state.sessions.write().unwrap();
        for (token, user) in [("test-admin", "admin"), ("test-user", "u1")] {
            sessions.insert(
                token.into(),
                Session {
                    user_id: user.into(),
                    username: user.into(),
                    expires_at: Utc::now() + Duration::hours(1),
                },
            );
        }
        drop(sessions);
        state
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
            self.schema.starts_with("invitation_test_")
                && self
                    .schema
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        let _ = postgres::Client::connect(&self.root, postgres::NoTls)
            .and_then(|mut c| c.batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema)));
    }
}
fn call(
    runtime: &tokio::runtime::Runtime,
    state: &AppState,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value, HeaderMap) {
    runtime.block_on(async {
        let mut request = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header("Content-Type", "application/json");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let bytes = if method == "GET" {
            String::new()
        } else {
            body.to_string()
        };
        let response = crate::app(state.clone())
            .oneshot(request.body(Body::from(bytes)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            headers,
        )
    })
}
fn invite(runtime: &tokio::runtime::Runtime, state: &AppState, kind: &str) -> CreatedInvitation {
    let (s, b, h) = call(
        runtime,
        state,
        "POST",
        "/v1/admin/invitations",
        Some("test-admin"),
        json!({"kind":kind,"note":"integration","expires_at":null,"grant":InvitationGrant::default()}),
    );
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(h[header::CACHE_CONTROL], "no-store");
    serde_json::from_value(b).unwrap()
}

#[test]
#[ignore = "requires dedicated PostgreSQL; see docs/INVITATIONS.md"]
fn postgres_invitation_registration_passwords_survive_login_and_reload() {
    let database = Database::new();
    let state = database.state();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let invitation = invite(&runtime, &state, "unlimited");
    let passwords = [
        "test-registration-password".to_owned(),
        "  registration password  ".to_owned(),
        " 受邀密码🔐测试 ".to_owned(),
        "p".repeat(256),
    ];

    for (index, password) in passwords.iter().enumerate() {
        let username = format!("Invited.Password-{index}");
        let registered = call(
            &runtime,
            &state,
            "POST",
            "/v1/auth/register",
            None,
            json!({"username":username,"password":password,"code":invitation.code}),
        );
        assert_eq!(registered.0, StatusCode::CREATED);
        assert_eq!(registered.1, json!({"username":username}));
        assert_eq!(registered.2[header::CACHE_CONTROL], "no-store");
        assert!(state
            .sessions
            .read()
            .unwrap()
            .values()
            .all(|session| session.username != username));
        let original_hash = state.users.read().unwrap()[&username].password_hash.clone();
        let original_user_id = state.users.read().unwrap()[&username].user_id.clone();
        assert_ne!(original_user_id, username);
        assert!(Uuid::parse_str(&original_user_id).is_ok());
        assert!(original_hash.starts_with(&format!("{PASSWORD_HASH_PREFIX}$")));
        assert!(verify_password(&original_hash, password).0);

        // Exercise the actual login handler both immediately and after a fresh
        // database load. Neither path may require an administrator password reset.
        for reload in [false, true] {
            if reload {
                state.users.write().unwrap().clear();
                state.sessions.write().unwrap().clear();
                load_user_registry(&state).unwrap();
            }
            let login = call(
                &runtime,
                &state,
                "POST",
                "/v1/auth/login",
                None,
                json!({"username":username,"password":password}),
            );
            assert_eq!(login.0, StatusCode::OK);
            assert_eq!(login.1["username"], username);
            assert_eq!(login.1["user_id"], original_user_id);
            assert_eq!(login.1["role"], "user");
            assert_eq!(login.1["must_change_password"], false);
            assert!(state.users.read().unwrap()[&username].password_hash == original_hash);

            let token = login.1["token"].as_str().unwrap();
            assert_eq!(
                call(
                    &runtime,
                    &state,
                    "GET",
                    "/v1/client/invitations/policy",
                    Some(token),
                    Value::Null,
                )
                .0,
                StatusCode::OK
            );
            // Login keys are case-sensitive usernames, while bearer sessions
            // carry the separate UUID. Do not conflate either lookup key.
            for wrong_username in [username.to_ascii_lowercase(), original_user_id.clone()] {
                assert_eq!(
                    call(
                        &runtime,
                        &state,
                        "POST",
                        "/v1/auth/login",
                        None,
                        json!({"username":wrong_username,"password":password}),
                    )
                    .0,
                    StatusCode::UNAUTHORIZED
                );
            }
            // Whitespace is part of a password. A trimmed version must not log in.
            let wrong_password = if password.trim() != password {
                password.trim()
            } else {
                "incorrect-registration-password"
            };
            assert_eq!(
                call(
                    &runtime,
                    &state,
                    "POST",
                    "/v1/auth/login",
                    None,
                    json!({"username":username,"password":wrong_password}),
                )
                .0,
                StatusCode::UNAUTHORIZED
            );
        }
    }
    // Publishing a newly registered account must retain existing credentials.
    for (username, password) in [("admin", "adminpw"), ("u1", "userpw")] {
        assert_eq!(
            call(
                &runtime,
                &state,
                "POST",
                "/v1/auth/login",
                None,
                json!({"username":username,"password":password}),
            )
            .0,
            StatusCode::OK
        );
    }
}

#[test]
#[ignore = "requires dedicated PostgreSQL; see docs/INVITATIONS.md"]
fn postgres_registration_lifecycle_concurrency_and_rollback() {
    let database = Database::new();
    let state = database.state();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let password = hash_password("test-registration-password");
    // Initial passwords do not grant invitation privileges, or block users
    // whom the administrator explicitly authorizes to invite.
    state
        .users
        .write()
        .unwrap()
        .get_mut("u1")
        .unwrap()
        .must_change_password = true;
    persist_users(&state, &state.users.read().unwrap()).unwrap();
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/client/invitations",
            Some("test-user"),
            json!({})
        )
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "GET",
            "/v1/admin/invitations",
            Some("test-user"),
            Value::Null
        )
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "GET",
            "/v1/admin/invitations",
            None,
            Value::Null
        )
        .0,
        StatusCode::UNAUTHORIZED
    );
    let p = InvitationPolicy {
        enabled: true,
        max_unused: 2,
        daily_limit: 3,
        valid_days: 7,
        ..Default::default()
    };
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(p)
        )
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "GET",
            "/v1/client/invitations/policy",
            Some("test-user"),
            Value::Null,
        )
        .0,
        StatusCode::OK
    );
    let (s, b, _) = call(
        &runtime,
        &state,
        "POST",
        "/v1/client/invitations",
        Some("test-user"),
        json!({}),
    );
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(state.users.read().unwrap()["u1"].must_change_password);
    let user_invite: CreatedInvitation = serde_json::from_value(b).unwrap();
    let mut changed = p.clone();
    changed.grant.allow_ipv4_split = true;
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(changed)
        )
        .0,
        StatusCode::OK
    );
    register_account(
        &state,
        &database.store,
        "invited-live".into(),
        user_invite.code.clone(),
        password.clone(),
    )
    .unwrap();
    {
        let users = state.users.read().unwrap();
        let u = &users["invited-live"];
        assert!(u.allow_ipv4_split);
        assert!(!u.must_change_password);
        assert_eq!(u.role, "user");
        assert_eq!(u.address_mode, "automatic");
    }
    assert!(register_account(
        &state,
        &database.store,
        "reused".into(),
        user_invite.code,
        password.clone()
    )
    .is_err());
    let code = invite(&runtime, &state, "single");
    assert_eq!(
        register_account(
            &state,
            &database.store,
            "u1".into(),
            code.code.clone(),
            password.clone()
        )
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
    let handles = (0..12)
        .map(|i| {
            let mut worker = state.clone();
            // Separate registries deliberately exercise the PostgreSQL row locks,
            // not only the single-master in-process registry lock.
            worker.users = Arc::new(RwLock::new(state.users.read().unwrap().clone()));
            let store = database.store.clone();
            let code = code.code.clone();
            let password = password.clone();
            std::thread::spawn(move || {
                register_account(&worker, &store, format!("racer-{i}"), code, password).is_ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        handles
            .into_iter()
            .filter(|h| h.thread().id() != std::thread::current().id())
            .map(|h| h.join().unwrap() as usize)
            .sum::<usize>(),
        1
    );
    // Reload after independent worker registries, like a master restart.
    load_user_registry(&state).unwrap();
    let unlimited = invite(&runtime, &state, "unlimited");
    let handles = (0..8)
        .map(|i| {
            let mut worker = state.clone();
            worker.users = Arc::new(RwLock::new(state.users.read().unwrap().clone()));
            let store = database.store.clone();
            let code = unlimited.code.clone();
            let password = password.clone();
            std::thread::spawn(move || {
                register_account(&worker, &store, format!("unlimited-{i}"), code, password).unwrap()
            })
        })
        .collect::<Vec<_>>();
    for h in handles {
        h.join().unwrap();
    }
    load_user_registry(&state).unwrap();
    let (s, body, _) = call(
        &runtime,
        &state,
        "GET",
        "/v1/admin/invitations?page_size=100",
        Some("test-admin"),
        Value::Null,
    );
    assert_eq!(s, StatusCode::OK);
    let list: InvitationPage<InvitationView> = serde_json::from_value(body).unwrap();
    assert_eq!(
        list.items
            .iter()
            .find(|i| i.id == unlimited.invitation.id)
            .unwrap()
            .use_count,
        8
    );
    let serialized = json!(list).to_string();
    assert!(!serialized.contains(&unlimited.code));
    assert!(!serialized.contains("code_hash"));
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            &format!("/v1/client/invitations/{}/revoke", unlimited.invitation.id),
            Some("test-user"),
            Value::Null
        )
        .0,
        StatusCode::FORBIDDEN
    );
    // Failure after account UPDATE must roll back the entire transaction.
    let rollback = invite(&runtime, &state, "single");
    database.execute("CREATE FUNCTION reject_test_policy() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected write failure'; END $$; CREATE TRIGGER reject_test_policy BEFORE INSERT ON user_forward_policies FOR EACH ROW EXECUTE FUNCTION reject_test_policy()");
    assert_eq!(
        register_account(
            &state,
            &database.store,
            "must-rollback".into(),
            rollback.code.clone(),
            password.clone()
        )
        .unwrap_err()
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!state.users.read().unwrap().contains_key("must-rollback"));
    database.execute("DROP TRIGGER reject_test_policy ON user_forward_policies; DROP FUNCTION reject_test_policy()");
    register_account(
        &state,
        &database.store,
        "after-rollback".into(),
        rollback.code,
        password.clone(),
    )
    .unwrap();
    let revoked = invite(&runtime, &state, "single");
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            &format!("/v1/admin/invitations/{}/revoke", revoked.invitation.id),
            Some("test-admin"),
            Value::Null
        )
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(register_account(
        &state,
        &database.store,
        "revoked".into(),
        revoked.code,
        password.clone()
    )
    .is_err());
    // User quota limits and revocation do not refund daily generation.
    let a = call(
        &runtime,
        &state,
        "POST",
        "/v1/client/invitations",
        Some("test-user"),
        json!({}),
    );
    assert_eq!(a.0, StatusCode::OK);
    let a: CreatedInvitation = serde_json::from_value(a.1).unwrap();
    let b = call(
        &runtime,
        &state,
        "POST",
        "/v1/client/invitations",
        Some("test-user"),
        json!({}),
    );
    assert_eq!(b.0, StatusCode::OK);
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/client/invitations",
            Some("test-user"),
            json!({})
        )
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            &format!("/v1/client/invitations/{}/revoke", a.invitation.id),
            Some("test-user"),
            Value::Null
        )
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/client/invitations",
            Some("test-user"),
            json!({})
        )
        .0,
        StatusCode::CONFLICT
    );
    let b: CreatedInvitation = serde_json::from_value(b.1).unwrap();
    let mut disabled = p.clone();
    disabled.enabled = false;
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(disabled)
        )
        .0,
        StatusCode::OK
    );
    assert!(register_account(
        &state,
        &database.store,
        "disabled-policy".into(),
        b.code.clone(),
        password.clone()
    )
    .is_err());
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(p)
        )
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/status",
            Some("test-admin"),
            json!({"enabled":false})
        )
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(register_account(
        &state,
        &database.store,
        "disabled-owner".into(),
        b.code.clone(),
        password.clone()
    )
    .is_err());
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/status",
            Some("test-admin"),
            json!({"enabled":true})
        )
        .0,
        StatusCode::NO_CONTENT
    );
    register_account(
        &state,
        &database.store,
        "reenabled".into(),
        b.code,
        password.clone(),
    )
    .unwrap();
    let expired = invite(&runtime, &state, "single");
    database.execute(&format!(
        "UPDATE invitations SET expires_at=CURRENT_TIMESTAMP-interval '1 second' WHERE id='{}'",
        expired.invitation.id
    ));
    assert!(register_account(
        &state,
        &database.store,
        "expired".into(),
        expired.code,
        password.clone()
    )
    .is_err());
    // Real HTTP registration preserves login compatibility and no forced reset.
    let http = invite(&runtime, &state, "single");
    let registered = call(
        &runtime,
        &state,
        "POST",
        "/v1/auth/register",
        None,
        json!({"username":"http-user","password":"test-registration-password","code":http.code}),
    );
    assert_eq!(registered.0, StatusCode::CREATED, "{}", registered.1);
    let login = call(
        &runtime,
        &state,
        "POST",
        "/v1/auth/login",
        None,
        json!({"username":"http-user","password":"test-registration-password"}),
    );
    assert_eq!(login.0, StatusCode::OK);
    assert_eq!(login.1["must_change_password"], false);
    let token = login.1["token"].as_str().unwrap();
    let policy = call(
        &runtime,
        &state,
        "GET",
        "/v1/client/invitations/policy",
        Some(token),
        Value::Null,
    );
    assert_eq!(policy.1["policy"]["enabled"], false);
    let mine = call(
        &runtime,
        &state,
        "GET",
        "/v1/client/invitation-registrations",
        Some(token),
        Value::Null,
    );
    assert_eq!(mine.1["total"], 0);
    let rec = call(
        &runtime,
        &state,
        "GET",
        "/v1/admin/invitation-registrations?username=http-user",
        Some("test-admin"),
        Value::Null,
    );
    assert_eq!(rec.1["total"], 1);
    assert!(rec.1["items"][0]["grant"].is_object());
    let over = call(
        &runtime,
        &state,
        "GET",
        "/v1/admin/invitations?page_size=101",
        Some("test-admin"),
        Value::Null,
    );
    assert_eq!(over.0, StatusCode::BAD_REQUEST);
    let elevated = call(
        &runtime,
        &state,
        "POST",
        "/v1/auth/register",
        None,
        json!({"username":"evil","password":"test-registration-password","code":unlimited.code,"role":"admin"}),
    );
    assert_eq!(elevated.0, StatusCode::UNPROCESSABLE_ENTITY);
    let page = call(&runtime, &state, "GET", "/register", None, Value::Null);
    assert_eq!(page.0, StatusCode::OK);
    // Durable reload retains every account and its forward policy.
    let before = state.users.read().unwrap().len();
    state.users.write().unwrap().clear();
    load_user_registry(&state).unwrap();
    assert_eq!(state.users.read().unwrap().len(), before);
    assert!(
        !state
            .port_forward_store
            .policy(&state.users.read().unwrap()["http-user"].user_id)
            .unwrap()
            .enabled
    );
}

#[test]
#[ignore = "requires dedicated PostgreSQL; see docs/INVITATIONS.md"]
fn postgres_stale_entry_and_daily_reset() {
    let database = Database::new();
    let state = database.state();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut p = InvitationPolicy {
        enabled: true,
        max_unused: 1,
        daily_limit: 1,
        valid_days: 7,
        ..Default::default()
    };
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(p)
        )
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/client/invitations",
            Some("test-user"),
            json!({})
        )
        .0,
        StatusCode::OK
    );
    database.execute("UPDATE invitations SET created_at=CURRENT_TIMESTAMP-interval '1 day',expires_at=CURRENT_TIMESTAMP-interval '1 second'");
    let view = call(
        &runtime,
        &state,
        "GET",
        "/v1/client/invitations/policy",
        Some("test-user"),
        Value::Null,
    );
    assert_eq!(view.1["generated_today"], 0);
    assert_eq!(view.1["unused"], 0);
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/client/invitations",
            Some("test-user"),
            json!({})
        )
        .0,
        StatusCode::OK
    );
    p.max_unused = 3;
    p.daily_limit = 5;
    p.grant.port_forward.enabled = true;
    p.grant.port_forward.max_active_forwards = 2;
    p.grant.port_forward.allowed_entry_ids = vec!["gone".into()];
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(p)
        )
        .0,
        StatusCode::CONFLICT
    );
    // An entry may disappear after policy save/code creation. Redemption must
    // fail atomically, not silently drop the forwarding grant or consume a code.
    database.execute("INSERT INTO forward_entries(entry_id,node_id,name,public_address,family,protocols,enabled) VALUES('integration-entry','node','test','203.0.113.7','ipv4','[\"tcp\"]',true)");
    p.grant.port_forward.allowed_entry_ids = vec!["integration-entry".into()];
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(p)
        )
        .0,
        StatusCode::OK
    );
    let created = call(
        &runtime,
        &state,
        "POST",
        "/v1/client/invitations",
        Some("test-user"),
        json!({}),
    );
    assert_eq!(created.0, StatusCode::OK);
    let created: CreatedInvitation = serde_json::from_value(created.1).unwrap();
    database.execute("DELETE FROM forward_entries WHERE entry_id='integration-entry'");
    let password = hash_password("stale-entry-test-password");
    assert_eq!(
        register_account(
            &state,
            &database.store,
            "entry-rollback".into(),
            created.code.clone(),
            password.clone()
        )
        .unwrap_err()
        .0,
        StatusCode::CONFLICT
    );
    assert!(!state.users.read().unwrap().contains_key("entry-rollback"));
    let id = created.invitation.id.clone();
    assert_eq!(
        transaction(&database.store, move |tx| Ok(read_view(tx, &id)?.use_count)).unwrap(),
        0
    );
    p.grant = InvitationGrant::default();
    p.max_unused = 1;
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/admin/users/u1/invitation-policy",
            Some("test-admin"),
            json!(p)
        )
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &runtime,
            &state,
            "POST",
            "/v1/client/invitations",
            Some("test-user"),
            json!({})
        )
        .0,
        StatusCode::CONFLICT
    );
    register_account(
        &state,
        &database.store,
        "entry-recovered".into(),
        created.code,
        password,
    )
    .unwrap();
}
