use chrono::{DateTime, Duration, Utc};
use postgres::{Client, GenericClient, NoTls, Row};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};
use uuid::Uuid;

use pmman_protocol::PortForwardProtocol;

pub const GRACE_SECONDS: i64 = 300;
// Allocation checks (per-user quota and port selection) must be one critical
// section inside a single master process. PostgreSQL's unique index still
// protects the final insert, but without this lock two concurrent requests can
// both pass the quota check and create more leases than the policy allows.
// The control plane is intentionally single-master; a future multi-master
// deployment must move this whole check into a PostgreSQL transaction.
static ALLOCATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn allocation_lock() -> &'static Mutex<()> {
    ALLOCATION_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone)]
pub struct PortForwardStore {
    backend: Backend,
}

#[derive(Clone)]
enum Backend {
    Postgres(Arc<String>),
    #[cfg(test)]
    Memory(Arc<Mutex<MemoryState>>),
}

#[cfg(test)]
#[derive(Default)]
struct MemoryState {
    entries: HashMap<String, EntryRecord>,
    policies: HashMap<String, PolicyRecord>,
    allocations: HashMap<String, AllocationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryRecord {
    pub entry_id: String,
    pub node_id: String,
    pub name: String,
    pub public_address: String,
    pub hostname: Option<String>,
    pub family: String,
    pub protocols: Vec<PortForwardProtocol>,
    pub ranges: Vec<PortRange>,
    pub reserved_ports: Vec<u16>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyRecord {
    pub enabled: bool,
    pub max_active_forwards: u32,
    pub allowed_entry_ids: Vec<String>,
    pub allowed_protocols: Vec<PortForwardProtocol>,
    pub allowed_target_ports: Option<Vec<u16>>,
    pub allowed_families: Vec<String>,
}

impl Default for PolicyRecord {
    fn default() -> Self {
        Self {
            enabled: false,
            max_active_forwards: 0,
            allowed_entry_ids: Vec::new(),
            allowed_protocols: Vec::new(),
            allowed_target_ports: None,
            allowed_families: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AllocationRecord {
    pub forward_id: String,
    pub user_id: String,
    pub username: String,
    pub device_id: Uuid,
    pub entry_id: String,
    pub node_id: String,
    pub protocols: Vec<PortForwardProtocol>,
    pub public_port: u16,
    pub target_address: String,
    pub target_port: u16,
    pub state: String,
    pub config_version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

impl PortForwardStore {
    /// Open the production PostgreSQL backend. A filesystem path is never
    /// accepted here, which prevents accidental reintroduction of SQLite.
    pub fn open(url: &str) -> Result<Self, String> {
        Client::connect(url, NoTls)
            .map_err(|error| format!("无法连接端口转发 PostgreSQL：{error}"))?;
        let store = Self {
            backend: Backend::Postgres(Arc::new(url.to_owned())),
        };
        store.initialize()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self, String> {
        Ok(Self {
            backend: Backend::Memory(Arc::new(Mutex::new(MemoryState::default()))),
        })
    }

    fn initialize(&self) -> Result<(), String> {
        let url = match &self.backend {
            Backend::Postgres(url) => url,
            #[cfg(test)]
            Backend::Memory(_) => return Ok(()),
        };
        pg_with(url, |client| {
            client.batch_execute(
            "CREATE TABLE IF NOT EXISTS forward_entries (
                entry_id TEXT PRIMARY KEY, node_id TEXT NOT NULL, name TEXT NOT NULL,
                public_address TEXT NOT NULL, hostname TEXT, family TEXT NOT NULL,
                protocols TEXT NOT NULL, enabled BOOLEAN NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS forward_entry_ranges (
                entry_id TEXT NOT NULL REFERENCES forward_entries(entry_id) ON DELETE CASCADE,
                start_port INTEGER NOT NULL, end_port INTEGER NOT NULL,
                PRIMARY KEY(entry_id, start_port, end_port)
            );
            CREATE TABLE IF NOT EXISTS forward_entry_reserved_ports (
                entry_id TEXT NOT NULL REFERENCES forward_entries(entry_id) ON DELETE CASCADE,
                port INTEGER NOT NULL, PRIMARY KEY(entry_id, port)
            );
            CREATE TABLE IF NOT EXISTS user_forward_policies (
                user_id TEXT PRIMARY KEY, enabled BOOLEAN NOT NULL,
                max_active_forwards INTEGER NOT NULL, allowed_entry_ids TEXT NOT NULL,
                allowed_protocols TEXT NOT NULL, allowed_target_ports TEXT,
                allowed_families TEXT NOT NULL, updated_at TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE IF NOT EXISTS port_forward_allocations (
                forward_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, username TEXT NOT NULL,
                device_id UUID NOT NULL, entry_id TEXT NOT NULL REFERENCES forward_entries(entry_id),
                node_id TEXT NOT NULL, protocols TEXT NOT NULL, public_port INTEGER NOT NULL,
                target_address TEXT NOT NULL, target_port INTEGER NOT NULL, state TEXT NOT NULL,
                config_version BIGINT NOT NULL, created_at TIMESTAMPTZ NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL, expires_at TIMESTAMPTZ, error_message TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pf_alloc_live_port
                ON port_forward_allocations(entry_id, public_port)
                WHERE state NOT IN ('released','expired');
            CREATE INDEX IF NOT EXISTS idx_pf_alloc_node_state
                ON port_forward_allocations(node_id, state);
            CREATE INDEX IF NOT EXISTS idx_pf_alloc_user_state
                ON port_forward_allocations(user_id, state);
            CREATE TABLE IF NOT EXISTS port_forward_events (
                event_id BIGSERIAL PRIMARY KEY, forward_id TEXT,
                event_type TEXT NOT NULL, detail TEXT, created_at TIMESTAMPTZ NOT NULL
            );
            -- The uniqueness rule was added after early deployments could
            -- create duplicate live mappings. Keep the newest allocation in
            -- each node/device/target-port group and release older rows
            -- before creating the index, so a production restart cannot fail
            -- merely because legacy data needs normalizing.
            UPDATE port_forward_allocations AS allocation
            SET state = 'released',
                updated_at = CURRENT_TIMESTAMP,
                expires_at = NULL,
                error_message = COALESCE(
                    allocation.error_message,
                    '历史重复转发规则，已按节点、设备和目标端口去重'
                )
            WHERE allocation.forward_id IN (
                SELECT forward_id
                FROM (
                    SELECT forward_id,
                           ROW_NUMBER() OVER (
                               PARTITION BY node_id, device_id, target_port
                               ORDER BY created_at DESC, forward_id DESC
                           ) AS duplicate_rank
                    FROM port_forward_allocations
                    WHERE state NOT IN ('released', 'expired')
                ) AS ranked
                WHERE ranked.duplicate_rank > 1
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_pf_alloc_live_node_device_target
                ON port_forward_allocations(node_id, device_id, target_port)
                WHERE state NOT IN ('released','expired');
            INSERT INTO schema_migrations(version)
                VALUES (2) ON CONFLICT (version) DO NOTHING;
            ",
        )
        .map_err(|error| format!("初始化端口转发 PostgreSQL 表失败：{error}"))
        })
    }

    pub fn entry(&self, entry_id: &str) -> Result<Option<EntryRecord>, String> {
        match &self.backend {
            Backend::Postgres(url) => {
                let entry_id = entry_id.to_owned();
                pg_with(url, move |client| load_entry(client, &entry_id))
            }
            #[cfg(test)]
            Backend::Memory(state) => Ok(state
                .lock()
                .map_err(|_| "端口转发内存锁损坏".to_string())?
                .entries
                .get(entry_id)
                .cloned()),
        }
    }

    pub fn entries(&self) -> Result<Vec<EntryRecord>, String> {
        match &self.backend {
            Backend::Postgres(url) => pg_with(url, |client| {
                let ids = client
                    .query(
                        "SELECT entry_id FROM forward_entries ORDER BY entry_id",
                        &[],
                    )
                    .map_err(|e| e.to_string())?;
                let mut entries = Vec::new();
                for row in ids {
                    if let Some(entry) = load_entry(client, row.get(0))? {
                        entries.push(entry);
                    }
                }
                Ok(entries)
            }),
            #[cfg(test)]
            Backend::Memory(state) => {
                let mut values = state
                    .lock()
                    .map_err(|_| "端口转发内存锁损坏".to_string())?
                    .entries
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                values.sort_by(|a, b| a.entry_id.cmp(&b.entry_id));
                Ok(values)
            }
        }
    }

    pub fn save_entry(&self, entry: &EntryRecord) -> Result<(), String> {
        validate_entry(entry)?;
        match &self.backend {
            Backend::Postgres(url) => {
                let entry = entry.clone();
                pg_with(url, move |client| {
                    let mut tx = client.transaction().map_err(|e| e.to_string())?;
                    let existing = tx.query_opt("SELECT node_id,public_address,family,enabled FROM forward_entries WHERE entry_id=$1", &[&entry.entry_id]).map_err(|e| e.to_string())?;
                    if let Some(row) = existing {
                        let active = tx.query("SELECT public_port,protocols FROM port_forward_allocations WHERE entry_id=$1 AND state NOT IN ('released','expired')", &[&entry.entry_id]).map_err(|e| e.to_string())?;
                        if !active.is_empty() {
                            let changed = row.get::<_, String>(0) != entry.node_id
                                || row.get::<_, String>(1) != entry.public_address
                                || row.get::<_, String>(2) != entry.family
                                || !row.get::<_, bool>(3)
                                || !entry.enabled;
                            if changed {
                                return Err("入口仍有活动转发，不能停用或修改节点、公网地址和地址族；请先释放转发".into());
                            }
                            for item in active {
                                let port: u16 = item.get::<_, i32>(0) as u16;
                                let protocols: Vec<PortForwardProtocol> =
                                    serde_json::from_str(item.get::<_, String>(1).as_str())
                                        .map_err(|e| e.to_string())?;
                                if !port_in_entry(&entry, port)
                                    || !protocols_supported(&protocols, &entry.protocols)
                                {
                                    return Err(
                                    "新的端口范围或协议不能覆盖已有活动转发，请先释放受影响的转发"
                                        .into(),
                                );
                                }
                            }
                        }
                    }
                    let protocols =
                        serde_json::to_string(&entry.protocols).map_err(|e| e.to_string())?;
                    tx.execute("INSERT INTO forward_entries(entry_id,node_id,name,public_address,hostname,family,protocols,enabled,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,CURRENT_TIMESTAMP) ON CONFLICT(entry_id) DO UPDATE SET node_id=EXCLUDED.node_id,name=EXCLUDED.name,public_address=EXCLUDED.public_address,hostname=EXCLUDED.hostname,family=EXCLUDED.family,protocols=EXCLUDED.protocols,enabled=EXCLUDED.enabled,updated_at=CURRENT_TIMESTAMP", &[&entry.entry_id,&entry.node_id,&entry.name,&entry.public_address,&entry.hostname,&entry.family,&protocols,&entry.enabled]).map_err(|e| e.to_string())?;
                    tx.execute(
                        "DELETE FROM forward_entry_ranges WHERE entry_id=$1",
                        &[&entry.entry_id],
                    )
                    .map_err(|e| e.to_string())?;
                    for range in &entry.ranges {
                        tx.execute("INSERT INTO forward_entry_ranges(entry_id,start_port,end_port) VALUES($1,$2,$3)",&[&entry.entry_id,&(range.start as i32),&(range.end as i32)]).map_err(|e| e.to_string())?;
                    }
                    tx.execute(
                        "DELETE FROM forward_entry_reserved_ports WHERE entry_id=$1",
                        &[&entry.entry_id],
                    )
                    .map_err(|e| e.to_string())?;
                    for port in &entry.reserved_ports {
                        tx.execute(
                            "INSERT INTO forward_entry_reserved_ports(entry_id,port) VALUES($1,$2)",
                            &[&entry.entry_id, &(*port as i32)],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    tx.commit().map_err(|e| e.to_string())?;
                    Ok(())
                })
            }
            #[cfg(test)]
            Backend::Memory(state) => {
                let mut state = state.lock().map_err(|_| "端口转发内存锁损坏".to_string())?;
                if let Some(old) = state.entries.get(&entry.entry_id) {
                    let active = state
                        .allocations
                        .values()
                        .filter(|a| a.entry_id == entry.entry_id && is_live(&a.state))
                        .collect::<Vec<_>>();
                    if !active.is_empty()
                        && (!entry.enabled
                            || old.node_id != entry.node_id
                            || old.public_address != entry.public_address
                            || old.family != entry.family)
                    {
                        return Err(
                            "入口仍有活动转发，不能停用或修改节点、公网地址和地址族；请先释放转发"
                                .into(),
                        );
                    }
                    if active.iter().any(|a| {
                        !port_in_entry(entry, a.public_port)
                            || !protocols_supported(&a.protocols, &entry.protocols)
                    }) {
                        return Err(
                            "新的端口范围或协议不能覆盖已有活动转发，请先释放受影响的转发".into(),
                        );
                    }
                }
                state.entries.insert(entry.entry_id.clone(), entry.clone());
                Ok(())
            }
        }
    }

    pub fn delete_entry(&self, entry_id: &str) -> Result<(), String> {
        if self
            .allocations(None)?
            .into_iter()
            .any(|a| a.entry_id == entry_id && is_live(&a.state))
        {
            return Err("入口仍有活动或保留中的端口转发，不能删除".into());
        }
        match &self.backend {
            Backend::Postgres(url) => {
                let entry_id = entry_id.to_owned();
                pg_with(url, move |client| {
                    client
                        .execute(
                            "DELETE FROM forward_entries WHERE entry_id=$1",
                            &[&entry_id],
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })
            }
            #[cfg(test)]
            Backend::Memory(state) => {
                state
                    .lock()
                    .map_err(|_| "端口转发内存锁损坏".to_string())?
                    .entries
                    .remove(entry_id);
                Ok(())
            }
        }
    }

    pub fn policy(&self, user_id: &str) -> Result<PolicyRecord, String> {
        match &self.backend {
            Backend::Postgres(url) => {
                let user_id = user_id.to_owned();
                pg_with(url, move |client| load_policy(client, &user_id))
            }
            #[cfg(test)]
            Backend::Memory(state) => Ok(state
                .lock()
                .map_err(|_| "端口转发内存锁损坏".to_string())?
                .policies
                .get(user_id)
                .cloned()
                .unwrap_or_default()),
        }
    }

    pub fn save_policy(&self, user_id: &str, policy: &PolicyRecord) -> Result<(), String> {
        validate_policy(policy)?;
        match &self.backend {
            Backend::Postgres(url) => {
                let user_id = user_id.to_owned();
                let policy = policy.clone();
                pg_with(url, move |client| {
                    let entries = serde_json::to_string(&policy.allowed_entry_ids)
                        .map_err(|error| format!("编码允许入口失败：{error}"))?;
                    let protocols = serde_json::to_string(&policy.allowed_protocols)
                        .map_err(|error| format!("编码允许协议失败：{error}"))?;
                    let ports = policy
                        .allowed_target_ports
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(|error| format!("编码允许目标端口失败：{error}"))?;
                    let families = serde_json::to_string(&policy.allowed_families)
                        .map_err(|error| format!("编码允许地址族失败：{error}"))?;
                    client.execute("INSERT INTO user_forward_policies(user_id,enabled,max_active_forwards,allowed_entry_ids,allowed_protocols,allowed_target_ports,allowed_families,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,CURRENT_TIMESTAMP) ON CONFLICT(user_id) DO UPDATE SET enabled=EXCLUDED.enabled,max_active_forwards=EXCLUDED.max_active_forwards,allowed_entry_ids=EXCLUDED.allowed_entry_ids,allowed_protocols=EXCLUDED.allowed_protocols,allowed_target_ports=EXCLUDED.allowed_target_ports,allowed_families=EXCLUDED.allowed_families,updated_at=CURRENT_TIMESTAMP", &[&user_id,&policy.enabled,&(policy.max_active_forwards as i32),&entries,&protocols,&ports,&families]).map_err(|e|format!("保存端口转发策略失败：{e}"))?;
                    Ok(())
                })?;
            }
            #[cfg(test)]
            Backend::Memory(state) => {
                state
                    .lock()
                    .map_err(|_| "端口转发内存锁损坏".to_string())?
                    .policies
                    .insert(user_id.into(), policy.clone());
            }
        }
        self.enforce_policy(user_id, policy)
    }

    fn enforce_policy(&self, user_id: &str, policy: &PolicyRecord) -> Result<(), String> {
        let by_id = self
            .entries()?
            .into_iter()
            .map(|e| (e.entry_id.clone(), e))
            .collect::<HashMap<_, _>>();
        let mut valid = 0u32;
        for allocation in self.allocations(Some(user_id))? {
            if !matches!(allocation.state.as_str(), "pending" | "applying" | "active") {
                continue;
            }
            let allowed = policy.enabled
                && valid < policy.max_active_forwards
                && by_id.get(&allocation.entry_id).is_some_and(|entry| {
                    entry.enabled
                        && policy.allowed_entry_ids.contains(&entry.entry_id)
                        && policy.allowed_families.contains(&entry.family)
                        && protocols_supported(&allocation.protocols, &policy.allowed_protocols)
                        && policy
                            .allowed_target_ports
                            .as_ref()
                            .is_none_or(|ports| ports.contains(&allocation.target_port))
                });
            if allowed {
                valid += 1;
            } else {
                self.set_state(
                    &allocation.forward_id,
                    "released",
                    Some("端口转发授权已变更"),
                    None,
                )?;
            }
        }
        Ok(())
    }

    pub fn active_count(&self, user_id: &str) -> Result<u32, String> {
        Ok(self
            .allocations(Some(user_id))?
            .into_iter()
            .filter(|a| is_live(&a.state))
            .count() as u32)
    }

    pub fn allocate(
        &self,
        user_id: &str,
        username: &str,
        device_id: Uuid,
        entry: &EntryRecord,
        protocols: &[PortForwardProtocol],
        target_address: &str,
        target_port: u16,
    ) -> Result<AllocationRecord, String> {
        let _allocation_guard = allocation_lock()
            .lock()
            .map_err(|_| "端口转发分配锁损坏".to_string())?;
        validate_protocols(protocols)?;
        let policy = self.policy(user_id)?;
        if !policy.enabled
            || !policy.allowed_entry_ids.contains(&entry.entry_id)
            || !policy.allowed_families.contains(&entry.family)
            || !protocols_supported(protocols, &policy.allowed_protocols)
            || policy
                .allowed_target_ports
                .as_ref()
                .is_some_and(|p| !p.contains(&target_port))
        {
            return Err("端口转发请求未获授权".into());
        }
        if self.active_count(user_id)? >= policy.max_active_forwards {
            return Err("已达到端口转发最大并发数（含保留和释放中租约）".into());
        }
        if self.allocations(None)?.into_iter().any(|allocation| {
            allocation.node_id == entry.node_id
                && allocation.device_id == device_id
                && allocation.target_port == target_port
                && is_live(&allocation.state)
        }) {
            return Err("该节点已经存在转发到此设备目标端口的规则".into());
        }
        let current = self.entry(&entry.entry_id)?.ok_or("入口不存在")?;
        if !current.enabled
            || current.node_id != entry.node_id
            || current.public_address != entry.public_address
            || current.family != entry.family
            || !protocols_supported(protocols, &current.protocols)
            || target_port == 0
        {
            return Err("入口配置已变更，请刷新后重试".into());
        }
        let used = self
            .allocations(None)?
            .into_iter()
            .filter(|a| a.entry_id == current.entry_id && is_live(&a.state))
            .map(|a| a.public_port)
            .collect::<HashSet<_>>();
        let mut ports = Vec::new();
        for range in &current.ranges {
            for port in range.start..=range.end {
                if !current.reserved_ports.contains(&port) && !used.contains(&port) {
                    ports.push(port)
                }
            }
        }
        if ports.is_empty() {
            return Err("入口端口池已耗尽".into());
        }
        let public_port = ports[(Uuid::new_v4().as_u128() as usize) % ports.len()];
        let now = Utc::now();
        let record = AllocationRecord {
            forward_id: format!("pf_{}", Uuid::new_v4().simple()),
            user_id: user_id.into(),
            username: username.into(),
            device_id,
            entry_id: current.entry_id.clone(),
            node_id: current.node_id.clone(),
            protocols: protocols.to_vec(),
            public_port,
            target_address: target_address.into(),
            target_port,
            state: "pending".into(),
            config_version: 1,
            created_at: now,
            updated_at: now,
            expires_at: None,
            error_message: None,
        };
        match &self.backend {
            Backend::Postgres(url) => {
                let record = record.clone();
                pg_with(url, move |client| {
                    let mut tx = client.transaction().map_err(|e| e.to_string())?;
                    tx.batch_execute(
                        "SELECT pg_advisory_xact_lock(hashtextextended('pmman-port-forward', 0));",
                    )
                    .map_err(|e| e.to_string())?;
                    let live:i64=tx.query_one("SELECT COUNT(*) FROM port_forward_allocations WHERE entry_id=$1 AND public_port=$2 AND state NOT IN ('released','expired')",&[&record.entry_id,&(record.public_port as i32)]).map_err(|e|e.to_string())?.get(0);
                    if live > 0 {
                        return Err("端口刚刚被其他请求分配，请重试".into());
                    };
                    let duplicate:i64=tx.query_one("SELECT COUNT(*) FROM port_forward_allocations WHERE node_id=$1 AND device_id=$2 AND target_port=$3 AND state NOT IN ('released','expired')",&[&record.node_id,&record.device_id,&(record.target_port as i32)]).map_err(|e|e.to_string())?.get(0);
                    if duplicate > 0 {
                        return Err("该节点已经存在转发到此设备目标端口的规则".into());
                    };
                    let policy = load_policy(&mut tx, &record.user_id)?;
                    let count:i64 = tx.query_one("SELECT COUNT(*) FROM port_forward_allocations WHERE user_id=$1 AND state NOT IN ('released','expired')", &[&record.user_id]).map_err(|e|e.to_string())?.get(0);
                    if !policy.enabled || count >= i64::from(policy.max_active_forwards) {
                        return Err("已达到端口转发配额或授权已撤销".into());
                    }
                    let protocols = serde_json::to_string(&record.protocols)
                        .map_err(|error| format!("编码端口转发协议失败：{error}"))?;
                    tx.execute("INSERT INTO port_forward_allocations(forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at,expires_at,error_message) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",&[&record.forward_id,&record.user_id,&record.username,&record.device_id,&record.entry_id,&record.node_id,&protocols,&(record.public_port as i32),&record.target_address,&(record.target_port as i32),&record.state,&(record.config_version as i64),&record.created_at,&record.updated_at,&record.expires_at,&record.error_message]).map_err(|e|format!("锁定转发端口失败：{e}"))?;
                    tx.execute("INSERT INTO port_forward_events(forward_id,event_type,created_at) VALUES($1,'created',$2)",&[&record.forward_id,&now]).map_err(|e|e.to_string())?;
                    tx.commit().map_err(|e| e.to_string())?;
                    Ok(())
                })?;
            }
            #[cfg(test)]
            Backend::Memory(state) => {
                let mut s = state.lock().map_err(|_| "端口转发内存锁损坏".to_string())?;
                if s.allocations.values().any(|a| {
                    a.entry_id == record.entry_id
                        && a.public_port == record.public_port
                        && is_live(&a.state)
                }) {
                    return Err("端口刚刚被其他请求分配，请重试".into());
                };
                if s.allocations.values().any(|a| {
                    a.node_id == record.node_id
                        && a.device_id == record.device_id
                        && a.target_port == record.target_port
                        && is_live(&a.state)
                }) {
                    return Err("该节点已经存在转发到此设备目标端口的规则".into());
                }
                s.allocations
                    .insert(record.forward_id.clone(), record.clone());
            }
        }
        Ok(record)
    }

    pub fn allocations(&self, user_id: Option<&str>) -> Result<Vec<AllocationRecord>, String> {
        match &self.backend {
            Backend::Postgres(url) => {
                let user_id = user_id.map(str::to_owned);
                pg_with(url, move |client| {
                    let rows = if let Some(user) = user_id.as_deref() {
                        client.query(ALLOCATION_SQL_USER, &[&user])
                    } else {
                        client.query(ALLOCATION_SQL, &[])
                    }
                    .map_err(|e| e.to_string())?;
                    rows.iter().map(allocation_from_row).collect()
                })
            }
            #[cfg(test)]
            Backend::Memory(state) => {
                let mut v = state
                    .lock()
                    .map_err(|_| "端口转发内存锁损坏".to_string())?
                    .allocations
                    .values()
                    .filter(|a| user_id.is_none_or(|u| a.user_id == u))
                    .cloned()
                    .collect::<Vec<_>>();
                v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                Ok(v)
            }
        }
    }

    pub fn set_state(
        &self,
        forward_id: &str,
        requested: &str,
        error: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        validate_state(requested)?;
        let state = if requested == "released" {
            "releasing"
        } else {
            requested
        }
        .to_owned();
        match &self.backend {
            Backend::Postgres(url) => {
                let forward_id = forward_id.to_owned();
                let error = error.map(str::to_owned);
                pg_with(url, move |client| {
                    let predicate = match state.as_str() {
                        "active" | "error" => "state IN ('pending','applying','active')",
                        "applying" => "state IN ('pending','applying','error')",
                        "pending" => "state = 'pending'",
                        "releasing" => "state NOT IN ('released','expired','releasing')",
                        "expired" => "state IN ('error','releasing')",
                        _ => "FALSE",
                    };
                    let n=client.execute(&format!("UPDATE port_forward_allocations SET state=$1,updated_at=$2,expires_at=$3,error_message=$4 WHERE forward_id=$5 AND {predicate}"),&[&state,&Utc::now(),&expires_at,&error,&forward_id]).map_err(|e|e.to_string())?;
                    if n > 0 {
                        return Ok(());
                    };
                    let current = client
                        .query_opt(
                            "SELECT state FROM port_forward_allocations WHERE forward_id=$1",
                            &[&forward_id],
                        )
                        .map_err(|e| e.to_string())?
                        .map(|r| r.get::<_, String>(0));
                    match current.as_deref() {
                        Some("releasing" | "released" | "expired") if state == "releasing" => {
                            Ok(())
                        }
                        Some(v) => Err(format!("端口转发状态不能从 {v} 转换为 {state}")),
                        None => Err("端口转发不存在".into()),
                    }
                })
            }
            #[cfg(test)]
            Backend::Memory(memory) => {
                let mut memory = memory
                    .lock()
                    .map_err(|_| "端口转发内存锁损坏".to_string())?;
                let a = memory
                    .allocations
                    .get_mut(forward_id)
                    .ok_or("端口转发不存在")?;
                if matches!(a.state.as_str(), "releasing" | "released" | "expired")
                    && state == "releasing"
                {
                    return Ok(());
                }
                if !transition_allowed(&a.state, &state) {
                    return Err(format!("端口转发状态不能从 {} 转换为 {state}", a.state));
                }
                a.state = state.into();
                a.updated_at = Utc::now();
                a.expires_at = expires_at;
                a.error_message = error.map(str::to_string);
                Ok(())
            }
        }
    }

    pub fn allocation(&self, id: &str) -> Result<Option<AllocationRecord>, String> {
        Ok(self
            .allocations(None)?
            .into_iter()
            .find(|a| a.forward_id == id))
    }
    pub fn desired_for_node(&self, node_id: &str) -> Result<Vec<AllocationRecord>, String> {
        Ok(self
            .allocations(None)?
            .into_iter()
            .filter(|a| {
                a.node_id == node_id
                    && matches!(a.state.as_str(), "pending" | "applying" | "active")
            })
            .collect())
    }
    pub fn expire_grace(&self) -> Result<(), String> {
        let now = Utc::now();
        for a in self.allocations(None)? {
            if a.state == "error" && a.expires_at.is_some_and(|t| t <= now) {
                self.set_state(&a.forward_id, "released", None, None)?;
            }
        }
        Ok(())
    }
    pub fn disconnect_device(&self, id: Uuid) -> Result<(), String> {
        let expires = Utc::now() + Duration::seconds(GRACE_SECONDS);
        for a in self
            .allocations(None)?
            .into_iter()
            .filter(|a| a.device_id == id && is_live(&a.state))
        {
            self.set_state(
                &a.forward_id,
                "error",
                Some("客户端已断开，等待重连恢复"),
                Some(expires),
            )?;
        }
        Ok(())
    }
    pub fn restore_device(&self, id: Uuid, ipv4: &str, ipv6: &str) -> Result<(), String> {
        let now = Utc::now();
        for a in self.allocations(None)?.into_iter().filter(|a| {
            a.device_id == id
                && a.state == "error"
                && a.expires_at.is_some_and(|t| t > now)
                && (a.target_address == ipv4 || a.target_address == ipv6)
        }) {
            self.set_state(&a.forward_id, "applying", None, None)?;
        }
        Ok(())
    }
    pub fn disable_node(&self, node_id: &str) -> Result<(), String> {
        for a in self
            .allocations(None)?
            .into_iter()
            .filter(|a| a.node_id == node_id && is_live(&a.state))
        {
            self.set_state(&a.forward_id, "released", Some("节点已停用"), None)?;
        }
        Ok(())
    }
    pub fn confirm_release(&self, id: &str) -> Result<(), String> {
        match &self.backend {
            Backend::Postgres(url) => {
                let id = id.to_owned();
                pg_with(url, move |client| {
                    client.execute("UPDATE port_forward_allocations SET state='released',updated_at=$1,expires_at=NULL WHERE forward_id=$2 AND state='releasing'",&[&Utc::now(),&id]).map_err(|e|e.to_string())?;
                    Ok(())
                })
            }
            #[cfg(test)]
            Backend::Memory(s) => {
                let mut s = s.lock().map_err(|_| "端口转发内存锁损坏".to_string())?;
                if let Some(a) = s.allocations.get_mut(id) {
                    if a.state == "releasing" {
                        a.state = "released".into();
                        a.updated_at = Utc::now();
                    }
                }
                Ok(())
            }
        }
    }

    /// Import a legacy allocation without changing its lifecycle timestamps.
    /// This is intentionally only used by the one-shot migration binary.
    #[cfg(feature = "legacy-migration")]
    pub fn import_allocation(&self, record: &AllocationRecord) -> Result<(), String> {
        validate_state(&record.state)?;
        validate_protocols(&record.protocols)?;
        if record.public_port == 0 || record.target_port == 0 {
            return Err("导入端口转发记录包含无效端口".into());
        }
        match &self.backend {
            Backend::Postgres(url) => {
                let mut client = pg_client(url)?;
                let protocols =
                    serde_json::to_string(&record.protocols).map_err(|e| e.to_string())?;
                client.execute("INSERT INTO port_forward_allocations(forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at,expires_at,error_message) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) ON CONFLICT(forward_id) DO NOTHING", &[&record.forward_id,&record.user_id,&record.username,&record.device_id,&record.entry_id,&record.node_id,&protocols,&(record.public_port as i32),&record.target_address,&(record.target_port as i32),&record.state,&(record.config_version as i64),&record.created_at,&record.updated_at,&record.expires_at,&record.error_message]).map_err(|e| format!("导入端口租约失败：{e}"))?;
                Ok(())
            }
            #[cfg(test)]
            Backend::Memory(memory) => {
                memory
                    .lock()
                    .map_err(|_| "端口转发内存锁损坏".to_string())?
                    .allocations
                    .entry(record.forward_id.clone())
                    .or_insert_with(|| record.clone());
                Ok(())
            }
        }
    }

    #[cfg(feature = "legacy-migration")]
    pub fn import_event(
        &self,
        forward_id: Option<&str>,
        event_type: &str,
        detail: Option<&str>,
        created_at: DateTime<Utc>,
    ) -> Result<(), String> {
        let url = match &self.backend {
            Backend::Postgres(url) => url,
            #[cfg(test)]
            Backend::Memory(_) => return Ok(()),
        };
        let mut client = pg_client(url)?;
        client.execute("INSERT INTO port_forward_events(forward_id,event_type,detail,created_at) VALUES($1,$2,$3,$4)", &[&forward_id,&event_type,&detail,&created_at]).map_err(|e| format!("导入端口事件失败：{e}"))?;
        Ok(())
    }
}

/// Import all legacy port-forward records into a caller-owned transaction.
/// The migration command parses every source row before invoking this helper,
/// so no target row is written when the source is malformed or any target
/// statement fails.
#[cfg(feature = "legacy-migration")]
pub fn import_legacy_transaction(
    tx: &mut postgres::Transaction<'_>,
    entries: &[EntryRecord],
    policies: &[(String, PolicyRecord)],
    allocations: &[AllocationRecord],
    events: &[(Option<String>, String, Option<String>, DateTime<Utc>)],
) -> Result<(), String> {
    for entry in entries {
        validate_entry(entry)?;
        if let Some(existing) = tx
            .query_opt(
                "SELECT node_id,public_address,family,enabled FROM forward_entries WHERE entry_id=$1",
                &[&entry.entry_id],
            )
            .map_err(|e| format!("读取已有端口入口失败：{e}"))?
        {
            let active = tx
                .query(
                    "SELECT public_port,protocols FROM port_forward_allocations
                     WHERE entry_id=$1 AND state NOT IN ('released','expired')",
                    &[&entry.entry_id],
                )
                .map_err(|e| format!("检查已有端口租约失败：{e}"))?;
            if !active.is_empty()
                && (existing.get::<_, String>(0) != entry.node_id
                    || existing.get::<_, String>(1) != entry.public_address
                    || existing.get::<_, String>(2) != entry.family
                    || !existing.get::<_, bool>(3)
                    || !entry.enabled)
            {
                return Err("入口仍有活动转发，不能在迁移时停用或修改节点、公网地址和地址族".into());
            }
            for allocation in active {
                let port = allocation.get::<_, i32>(0) as u16;
                let protocols: Vec<PortForwardProtocol> = serde_json::from_str(
                    allocation.get::<_, String>(1).as_str(),
                )
                .map_err(|e| format!("已有端口租约协议格式错误：{e}"))?;
                if !port_in_entry(entry, port) || !protocols_supported(&protocols, &entry.protocols)
                {
                    return Err("迁移后的端口范围或协议不能覆盖已有活动转发".into());
                }
            }
        }
        let protocols = serde_json::to_string(&entry.protocols).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO forward_entries(entry_id,node_id,name,public_address,hostname,family,protocols,enabled,updated_at)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,CURRENT_TIMESTAMP)
             ON CONFLICT(entry_id) DO UPDATE SET node_id=EXCLUDED.node_id,name=EXCLUDED.name,
             public_address=EXCLUDED.public_address,hostname=EXCLUDED.hostname,family=EXCLUDED.family,
             protocols=EXCLUDED.protocols,enabled=EXCLUDED.enabled,updated_at=CURRENT_TIMESTAMP",
            &[
                &entry.entry_id,
                &entry.node_id,
                &entry.name,
                &entry.public_address,
                &entry.hostname,
                &entry.family,
                &protocols,
                &entry.enabled,
            ],
        )
        .map_err(|e| format!("导入端口入口失败：{e}"))?;
        tx.execute(
            "DELETE FROM forward_entry_ranges WHERE entry_id=$1",
            &[&entry.entry_id],
        )
        .map_err(|e| format!("导入端口范围失败：{e}"))?;
        for range in &entry.ranges {
            tx.execute(
                "INSERT INTO forward_entry_ranges(entry_id,start_port,end_port) VALUES($1,$2,$3)",
                &[&entry.entry_id, &(range.start as i32), &(range.end as i32)],
            )
            .map_err(|e| format!("导入端口范围失败：{e}"))?;
        }
        tx.execute(
            "DELETE FROM forward_entry_reserved_ports WHERE entry_id=$1",
            &[&entry.entry_id],
        )
        .map_err(|e| format!("导入保留端口失败：{e}"))?;
        for port in &entry.reserved_ports {
            tx.execute(
                "INSERT INTO forward_entry_reserved_ports(entry_id,port) VALUES($1,$2)",
                &[&entry.entry_id, &(*port as i32)],
            )
            .map_err(|e| format!("导入保留端口失败：{e}"))?;
        }
    }

    for (user_id, policy) in policies {
        validate_policy(policy)?;
        let entries_json = serde_json::to_string(&policy.allowed_entry_ids)
            .map_err(|e| format!("编码允许入口失败：{e}"))?;
        let protocols = serde_json::to_string(&policy.allowed_protocols)
            .map_err(|e| format!("编码允许协议失败：{e}"))?;
        let ports = policy
            .allowed_target_ports
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("编码允许目标端口失败：{e}"))?;
        let families = serde_json::to_string(&policy.allowed_families)
            .map_err(|e| format!("编码允许地址族失败：{e}"))?;
        tx.execute(
            "INSERT INTO user_forward_policies(user_id,enabled,max_active_forwards,allowed_entry_ids,allowed_protocols,allowed_target_ports,allowed_families,updated_at)
             VALUES($1,$2,$3,$4,$5,$6,$7,CURRENT_TIMESTAMP)
             ON CONFLICT(user_id) DO UPDATE SET enabled=EXCLUDED.enabled,max_active_forwards=EXCLUDED.max_active_forwards,
             allowed_entry_ids=EXCLUDED.allowed_entry_ids,allowed_protocols=EXCLUDED.allowed_protocols,
             allowed_target_ports=EXCLUDED.allowed_target_ports,allowed_families=EXCLUDED.allowed_families,
             updated_at=CURRENT_TIMESTAMP",
            &[
                user_id,
                &policy.enabled,
                &(policy.max_active_forwards as i32),
                &entries_json,
                &protocols,
                &ports,
                &families,
            ],
        )
        .map_err(|e| format!("导入端口转发策略失败：{e}"))?;
    }

    for record in allocations {
        validate_state(&record.state)?;
        validate_protocols(&record.protocols)?;
        if record.public_port == 0 || record.target_port == 0 {
            return Err("导入端口转发记录包含无效端口".into());
        }
        let protocols = serde_json::to_string(&record.protocols).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO port_forward_allocations(forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at,expires_at,error_message)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
             ON CONFLICT(forward_id) DO NOTHING",
            &[
                &record.forward_id,
                &record.user_id,
                &record.username,
                &record.device_id,
                &record.entry_id,
                &record.node_id,
                &protocols,
                &(record.public_port as i32),
                &record.target_address,
                &(record.target_port as i32),
                &record.state,
                &(record.config_version as i64),
                &record.created_at,
                &record.updated_at,
                &record.expires_at,
                &record.error_message,
            ],
        )
        .map_err(|e| format!("导入端口租约失败：{e}"))?;
    }

    for (forward_id, event_type, detail, created_at) in events {
        tx.execute(
            "INSERT INTO port_forward_events(forward_id,event_type,detail,created_at)
             SELECT $1,$2,$3,$4
             WHERE NOT EXISTS (
                 SELECT 1 FROM port_forward_events
                 WHERE forward_id IS NOT DISTINCT FROM $1
                   AND event_type=$2 AND detail IS NOT DISTINCT FROM $3 AND created_at=$4
             )",
            &[forward_id, event_type, detail, created_at],
        )
        .map_err(|e| format!("导入端口事件失败：{e}"))?;
    }
    Ok(())
}

fn pg_client(url: &str) -> Result<Client, String> {
    Client::connect(url, NoTls).map_err(|error| format!("连接端口转发 PostgreSQL 失败：{error}"))
}

/// The synchronous postgres client owns an internal Tokio runtime. Keep it on
/// a plain OS thread whenever a store method is called by an HTTP handler.
fn pg_with<T, F>(url: &Arc<String>, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut Client) -> Result<T, String> + Send + 'static,
{
    let url = url.clone();
    crate::db::run_database_operation(move || {
        let mut client = pg_client(url.as_str())?;
        operation(&mut client)
    })
}

const ALLOCATION_SQL: &str = "SELECT forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at,expires_at,error_message FROM port_forward_allocations ORDER BY created_at DESC";
const ALLOCATION_SQL_USER: &str = "SELECT forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at,expires_at,error_message FROM port_forward_allocations WHERE user_id=$1 ORDER BY created_at DESC";

fn allocation_from_row(row: &Row) -> Result<AllocationRecord, String> {
    Ok(AllocationRecord {
        forward_id: row.get(0),
        user_id: row.get(1),
        username: row.get(2),
        device_id: row.get(3),
        entry_id: row.get(4),
        node_id: row.get(5),
        protocols: serde_json::from_str(row.get::<_, String>(6).as_str())
            .map_err(|e| e.to_string())?,
        public_port: row.get::<_, i32>(7) as u16,
        target_address: row.get(8),
        target_port: row.get::<_, i32>(9) as u16,
        state: row.get(10),
        config_version: row.get::<_, i64>(11) as u64,
        created_at: row.get(12),
        updated_at: row.get(13),
        expires_at: row.get(14),
        error_message: row.get(15),
    })
}

fn load_entry<C: GenericClient>(client: &mut C, id: &str) -> Result<Option<EntryRecord>, String> {
    let Some(row)=client.query_opt("SELECT entry_id,node_id,name,public_address,hostname,family,protocols,enabled FROM forward_entries WHERE entry_id=$1",&[&id]).map_err(|e|e.to_string())? else{return Ok(None)};
    let ranges=client.query("SELECT start_port,end_port FROM forward_entry_ranges WHERE entry_id=$1 ORDER BY start_port",&[&id]).map_err(|e|e.to_string())?.into_iter().map(|r|PortRange{start:r.get::<_,i32>(0) as u16,end:r.get::<_,i32>(1) as u16}).collect();
    let reserved_ports = client
        .query(
            "SELECT port FROM forward_entry_reserved_ports WHERE entry_id=$1 ORDER BY port",
            &[&id],
        )
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| r.get::<_, i32>(0) as u16)
        .collect();
    Ok(Some(EntryRecord {
        entry_id: row.get(0),
        node_id: row.get(1),
        name: row.get(2),
        public_address: row.get(3),
        hostname: row.get(4),
        family: row.get(5),
        protocols: serde_json::from_str(row.get::<_, String>(6).as_str())
            .map_err(|e| e.to_string())?,
        ranges,
        reserved_ports,
        enabled: row.get(7),
    }))
}
fn load_policy<C: GenericClient>(client: &mut C, user_id: &str) -> Result<PolicyRecord, String> {
    let Some(row)=client.query_opt("SELECT enabled,max_active_forwards,allowed_entry_ids,allowed_protocols,allowed_target_ports,allowed_families FROM user_forward_policies WHERE user_id=$1",&[&user_id]).map_err(|e|e.to_string())? else{return Ok(PolicyRecord::default())};
    Ok(PolicyRecord {
        enabled: row.get(0),
        max_active_forwards: row.get::<_, i32>(1) as u32,
        allowed_entry_ids: serde_json::from_str(row.get::<_, String>(2).as_str())
            .map_err(|e| e.to_string())?,
        allowed_protocols: serde_json::from_str(row.get::<_, String>(3).as_str())
            .map_err(|e| e.to_string())?,
        allowed_target_ports: row
            .get::<_, Option<String>>(4)
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| e.to_string())?,
        allowed_families: serde_json::from_str(row.get::<_, String>(5).as_str())
            .map_err(|e| e.to_string())?,
    })
}
pub(crate) fn validate_policy(policy: &PolicyRecord) -> Result<(), String> {
    if policy.max_active_forwards > 1024 {
        return Err("最大并发转发数不能超过 1024".into());
    }
    if policy.allowed_protocols.is_empty() || policy.allowed_families.is_empty() {
        return Err("至少需要允许一个协议和地址族".into());
    }
    if policy
        .allowed_families
        .iter()
        .any(|f| !matches!(f.as_str(), "ipv4" | "ipv6"))
    {
        return Err("地址族只能是 ipv4 或 ipv6".into());
    }
    validate_protocols(&policy.allowed_protocols)?;
    if policy
        .allowed_target_ports
        .as_ref()
        .is_some_and(|p| p.iter().any(|v| *v == 0))
    {
        return Err("目标端口必须在 1 到 65535 之间".into());
    }
    Ok(())
}
fn protocols_supported(
    requested: &[PortForwardProtocol],
    supported: &[PortForwardProtocol],
) -> bool {
    !requested.is_empty() && requested.iter().all(|p| supported.contains(p))
}
fn port_in_entry(entry: &EntryRecord, port: u16) -> bool {
    entry
        .ranges
        .iter()
        .any(|r| r.start <= port && port <= r.end)
        && !entry.reserved_ports.contains(&port)
}
fn validate_entry(entry: &EntryRecord) -> Result<(), String> {
    if entry.entry_id.trim().is_empty()
        || entry.node_id.trim().is_empty()
        || entry.public_address.trim().is_empty()
    {
        return Err("入口 ID、节点和公网地址不能为空".into());
    }
    if !matches!(entry.family.as_str(), "ipv4" | "ipv6") {
        return Err("入口地址族必须是 ipv4 或 ipv6".into());
    }
    if entry.ranges.is_empty() {
        return Err("入口至少需要一个端口范围".into());
    }
    let mut ranges = entry.ranges.clone();
    ranges.sort_by_key(|r| r.start);
    for r in &ranges {
        if r.start == 0 || r.start > r.end {
            return Err("入口端口范围无效".into());
        }
    }
    for pair in ranges.windows(2) {
        if pair[1].start <= pair[0].end {
            return Err("入口端口范围存在重叠".into());
        }
    }
    let mut reserved = HashSet::new();
    for port in &entry.reserved_ports {
        if !ranges.iter().any(|r| r.start <= *port && *port <= r.end) {
            return Err(format!("保留端口 {port} 不在入口端口范围内"));
        }
        if !reserved.insert(*port) {
            return Err(format!("保留端口 {port} 重复"));
        }
    }
    validate_protocols(&entry.protocols)
}
fn validate_protocols(protocols: &[PortForwardProtocol]) -> Result<(), String> {
    if protocols.is_empty()
        || protocols.len() > 2
        || (protocols.len() == 2 && protocols[0] == protocols[1])
    {
        return Err("必须选择 TCP、UDP 或 TCP+UDP".into());
    }
    Ok(())
}

fn validate_state(state: &str) -> Result<(), String> {
    if matches!(
        state,
        "pending" | "applying" | "active" | "error" | "released" | "releasing" | "expired"
    ) {
        Ok(())
    } else {
        Err(format!("端口转发状态无效：{state}"))
    }
}
fn is_live(state: &str) -> bool {
    matches!(
        state,
        "pending" | "applying" | "active" | "error" | "releasing"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> EntryRecord {
        EntryRecord {
            entry_id: id.into(),
            node_id: "node-a".into(),
            name: id.into(),
            public_address: "203.0.113.10".into(),
            hostname: None,
            family: "ipv4".into(),
            protocols: vec![PortForwardProtocol::Tcp, PortForwardProtocol::Udp],
            ranges: vec![PortRange {
                start: 20000,
                end: 20000,
            }],
            reserved_ports: vec![],
            enabled: true,
        }
    }

    #[test]
    fn one_node_cannot_duplicate_device_target_port_across_entries() {
        let store = PortForwardStore::memory().unwrap();
        store.save_entry(&entry("entry-a")).unwrap();
        store.save_entry(&entry("entry-b")).unwrap();
        store
            .save_policy(
                "user-a",
                &PolicyRecord {
                    enabled: true,
                    max_active_forwards: 5,
                    allowed_entry_ids: vec!["entry-a".into(), "entry-b".into()],
                    allowed_protocols: vec![PortForwardProtocol::Tcp, PortForwardProtocol::Udp],
                    allowed_target_ports: None,
                    allowed_families: vec!["ipv4".into()],
                },
            )
            .unwrap();
        let device = Uuid::new_v4();
        store
            .allocate(
                "user-a",
                "alice",
                device,
                &entry("entry-a"),
                &[PortForwardProtocol::Tcp],
                "10.88.0.20",
                22,
            )
            .unwrap();
        let error = store
            .allocate(
                "user-a",
                "alice",
                device,
                &entry("entry-b"),
                &[PortForwardProtocol::Udp],
                "10.88.0.20",
                22,
            )
            .unwrap_err();
        assert!(error.contains("已经存在"));
    }
}
#[cfg(test)]
fn transition_allowed(from: &str, to: &str) -> bool {
    match to {
        "active" | "error" => matches!(from, "pending" | "applying" | "active"),
        "applying" => matches!(from, "pending" | "applying" | "error"),
        "pending" => from == "pending",
        "releasing" => !matches!(from, "released" | "expired" | "releasing"),
        "expired" => matches!(from, "error" | "releasing"),
        _ => false,
    }
}
