use super::*;

type ApiError = (StatusCode, Json<ErrorResponse>);

fn error(status: StatusCode, message: &str) -> ApiError {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}

enum Outcome {
    Rejected(StatusCode, &'static str),
    Deleted(HashMap<String, UserAccount>, HashMap<Uuid, Device>, String),
}

fn guard(
    users: &HashMap<String, UserAccount>,
    username: &str,
    actor_id: &str,
) -> Result<String, (StatusCode, &'static str)> {
    let user = users
        .get(username)
        .ok_or((StatusCode::NOT_FOUND, "账户不存在"))?;
    let usable =
        |user: &&UserAccount| user.enabled && user.role == "admin" && !user.must_change_password;
    if usable(&user) && users.values().filter(usable).count() <= 1 {
        return Err((StatusCode::CONFLICT, "不能删除最后一个可用管理员"));
    }
    if user.user_id == actor_id {
        return Err((StatusCode::CONFLICT, "不能删除当前登录账户"));
    }
    Ok(user.user_id.clone())
}

pub(super) async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<StatusCode, ApiError> {
    let actor =
        require_admin(&state, &headers).map_err(|status| error(status, "需要管理员权限"))?;
    // Match connection/address/P2P operations: devices -> users -> sessions.
    // Hold both registries through commit, so heartbeats cannot republish the
    // old device generation and registration cannot race account deletion.
    let mut devices = state.devices.write().expect("device registry poisoned");
    let mut users = state.users.write().expect("user registry poisoned");
    let expected_id = guard(&users, &username, &actor.user_id).map_err(|(s, m)| error(s, m))?;
    let database = state.database.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "账户删除存储暂不可用，请稍后重试",
        )
    })?;
    let mut next_devices = devices.clone();
    let outcome = database.on_transaction(move |tx| {
        // Same durable identity lock as invitation registration. Never replace
        // all users from an old runtime snapshot or reset unrelated accounts.
        let row = tx.query_one(
            "SELECT payload FROM pmman_documents WHERE document_key='users' FOR UPDATE", &[],
        ).map_err(|_| "读取账户记录失败")?;
        let mut next: HashMap<String, UserAccount> = serde_json::from_str(&row.get::<_, String>(0))
            .map_err(|_| "账户记录格式错误")?;
        if !next.get(&actor.username).is_some_and(|u| {
            u.user_id == actor.user_id && u.enabled && u.role == "admin" && !u.must_change_password
        }) {
            return Ok(Outcome::Rejected(StatusCode::FORBIDDEN, "需要管理员权限"));
        }
        let id = match guard(&next, &username, &actor.user_id) {
            Ok(id) => id,
            Err((s, m)) => return Ok(Outcome::Rejected(s, m)),
        };
        // A runtime/document identity mismatch must fail closed, not revoke
        // one identity's sessions while deleting another account of that name.
        if id != expected_id {
            return Err("账户身份已变化，请刷新后重试".into());
        }
        // Devices are the runtime-authoritative generation under its write lock.
        for device in next_devices.values_mut().filter(|d| d.user_id == id) {
            device.status = "revoked".into();
            device.last_seen_at = Utc::now();
        }
        next.remove(&username);
        let payload = serde_json::to_string(&next).map_err(|_| "账户记录编码失败")?;
        tx.execute("UPDATE pmman_documents SET payload=$1,revision=revision+1,updated_at=CURRENT_TIMESTAMP WHERE document_key='users'", &[&payload])
            .map_err(|_| "删除账户记录失败")?;
        let payload = serde_json::to_string(&next_devices).map_err(|_| "设备记录编码失败")?;
        tx.execute("INSERT INTO pmman_documents(document_key,payload) VALUES('devices',$1) ON CONFLICT(document_key) DO UPDATE SET payload=EXCLUDED.payload,revision=pmman_documents.revision+1,updated_at=CURRENT_TIMESTAMP", &[&payload])
            .map_err(|_| "保存设备撤销失败")?;
        // Keep the port reserved until the existing node heartbeat acknowledges
        // rule removal. Late application reports cannot reactivate releasing rows.
        tx.execute("WITH revoked AS (UPDATE port_forward_allocations SET state='releasing',updated_at=CURRENT_TIMESTAMP,expires_at=NULL,error_message=NULL WHERE user_id=$1 AND state NOT IN ('released','expired','releasing') RETURNING forward_id) INSERT INTO port_forward_events(forward_id,event_type,detail,created_at) SELECT forward_id,'user.deleted',NULL,CURRENT_TIMESTAMP FROM revoked", &[&id])
            .map_err(|_| "释放端口转发失败")?;
        tx.execute("DELETE FROM user_forward_policies WHERE user_id=$1", &[&id])
            .map_err(|_| "删除端口转发授权失败")?;
        tx.execute("DELETE FROM user_address_policies WHERE user_id=$1", &[&id])
            .map_err(|_| "删除地址分配策略失败")?;
        tx.execute("UPDATE address_leases SET state='releasing',updated_at=CURRENT_TIMESTAMP WHERE user_id=$1 AND state<>'released'", &[&id])
            .map_err(|_| "撤销地址租约失败")?;
        // Revoke unlimited codes too, even when previously redeemed. Preserve
        // used single-use codes, all registration snapshots and all audit rows.
        tx.execute("UPDATE invitations SET revoked_at=CURRENT_TIMESTAMP WHERE inviter_user_id=$1 AND revoked_at IS NULL AND (kind='unlimited' OR use_count=0)", &[&id])
            .map_err(|_| "撤销邀请码失败")?;
        tx.execute("DELETE FROM invitation_policies WHERE user_id=$1", &[&id])
            .map_err(|_| "删除邀请授权失败")?;
        tx.execute("INSERT INTO pmman_audit_events(actor,event_type,resource_type,resource_id,detail) VALUES($1,'user.deleted','user',$2,$3)", &[&actor.username,&id,&username])
            .map_err(|_| "写入账户删除审计失败")?;
        Ok(Outcome::Deleted(next, next_devices, id))
    }).map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "账户删除存储暂不可用，请稍后重试"))?;
    match outcome {
        Outcome::Rejected(s, m) => Err(error(s, m)),
        Outcome::Deleted(next, next_devices, id) => {
            let revoked_ids: HashSet<_> = next_devices
                .values()
                .filter(|d| d.user_id == id)
                .map(|d| d.device_id)
                .collect();
            *devices = next_devices;
            *users = next;
            invalidate_user_sessions(&state, &id);
            state
                .p2p
                .write()
                .expect("p2p registry poisoned")
                .retain(|device_id, _| !revoked_ids.contains(device_id));
            drop(users);
            drop(devices);
            // The legacy gateway has no node heartbeat. Durable revocation is
            // already committed; failures leave the orphan quarantined for the
            // periodic retry worker, including after a master restart.
            if retry_legacy_cleanup(&state).is_err() {
                eprintln!("reason=legacy_cleanup_pending");
            }
            Ok(StatusCode::NO_CONTENT)
        }
    }
}

