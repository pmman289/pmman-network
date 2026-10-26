//! Independent address-family allocation. Intervals, not subnet enumeration,
//! bound the work even for an IPv6 /0 or a list of thousands of host routes.
use super::*;
use std::collections::BTreeMap;

type ApiError = (StatusCode, Json<ErrorResponse>);
fn error(status: StatusCode, message: impl Into<String>) -> ApiError {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
}
fn bad(message: impl Into<String>) -> ApiError {
    error(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> ApiError {
    error(StatusCode::CONFLICT, message)
}
fn storage(message: impl Into<String>) -> ApiError {
    error(StatusCode::SERVICE_UNAVAILABLE, message)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Pool {
    pub pool_id: String,
    pub name: String,
    pub description: String,
    pub segments: Vec<String>,
    pub exclusions: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct FamilyPolicy {
    #[serde(default = "default_address_mode")]
    pub mode: String,
    pub pool_id: Option<String>,
    pub address: Option<String>,
}
impl FamilyPolicy {
    fn automatic() -> Self {
        Self {
            mode: "automatic".into(),
            pool_id: None,
            address: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Policy {
    pub ipv4: FamilyPolicy,
    pub ipv6: FamilyPolicy,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            ipv4: FamilyPolicy::automatic(),
            ipv6: FamilyPolicy::automatic(),
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Lease {
    id: Uuid,
    user_id: String,
    address: IpAddr,
    pool_id: Option<String>,
    state: String,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Registry {
    pub pools: BTreeMap<String, Pool>,
    pub default4: String,
    pub default6: String,
    pub revision: u64,
    pub policies: HashMap<String, Policy>,
    leases: Vec<Lease>,
    forward_cleanup: Vec<ForwardCleanup>,
}

/// Keep administrator routes separate from derived pool routes. Callers
/// already holding the registry lock must pass their snapshot, not re-lock it.
pub(super) fn internal_routes_with_registry(
    state: &AppState,
    registry: Option<&Registry>,
) -> Vec<String> {
    let mut routes = state
        .internal_cidrs
        .read()
        .expect("routes poisoned")
        .clone();
    if let Some(registry) = registry {
        routes.extend(
            registry
                .pools
                .values()
                .flat_map(|pool| pool.segments.clone()),
        );
    }
    let mut seen = HashSet::new();
    routes.retain(|route| seen.insert(route.clone()));
    routes
}

pub(super) fn internal_routes(state: &AppState) -> Vec<String> {
    let registry = state.address_pools.read().expect("pools poisoned");
    internal_routes_with_registry(state, registry.as_ref())
}
#[derive(Clone, Debug)]
struct ForwardCleanup {
    device_id: Uuid,
    forward_id: String,
    node_id: String,
    acknowledged: bool,
}

// Terminal forwarding leases can outlive their rules on an offline node. Only
// an authenticated removal acknowledgement, not a lease timeout, frees an IP.
pub(super) fn prepare_forward_cleanup(
    state: &AppState,
    registry: &mut Option<Registry>,
    users: &HashMap<String, UserAccount>,
    devices: &HashMap<Uuid, Device>,
    device: &Device,
    changed4: bool,
    changed6: bool,
) -> Result<bool, String> {
    let Some(current) = registry.as_ref() else {
        return Ok(true);
    };
    let allocations = state
        .port_forward_store
        .allocations(Some(&device.user_id))?;
    let relevant: Vec<_> = allocations
        .iter()
        .filter(|a| {
            a.device_id == device.device_id
                && ((changed4 && a.target_address == device.ipv4)
                    || (changed6 && a.target_address == device.ipv6))
        })
        .collect();
    let mut next = current.clone();
    let mut changed = false;
    for allocation in &relevant {
        if !next
            .forward_cleanup
            .iter()
            .any(|b| b.device_id == device.device_id && b.forward_id == allocation.forward_id)
        {
            next.forward_cleanup.push(ForwardCleanup {
                device_id: device.device_id,
                forward_id: allocation.forward_id.clone(),
                node_id: allocation.node_id.clone(),
                acknowledged: false,
            });
            changed = true;
        }
    }
    let ready = relevant.iter().all(|a| {
        next.forward_cleanup.iter().any(|b| {
            b.device_id == device.device_id && b.forward_id == a.forward_id && b.acknowledged
        })
    });
    if changed {
        save(state, current.revision, &mut next, users, devices, false)?;
        *registry = Some(next);
    }
    Ok(ready)
}

pub(super) fn acknowledge_forward_cleanup(
    state: &AppState,
    node: &str,
    ids: &[String],
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let devices = state.devices.read().expect("devices poisoned");
    let users = state.users.read().expect("users poisoned");
    let mut registry = state.address_pools.write().expect("address pools poisoned");
    let Some(current) = registry.as_ref() else {
        return Ok(());
    };
    let mut next = current.clone();
    let mut changed = false;
    for barrier in &mut next.forward_cleanup {
        if !barrier.acknowledged && barrier.node_id == node && ids.contains(&barrier.forward_id) {
            barrier.acknowledged = true;
            changed = true;
        }
    }
    if changed {
        save(state, current.revision, &mut next, &users, &devices, false)?;
        *registry = Some(next);
    }
    Ok(())
}

pub(super) fn pending_forward_cleanup(state: &AppState, node: &str) -> Vec<String> {
    state
        .address_pools
        .read()
        .expect("address pools poisoned")
        .as_ref()
        .map(|r| {
            r.forward_cleanup
                .iter()
                .filter(|b| !b.acknowledged && b.node_id == node)
                .map(|b| b.forward_id.clone())
                .collect()
        })
        .unwrap_or_default()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interval {
    family: u8,
    start: u128,
    end: u128,
}
fn ip_value(ip: IpAddr) -> (u8, u128) {
    match ip {
        IpAddr::V4(a) => (4, u32::from(a) as u128),
        IpAddr::V6(a) => (6, u128::from(a)),
    }
}
fn address(family: u8, value: u128) -> IpAddr {
    if family == 4 {
        IpAddr::V4(Ipv4Addr::from(value as u32))
    } else {
        IpAddr::V6(Ipv6Addr::from(value))
    }
}
fn cidr(value: &str) -> Result<(Interval, String, u8), String> {
    let (ip, prefix) = value
        .trim()
        .split_once('/')
        .ok_or("地址段必须包含 CIDR 前缀")?;
    let ip: IpAddr = ip.parse().map_err(|_| "地址段格式无效")?;
    let prefix: u8 = prefix.parse().map_err(|_| "CIDR 前缀无效")?;
    let (family, n) = ip_value(ip);
    let bits = if family == 4 { 32 } else { 128 };
    if prefix > bits {
        return Err("CIDR 前缀超出范围".into());
    }
    let host_mask = if prefix == 0 && bits == 128 {
        u128::MAX
    } else if prefix == bits {
        0
    } else {
        (1u128 << (bits - prefix)) - 1
    };
    let start = n & !host_mask;
    Ok((
        Interval {
            family,
            start,
            end: start | host_mask,
        },
        format!("{}/{}", address(family, start), prefix),
        prefix,
    ))
}
fn merge(mut ranges: Vec<Interval>) -> Vec<Interval> {
    ranges.sort_by_key(|r| (r.family, r.start, r.end));
    let mut result: Vec<Interval> = Vec::new();
    for r in ranges {
        if let Some(last) = result.last_mut() {
            if r.family == last.family
                && (r.start <= last.end || last.end.checked_add(1) == Some(r.start))
            {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        result.push(r);
    }
    result
}
fn subtract(ranges: Vec<Interval>, exclusions: &[Interval]) -> Vec<Interval> {
    let mut result = Vec::new();
    for r in ranges {
        let first = exclusions
            .partition_point(|e| e.family < r.family || (e.family == r.family && e.end < r.start));
        let mut cursor = Some(r.start);
        for e in &exclusions[first..] {
            if e.family != r.family || e.start > r.end {
                break;
            }
            let start = cursor.expect("covered range exits loop");
            if e.start > start {
                result.push(Interval {
                    start,
                    end: e.start - 1,
                    ..r
                });
            }
            if e.end >= r.end {
                cursor = None;
                break;
            }
            cursor = Some(start.max(e.end + 1));
        }
        if let Some(start) = cursor {
            result.push(Interval { start, ..r });
        }
    }
    result
}
fn in_ranges(ip: IpAddr, ranges: &[Interval]) -> bool {
    let (family, n) = ip_value(ip);
    ranges
        .iter()
        .any(|r| r.family == family && r.start <= n && n <= r.end)
}
fn special() -> Vec<Interval> {
    [
        "0.0.0.0/8",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "224.0.0.0/3",
        "::/128",
        "::1/128",
        "::ffff:0:0/96",
        "fe80::/10",
        "ff00::/8",
    ]
    .iter()
    .map(|s| cidr(s).unwrap().0)
    .collect()
}
fn valid_host(value: &str, family: u8) -> Result<IpAddr, String> {
    let ip: IpAddr = value.parse().map_err(|_| "请填写单个 IP 地址，不带前缀")?;
    if ip_value(ip).0 != family || in_ranges(ip, &special()) {
        return Err("地址必须是对应地址族的可用单播地址".into());
    }
    Ok(ip)
}
fn gateways(state: &AppState) -> Vec<Interval> {
    let nodes = state.nodes.read().expect("nodes poisoned");
    nodes
        .values()
        .flat_map(|n| {
            [
                &n.network,
                n.network_override.as_ref().unwrap_or(&n.network),
            ]
        })
        .flat_map(|n| [n.gateway_ipv4.as_ref(), n.gateway_ipv6.as_ref()])
        .flatten()
        .filter_map(|s| s.split('/').next()?.parse::<IpAddr>().ok())
        .map(|ip| {
            let (family, n) = ip_value(ip);
            Interval {
                family,
                start: n,
                end: n,
            }
        })
        .collect()
}
fn allocation_exclusions(state: &AppState, registry: &Registry) -> Vec<Interval> {
    let mut blocked = gateways(state);
    blocked.extend(
        registry
            .pools
            .values()
            .flat_map(|p| p.exclusions.iter())
            .filter_map(|s| cidr(s).ok().map(|c| c.0)),
    );
    merge(blocked)
}
impl Pool {
    fn normalize(&mut self) -> Result<(), String> {
        self.name = self.name.trim().to_string();
        self.description = self.description.trim().to_string();
        if self.name.is_empty() || self.name.len() > 128 || self.description.len() > 2048 {
            return Err("地址池名称或说明长度无效".into());
        }
        if self.segments.is_empty() || self.segments.len() > 20000 || self.exclusions.len() > 20000
        {
            return Err("地址段至少一条，每类最多 20000 条".into());
        }
        for list in [&mut self.segments, &mut self.exclusions] {
            *list = list
                .iter()
                .map(|v| cidr(v).map(|(_, s, _)| s))
                .collect::<Result<Vec<_>, _>>()?;
            list.sort();
            list.dedup();
        }
        Ok(())
    }
    fn raw(&self) -> Vec<Interval> {
        self.segments.iter().map(|s| cidr(s).unwrap().0).collect()
    }
    fn ranges(&self, family: u8, gateways: &[Interval]) -> Vec<Interval> {
        let mut ranges = Vec::new();
        for segment in &self.segments {
            let (mut r, _, prefix) = cidr(segment).unwrap();
            if r.family != family {
                continue;
            }
            if family == 4 && prefix <= 30 {
                r.start += 1;
                r.end -= 1;
            }
            if family == 6 && prefix < 128 {
                r.start += 1;
            }
            ranges.push(r);
        }
        let mut excludes = special();
        excludes.extend_from_slice(gateways);
        excludes.extend(self.exclusions.iter().map(|s| cidr(s).unwrap().0));
        subtract(merge(ranges), &merge(excludes))
    }
}
impl Registry {
    pub fn policy(&self, user: &UserAccount) -> Policy {
        self.policies
            .get(&user.user_id)
            .cloned()
            .unwrap_or_default()
    }
    fn pool_for(&self, policy: &FamilyPolicy, family: u8) -> Result<&Pool, String> {
        let id = policy.pool_id.as_ref().unwrap_or(if family == 4 {
            &self.default4
        } else {
            &self.default6
        });
        self.pools.get(id).ok_or_else(|| "所选地址池不存在".into())
    }
    fn occupied(
        &self,
        users: &HashMap<String, UserAccount>,
        devices: &HashMap<Uuid, Device>,
        owner: &str,
    ) -> HashSet<IpAddr> {
        let (v4, v6) = address_occupancy(users, devices, Some(owner));
        let mut used: HashSet<IpAddr> = v4
            .into_iter()
            .map(|a| IpAddr::V4(a.into()))
            .chain(v6.into_iter().map(|a| IpAddr::V6(a.into())))
            .collect();
        used.extend(
            self.leases
                .iter()
                .filter(|l| l.state != "released" && l.user_id != owner)
                .map(|l| l.address),
        );
        used
    }
    pub fn allocate(
        &self,
        state: &AppState,
        user: &UserAccount,
        users: &HashMap<String, UserAccount>,
        devices: &HashMap<Uuid, Device>,
        old: Option<&(String, String)>,
    ) -> Result<(String, String), String> {
        let policy = self.policy(user);
        let used = self.occupied(users, devices, &user.user_id);
        let gateway = allocation_exclusions(state, self);
        let choose = |p: &FamilyPolicy, family: u8, old: Option<&str>| -> Result<String, String> {
            if p.mode != "automatic" {
                let ip = valid_host(p.address.as_deref().ok_or("必须选择固定地址")?, family)?;
                if used.contains(&ip) || in_ranges(ip, &gateway) {
                    return Err("固定地址已占用、被保留或是节点网关".into());
                }
                if p.mode == "pool_fixed"
                    && !in_ranges(ip, &self.pool_for(p, family)?.ranges(family, &gateway))
                {
                    return Err("所选固定地址已不在地址池可用范围内".into());
                }
                return Ok(ip.to_string());
            }
            let pool = self.pool_for(p, family)?;
            let ranges = pool.ranges(family, &gateway);
            if let Some(ip) = old
                .and_then(|s| s.parse::<IpAddr>().ok())
                .filter(|ip| in_ranges(*ip, &ranges) && !used.contains(ip))
            {
                return Ok(ip.to_string());
            }
            // Skip only occupied hosts. Work is proportional to occupancy,
            // never to the numerical size of an IPv6 subnet.
            for r in &ranges {
                let mut n = r.start;
                loop {
                    let ip = address(family, n);
                    if !used.contains(&ip) {
                        return Ok(ip.to_string());
                    }
                    if n == r.end {
                        break;
                    }
                    n += 1;
                }
            }
            Err(format!("地址池「{}」IPv{} 地址已耗尽", pool.name, family))
        };
        Ok((
            choose(&policy.ipv4, 4, old.map(|p| p.0.as_str()))?,
            choose(&policy.ipv6, 6, old.map(|p| p.1.as_str()))?,
        ))
    }
    pub(super) fn refresh(
        &mut self,
        users: &HashMap<String, UserAccount>,
        devices: &HashMap<Uuid, Device>,
    ) {
        self.policies
            .retain(|id, _| users.values().any(|u| &u.user_id == id));
        let mut wanted: HashMap<(String, IpAddr), (Option<String>, String)> = HashMap::new();
        for user in users.values() {
            let policy = self.policy(user);
            for (family, p, old) in [
                (4, &policy.ipv4, &user.leased_ipv4),
                (6, &policy.ipv6, &user.leased_ipv6),
            ] {
                let pool = if p.mode == "manual" {
                    None
                } else {
                    self.pool_for(p, family).ok().map(|p| p.pool_id.clone())
                };
                if let Some(ip) = p
                    .address
                    .as_ref()
                    .filter(|_| p.mode != "automatic")
                    .and_then(|s| s.parse::<IpAddr>().ok())
                {
                    wanted.insert(
                        (user.user_id.clone(), ip),
                        (pool.clone(), "reserved".into()),
                    );
                }
                if let Some(ip) = old.as_ref().and_then(|s| s.parse::<IpAddr>().ok()) {
                    let origin = self
                        .leases
                        .iter()
                        .find(|l| {
                            l.user_id == user.user_id && l.address == ip && l.state != "released"
                        })
                        .and_then(|l| l.pool_id.clone())
                        .or_else(|| {
                            self.pools
                                .values()
                                .find(|p| in_ranges(ip, &p.raw()))
                                .map(|p| p.pool_id.clone())
                        });
                    wanted
                        .entry((user.user_id.clone(), ip))
                        .or_insert((origin, "allocated".into()));
                }
            }
        }
        for device in devices.values().filter(|d| d.status != "released") {
            for ip in [&device.ipv4, &device.ipv6]
                .into_iter()
                .filter_map(|s| s.parse::<IpAddr>().ok())
            {
                let origin = self
                    .pools
                    .values()
                    .find(|p| in_ranges(ip, &p.raw()))
                    .map(|p| p.pool_id.clone());
                wanted
                    .entry((device.user_id.clone(), ip))
                    .or_insert((origin, "releasing".into()));
            }
        }
        for lease in &mut self.leases {
            if let Some((pool, status)) = wanted.remove(&(lease.user_id.clone(), lease.address)) {
                lease.pool_id = pool;
                lease.state = status;
            } else {
                lease.state = "released".into();
            }
        }
        self.leases.extend(
            wanted
                .into_iter()
                .map(|((user_id, address), (pool_id, state))| Lease {
                    id: Uuid::new_v4(),
                    user_id,
                    address,
                    pool_id,
                    state,
                }),
        );
    }
}

// One transaction publishes the allocation ledger and compatibility documents.
// Existing account writers share the devices -> users -> registry lock order.
pub(super) fn save(
    state: &AppState,
    previous: u64,
    registry: &mut Registry,
    users: &HashMap<String, UserAccount>,
    devices: &HashMap<Uuid, Device>,
    write_documents: bool,
) -> Result<(), String> {
    registry.refresh(users, devices);
    registry.revision = previous.checked_add(1).ok_or("地址池版本溢出")?;
    if let Some(db) = &state.database {
        let next = registry.clone();
        let users = serde_json::to_string(users).map_err(|_| "用户编码失败")?;
        let devices = serde_json::to_string(devices).map_err(|_| "设备编码失败")?;
        db.on_transaction(move |tx| {
            tx.query_one("SELECT pg_advisory_xact_lock(736196540)",&[]).map_err(|_|"地址池事务锁失败")?;
            let current=tx.query_opt("SELECT revision FROM address_pool_defaults WHERE singleton FOR UPDATE",&[]).map_err(|_|"读取地址池版本失败")?.map(|r|r.get::<_,i64>(0) as u64).unwrap_or(0);
            if current!=previous {return Err("地址池版本冲突，请刷新后重试".into());}
            for p in next.pools.values() {
                let mut stored_segments:Vec<String>=tx.query("SELECT segment::text FROM address_pool_segments WHERE pool_id=$1",&[&p.pool_id]).map_err(|_|"读取原地址段失败")?.into_iter().map(|row|row.get(0)).collect();
                stored_segments.sort();
                let mut stored_exclusions:Vec<String>=tx.query("SELECT segment::text FROM address_pool_exclusions WHERE pool_id=$1",&[&p.pool_id]).map_err(|_|"读取原保留地址失败")?.into_iter().map(|row|row.get(0)).collect();
                stored_exclusions.sort();
                tx.execute("INSERT INTO address_pools VALUES($1,$2,$3) ON CONFLICT(pool_id) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description",&[&p.pool_id,&p.name,&p.description]).map_err(|_|"保存地址池失败（名称可能重复）")?;
                let mut segments=p.segments.clone();segments.sort();let mut exclusions=p.exclusions.clone();exclusions.sort();
                if stored_segments!=segments {
                    tx.execute("DELETE FROM address_pool_segments WHERE pool_id=$1",&[&p.pool_id]).map_err(|_|"保存地址段失败")?;
                    for s in &p.segments {tx.execute("INSERT INTO address_pool_segments VALUES($1,$2::text::cidr)",&[&p.pool_id,s]).map_err(|_|"保存地址段失败")?;}
                }
                if stored_exclusions!=exclusions {
                    tx.execute("DELETE FROM address_pool_exclusions WHERE pool_id=$1",&[&p.pool_id]).map_err(|_|"保存保留地址失败")?;
                    for s in &p.exclusions {tx.execute("INSERT INTO address_pool_exclusions VALUES($1,$2::text::cidr)",&[&p.pool_id,s]).map_err(|_|"保存保留地址失败")?;}
                }
            }
            tx.execute("INSERT INTO address_pool_defaults VALUES(TRUE,$1,$2,$3) ON CONFLICT(singleton) DO UPDATE SET ipv4_pool_id=EXCLUDED.ipv4_pool_id,ipv6_pool_id=EXCLUDED.ipv6_pool_id,revision=EXCLUDED.revision",&[&next.default4,&next.default6,&(next.revision as i64)]).map_err(|_|"保存主地址池失败")?;
            tx.execute("DELETE FROM user_address_policies",&[]).map_err(|_|"保存分配策略失败")?;
            for (id,p) in &next.policies {
                tx.execute("INSERT INTO user_address_policies VALUES($1,$2,$3,$4::text::inet,$5,$6,$7::text::inet)",&[id,&p.ipv4.mode,&p.ipv4.pool_id,&p.ipv4.address,&p.ipv6.mode,&p.ipv6.pool_id,&p.ipv6.address]).map_err(|_|"保存分配策略失败")?;
            }
            tx.execute("UPDATE address_leases SET state='released' WHERE state<>'released'",&[]).map_err(|_|"更新地址占用失败")?;
            for l in &next.leases {
                tx.execute("INSERT INTO address_leases VALUES($1,$2,$3::text::inet,$4,$5,CURRENT_TIMESTAMP) ON CONFLICT(lease_id) DO UPDATE SET pool_id=EXCLUDED.pool_id,state=EXCLUDED.state,updated_at=CURRENT_TIMESTAMP",&[&l.id,&l.user_id,&l.address.to_string(),&l.pool_id,&l.state]).map_err(|_|"地址已被占用或租约保存失败")?;
            }
            for barrier in &next.forward_cleanup {
                tx.execute("INSERT INTO address_forward_cleanup VALUES($1,$2,$3,$4) ON CONFLICT(device_id,forward_id) DO UPDATE SET acknowledged=EXCLUDED.acknowledged",&[&barrier.device_id,&barrier.forward_id,&barrier.node_id,&barrier.acknowledged]).map_err(|_|"保存转发清理确认失败")?;
            }
            let ids:Vec<String>=next.pools.keys().cloned().collect();
            tx.execute("DELETE FROM address_pools WHERE NOT(pool_id=ANY($1))",&[&ids]).map_err(|_|"地址池仍有引用，无法删除")?;
            if write_documents {
                for (key,value) in [("users",users),("devices",devices)] {tx.execute("INSERT INTO pmman_documents(document_key,payload) VALUES($1,$2) ON CONFLICT(document_key) DO UPDATE SET payload=EXCLUDED.payload,revision=pmman_documents.revision+1,updated_at=CURRENT_TIMESTAMP",&[&key,&value]).map_err(|_|"保存地址所属账号与设备失败")?;}
            }
            tx.execute("INSERT INTO pmman_audit_events(event_type,resource_type) VALUES('address-pools.updated','address-pools')",&[]).map_err(|_|"写入地址池审计失败")?;
            Ok(())
        })?;
    }
    Ok(())
}

pub(super) fn initialize(state: &AppState) -> Result<(), String> {
    let mut registry = Registry::default();
    if let Some(db) = &state.database {
        registry=db.on_thread(|client| {
            let mut r=Registry::default();
            if let Some(row)=client.query_opt("SELECT ipv4_pool_id,ipv6_pool_id,revision FROM address_pool_defaults WHERE singleton",&[]).map_err(|_|"读取主地址池失败")? {
                r.default4=row.get(0);r.default6=row.get(1);r.revision=row.get::<_,i64>(2) as u64;
                for row in client.query("SELECT pool_id,name,description FROM address_pools ORDER BY pool_id",&[]).map_err(|_|"读取地址池失败")? {
                    let id:String=row.get(0);r.pools.insert(id.clone(),Pool{pool_id:id,name:row.get(1),description:row.get(2),segments:vec![],exclusions:vec![]});
                }
                for (table,excluded) in [("address_pool_segments",false),("address_pool_exclusions",true)] {
                    for row in client.query(&format!("SELECT pool_id,segment::text FROM {table} ORDER BY segment"),&[]).map_err(|_|"读取地址段失败")? {
                        let id:String=row.get(0);let p=r.pools.get_mut(&id).ok_or("地址池关联损坏")?;
                        if excluded {p.exclusions.push(row.get(1));} else {p.segments.push(row.get(1));}
                    }
                }
                for row in client.query("SELECT user_id,ipv4_mode,ipv4_pool_id,host(ipv4_address),ipv6_mode,ipv6_pool_id,host(ipv6_address) FROM user_address_policies",&[]).map_err(|_|"读取分配策略失败")? {
                    r.policies.insert(row.get(0),Policy{ipv4:FamilyPolicy{mode:row.get(1),pool_id:row.get(2),address:row.get(3)},ipv6:FamilyPolicy{mode:row.get(4),pool_id:row.get(5),address:row.get(6)}});
                }
                for row in client.query("SELECT lease_id,user_id,host(address),pool_id,state FROM address_leases",&[]).map_err(|_|"读取地址租约失败")? {
                    let ip:String=row.get(2);r.leases.push(Lease{id:row.get(0),user_id:row.get(1),address:ip.parse().map_err(|_|"地址租约损坏")?,pool_id:row.get(3),state:row.get(4)});
                }
                for row in client.query("SELECT device_id,forward_id,node_id,acknowledged FROM address_forward_cleanup",&[]).map_err(|_|"读取转发清理确认失败")? {
                    r.forward_cleanup.push(ForwardCleanup{device_id:row.get(0),forward_id:row.get(1),node_id:row.get(2),acknowledged:row.get(3)});
                }
            } else {
                let partial:bool=client.query_one("SELECT EXISTS(SELECT 1 FROM address_pools) OR EXISTS(SELECT 1 FROM user_address_policies) OR EXISTS(SELECT 1 FROM address_leases)",&[]).map_err(|_|"检查地址池迁移状态失败")?.get(0);
                if partial { return Err("地址池主配置缺失，请恢复数据库备份；拒绝重新生成默认池".into()); }
            }
            Ok(r)
        })?;
    }
    if registry.revision == 0 {
        let old = state.pool.read().expect("pool poisoned");
        let id = Uuid::new_v4().to_string();
        registry.pools.insert(
            id.clone(),
            Pool {
                pool_id: id.clone(),
                name: "主地址池".into(),
                description: "由原地址池迁移".into(),
                segments: vec![old.ipv4_cidr(), old.ipv6_cidr()],
                exclusions: vec![
                    format!("{}/32", Ipv4Addr::from(old.ipv4_network + 1)),
                    format!("{}/128", Ipv6Addr::from(old.ipv6_network + 1)),
                ],
            },
        );
        registry.default4 = id.clone();
        registry.default6 = id;
        let users = state.users.read().expect("users poisoned");
        let devices = state.devices.read().expect("devices poisoned");
        for u in users.values().filter(|u| u.address_mode == "fixed") {
            let convert = |s: &Option<String>, f| -> FamilyPolicy {
                let value = s.as_ref().unwrap();
                let pool = registry.pools.values().next().unwrap();
                let inside = value
                    .parse()
                    .is_ok_and(|ip| in_ranges(ip, &pool.ranges(f, &[])));
                FamilyPolicy {
                    mode: if inside { "pool_fixed" } else { "manual" }.into(),
                    pool_id: if inside {
                        Some(pool.pool_id.clone())
                    } else {
                        None
                    },
                    address: Some(value.clone()),
                }
            };
            registry.policies.insert(
                u.user_id.clone(),
                Policy {
                    ipv4: convert(&u.reserved_ipv4, 4),
                    ipv6: convert(&u.reserved_ipv6, 6),
                },
            );
        }
        save(state, 0, &mut registry, &users, &devices, false)?;
    }
    validate(&registry)?;
    *state.address_pools.write().expect("pools poisoned") = Some(registry);
    Ok(())
}
fn validate(r: &Registry) -> Result<(), String> {
    let mut ownership = Vec::new();
    for p in r.pools.values() {
        for s in p.segments.iter().chain(&p.exclusions) {
            cidr(s)?;
        }
        ownership.extend(merge(p.raw()).into_iter().map(|range| (range, p)));
    }
    ownership.sort_by_key(|(range, _)| (range.family, range.start));
    for pair in ownership.windows(2) {
        let (a, p) = pair[0];
        let (b, other) = pair[1];
        if a.family == b.family && b.start <= a.end && p.pool_id != other.pool_id {
            return Err(format!(
                "地址池「{}」与「{}」地址段重叠",
                p.name, other.name
            ));
        }
    }
    for (family, id) in [(4, &r.default4), (6, &r.default6)] {
        let p = r.pools.get(id).ok_or("主地址池不存在")?;
        if p.ranges(family, &[]).is_empty() {
            return Err(format!("IPv{family} 主地址池没有可分配地址段"));
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub(super) struct Stats {
    capacity: String,
    allocated: String,
    releasing: String,
    available: String,
}
#[derive(Serialize)]
pub(super) struct PoolView {
    #[serde(flatten)]
    pool: Pool,
    ipv4: Stats,
    ipv6: Stats,
    warnings: Vec<String>,
}
#[derive(Serialize)]
pub(super) struct PoolList {
    revision: u64,
    ipv4_pool_id: String,
    ipv6_pool_id: String,
    pools: Vec<PoolView>,
}
fn stats(p: &Pool, f: u8, r: &Registry, g: &[Interval]) -> Stats {
    let ranges = p.ranges(f, g);
    let capacity: u128 = ranges.iter().map(|r| r.end - r.start + 1).sum();
    let allocated: HashSet<_> = r
        .leases
        .iter()
        .filter(|l| {
            l.state != "released"
                && ip_value(l.address).0 == f
                && (l.pool_id.as_ref() == Some(&p.pool_id) || in_ranges(l.address, &ranges))
        })
        .map(|l| l.address)
        .collect();
    let releasing: HashSet<_> = r
        .leases
        .iter()
        .filter(|l| {
            l.state == "releasing"
                && ip_value(l.address).0 == f
                && (l.pool_id.as_ref() == Some(&p.pool_id) || in_ranges(l.address, &ranges))
        })
        .map(|l| l.address)
        .collect();
    Stats {
        capacity: capacity.to_string(),
        allocated: allocated.len().to_string(),
        releasing: releasing.len().to_string(),
        available: capacity
            .saturating_sub(
                allocated
                    .iter()
                    .filter(|ip| in_ranges(**ip, &ranges))
                    .count() as u128,
            )
            .to_string(),
    }
}
fn list_view(state: &AppState, r: &Registry) -> PoolList {
    let g = allocation_exclusions(state, r);
    PoolList {
        revision: r.revision,
        ipv4_pool_id: r.default4.clone(),
        ipv6_pool_id: r.default6.clone(),
        pools: r
            .pools
            .values()
            .map(|p| PoolView {
                pool: p.clone(),
                ipv4: stats(p, 4, r, &g),
                ipv6: stats(p, 6, r, &g),
                // Every pool segment is now included in the effective routes.
                warnings: Vec::new(),
            })
            .collect(),
    }
}
pub(super) fn compatibility_settings(
    state: &AppState,
    users: &HashMap<String, UserAccount>,
    devices: &HashMap<Uuid, Device>,
) -> Option<PoolSettings> {
    let guard = state.address_pools.read().expect("pools poisoned");
    let mut r = guard.as_ref()?.clone();
    r.refresh(users, devices);
    let g = allocation_exclusions(state, &r);
    let p4 = r.pools.get(&r.default4)?;
    let p6 = r.pools.get(&r.default6)?;
    let s4 = stats(p4, 4, &r, &g);
    let s6 = stats(p6, 6, &r, &g);
    let capacity = s4
        .capacity
        .parse::<u128>()
        .ok()?
        .min(s6.capacity.parse::<u128>().ok()?)
        .min(u64::MAX as u128) as u64;
    let available = s4
        .available
        .parse::<u128>()
        .ok()?
        .min(s6.available.parse::<u128>().ok()?)
        .min(capacity as u128) as u64;
    Some(PoolSettings {
        ipv4_pool: p4
            .segments
            .iter()
            .filter(|s| cidr(s).unwrap().0.family == 4)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        ipv6_pool: p6
            .segments
            .iter()
            .filter(|s| cidr(s).unwrap().0.family == 6)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        internal_cidrs: state.internal_cidrs.read().unwrap().clone(),
        capacity,
        allocated: capacity - available,
        available,
    })
}
fn admin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    require_admin(state, headers)
        .map(|_| ())
        .map_err(|_| error(StatusCode::FORBIDDEN, "需要管理员权限"))
}
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PoolList>, ApiError> {
    admin(&state, &headers)?;
    let devices = state.devices.read().unwrap();
    let users = state.users.read().unwrap();
    let guard = state.address_pools.read().unwrap();
    let mut r = guard
        .as_ref()
        .ok_or_else(|| storage("地址池尚未初始化"))?
        .clone();
    r.refresh(&users, &devices);
    Ok(Json(list_view(&state, &r)))
}
#[derive(Deserialize)]
pub(super) struct PoolRequest {
    revision: u64,
    name: String,
    #[serde(default)]
    description: String,
    segments: Vec<String>,
    #[serde(default)]
    exclusions: Vec<String>,
}
pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<PoolRequest>,
) -> Result<Json<PoolList>, ApiError> {
    edit_pool(state, headers, None, input)
}
pub(super) async fn edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<PoolRequest>,
) -> Result<Json<PoolList>, ApiError> {
    edit_pool(state, headers, Some(id), input)
}
fn edit_pool(
    state: AppState,
    headers: HeaderMap,
    id: Option<String>,
    input: PoolRequest,
) -> Result<Json<PoolList>, ApiError> {
    admin(&state, &headers)?;
    let devices = state.devices.read().unwrap();
    let users = state.users.read().unwrap();
    let mut guard = state.address_pools.write().unwrap();
    let current = guard.as_ref().ok_or_else(|| storage("地址池尚未初始化"))?;
    if input.revision != current.revision {
        return Err(conflict("配置已变更，请刷新后重试"));
    }
    if id
        .as_ref()
        .is_some_and(|id| !current.pools.contains_key(id))
    {
        return Err(error(StatusCode::NOT_FOUND, "地址池不存在"));
    }
    let mut p = Pool {
        pool_id: id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: input.name,
        description: input.description,
        segments: input.segments,
        exclusions: input.exclusions,
    };
    p.normalize().map_err(bad)?;
    if current
        .pools
        .values()
        .any(|o| o.pool_id != p.pool_id && o.name == p.name)
    {
        return Err(conflict("地址池名称已存在"));
    }
    let mut next = current.clone();
    next.pools.insert(p.pool_id.clone(), p.clone());
    validate(&next).map_err(bad)?;
    for u in users.values() {
        let policy = next.policy(u);
        for (family, fp) in [(4, &policy.ipv4), (6, &policy.ipv6)] {
            if fp.mode == "pool_fixed"
                && next.pool_for(fp, family).map_err(bad)?.pool_id == p.pool_id
            {
                let ip = valid_host(
                    fp.address
                        .as_deref()
                        .ok_or_else(|| bad("固定策略缺少地址"))?,
                    family,
                )
                .map_err(bad)?;
                if !in_ranges(ip, &p.ranges(family, &allocation_exclusions(&state, &next))) {
                    return Err(conflict(format!(
                        "修改会移除用户 {} 的池内固定地址 {}",
                        u.username, ip
                    )));
                }
            }
        }
    }
    save(&state, current.revision, &mut next, &users, &devices, false).map_err(storage)?;
    let result = list_view(&state, &next);
    *guard = Some(next);
    Ok(Json(result))
}
#[derive(Deserialize)]
pub(super) struct DefaultsRequest {
    revision: u64,
    ipv4_pool_id: String,
    ipv6_pool_id: String,
}
pub(super) async fn defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<DefaultsRequest>,
) -> Result<Json<PoolList>, ApiError> {
    admin(&state, &headers)?;
    let devices = state.devices.read().unwrap();
    let users = state.users.read().unwrap();
    let mut guard = state.address_pools.write().unwrap();
    let current = guard.as_ref().ok_or_else(|| storage("地址池尚未初始化"))?;
    if current.revision != input.revision {
        return Err(conflict("配置已变更，请刷新后重试"));
    }
    let mut next = current.clone();
    next.default4 = input.ipv4_pool_id;
    next.default6 = input.ipv6_pool_id;
    validate(&next).map_err(bad)?;
    // Following-primary fixed addresses must remain valid after a default change.
    for u in users.values() {
        let p = next.policy(u);
        for (f, p) in [(4, &p.ipv4), (6, &p.ipv6)] {
            if p.mode == "pool_fixed"
                && p.pool_id.is_none()
                && !in_ranges(
                    valid_host(p.address.as_deref().ok_or_else(|| bad("固定地址缺失"))?, f)
                        .map_err(bad)?,
                    &next
                        .pool_for(p, f)
                        .map_err(bad)?
                        .ranges(f, &allocation_exclusions(&state, &next)),
                )
            {
                return Err(conflict(format!(
                    "用户 {} 的固定地址不在新主池中，请先调整其策略",
                    u.username
                )));
            }
        }
    }
    save(&state, current.revision, &mut next, &users, &devices, false).map_err(storage)?;
    let view = list_view(&state, &next);
    *guard = Some(next);
    Ok(Json(view))
}
#[derive(Deserialize)]
pub(super) struct VersionQuery {
    revision: u64,
}
pub(super) async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<VersionQuery>,
) -> Result<StatusCode, ApiError> {
    admin(&state, &headers)?;
    let devices = state.devices.read().unwrap();
    let users = state.users.read().unwrap();
    let mut guard = state.address_pools.write().unwrap();
    let current = guard.as_ref().ok_or_else(|| storage("地址池尚未初始化"))?;
    if query.revision != current.revision {
        return Err(conflict("配置已变更，请刷新后重试"));
    }
    if !current.pools.contains_key(&id) {
        return Err(error(StatusCode::NOT_FOUND, "地址池不存在"));
    }
    let mut next = current.clone();
    next.refresh(&users, &devices);
    let mut blockers = Vec::new();
    if id == next.default4 || id == next.default6 {
        blockers.push("主地址池".to_string());
    }
    for u in users.values() {
        let p = next.policy(u);
        if p.ipv4.pool_id.as_ref() == Some(&id) || p.ipv6.pool_id.as_ref() == Some(&id) {
            blockers.push(format!("用户 {}", u.username));
        }
    }
    if next
        .leases
        .iter()
        .any(|l| l.pool_id.as_ref() == Some(&id) && l.state != "released")
    {
        blockers.push("未释放租约".into());
    }
    if !blockers.is_empty() {
        return Err(conflict(format!(
            "无法删除，仍有引用：{}",
            blockers.join("、")
        )));
    }
    next.pools.remove(&id);
    for l in &mut next.leases {
        if l.pool_id.as_ref() == Some(&id) {
            l.pool_id = None;
        }
    }
    save(&state, current.revision, &mut next, &users, &devices, false).map_err(storage)?;
    *guard = Some(next);
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Serialize)]
pub(super) struct PolicyView {
    revision: u64,
    #[serde(flatten)]
    policy: Policy,
    current_ipv4: Option<String>,
    current_ipv6: Option<String>,
}
pub(super) async fn get_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<Json<PolicyView>, ApiError> {
    admin(&state, &headers)?;
    let users = state.users.read().unwrap();
    let u = users
        .get(&username)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "用户不存在"))?;
    let guard = state.address_pools.read().unwrap();
    let r = guard.as_ref().ok_or_else(|| storage("地址池尚未初始化"))?;
    Ok(Json(PolicyView {
        revision: r.revision,
        policy: r.policy(u),
        current_ipv4: u.leased_ipv4.clone(),
        current_ipv6: u.leased_ipv6.clone(),
    }))
}
#[derive(Deserialize)]
pub(super) struct PolicyRequest {
    revision: u64,
    #[serde(flatten)]
    policy: Policy,
}
pub(super) async fn set_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(mut input): Json<PolicyRequest>,
) -> Result<Json<PolicyView>, ApiError> {
    admin(&state, &headers)?;
    let devices = state.devices.read().unwrap();
    let mut users = state.users.write().unwrap();
    let mut guard = state.address_pools.write().unwrap();
    let u = users
        .get(&username)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "用户不存在"))?;
    let current = guard.as_ref().ok_or_else(|| storage("地址池尚未初始化"))?;
    if current.revision != input.revision {
        return Err(conflict("配置已变更，请刷新后重试"));
    }
    // Deletion cleanup can release a device without an address-settings write.
    // Validate against the current owners, not the ledger's cached tombstones.
    let mut occupancy = current.clone();
    occupancy.refresh(&users, &devices);
    let used = occupancy.occupied(&users, &devices, &u.user_id);
    let g = allocation_exclusions(&state, current);
    for (family, p) in [(4, &mut input.policy.ipv4), (6, &mut input.policy.ipv6)] {
        if !["automatic", "pool_fixed", "manual"].contains(&p.mode.as_str()) {
            return Err(bad("分配方式无效"));
        }
        if p.mode == "automatic" {
            if p.address.is_some() {
                return Err(bad("自动模式不能指定固定地址"));
            }
            current.pool_for(p, family).map_err(bad)?;
        } else {
            let ip = valid_host(
                p.address
                    .as_deref()
                    .ok_or_else(|| bad(format!("请选择 IPv{family} 固定地址")))?,
                family,
            )
            .map_err(bad)?;
            if used.contains(&ip) || in_ranges(ip, &g) {
                return Err(conflict(format!("地址 {ip} 已占用、被保留或是节点网关")));
            }
            if p.mode == "pool_fixed"
                && !in_ranges(
                    ip,
                    &current.pool_for(p, family).map_err(bad)?.ranges(family, &g),
                )
            {
                return Err(bad("固定地址不属于所选地址池的可用范围"));
            }
            if p.mode == "manual" {
                if p.pool_id.is_some() {
                    return Err(bad("手动指定不能同时绑定地址池"));
                }
            }
            p.address = Some(ip.to_string());
        }
        if p.mode != "manual"
            && current
                .pool_for(p, family)
                .map_err(bad)?
                .ranges(family, &g)
                .is_empty()
        {
            return Err(bad(format!("所选池没有 IPv{family} 可用地址段")));
        }
    }
    let mut next = current.clone();
    next.policies
        .insert(u.user_id.clone(), input.policy.clone());
    let mut next_users = users.clone();
    let user = next_users.get_mut(&username).unwrap();
    // Keep legacy documents readable by old clients. Per-family policies are
    // authoritative; mixed mode cannot be represented as the old fixed pair.
    if input.policy.ipv4.mode != "automatic" && input.policy.ipv6.mode != "automatic" {
        user.address_mode = "fixed".into();
        user.reserved_ipv4 = input.policy.ipv4.address.clone();
        user.reserved_ipv6 = input.policy.ipv6.address.clone();
    } else {
        user.address_mode = "automatic".into();
        user.reserved_ipv4 = None;
        user.reserved_ipv6 = None;
    }
    let view = PolicyView {
        revision: current.revision + 1,
        policy: input.policy,
        current_ipv4: user.leased_ipv4.clone(),
        current_ipv6: user.leased_ipv6.clone(),
    };
    save(
        &state,
        current.revision,
        &mut next,
        &next_users,
        &devices,
        true,
    )
    .map_err(storage)?;
    *users = next_users;
    *guard = Some(next);
    Ok(Json(view))
}

#[derive(Deserialize)]
pub(super) struct AddressQuery {
    family: u8,
    segment: Option<String>,
    status: Option<String>,
    address: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    user_id: Option<String>,
}
#[derive(Serialize)]
pub(super) struct AddressRow {
    address: String,
    status: String,
    user_id: Option<String>,
}
#[derive(Serialize)]
pub(super) struct AddressPage {
    revision: u64,
    items: Vec<AddressRow>,
    next_cursor: Option<String>,
}
pub(super) async fn addresses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AddressQuery>,
) -> Result<Json<AddressPage>, ApiError> {
    admin(&state, &headers)?;
    if ![4, 6].contains(&q.family)
        || !["available", "occupied", "all"].contains(&q.status.as_deref().unwrap_or("available"))
    {
        return Err(bad("地址族或状态无效"));
    }
    let devices = state.devices.read().unwrap();
    let users = state.users.read().unwrap();
    let guard = state.address_pools.read().unwrap();
    let current = guard.as_ref().ok_or_else(|| storage("地址池尚未初始化"))?;
    let mut r = current.clone();
    r.refresh(&users, &devices);
    let pool = r
        .pools
        .get(&id)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "地址池不存在"))?;
    let mut ranges = pool.ranges(q.family, &allocation_exclusions(&state, &r));
    if let Some(segment) = &q.segment {
        let filter = cidr(segment).map_err(bad)?.0;
        ranges = ranges
            .into_iter()
            .filter_map(|a| {
                if a.family == filter.family && a.start <= filter.end && filter.start <= a.end {
                    Some(Interval {
                        start: a.start.max(filter.start),
                        end: a.end.min(filter.end),
                        ..a
                    })
                } else {
                    None
                }
            })
            .collect();
    }
    let occupied: HashMap<IpAddr, &Lease> = r
        .leases
        .iter()
        .filter(|l| l.state != "released")
        .map(|l| (l.address, l))
        .collect();
    let row = |ip: IpAddr| -> AddressRow {
        let l = occupied.get(&ip);
        AddressRow {
            address: ip.to_string(),
            status: l
                .map(|l| {
                    if q.user_id.as_ref() == Some(&l.user_id) {
                        "owned"
                    } else {
                        &l.state
                    }
                })
                .unwrap_or("available")
                .into(),
            user_id: l.map(|l| l.user_id.clone()),
        }
    };
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let mut items = Vec::new();
    let after = q
        .cursor
        .as_deref()
        .map(|s| valid_host(s, q.family))
        .transpose()
        .map_err(bad)?
        .map(|ip| ip_value(ip).1);
    let allowed = |ip: IpAddr| match q.status.as_deref().unwrap_or("available") {
        "occupied" => occupied.contains_key(&ip),
        "all" => true,
        _ => {
            !occupied.contains_key(&ip)
                || occupied
                    .get(&ip)
                    .is_some_and(|l| q.user_id.as_ref() == Some(&l.user_id))
        }
    };
    if let Some(value) = q.address {
        let ip = valid_host(&value, q.family).map_err(bad)?;
        if in_ranges(ip, &ranges) && allowed(ip) {
            items.push(row(ip));
        }
    } else if q.status.as_deref() == Some("occupied") {
        let mut ips: Vec<_> = occupied
            .keys()
            .copied()
            .filter(|ip| in_ranges(*ip, &ranges) && after.is_none_or(|n| ip_value(*ip).1 > n))
            .collect();
        ips.sort_by_key(|ip| ip_value(*ip).1);
        items = ips.into_iter().take(limit + 1).map(row).collect();
    } else {
        'outer: for range in ranges {
            let mut n = match after {
                Some(a) if a >= range.end => continue,
                Some(a) if a >= range.start => a + 1,
                _ => range.start,
            };
            loop {
                let ip = address(q.family, n);
                if allowed(ip) {
                    items.push(row(ip));
                    if items.len() > limit {
                        break 'outer;
                    }
                }
                if n == range.end {
                    break;
                }
                n += 1;
            }
        }
    }
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|r| r.address.clone())
    } else {
        None
    };
    Ok(Json(AddressPage {
        revision: r.revision,
        items,
        next_cursor,
    }))
}

pub(super) async fn save_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<RoutesRequest>,
) -> Result<Json<PoolSettings>, ApiError> {
    admin(&state, &headers)?;
    let routes =
        normalize_internal_cidrs(input.internal_cidrs.iter().map(String::as_str)).map_err(bad)?;
    let pool = state.pool.read().unwrap();
    let mut current = state.internal_cidrs.write().unwrap();
    persist_network_settings(
        &state,
        &NetworkSettings {
            ipv4_pool: pool.ipv4_cidr(),
            ipv6_pool: pool.ipv6_cidr(),
            internal_cidrs: routes.clone(),
        },
    )
    .map_err(storage)?;
    *current = routes;
    drop(current);
    drop(pool);
    Ok(Json(pool_settings(&state)))
}
#[derive(Deserialize)]
pub(super) struct RoutesRequest {
    internal_cidrs: Vec<String>,
}

#[cfg(test)]
#[path = "address_pool_tests.rs"]
mod tests;
