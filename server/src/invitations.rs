use super::*;
use axum::{
    extract::{ConnectInfo, Request},
    middleware::Next,
};
use pmman_protocol::{
    CreatedInvitation, InvitationGrant, InvitationPage, InvitationPolicy, InvitationPolicyView,
    InvitationQuery, InvitationRegistration, InvitationView,
};
use postgres::{Row, Transaction};
use std::time::{Duration as StdDuration, Instant};

type ApiError = (StatusCode, Json<ErrorResponse>);
type Result<T> = std::result::Result<T, ApiError>;
fn error(status: StatusCode, message: &str) -> ApiError {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}
fn storage(_error: impl std::fmt::Display) -> ApiError {
    // SQL errors can contain payload parameters; do not send or log them.
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "邀请服务暂不可用，请稍后重试",
    )
}
fn unavailable() -> ApiError {
    error(StatusCode::BAD_REQUEST, "邀请码不可用或已过期")
}
fn auth(state: &AppState, headers: &HeaderMap, admin: bool) -> Result<Principal> {
    // Client invitations follow explicit invitation policy, not password age.
    // Keep the existing administrator authentication requirements unchanged.
    let principal = if admin {
        require_admin(state, headers)
    } else {
        authenticate(state, headers)
    }
    .map_err(|s| error(s, "请登录具有相应权限的账户"))?;
    Ok(principal)
}
fn db(state: &AppState) -> Result<db::PgStore> {
    state
        .database
        .as_ref()
        .map(|d| d.as_ref().clone())
        .ok_or_else(|| storage("database required"))
}
fn transaction<T, F>(database: &db::PgStore, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Transaction<'_>) -> Result<T> + Send + 'static,
{
    database
        .on_thread(move |client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            let result = operation(&mut tx);
            if result.is_ok() {
                tx.commit().map_err(|e| e.to_string())?;
            }
            Ok(result)
        })
        .map_err(storage)?
}
fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(storage)
}
fn parse<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(storage)
}
fn audit(tx: &mut Transaction<'_>, actor: &str, event: &str, id: &str) -> Result<()> {
    tx.execute("INSERT INTO pmman_audit_events(actor,event_type,resource_type,resource_id) VALUES($1,$2,'invitation',$3)", &[&actor,&event,&id]).map_err(storage)?;
    Ok(())
}
fn policy(tx: &mut Transaction<'_>, user_id: &str, lock: bool) -> Result<(InvitationPolicy, i64)> {
    let sql = if lock {
        "SELECT payload,revision FROM invitation_policies WHERE user_id=$1 FOR UPDATE"
    } else {
        "SELECT payload,revision FROM invitation_policies WHERE user_id=$1"
    };
    match tx.query_opt(sql, &[&user_id]).map_err(storage)? {
        Some(row) => Ok((parse(&row.get::<_, String>(0))?, row.get(1))),
        None => Ok((InvitationPolicy::default(), 0)),
    }
}
fn ensure_policy(tx: &mut Transaction<'_>, user_id: &str) -> Result<()> {
    tx.execute("INSERT INTO invitation_policies(user_id,payload) VALUES($1,$2) ON CONFLICT(user_id) DO NOTHING", &[&user_id,&json(&InvitationPolicy::default())?]).map_err(storage)?;
    Ok(())
}
fn validate_grant(tx: &mut Transaction<'_>, grant: &InvitationGrant) -> Result<()> {
    let forward: port_forward::PolicyRecord = parse(&json(&grant.port_forward)?)?;
    port_forward::validate_policy(&forward).map_err(|e| error(StatusCode::BAD_REQUEST, &e))?;
    if forward.enabled && (forward.max_active_forwards == 0 || forward.allowed_entry_ids.is_empty())
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "启用端口转发需要额度和可用入口",
        ));
    }
    for id in &forward.allowed_entry_ids {
        let entry = tx
            .query_opt(
                "SELECT enabled,family,protocols FROM forward_entries WHERE entry_id=$1 FOR SHARE",
                &[id],
            )
            .map_err(storage)?
            .ok_or_else(|| {
                error(
                    StatusCode::CONFLICT,
                    "受邀权限包含已删除的转发入口，请联系管理员",
                )
            })?;
        if forward.enabled {
            let protocols: Vec<pmman_protocol::PortForwardProtocol> =
                parse(&entry.get::<_, String>(2))?;
            if !entry.get::<_, bool>(0)
                || !forward
                    .allowed_families
                    .contains(&entry.get::<_, String>(1))
                || !forward
                    .allowed_protocols
                    .iter()
                    .any(|p| protocols.contains(p))
            {
                return Err(error(
                    StatusCode::CONFLICT,
                    "受邀权限中的转发入口不可用，请联系管理员",
                ));
            }
        }
    }
    Ok(())
}
fn counts(tx: &mut Transaction<'_>, user_id: &str) -> Result<(i64, i64)> {
    let row = tx.query_one("SELECT count(*) FILTER (WHERE revoked_at IS NULL AND use_count=0 AND (expires_at IS NULL OR expires_at>CURRENT_TIMESTAMP)), count(*) FILTER (WHERE created_at >= date_trunc('day',CURRENT_TIMESTAMP AT TIME ZONE 'UTC') AT TIME ZONE 'UTC') FROM invitations WHERE inviter_user_id=$1 AND source='user'", &[&user_id]).map_err(storage)?;
    Ok((row.get(0), row.get(1)))
}
fn policy_view(
    tx: &mut Transaction<'_>,
    user_id: &str,
    enabled: bool,
) -> Result<InvitationPolicyView> {
    let (policy, revision) = policy(tx, user_id, false)?;
    let (unused, generated_today) = counts(tx, user_id)?;
    let reset = Utc::now()
        .date_naive()
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    Ok(InvitationPolicyView {
        account_enabled: enabled,
        remaining_unused: if policy.enabled && enabled {
            (i64::from(policy.max_unused) - unused).max(0)
        } else {
            0
        },
        remaining_today: if policy.enabled && enabled {
            (i64::from(policy.daily_limit) - generated_today).max(0)
        } else {
            0
        },
        policy,
        revision,
        unused,
        generated_today,
        resets_at: reset.to_rfc3339(),
    })
}