pub(super) fn prior_device_cleanup_confirmed(state: &AppState, device: &Device) -> bool {
    if device.node_id == "legacy" {
        return state.gateway.interface.as_ref().is_none_or(|interface| {
            remove_gateway_peer(&state.gateway, interface, &device.public_key).is_ok()
        });
    }
    state
        .nodes
        .read()
        .expect("node registry poisoned")
        .get(&device.node_id)
        .is_some_and(|node| {
            node.last_seen_at >= device.last_seen_at
                && node.cleanup_ok
                && !node.applied.iter().any(|peer| {
                    peer.device_id == device.device_id || peer.public_key == device.public_key
                })
                && !node.sync_errors.iter().any(|peer| {
                    peer.device_id == device.device_id || peer.public_key == device.public_key
                })
        })
}

pub(super) fn retry_legacy_cleanup(state: &AppState) -> Result<(), String> {
    retry_legacy_cleanup_with(state, |interface, key| {
        remove_gateway_peer(&state.gateway, interface, key)
    })
}

fn retry_legacy_cleanup_with(
    state: &AppState,
    mut remove: impl FnMut(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let Some(database) = state.database.as_ref() else {
        return Ok(());
    };
    let mut devices = state.devices.write().expect("device registry poisoned");
    let users = state.users.read().expect("user registry poisoned");
    let mut removed = Vec::new();
    for device in devices.values().filter(|device| {
        device.node_id == "legacy"
            && device.status == "revoked"
            && !users.values().any(|user| user.user_id == device.user_id)
    }) {
        if let Some(interface) = &state.gateway.interface {
            remove(interface, &device.public_key)?;
        }
        removed.push((device.device_id, device.user_id.clone()));
    }
    if removed.is_empty() {
        return Ok(());
    }
    let mut next = devices.clone();
    let mut address_registry = state.address_pools.write().expect("address pools poisoned");
    let mut confirmed = Vec::new();
    for (device_id, id) in removed {
        let device = devices.get(&device_id).expect("device disappeared");
        if address_pools::prepare_forward_cleanup(
            state,
            &mut address_registry,
            &users,
            &devices,
            device,
            true,
            true,
        )? {
            confirmed.push((device_id, id));
        }
    }
    let next = database.on_transaction(move |tx| {
        for (device_id, id) in confirmed {
            let pending: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM port_forward_allocations WHERE user_id=$1 AND state NOT IN ('released','expired'))", &[&id])
                .map_err(|_| "读取待释放转发失败")?.get(0);
            if !pending {
                next.get_mut(&device_id).expect("device disappeared").status = "released".into();
            }
        }
        let payload = serde_json::to_string(&next).map_err(|_| "设备记录编码失败")?;
        tx.execute("INSERT INTO pmman_documents(document_key,payload) VALUES('devices',$1) ON CONFLICT(document_key) DO UPDATE SET payload=EXCLUDED.payload,revision=pmman_documents.revision+1,updated_at=CURRENT_TIMESTAMP", &[&payload])
            .map_err(|_| "保存网关清理状态失败")?;
        Ok(next)
    })?;
    *devices = next;
    Ok(())
}

