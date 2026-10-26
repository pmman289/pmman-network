use super::*;
use std::sync::{mpsc, TryLockError};
use std::time::Duration as StdDuration;

/// Exercise the real handler, not a duplicate of its lock acquisition code.
/// All controller waits are bounded; a regression must fail, not hang the suite.
#[test]
fn node_update_waiting_for_devices_does_not_hold_nodes() {
    const WAIT: StdDuration = StdDuration::from_secs(5);
    const PROBES: usize = 128;

    let state = crate::tests::test_state();
    let admin_id = state.users.read().unwrap()["admin"].user_id.clone();
    state.sessions.write().unwrap().insert(
        "node-lock-admin".into(),
        Session {
            user_id: admin_id,
            username: "admin".into(),
            expires_at: Utc::now() + Duration::hours(1),
        },
    );

    // Only memory stores are used: no server, database, network or gateway tools.
    assert!(state.database.is_none());
    assert!(state.node_store.is_none());
    assert!(state.gateway.interface.is_none());

    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        "Bearer node-lock-admin".parse().unwrap(),
    );
    let input: NodeSettingsRequest = serde_json::from_value(serde_json::json!({
        "enabled": false,
        "wireguard_endpoint": "127.0.0.1:51820",
        "probe_endpoint": "127.0.0.1:51840"
    }))
    .unwrap();
    assert!(state.nodes.read().unwrap()["node-a"].enabled);

    let worker_state = state.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let (running_tx, running_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);

    // The controller represents the forwarding reader. It never blocks on nodes.
    let devices_reader = state.devices.read().unwrap();
    let worker = std::thread::Builder::new()
        .name("node-settings-lock-regression".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            ready_tx.send(()).unwrap();
            if start_rx.recv_timeout(WAIT).is_err() {
                return;
            }
            running_tx.send(()).unwrap();
            let result = runtime
                .block_on(update_node_settings(
                    State(worker_state),
                    headers,
                    Path("node-a".into()),
                    Json(input),
                ))
                .map(|Json(node)| (node.node_id, node.enabled))
                .map_err(|(status, _)| status);
            drop(runtime);
            let _ = done_tx.send(result);
        })
        .expect("start node settings worker");

    // Do not use scoped threads or an unconditional join: either can turn a
    // lock-order regression into an unbounded wait during panic/unwinding.
    drop(worker);
    ready_rx.recv_timeout(WAIT).expect("worker ready");
    start_tx.send(()).unwrap();
    running_rx
        .recv_timeout(WAIT)
        .expect("worker started handler");

    let mut unavailable = 0;
    let mut poisoned = false;
    let mut changed_while_reader_held = false;
    // No production hook exists at the lock boundary. Sample throughout a
    // bounded scheduling window rather than passing on one early successful
    // read. Ready/start alone does not prove the worker reached its first lock.
    for _ in 0..PROBES {
        match state.nodes.try_read() {
            Ok(nodes) => {
                changed_while_reader_held |= !nodes["node-a"].enabled;
            }
            Err(TryLockError::WouldBlock) => unavailable += 1,
            Err(TryLockError::Poisoned(_)) => poisoned = true,
        }
        std::thread::sleep(StdDuration::from_millis(2));
    }
    let before_release = done_rx.try_recv();

    // Release before asserting probe results. Even the old nodes -> devices
    // implementation can now finish; the test never creates a real lock cycle.
    drop(devices_reader);
    let (completed_early, completion) = match before_release {
        Ok(result) => (true, Ok(result)),
        Err(mpsc::TryRecvError::Empty) => (false, done_rx.recv_timeout(WAIT)),
        Err(mpsc::TryRecvError::Disconnected) => (false, Err(mpsc::RecvTimeoutError::Disconnected)),
    };
    let result = completion.expect("node update must finish after releasing devices reader");
    assert_eq!(result, Ok(("node-a".into(), false)));
    assert!(!completed_early, "update must wait for devices.write");
    assert!(
        !changed_while_reader_held,
        "node changed before acquiring devices.write"
    );
    assert!(!poisoned, "node registry poisoned during update");
    assert_eq!(
        unavailable, 0,
        "update retained or queued nodes.write while blocked by devices.read"
    );
}