pub async fn client_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<InvitationPolicyView>> {
    let principal = auth(&state, &headers, false)?;
    transaction(&db(&state)?, move |tx| {
        Ok(Json(policy_view(tx, &principal.user_id, true)?))
    })
}
pub async fn admin_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<InvitationPolicyView>> {
    auth(&state, &headers, true)?;
    let users = state.users.read().unwrap();
    let user = users
        .get(&username)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "用户不存在"))?;
    let (id, enabled) = (user.user_id.clone(), user.enabled);
    transaction(&db(&state)?, move |tx| {
        Ok(Json(policy_view(tx, &id, enabled)?))
    })
}
pub async fn save_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(input): Json<InvitationPolicy>,
) -> Result<Json<InvitationPolicyView>> {
    let actor = auth(&state, &headers, true)?;
    if input.max_unused > 10000
        || input.daily_limit > 10000
        || !(1..=365).contains(&input.valid_days)
        || (input.enabled && (input.max_unused == 0 || input.daily_limit == 0))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "邀请码限额需为 1 至 10000，有效期为 1 至 365 天",
        ));
    }
    let users = state.users.read().unwrap();
    let user = users
        .get(&username)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "用户不存在"))?;
    let (id, enabled) = (user.user_id.clone(), user.enabled);
    transaction(&db(&state)?, move |tx| {
        ensure_policy(tx, &id)?;
        policy(tx, &id, true)?;
        validate_grant(tx, &input.grant)?;
        tx.execute("UPDATE invitation_policies SET payload=$2,revision=revision+1,updated_at=CURRENT_TIMESTAMP WHERE user_id=$1", &[&id,&json(&input)?]).map_err(storage)?;
        audit(tx, &actor.username, "invitation.policy.updated", &id)?;
        Ok(Json(policy_view(tx, &id, enabled)?))
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCreate {
    kind: String,
    #[serde(default)]
    note: String,
    expires_at: Option<DateTime<Utc>>,
    grant: InvitationGrant,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminEdit {
    note: String,
    expires_at: Option<DateTime<Utc>>,
    grant: Option<InvitationGrant>,
}
fn validate_expiry(expiry: Option<DateTime<Utc>>, note: &str) -> Result<()> {
    if expiry.is_some_and(|e| e <= Utc::now()) || note.chars().count() > 200 {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "有效期必须晚于当前时间，备注不能超过 200 字",
        ));
    }
    Ok(())
}
fn fresh_code() -> String {
    // Two independently generated UUID v4 values provide 244 random bits.
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn code_hash(code: &str) -> String {
    format!("{:x}", Sha256::digest(code.as_bytes()))
}
pub async fn client_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<CreatedInvitation>> {
    if !body.is_empty() && body.as_ref() != b"{}" {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "不能指定邀请码权限或使用次数",
        ));
    }
    let principal = auth(&state, &headers, false)?;
    let users = state.users.read().unwrap();
    if !users.get(&principal.username).is_some_and(|u| u.enabled) {
        return Err(unavailable());
    }
    transaction(&db(&state)?, move |tx| {
        ensure_policy(tx, &principal.user_id)?;
        let (p, _) = policy(tx, &principal.user_id, true)?;
        if !p.enabled {
            return Err(error(StatusCode::FORBIDDEN, "尚未获得邀请权限"));
        }
        let (unused, daily) = counts(tx, &principal.user_id)?;
        if unused >= i64::from(p.max_unused) || daily >= i64::from(p.daily_limit) {
            return Err(error(
                StatusCode::CONFLICT,
                "已达到邀请码数量或今日生成上限",
            ));
        }
        validate_grant(tx, &p.grant)?;
        let expires = Some(Utc::now() + Duration::days(i64::from(p.valid_days)));
        create(tx, &principal, "user", "single", "", expires, None)
    })
    .map(Json)
}
pub async fn admin_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<AdminCreate>,
) -> Result<Json<CreatedInvitation>> {
    let actor = auth(&state, &headers, true)?;
    if !matches!(input.kind.as_str(), "single" | "unlimited") {
        return Err(error(StatusCode::BAD_REQUEST, "邀请码类型无效"));
    }
    validate_expiry(input.expires_at, &input.note)?;
    let _users = state.users.read().unwrap();
    transaction(&db(&state)?, move |tx| {
        validate_grant(tx, &input.grant)?;
        create(
            tx,
            &actor,
            "admin",
            &input.kind,
            &input.note,
            input.expires_at,
            Some(input.grant),
        )
    })
    .map(Json)
}
fn create(
    tx: &mut Transaction<'_>,
    actor: &Principal,
    source: &str,
    kind: &str,
    note: &str,
    expires: Option<DateTime<Utc>>,
    grant: Option<InvitationGrant>,
) -> Result<CreatedInvitation> {
    let code = fresh_code();
    let id = Uuid::new_v4().to_string();
    let prefix = code[..8].to_owned();
    let payload = grant.as_ref().map(json).transpose()?;
    tx.execute("INSERT INTO invitations(id,code_hash,prefix,inviter_user_id,inviter_username,creator_username,source,kind,note,expires_at,grant_payload) VALUES($1,$2,$3,$4,$5,$5,$6,$7,$8,$9,$10)", &[&id,&code_hash(&code),&prefix,&actor.user_id,&actor.username,&source,&kind,&note,&expires,&payload]).map_err(storage)?;
    audit(tx, &actor.username, "invitation.created", &id)?;
    let invitation = read_view(tx, &id)?;
    Ok(CreatedInvitation {
        invitation,
        registration_path: format!("/register#invite={code}"),
        registration_url: None,
        code,
    })
}

