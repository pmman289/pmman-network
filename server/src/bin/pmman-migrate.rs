//! One-shot migration from the pre-PostgreSQL JSON/SQLite stores.
//!
//! This binary is built only with `--features legacy-migration`. The running
//! master has no rusqlite dependency and never opens the legacy database.

#[allow(dead_code)]
#[path = "../db.rs"]
mod db;
#[allow(dead_code)]
#[path = "../port_forward.rs"]
mod port_forward;

use chrono::{DateTime, Utc};
use port_forward::{
    import_legacy_transaction, AllocationRecord, EntryRecord, PolicyRecord, PortForwardStore,
    PortRange,
};
use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{env, fs, path::PathBuf};
use uuid::Uuid;

fn read_document(path: &PathBuf) -> Result<Option<String>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    let incoming: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} 格式错误：{error}", path.display()))?;
    serde_json::to_string(&incoming)
        .map(Some)
        .map_err(|error| format!("{} 编码失败：{error}", path.display()))
}

fn put_document_tx(
    tx: &mut postgres::Transaction<'_>,
    key: &str,
    payload: &str,
) -> Result<(), String> {
    let incoming: Value = serde_json::from_str(payload)
        .map_err(|error| format!("迁移文档 {key} 格式错误：{error}"))?;
    if let Some(row) = tx
        .query_opt(
            "SELECT payload FROM pmman_documents WHERE document_key=$1",
            &[&key],
        )
        .map_err(|error| format!("读取 PostgreSQL 文档 {key} 失败：{error}"))?
    {
        let existing: String = row.get(0);
        let current: Value = serde_json::from_str(&existing)
            .map_err(|error| format!("PostgreSQL 文档 {key} 格式错误：{error}"))?;
        if current != incoming {
            return Err(format!("PostgreSQL 文档 {key} 已存在且内容不同，拒绝覆盖"));
        }
        return Ok(());
    }
    tx.execute(
        "INSERT INTO pmman_documents(document_key,payload,revision,updated_at)
         VALUES($1,$2,1,CURRENT_TIMESTAMP)",
        &[&key, &payload],
    )
    .map_err(|error| format!("写入 PostgreSQL 文档 {key} 失败：{error}"))?;
    Ok(())
}

fn parse_time(value: String) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| format!("时间格式错误 {value}：{error}"))
}

fn sqlite_message(error: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        error,
    )))
}

fn parse_json_column<T: DeserializeOwned>(value: String, column: &str) -> rusqlite::Result<T> {
    serde_json::from_str(&value)
        .map_err(|error| sqlite_message(format!("旧端口转发字段 {column} 格式错误：{error}")))
}

#[derive(Default)]
struct LegacyPortForwardData {
    entries: Vec<EntryRecord>,
    policies: Vec<(String, PolicyRecord)>,
    allocations: Vec<AllocationRecord>,
    events: Vec<(Option<String>, String, Option<String>, DateTime<Utc>)>,
}