/// A revoked orphan holds its old addresses until its owning node confirms
/// peer/route cleanup and every forwarding node has released its old mappings.
/// Retain the device history, but stop reserving its addresses afterwards.
pub(super) fn confirm_device_cleanup(
    state: &AppState,
    node_id: &str,
    report: &NodeHeartbeatRequest,
) -> Result<(), String> {
    if !report.cleanup_ok {
        return Ok(());
    }
    let Some(database) = state.database.as_ref() else {
        return Ok(());
    };
    let mut devices = state.devices.write().expect("device registry poisoned");
    let users = state.users.read().expect("user registry poisoned");
    let candidates: Vec<_> = devices
        .values()
        .filter(|device| {
            device.node_id == node_id
                && device.status == "revoked"
                && !users.values().any(|user| user.user_id == device.user_id)
                && !report.applied.iter().any(|peer| {
                    peer.device_id == device.device_id || peer.public_key == device.public_key
                })
                && !report.errors.iter().any(|peer| {
                    peer.device_id == device.device_id || peer.public_key == device.public_key
                })
        })
        .map(|device| (device.device_id, device.user_id.clone()))
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let mut next = devices.clone();
    let mut address_registry = state.address_pools.write().expect("address pools poisoned");
    let mut confirmed = Vec::new();
    for (device_id, id) in candidates {
        let device = devices.get(&device_id).expect("device disappeared");
        if address_pools::prepare_forward_cleanup(
            state,
            &mut address_registry,
            &users,
            &devices,
            device,
            true,
            true,
        )? {
            confirmed.push((device_id, id));
        }
    }
    let next = database.on_transaction(move |tx| {
        for (device_id, id) in confirmed {
            let pending: bool = tx.query_one(
                "SELECT EXISTS(SELECT 1 FROM port_forward_allocations WHERE user_id=$1 AND state NOT IN ('released','expired'))", &[&id],
            ).map_err(|_| "读取待释放转发失败")?.get(0);
            if !pending {
                if let Some(device) = next.get_mut(&device_id) {
                    device.status = "released".into();
                }
            }
        }
        let payload = serde_json::to_string(&next).map_err(|_| "设备记录编码失败")?;
        tx.execute("INSERT INTO pmman_documents(document_key,payload) VALUES('devices',$1) ON CONFLICT(document_key) DO UPDATE SET payload=EXCLUDED.payload,revision=pmman_documents.revision+1,updated_at=CURRENT_TIMESTAMP", &[&payload])
            .map_err(|_| "保存地址释放状态失败")?;
        Ok(next)
    })?;
    *devices = next;
    Ok(())
}

#[cfg(test)]
#[path = "user_deletion_tests.rs"]
mod tests;