const SELECT: &str = "SELECT c.*, CASE WHEN c.source='user' THEN p.payload::jsonb->'grant' ELSE c.grant_payload::jsonb END AS effective_grant, CASE WHEN c.source='user' THEN p.revision ELSE c.grant_revision END AS effective_revision, CASE WHEN c.revoked_at IS NOT NULL THEN 'revoked' WHEN c.kind='single' AND c.use_count>0 THEN 'used' WHEN c.expires_at<=CURRENT_TIMESTAMP THEN 'expired' WHEN COALESCE((d.payload::jsonb->c.inviter_username->>'enabled')::boolean,false)=false OR (c.source='user' AND COALESCE((p.payload::jsonb->>'enabled')::boolean,false)=false) THEN 'disabled' ELSE 'available' END AS state FROM invitations c LEFT JOIN invitation_policies p ON p.user_id=c.inviter_user_id LEFT JOIN pmman_documents d ON d.document_key='users'";
fn view(row: &Row) -> Result<InvitationView> {
    // Cast JSON in SELECT to text rather than requiring postgres JSON features.
    Ok(InvitationView {
        id: row.get("id"),
        prefix: row.get("prefix"),
        inviter_username: row.get("inviter_username"),
        creator_username: row.get("creator_username"),
        source: row.get("source"),
        kind: row.get("kind"),
        state: row.get("state"),
        note: row.get("note"),
        created_at: row.get::<_, DateTime<Utc>>("created_at").to_rfc3339(),
        expires_at: row
            .get::<_, Option<DateTime<Utc>>>("expires_at")
            .map(|v| v.to_rfc3339()),
        use_count: row.get("use_count"),
        grant: parse(&row.get::<_, String>("grant_text"))?,
        policy_revision: row.get::<_, Option<i64>>("effective_revision").unwrap_or(0),
    })
}
fn read_view(tx: &mut Transaction<'_>, id: &str) -> Result<InvitationView> {
    let sql = format!(
        "SELECT s.*,COALESCE(effective_grant::text,$2) AS grant_text FROM ({SELECT}) s WHERE id=$1"
    );
    let row = tx
        .query_opt(&sql, &[&id, &json(&InvitationGrant::default())?])
        .map_err(storage)?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "邀请码不存在"))?;
    view(&row)
}
pub async fn edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<AdminEdit>,
) -> Result<Json<InvitationView>> {
    let actor = auth(&state, &headers, true)?;
    validate_expiry(input.expires_at, &input.note)?;
    let _users = state.users.read().unwrap();
    transaction(&db(&state)?, move |tx| {
        let row = tx
            .query_opt(
                "SELECT source,kind,use_count,revoked_at FROM invitations WHERE id=$1 FOR UPDATE",
                &[&id],
            )
            .map_err(storage)?
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "邀请码不存在"))?;
        if row.get::<_, Option<DateTime<Utc>>>("revoked_at").is_some()
            || (row.get::<_, String>("kind") == "single" && row.get::<_, i64>("use_count") > 0)
        {
            return Err(error(
                StatusCode::CONFLICT,
                "已撤销或已使用的单次邀请码不能编辑",
            ));
        }
        if row.get::<_, String>("source") == "user" && input.grant.is_some() {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "用户邀请码权限请在邀请授权中修改",
            ));
        }
        if let Some(grant) = &input.grant {
            validate_grant(tx, grant)?;
        }
        let grant = input.grant.as_ref().map(json).transpose()?;
        tx.execute("UPDATE invitations SET note=$2,expires_at=$3,grant_payload=COALESCE($4,grant_payload),grant_revision=grant_revision+CASE WHEN $4::text IS NULL THEN 0 ELSE 1 END WHERE id=$1", &[&id,&input.note,&input.expires_at,&grant]).map_err(storage)?;
        audit(tx, &actor.username, "invitation.updated", &id)?;
        Ok(Json(read_view(tx, &id)?))
    })
}
async fn revoke_inner(
    state: AppState,
    headers: HeaderMap,
    id: String,
    admin: bool,
) -> Result<StatusCode> {
    let actor = auth(&state, &headers, admin)?;
    transaction(&db(&state)?, move |tx| {
        let row=tx.query_opt("SELECT inviter_user_id,source,kind,use_count FROM invitations WHERE id=$1 FOR UPDATE", &[&id]).map_err(storage)?.ok_or_else(||error(StatusCode::NOT_FOUND,"邀请码不存在"))?;
        if !admin
            && (row.get::<_, String>("inviter_user_id") != actor.user_id
                || row.get::<_, String>("source") != "user")
        {
            return Err(error(StatusCode::FORBIDDEN, "不能撤销其他人的邀请码"));
        }
        if row.get::<_, String>("kind") == "single" && row.get::<_, i64>("use_count") > 0 {
            return Err(error(StatusCode::CONFLICT, "邀请码已使用"));
        }
        tx.execute(
            "UPDATE invitations SET revoked_at=COALESCE(revoked_at,CURRENT_TIMESTAMP) WHERE id=$1",
            &[&id],
        )
        .map_err(storage)?;
        audit(tx, &actor.username, "invitation.revoked", &id)?;
        Ok(StatusCode::NO_CONTENT)
    })
}
pub async fn client_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    revoke_inner(state, headers, id, false).await
}
pub async fn admin_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    revoke_inner(state, headers, id, true).await
}