fn read_port_forwards(path: &PathBuf) -> Result<LegacyPortForwardData, String> {
    if !path.is_file() {
        return Ok(LegacyPortForwardData::default());
    }
    let sqlite =
        Connection::open(path).map_err(|error| format!("打开旧端口数据库失败：{error}"))?;
    let has_table = |name: &str| -> Result<bool, String> {
        sqlite
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(|error| error.to_string())
    };
    if !has_table("forward_entries")? {
        return Ok(LegacyPortForwardData::default());
    }

    let mut entries = sqlite.prepare("SELECT entry_id,node_id,name,public_address,hostname,family,protocols,enabled FROM forward_entries ORDER BY entry_id").map_err(|e|e.to_string())?;
    let rows = entries.query_map([], |row| {
        let id: String = row.get(0)?;
        let ranges = sqlite.prepare("SELECT start_port,end_port FROM forward_entry_ranges WHERE entry_id=?1 ORDER BY start_port").and_then(|mut s| s.query_map([&id], |r| Ok(PortRange { start: r.get::<_,u16>(0)?, end: r.get::<_,u16>(1)? })).and_then(|it| it.collect::<Result<Vec<_>,_>>()))?;
        let reserved_ports = sqlite.prepare("SELECT port FROM forward_entry_reserved_ports WHERE entry_id=?1 ORDER BY port").and_then(|mut s| s.query_map([&id], |r| r.get::<_,u16>(0)).and_then(|it| it.collect::<Result<Vec<_>,_>>()))?;
        Ok(EntryRecord { entry_id:id, node_id:row.get(1)?, name:row.get(2)?, public_address:row.get(3)?, hostname:row.get(4)?, family:row.get(5)?, protocols:serde_json::from_str(&row.get::<_,String>(6)?).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?, ranges, reserved_ports, enabled:row.get(7)? })
    }).map_err(|e|e.to_string())?;
    let entries = rows
        .map(|row| row.map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;

    let mut policies = Vec::new();
    if has_table("user_forward_policies")? {
        let mut policy_statement = sqlite.prepare("SELECT user_id,enabled,max_active_forwards,allowed_entry_ids,allowed_protocols,allowed_target_ports,allowed_families FROM user_forward_policies").map_err(|e|e.to_string())?;
        let rows = policy_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    PolicyRecord {
                        enabled: row.get(1)?,
                        max_active_forwards: row.get::<_, i64>(2)? as u32,
                        allowed_entry_ids: parse_json_column(
                            row.get::<_, String>(3)?,
                            "allowed_entry_ids",
                        )?,
                        allowed_protocols: parse_json_column(
                            row.get::<_, String>(4)?,
                            "allowed_protocols",
                        )?,
                        allowed_target_ports: row
                            .get::<_, Option<String>>(5)?
                            .map(|value| parse_json_column(value, "allowed_target_ports"))
                            .transpose()?,
                        allowed_families: parse_json_column(
                            row.get::<_, String>(6)?,
                            "allowed_families",
                        )?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        policies = rows
            .map(|row| row.map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
    }

    let mut allocations = Vec::new();
    if has_table("port_forward_allocations")? {
        let mut allocation_statement = sqlite.prepare("SELECT forward_id,user_id,username,device_id,entry_id,node_id,protocols,public_port,target_address,target_port,state,config_version,created_at,updated_at,expires_at,error_message FROM port_forward_allocations").map_err(|e|e.to_string())?;
        let rows = allocation_statement
            .query_map([], |row| {
                Ok(AllocationRecord {
                    forward_id: row.get(0)?,
                    user_id: row.get(1)?,
                    username: row.get(2)?,
                    device_id: Uuid::parse_str(&row.get::<_, String>(3)?)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    entry_id: row.get(4)?,
                    node_id: row.get(5)?,
                    protocols: serde_json::from_str(&row.get::<_, String>(6)?)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                    public_port: row.get(7)?,
                    target_address: row.get(8)?,
                    target_port: row.get(9)?,
                    state: row.get(10)?,
                    config_version: row.get(11)?,
                    created_at: parse_time(row.get(12)?).map_err(sqlite_message)?,
                    updated_at: parse_time(row.get(13)?).map_err(sqlite_message)?,
                    expires_at: row
                        .get::<_, Option<String>>(14)?
                        .map(parse_time)
                        .transpose()
                        .map_err(sqlite_message)?,
                    error_message: row.get(15)?,
                })
            })
            .map_err(|e| e.to_string())?;
        allocations = rows
            .map(|row| row.map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
    }
    let mut events = Vec::new();
    if has_table("port_forward_events")? {
        let mut event_statement = sqlite
            .prepare("SELECT forward_id,event_type,detail,created_at FROM port_forward_events ORDER BY event_id")
            .map_err(|e| e.to_string())?;
        let rows = event_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    parse_time(row.get::<_, String>(3)?).map_err(sqlite_message)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        events = rows
            .map(|row| row.map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(LegacyPortForwardData {
        entries,
        policies,
        allocations,
        events,
    })
}

fn main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let database_url = args
        .next()
        .ok_or("用法：pmman-migrate <postgres-url> <legacy-root>")?;
    let root = PathBuf::from(
        args.next()
            .ok_or("用法：pmman-migrate <postgres-url> <legacy-root>")?,
    );
    if args.next().is_some() {
        return Err("参数过多".into());
    }
    if !root.is_dir() {
        return Err(format!("旧数据目录不存在：{}", root.display()));
    }
    let database = db::PgStore::connect(&database_url)?;
    let documents = [
        ("users", read_document(&root.join("user-registry.json"))?),
        ("nodes", read_document(&root.join("node-registry.json"))?),
        (
            "network_settings",
            read_document(&root.join("network-settings.json"))?,
        ),
    ];
    let _forwards = PortForwardStore::open(&database_url)?;
    let legacy = read_port_forwards(&root.join("port-forward.db"))?;
    database.with_transaction(|tx| {
        for (key, payload) in &documents {
            if let Some(payload) = payload {
                put_document_tx(tx, key, payload)?;
            }
        }
        import_legacy_transaction(
            tx,
            &legacy.entries,
            &legacy.policies,
            &legacy.allocations,
            &legacy.events,
        )?;
        tx.execute(
            "INSERT INTO pmman_audit_events(event_type,detail)
             VALUES ('legacy-import.completed','JSON and legacy port-forward data imported')",
            &[],
        )
        .map_err(|error| format!("写入迁移审计事件失败：{error}"))?;
        Ok(())
    })?;
    println!("legacy_migration=passed");
    Ok(())
}
