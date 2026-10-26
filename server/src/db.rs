use postgres::{Client, NoTls};
use std::sync::Arc;

/// The production control plane persistence boundary.
///
/// The master is currently a single process, so this small store opens short
/// lived PostgreSQL connections while the request handlers are migrated away
/// from their in-memory registries. All durable
/// state is stored in PostgreSQL; the JSON document column is a compatibility
/// envelope for the existing serde models and is versioned by schema_migrations.
#[derive(Clone)]
pub struct PgStore {
    url: Arc<String>,
}

pub(crate) fn run_database_operation<T, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let run = move || {
        std::thread::spawn(operation)
            .join()
            .map_err(|_| "PostgreSQL 工作线程异常退出".to_string())?
    };
    // Allow Tokio to replace this worker while a synchronous caller waits.
    // Current-thread runtimes (unit tests) cannot use block_in_place.
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(run)
    } else {
        run()
    }
}

impl PgStore {
    pub fn connect(url: &str) -> Result<Self, String> {
        if url.trim().is_empty() {
            return Err("PMMAN_DATABASE_URL 不能为空".into());
        }
        Client::connect(url, NoTls).map_err(|error| format!("连接 PostgreSQL 失败：{error}"))?;
        let store = Self {
            url: Arc::new(url.to_owned()),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Open a fresh connection for each operation. The master is a single
    /// process and its writes are small; reconnecting here is preferable to
    /// retaining a dead socket after a PostgreSQL container restart. The
    /// database remains the sole source of truth and callers still get a
    /// concrete error when it is unavailable.
    fn client(&self) -> Result<Client, String> {
        Client::connect(self.url.as_str(), NoTls)
            .map_err(|error| format!("连接 PostgreSQL 失败：{error}"))
    }

    /// `postgres::Client` is synchronous and creates an internal Tokio
    /// runtime. It must never be opened from an async request handler, or the
    /// nested runtime will panic. Run each short database operation on a plain
    /// OS thread so the HTTP runtime remains isolated from the synchronous
    /// driver.
    pub(crate) fn on_thread<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Client) -> Result<T, String> + Send + 'static,
    {
        let url = self.url.clone();
        run_database_operation(move || {
            let mut client = Client::connect(url.as_str(), NoTls)
                .map_err(|error| format!("连接 PostgreSQL 失败：{error}"))?;
            operation(&mut client)
        })
    }

    pub fn migrate(&self) -> Result<(), String> {
        let mut client = self.client()?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version BIGINT PRIMARY KEY,
                    applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS pmman_documents (
                    document_key TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    revision BIGINT NOT NULL DEFAULT 1,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE IF NOT EXISTS pmman_audit_events (
                    event_id BIGSERIAL PRIMARY KEY,
                    actor TEXT,
                    event_type TEXT NOT NULL,
                    resource_type TEXT,
                    resource_id TEXT,
                    detail TEXT,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
                );
                INSERT INTO schema_migrations(version)
                    VALUES (1) ON CONFLICT (version) DO NOTHING;",
            )
            .map_err(|error| format!("初始化 PostgreSQL schema 失败：{error}"))?;
        client
            .batch_execute(include_str!("invitations.sql"))
            .map_err(|error| format!("初始化邀请注册数据失败：{error}"))?;
        client.batch_execute(include_str!("address_pools.sql"))
            .map_err(|error| format!("初始化地址池数据失败：{error}"))?;
        // Historical registration identity is the immutable user_id. A deleted
        // username may be registered again without rewriting its old history.
        client.batch_execute(
            "BEGIN;
             ALTER TABLE invitation_registrations DROP CONSTRAINT IF EXISTS invitation_registrations_username_key;
             CREATE INDEX IF NOT EXISTS invitation_registrations_username ON invitation_registrations(username);
             INSERT INTO schema_migrations(version) VALUES(4) ON CONFLICT(version) DO NOTHING;
             COMMIT;"
        ).map_err(|_| "更新历史注册用户名约束失败".to_string())?;
        Ok(())
    }

    /// Publish a related set of durable changes atomically. Callers publish
    /// their in-memory generation only after this returns successfully.
    pub(crate) fn on_transaction<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut postgres::Transaction<'_>) -> Result<T, String> + Send + 'static,
    {
        self.on_thread(move |client| {
            let mut tx = client.transaction().map_err(|_| "开启数据库事务失败")?;
            let result = operation(&mut tx)?;
            tx.commit().map_err(|_| "提交数据库事务失败")?;
            Ok(result)
        })
    }

    pub fn get_document(&self, key: &str) -> Result<Option<String>, String> {
        let key = key.to_owned();
        self.on_thread(move |client| {
            client
                .query_opt(
                    "SELECT payload FROM pmman_documents WHERE document_key = $1",
                    &[&key],
                )
                .map(|row| row.map(|row| row.get::<_, String>(0)))
                .map_err(|error| format!("读取 PostgreSQL 数据失败：{error}"))
        })
    }

    pub fn put_document(&self, key: &str, payload: &str) -> Result<(), String> {
        let key = key.to_owned();
        let payload = payload.to_owned();
        self.on_thread(move |client| {
            client
                .execute(
                    "INSERT INTO pmman_documents(document_key, payload, revision, updated_at)
                     VALUES ($1, $2, 1, CURRENT_TIMESTAMP)
                     ON CONFLICT(document_key) DO UPDATE
                     SET payload = EXCLUDED.payload,
                         revision = pmman_documents.revision + 1,
                         updated_at = CURRENT_TIMESTAMP",
                    &[&key, &payload],
                )
                .map_err(|error| format!("写入 PostgreSQL 数据失败：{error}"))?;
            Ok(())
        })
    }

    pub fn delete_document(&self, key: &str) -> Result<(), String> {
        let key = key.to_owned();
        self.on_thread(move |client| {
            client
                .execute(
                    "DELETE FROM pmman_documents WHERE document_key = $1",
                    &[&key],
                )
                .map_err(|error| format!("删除 PostgreSQL 数据失败：{error}"))?;
            Ok(())
        })
    }

    pub fn append_audit(
        &self,
        actor: Option<&str>,
        event_type: &str,
        resource_type: Option<&str>,
        resource_id: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), String> {
        let actor = actor.map(str::to_owned);
        let event_type = event_type.to_owned();
        let resource_type = resource_type.map(str::to_owned);
        let resource_id = resource_id.map(str::to_owned);
        let detail = detail.map(str::to_owned);
        self.on_thread(move |client| {
            client
                .execute(
                    "INSERT INTO pmman_audit_events(actor,event_type,resource_type,resource_id,detail)
                     VALUES ($1,$2,$3,$4,$5)",
                    &[&actor, &event_type, &resource_type, &resource_id, &detail],
                )
                .map_err(|error| format!("写入审计事件失败：{error}"))?;
            Ok(())
        })
    }

    pub fn ping(&self) -> Result<(), String> {
        self.on_thread(|client| {
            client
                .simple_query("SELECT 1")
                .map_err(|error| format!("PostgreSQL 健康检查失败：{error}"))?;
            Ok(())
        })
    }

    /// Run a migration batch in one PostgreSQL transaction. This is kept out
    /// of the request path because the synchronous driver is deliberately
    /// isolated from Tokio there; the one-shot migration binary uses it to
    /// prevent a half-imported control plane after a later validation or write
    /// failure.
    #[cfg(feature = "legacy-migration")]
    pub fn with_transaction<T, F>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce(&mut postgres::Transaction<'_>) -> Result<T, String>,
    {
        let mut client = self.client()?;
        let mut transaction = client
            .transaction()
            .map_err(|error| format!("开启 PostgreSQL 事务失败：{error}"))?;
        match operation(&mut transaction) {
            Ok(value) => {
                transaction
                    .commit()
                    .map_err(|error| format!("提交 PostgreSQL 迁移事务失败：{error}"))?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run_database_operation;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_driver_is_isolated_from_http_runtime() {
        assert_eq!(
            run_database_operation(|| {
                assert!(tokio::runtime::Handle::try_current().is_err());
                Ok(42)
            })
            .unwrap(),
            42
        );
        assert!(run_database_operation::<(), _>(|| Err("database unavailable".into())).is_err());
    }

    #[tokio::test]
    async fn current_thread_runtime_can_use_sync_store() {
        assert!(run_database_operation(|| {
            assert!(tokio::runtime::Handle::try_current().is_err());
            Ok(())
        })
        .is_ok());
    }
}