fn page_query(
    query: &InvitationQuery,
) -> Result<(i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    let page = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    if page == 0
        || size == 0
        || size > 100
        || page > 1_000_000
        || query.inviter.as_ref().is_some_and(|v| v.len() > 64)
        || query.username.as_ref().is_some_and(|v| v.len() > 64)
    {
        return Err(error(StatusCode::BAD_REQUEST, "分页或筛选参数无效"));
    }
    let since = query
        .since
        .as_ref()
        .map(|v| DateTime::parse_from_rfc3339(v).map(|d| d.with_timezone(&Utc)))
        .transpose()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "开始时间无效"))?;
    let until = query
        .until
        .as_ref()
        .map(|v| DateTime::parse_from_rfc3339(v).map(|d| d.with_timezone(&Utc)))
        .transpose()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "结束时间无效"))?;
    if since.zip(until).is_some_and(|(a, b)| a > b) {
        return Err(error(StatusCode::BAD_REQUEST, "时间范围无效"));
    }
    if query
        .kind
        .as_ref()
        .is_some_and(|v| !matches!(v.as_str(), "single" | "unlimited"))
        || query.state.as_ref().is_some_and(|v| {
            !matches!(
                v.as_str(),
                "available" | "used" | "revoked" | "expired" | "disabled"
            )
        })
    {
        return Err(error(StatusCode::BAD_REQUEST, "类型或状态无效"));
    }
    Ok((i64::from(page), i64::from(size), since, until))
}
async fn list_inner(
    state: AppState,
    headers: HeaderMap,
    query: InvitationQuery,
    admin: bool,
) -> Result<Json<InvitationPage<InvitationView>>> {
    let actor = auth(&state, &headers, admin)?;
    let (page, size, since, until) = page_query(&query)?;
    let _users = state.users.read().unwrap();
    transaction(&db(&state)?, move |tx| {
        let owner = if admin { None } else { Some(actor.user_id) };
        let filter=" WHERE ($1::text IS NULL OR inviter_user_id=$1) AND ($2::text IS NULL OR inviter_username ILIKE '%'||$2||'%') AND ($3::text IS NULL OR kind=$3) AND ($4::text IS NULL OR state=$4) AND ($5::timestamptz IS NULL OR created_at>=$5) AND ($6::timestamptz IS NULL OR created_at<=$6)";
        let base = format!("WITH s AS ({SELECT})");
        let total: i64 = tx
            .query_one(
                &format!("{base} SELECT count(*) FROM s{filter}"),
                &[
                    &owner,
                    &query.inviter,
                    &query.kind,
                    &query.state,
                    &since,
                    &until,
                ],
            )
            .map_err(storage)?
            .get(0);
        let rows=tx.query(&format!("{base} SELECT s.*,COALESCE(effective_grant::text,$9) AS grant_text FROM s{filter} ORDER BY created_at DESC,id DESC LIMIT $7 OFFSET $8"),&[&owner,&query.inviter,&query.kind,&query.state,&since,&until,&size,&((page-1)*size),&json(&InvitationGrant::default())?]).map_err(storage)?;
        Ok(Json(InvitationPage {
            items: rows.iter().map(view).collect::<Result<Vec<_>>>()?,
            total,
            page: page as u32,
            page_size: size as u32,
        }))
    })
}
pub async fn client_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InvitationQuery>,
) -> Result<Json<InvitationPage<InvitationView>>> {
    list_inner(state, headers, query, false).await
}
pub async fn admin_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InvitationQuery>,
) -> Result<Json<InvitationPage<InvitationView>>> {
    list_inner(state, headers, query, true).await
}
async fn registrations_inner(
    state: AppState,
    headers: HeaderMap,
    query: InvitationQuery,
    admin: bool,
) -> Result<Json<InvitationPage<InvitationRegistration>>> {
    let actor = auth(&state, &headers, admin)?;
    let (page, size, since, until) = page_query(&query)?;
    transaction(&db(&state)?, move |tx| {
        let owner = if admin { None } else { Some(actor.user_id) };
        let filter=" FROM invitation_registrations r JOIN invitations c ON c.id=r.invitation_id WHERE ($1::text IS NULL OR r.inviter_user_id=$1) AND ($2::text IS NULL OR r.inviter_username ILIKE '%'||$2||'%') AND ($3::text IS NULL OR r.username ILIKE '%'||$3||'%') AND ($4::timestamptz IS NULL OR r.created_at>=$4) AND ($5::timestamptz IS NULL OR r.created_at<=$5)";
        let total = tx
            .query_one(
                &format!("SELECT count(*){filter}"),
                &[&owner, &query.inviter, &query.username, &since, &until],
            )
            .map_err(storage)?
            .get(0);
        let rows=tx.query(&format!("SELECT r.*,c.prefix{filter} ORDER BY r.created_at DESC,r.id DESC LIMIT $6 OFFSET $7"),&[&owner,&query.inviter,&query.username,&since,&until,&size,&((page-1)*size)]).map_err(storage)?;
        let items = rows
            .iter()
            .map(|r| {
                Ok(InvitationRegistration {
                    id: r.get("id"),
                    invitation_id: r.get("invitation_id"),
                    prefix: r.get("prefix"),
                    inviter_username: r.get("inviter_username"),
                    username: r.get("username"),
                    created_at: r.get::<_, DateTime<Utc>>("created_at").to_rfc3339(),
                    grant: parse(&r.get::<_, String>("grant_payload"))?,
                    policy_revision: r.get("policy_revision"),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Json(InvitationPage {
            items,
            total,
            page: page as u32,
            page_size: size as u32,
        }))
    })
}
pub async fn client_registrations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InvitationQuery>,
) -> Result<Json<InvitationPage<InvitationRegistration>>> {
    registrations_inner(state, headers, query, false).await
}
pub async fn admin_registrations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InvitationQuery>,
) -> Result<Json<InvitationPage<InvitationRegistration>>> {
    registrations_inner(state, headers, query, true).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterInput {
    code: String,
    username: String,
    password: String,
}
pub async fn register(
    State(state): State<AppState>,
    Json(input): Json<RegisterInput>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    if !valid_username(&input.username)
        || !(6..=256).contains(&input.password.len())
        || input.code.len() != 64
        || !input.code.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "用户名、密码或邀请码格式无效",
        ));
    }
    let database = db(&state)?;
    // Hash off the HTTP worker; the registration limiter runs before this step.
    let password = input.password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(storage)?;
    let next = register_account(&state, &database, input.username, input.code, password_hash)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"username":next})),
    ))
}
fn register_account(
    state: &AppState,
    database: &db::PgStore,
    username: String,
    code: String,
    password_hash: String,
) -> Result<String> {
    let mut users = state.users.write().unwrap();
    let (next, registered) = transaction(database, move |tx| {
        let doc = tx
            .query_one(
                "SELECT payload FROM pmman_documents WHERE document_key='users' FOR UPDATE",
                &[],
            )
            .map_err(storage)?;
        let mut next: HashMap<String, UserAccount> = parse(&doc.get::<_, String>(0))?;
        if next.contains_key(&username) {
            return Err(error(
                StatusCode::CONFLICT,
                "用户名已存在；如刚完成注册，请尝试登录",
            ));
        }
        let hash = code_hash(&code.to_ascii_lowercase());
        let lookup=tx.query_opt("SELECT id,inviter_user_id,inviter_username,source FROM invitations WHERE code_hash=$1", &[&hash]).map_err(storage)?.ok_or_else(unavailable)?;
        let inviter_id: String = lookup.get("inviter_user_id");
        let inviter_name: String = lookup.get("inviter_username");
        if !next
            .get(&inviter_name)
            .is_some_and(|u| u.enabled && u.user_id == inviter_id)
        {
            return Err(unavailable());
        }
        let (p, revision) = policy(tx, &inviter_id, true)?;
        if lookup.get::<_, String>("source") == "user" && !p.enabled {
            return Err(unavailable());
        }
        let row = tx
            .query_one(
                "SELECT * FROM invitations WHERE code_hash=$1 FOR UPDATE",
                &[&hash],
            )
            .map_err(storage)?;
        if row.get::<_, Option<DateTime<Utc>>>("revoked_at").is_some()
            || row
                .get::<_, Option<DateTime<Utc>>>("expires_at")
                .is_some_and(|e| e <= Utc::now())
            || (row.get::<_, String>("kind") == "single" && row.get::<_, i64>("use_count") > 0)
        {
            return Err(unavailable());
        }
        let (grant, revision) = if row.get::<_, String>("source") == "user" {
            (p.grant, revision)
        } else {
            (
                parse::<InvitationGrant>(&row.get::<_, String>("grant_payload"))?,
                row.get::<_, i64>("grant_revision"),
            )
        };
        validate_grant(tx, &grant)?;
        let user_id = Uuid::new_v4().to_string();
        let id: String = row.get("id");
        next.insert(
            username.clone(),
            UserAccount {
                user_id: user_id.clone(),
                username: username.clone(),
                password_hash,
                role: "user".into(),
                enabled: true,
                allow_ipv4_global: grant.allow_ipv4_global,
                allow_ipv6_global: grant.allow_ipv6_global,
                allow_ipv4_split: grant.allow_ipv4_split,
                allow_ipv6_split: grant.allow_ipv6_split,
                created_at: Utc::now(),
                must_change_password: false,
                address_mode: default_address_mode(),
                reserved_ipv4: None,
                reserved_ipv6: None,
                leased_ipv4: None,
                leased_ipv6: None,
            },
        );
        tx.execute("UPDATE pmman_documents SET payload=$1,revision=revision+1,updated_at=CURRENT_TIMESTAMP WHERE document_key='users'", &[&json(&next)?]).map_err(storage)?;
        let f = &grant.port_forward;
        let ports = f.allowed_target_ports.as_ref().map(json).transpose()?;
        tx.execute("INSERT INTO user_forward_policies(user_id,enabled,max_active_forwards,allowed_entry_ids,allowed_protocols,allowed_target_ports,allowed_families,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,CURRENT_TIMESTAMP)", &[&user_id,&f.enabled,&(f.max_active_forwards as i32),&json(&f.allowed_entry_ids)?,&json(&f.allowed_protocols)?,&ports,&json(&f.allowed_families)?]).map_err(storage)?;
        tx.execute(
            "UPDATE invitations SET use_count=use_count+1 WHERE id=$1",
            &[&id],
        )
        .map_err(storage)?;
        tx.execute("INSERT INTO invitation_registrations(id,invitation_id,user_id,username,inviter_user_id,inviter_username,grant_payload,policy_revision) VALUES($1,$2,$3,$4,$5,$6,$7,$8)", &[&Uuid::new_v4().to_string(),&id,&user_id,&username,&inviter_id,&inviter_name,&json(&grant)?,&revision]).map_err(storage)?;
        audit(tx, &username, "invitation.registered", &id)?;
        Ok((next, username))
    })?;
    *users = next;
    Ok(registered)
}

#[derive(Default)]
struct RateState {
    start: Option<Instant>,
    total: u32,
    clients: HashMap<IpAddr, u32>,
}
static LIMITER: OnceLock<Mutex<RateState>> = OnceLock::new();
fn trusted_peer(peer: IpAddr) -> bool {
    env::var("PMMAN_TRUSTED_PROXY_IPS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .any(|p| p == peer)
}
fn source_ip_with(request: &Request, trusted: impl FnOnce(IpAddr) -> bool) -> IpAddr {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if trusted(peer) {
        // Reverse proxy must overwrite (not append) X-Real-IP.
        if let Some(ip) = request
            .headers()
            .get("x-real-ip")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse().ok())
        {
            return ip;
        }
    }
    peer
}
pub async fn rate_limit(request: Request, next: Next) -> Response {
    let ip = source_ip_with(&request, trusted_peer);
    let allowed = {
        let mut state = LIMITER
            .get_or_init(|| Mutex::new(RateState::default()))
            .lock()
            .unwrap();
        if state
            .start
            .is_none_or(|t| t.elapsed() >= StdDuration::from_secs(60))
        {
            *state = RateState {
                start: Some(Instant::now()),
                ..Default::default()
            };
        }
        if state.total >= 120 || state.clients.get(&ip).copied().unwrap_or(0) >= 10 {
            false
        } else {
            state.total += 1;
            *state.clients.entry(ip).or_default() += 1;
            true
        }
    };
    if !allowed {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [
                (header::RETRY_AFTER, "60"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Json(ErrorResponse {
                error: "注册请求过于频繁，请稍后重试".into(),
            }),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
pub async fn public_page() -> Response {
    let mut response = admin_response("register.html");
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}

pub async fn sensitive_responses(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let private = path.starts_with("/v1/")
        && (path == "/v1/auth/register"
            || path.contains("/invitations")
            || path.contains("/invitation-registrations")
            || path.ends_with("/invitation-policy"));
    let start = Instant::now();
    let mut response = next.run(request).await;
    if private {
        eprintln!(
            "invitation request status={} elapsed_ms={}",
            response.status().as_u16(),
            start.elapsed().as_millis()
        );
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
    }
    response
}

#[cfg(test)]
#[path = "invitation_postgres_tests.rs"]
mod postgres_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_password_does_not_block_client_invitation_authentication() {
        let state = crate::tests::test_state();
        for username in ["u1", "admin"] {
            state
                .users
                .write()
                .unwrap()
                .get_mut(username)
                .unwrap()
                .must_change_password = true;
            state.sessions.write().unwrap().insert(
                username.into(),
                Session {
                    user_id: username.into(),
                    username: username.into(),
                    expires_at: Utc::now() + Duration::hours(1),
                },
            );
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                format!("Bearer {username}").parse().unwrap(),
            );
            assert!(auth(&state, &headers, false).is_ok());
            assert_eq!(
                auth(&state, &headers, true).err().unwrap().0,
                StatusCode::FORBIDDEN
            );
            state
                .users
                .write()
                .unwrap()
                .get_mut(username)
                .unwrap()
                .enabled = false;
            assert_eq!(
                auth(&state, &headers, false).err().unwrap().0,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            auth(&state, &HeaderMap::new(), false).err().unwrap().0,
            StatusCode::UNAUTHORIZED
        );
    }
    #[test]
    fn defaults_are_least_privilege() {
        let p = InvitationPolicy::default();
        assert!(!p.enabled);
        assert_eq!((p.max_unused, p.daily_limit, p.valid_days), (10, 20, 7));
        assert!(!p.grant.allow_ipv4_global && !p.grant.port_forward.enabled);
        assert!(
            serde_json::from_value::<InvitationGrant>(serde_json::json!({"role":"admin"})).is_err()
        );
    }
    #[test]
    fn codes_are_random_and_only_digests_are_stored() {
        let a = fresh_code();
        let b = fresh_code();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_ne!(code_hash(&a), a);
    }
    #[test]
    fn pagination_and_expiry_are_bounded() {
        assert!(page_query(&InvitationQuery {
            page_size: Some(101),
            ..Default::default()
        })
        .is_err());
        assert!(page_query(&InvitationQuery {
            page: Some(0),
            ..Default::default()
        })
        .is_err());
        assert!(validate_expiry(Some(Utc::now() - Duration::seconds(1)), "").is_err());
    }
    #[test]
    fn forwarded_identity_is_only_used_for_explicit_proxies() {
        let mut r = Request::builder()
            .header("x-real-ip", "198.51.100.2")
            .body(Body::empty())
            .unwrap();
        r.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:40000".parse::<SocketAddr>().unwrap(),
        ));
        assert_eq!(
            source_ip_with(&r, |_| false),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            source_ip_with(&r, |ip| ip.is_loopback()),
            "198.51.100.2".parse::<IpAddr>().unwrap()
        );
    }
    #[tokio::test]
    async fn limiter_bounds_bodies_and_requests_before_password_hashing() {
        use tower::ServiceExt;
        let router = Router::new().route(
            "/register",
            post(|| async { "ok" }).layer(axum::middleware::from_fn(rate_limit)),
        );
        for i in 0..11 {
            let mut request = Request::builder()
                .method("POST")
                .uri("/register")
                .body(Body::empty())
                .unwrap();
            request.extensions_mut().insert(ConnectInfo(
                "192.0.2.250:40000".parse::<SocketAddr>().unwrap(),
            ));
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                if i < 10 {
                    StatusCode::OK
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                }
            );
            if i == 10 {
                assert_eq!(response.headers()[header::RETRY_AFTER], "60");
            }
        }
        let request=Request::builder().method("POST").uri("/v1/auth/register").header("Content-Type","application/json").body(Body::from(serde_json::json!({"username":"oversized","code":"a".repeat(64),"password":"x".repeat(9000)}).to_string())).unwrap();
        assert_eq!(
            crate::app(crate::tests::test_state())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
