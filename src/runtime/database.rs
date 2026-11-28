//! Opt-in SQLite/PostgreSQL runtime journals with namespace-bound keys and
//! renewable, database-clock leases. This SDK module does not authenticate an
//! HTTP caller or install a task queue. The optional app selects these stores
//! only through explicit database configuration and migration.
mod identity;
mod knowledge;
mod objects;
mod skills;
pub use identity::{
    ApiToken, AuthenticatedActor, IdentityEvent, IdentitySnapshot, IssuedToken, ProjectMembership,
    ProjectPermission, ProjectRole, ServiceTokenRecord, ServiceUser, SqlIdentityStore,
};
pub use knowledge::{SqlCorpusStore, SqlMemoryStore};
pub use objects::{SqlObjectLease, SqlObjectStore};
pub use skills::SqlSkillStore;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::TryStreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::{
    any::{AnyPoolOptions, AnyRow},
    Any, AnyPool, ConnectOptions, Row, Transaction,
};

use super::{
    workflow::{WorkflowLease, WorkflowState, WorkflowStore},
    BudgetSnapshot, RunLease, RunState, RunStore, RuntimeError, Session, SessionStore,
};

const SCHEMA_VERSION: i64 = 1;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct DatabaseOptions {
    pub max_connections: u32,
    pub max_record_bytes: u64,
    pub lease_duration: Duration,
}
impl Default for DatabaseOptions {
    fn default() -> Self {
        Self {
            max_connections: 4,
            max_record_bytes: MAX_BYTES,
            lease_duration: Duration::from_secs(10),
        }
    }
}
impl DatabaseOptions {
    fn validate(&self) -> Result<(), RuntimeError> {
        if !(2..=32).contains(&self.max_connections)
            || !(1024..=MAX_BYTES).contains(&self.max_record_bytes)
            || self.lease_duration < Duration::from_millis(100)
            || self.lease_duration > Duration::from_secs(300)
        {
            return Err(config("database limits require 2..32 connections, 1KiB..64MiB records and 100ms..300s leases"));
        }
        Ok(())
    }
}

/// Trusted SDK host scope, not a credential or caller-supplied ownership grant.
/// Select this only after authenticating/authorizing an actor in the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseNamespace(String);
impl DatabaseNamespace {
    pub fn new(tenant: &str, project: &str) -> Result<Self, RuntimeError> {
        for part in [tenant, project] {
            if part.is_empty()
                || part.len() > 64
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return Err(config(
                    "database tenant/project must contain 1..64 ASCII identifier characters",
                ));
            }
        }
        Ok(Self(
            serde_json::to_string(&(tenant, project)).map_err(storage)?,
        ))
    }
}

#[derive(Clone, Copy)]
enum Backend {
    Sqlite,
    Postgres,
}
impl Backend {
    fn now(self) -> &'static str {
        match self {
            Self::Sqlite => "CAST((julianday('now')-2440587.5)*86400000 AS BIGINT)",
            Self::Postgres => "CAST(floor(EXTRACT(EPOCH FROM clock_timestamp())*1000) AS BIGINT)",
        }
    }
    fn body(self) -> &'static str {
        match self {
            Self::Sqlite => "substr(CAST(body AS BLOB),1,$4)",
            Self::Postgres => "substring(convert_to(body,'UTF8') from 1 for $4)",
        }
    }
}

#[derive(Clone)]
pub struct SqlDatabase(Arc<DatabaseInner>);
struct DatabaseInner {
    pool: AnyPool,
    backend: Backend,
    options: DatabaseOptions,
    pending_releases: AtomicUsize,
    release_notify: tokio::sync::Notify,
}
impl SqlDatabase {
    /// A frozen, escaped SQLite URL suitable for another explicit SQL client.
    pub fn sqlite_url(path: impl AsRef<Path>) -> Result<String, RuntimeError> {
        sqlite_file_url(path.as_ref(), "mode=rwc&cache=private")
    }
    /// Create/open an explicit SQLite file, freezing its absolute path before
    /// any await. URI escaping preserves spaces, Unicode and Windows drives.
    pub async fn sqlite(
        path: impl AsRef<Path>,
        options: DatabaseOptions,
    ) -> Result<Self, RuntimeError> {
        Self::connect(&Self::sqlite_url(path)?, options).await
    }
    /// Connect only. Call `migrate` explicitly before admitting work. URLs may
    /// contain a database credential: never put their value in logs or prompts.
    pub async fn connect(url: &str, options: DatabaseOptions) -> Result<Self, RuntimeError> {
        options.validate()?;
        let (backend, normalized) = if url.starts_with("sqlite:") {
            // Parse SQLite before generic URL normalization: an unescaped
            // Windows drive colon can otherwise be interpreted as a URL port.
            let parsed: sqlx::sqlite::SqliteConnectOptions = url.parse().map_err(database_error)?;
            // Inspect normalized flags with a harmless filename: sqlx's URL
            // builder does not escape a Windows drive colon/backslash itself.
            let canonical = parsed
                .clone()
                .filename("taskloom-flags.sqlite")
                .to_url_lossy();
            if parsed.get_filename().as_os_str().is_empty()
                || parsed.get_filename() == Path::new(":memory:")
                || canonical
                    .query_pairs()
                    .any(|(key, value)| key == "mode" && value == "memory" || key == "vfs")
            {
                return Err(config(
                    "SQLite persistence requires a file and no memory mode/custom VFS",
                ));
            }
            (
                Backend::Sqlite,
                sqlite_file_url(
                    parsed.get_filename(),
                    canonical.query().unwrap_or("mode=rw"),
                )?,
            )
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            (Backend::Postgres, url.to_string())
        } else {
            return Err(config("choose a durable SQLite file or PostgreSQL URL"));
        };
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    let statements = match backend {
                        Backend::Sqlite => [
                            "PRAGMA busy_timeout=5000",
                            "PRAGMA synchronous=FULL",
                            "PRAGMA foreign_keys=ON",
                        ],
                        Backend::Postgres => [
                            "SET statement_timeout=5000",
                            "SET lock_timeout=5000",
                            "SET synchronous_commit=on",
                        ],
                    };
                    for statement in statements {
                        sqlx::query(statement).execute(&mut *connection).await?;
                    }
                    Ok(())
                })
            })
            .connect(&normalized)
            .await
            .map_err(database_error)?;
        if matches!(backend, Backend::Sqlite) {
            sqlx::query("PRAGMA journal_mode=WAL")
                .execute(&pool)
                .await
                .map_err(database_error)?;
        }
        Ok(Self(Arc::new(DatabaseInner {
            pool,
            backend,
            options,
            pending_releases: AtomicUsize::new(0),
            release_notify: tokio::sync::Notify::new(),
        })))
    }
    /// Stop active invocations first. Drain already-dropped lease releases
    /// before closing the pool; a killed process still relies on database TTL.
    pub async fn close(&self) {
        let _ = self.drain_releases().await;
        self.0.pool.close().await;
    }
    async fn drain_releases(&self) -> Result<(), RuntimeError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let notified = self.0.release_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.0.pending_releases.load(Ordering::SeqCst) == 0 {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(conflict(
                    "dropped SQL owners are still releasing; retry explicitly",
                ));
            }
        }
        Ok(())
    }
    /// Versioned, transactional DDL. No legacy file data is silently imported.
    pub async fn migrate(&self) -> Result<(), RuntimeError> {
        let mut tx = self.write_transaction().await?;
        if matches!(self.0.backend, Backend::Postgres) {
            sqlx::query("DO $$ BEGIN PERFORM pg_advisory_xact_lock(7351829041762441); END $$")
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
        }
        sqlx::query("CREATE TABLE IF NOT EXISTS taskloom_meta (singleton BIGINT PRIMARY KEY CHECK(singleton=1),version BIGINT NOT NULL)").execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("INSERT INTO taskloom_meta(singleton,version) VALUES(1,1) ON CONFLICT(singleton) DO NOTHING").execute(&mut *tx).await.map_err(database_error)?;
        let version: i64 =
            sqlx::query_scalar("SELECT version FROM taskloom_meta WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
        if version != SCHEMA_VERSION {
            return Err(config("unsupported database schema; migration refused"));
        }
        sqlx::query("CREATE TABLE IF NOT EXISTS taskloom_records (namespace TEXT NOT NULL,family TEXT NOT NULL,object_id TEXT NOT NULL,revision BIGINT NOT NULL CHECK(revision>=0),body TEXT NOT NULL,PRIMARY KEY(namespace,family,object_id))").execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS taskloom_leases (namespace TEXT NOT NULL,family TEXT NOT NULL,object_id TEXT NOT NULL,owner TEXT NOT NULL,epoch BIGINT NOT NULL CHECK(epoch>0),expires_ms BIGINT NOT NULL,PRIMARY KEY(namespace,family,object_id))").execute(&mut *tx).await.map_err(database_error)?;
        let identity_key = dataset_key()?;
        let value = uuid::Uuid::new_v4().to_string();
        let body = self.serialize(&identity_key, &value)?;
        sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) VALUES($1,$2,$3,0,$4) ON CONFLICT(namespace,family,object_id) DO NOTHING")
            .bind(&identity_key.namespace.0).bind(identity_key.family).bind(&identity_key.id).bind(body)
            .execute(&mut *tx).await.map_err(database_error)?;
        parse_dataset(
            self.read_tx(&mut tx, &identity_key)
                .await?
                .ok_or_else(|| storage("database identity missing"))?,
        )?;
        tx.commit().await.map_err(database_error)
    }
    /// Stable nonsecret dataset identity. Explicit migration establishes it;
    /// backup/restore retains it and rotating database credentials does not
    /// alter durable host contracts. Legacy schema-v1 stores must rerun migrate.
    pub async fn identity(&self) -> Result<String, RuntimeError> {
        parse_dataset(
            self.read(&dataset_key()?).await?.ok_or_else(|| {
                config("database dataset identity missing; run explicit migration")
            })?,
        )
    }
    pub fn runs(&self, namespace: DatabaseNamespace) -> SqlRunStore {
        SqlRunStore {
            db: self.clone(),
            namespace,
        }
    }
    pub fn workflows(&self, namespace: DatabaseNamespace) -> SqlWorkflowStore {
        SqlWorkflowStore {
            db: self.clone(),
            namespace,
        }
    }
    pub fn sessions(&self, namespace: DatabaseNamespace) -> SqlSessionStore {
        SqlSessionStore {
            db: self.clone(),
            namespace,
        }
    }
    pub fn corpus(&self, namespace: DatabaseNamespace) -> SqlCorpusStore {
        SqlCorpusStore::new(self.clone(), namespace)
    }
    pub fn memory(&self, namespace: DatabaseNamespace) -> SqlMemoryStore {
        SqlMemoryStore::new(self.clone(), namespace)
    }
    /// Trusted host identity administration; HTTP handlers must expose only
    /// authenticated, scoped operations rather than this raw store.
    pub fn identities(&self) -> SqlIdentityStore {
        SqlIdentityStore::new(self.clone())
    }
    /// Host-only auxiliary records, e.g. explicitly built embedding indexes
    /// and their independent spending journal. Not an HTTP object-write API.
    pub fn objects(&self, namespace: DatabaseNamespace) -> SqlObjectStore {
        SqlObjectStore::new(self.clone(), namespace)
    }
    pub fn skills(&self, namespace: DatabaseNamespace) -> SqlSkillStore {
        SqlSkillStore::new(self.clone(), namespace)
    }
    async fn schema(&self) -> Result<(), RuntimeError> {
        let value: Option<i64> =
            sqlx::query_scalar("SELECT version FROM taskloom_meta WHERE singleton=1")
                .fetch_optional(&self.0.pool)
                .await
                .map_err(database_error)?;
        if value != Some(SCHEMA_VERSION) {
            return Err(config(
                "database schema is absent or unsupported; migrate explicitly",
            ));
        }
        Ok(())
    }
    async fn write_transaction(&self) -> Result<Transaction<'static, Any>, RuntimeError> {
        self.0
            .pool
            .begin_with(match self.0.backend {
                Backend::Sqlite => "BEGIN IMMEDIATE",
                Backend::Postgres => "BEGIN",
            })
            .await
            .map_err(database_error)
    }
    fn read_query(&self) -> String {
        format!("SELECT revision,{} AS payload FROM taskloom_records WHERE namespace=$1 AND family=$2 AND object_id=$3",self.0.backend.body())
    }
    async fn read(&self, key: &RecordKey) -> Result<Option<Record>, RuntimeError> {
        self.schema().await?;
        let row = sqlx::query(&self.read_query())
            .bind(&key.namespace.0)
            .bind(key.family)
            .bind(&key.id)
            .bind((self.0.options.max_record_bytes + 1) as i32)
            .fetch_optional(&self.0.pool)
            .await
            .map_err(database_error)?;
        row.map(|r| decode_record(r, key.clone(), self.0.options.max_record_bytes))
            .transpose()
    }
    async fn list<T>(
        &self,
        namespace: &DatabaseNamespace,
        family: &'static str,
        parse: impl Fn(Record) -> Result<T, RuntimeError>,
    ) -> Result<Vec<T>, RuntimeError> {
        self.schema().await?;
        let query=format!("SELECT object_id,revision,{} AS payload FROM taskloom_records WHERE namespace=$1 AND family=$2 ORDER BY object_id LIMIT $3",self.0.backend.body());
        let mut rows = sqlx::query(&query)
            .bind(&namespace.0)
            .bind(family)
            .bind(1000_i64)
            .bind((self.0.options.max_record_bytes + 1) as i32)
            .fetch(&self.0.pool);
        let mut values = Vec::new();
        let mut bytes = 0_u64;
        while let Some(row) = rows.try_next().await.map_err(database_error)? {
            let id: String = row.try_get("object_id").map_err(database_error)?;
            let record = decode_record(
                row,
                RecordKey::new(namespace.clone(), family, &id)?,
                self.0.options.max_record_bytes,
            )?;
            bytes = bytes.saturating_add(record.body.len() as u64);
            if bytes > MAX_BYTES {
                return Err(RuntimeError::StorageLimit(
                    "database listing exceeds 64MiB; inspect individual records".into(),
                ));
            }
            values.push(parse(record)?);
        }
        Ok(values)
    }
    async fn read_tx(
        &self,
        tx: &mut Transaction<'_, Any>,
        key: &RecordKey,
    ) -> Result<Option<Record>, RuntimeError> {
        let row = sqlx::query(&self.read_query())
            .bind(&key.namespace.0)
            .bind(key.family)
            .bind(&key.id)
            .bind((self.0.options.max_record_bytes + 1) as i32)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
        row.map(|r| decode_record(r, key.clone(), self.0.options.max_record_bytes))
            .transpose()
    }
    fn serialize<T: Serialize>(&self, key: &RecordKey, value: &T) -> Result<String, RuntimeError> {
        let body = serde_json::to_string(&Envelope {
            version: 1,
            namespace: key.namespace.0.clone(),
            family: key.family.to_string(),
            id: key.id.clone(),
            value,
        })
        .map_err(storage)?;
        if body.len() as u64 > self.0.options.max_record_bytes {
            return Err(RuntimeError::StorageLimit(
                "database record exceeds configured byte limit".into(),
            ));
        }
        Ok(body)
    }
    async fn replace(
        &self,
        tx: &mut Transaction<'_, Any>,
        key: &RecordKey,
        revision: u64,
        body: &str,
    ) -> Result<(), RuntimeError> {
        sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) VALUES($1,$2,$3,$4,$5) ON CONFLICT(namespace,family,object_id) DO UPDATE SET revision=excluded.revision,body=excluded.body")
            .bind(&key.namespace.0).bind(key.family).bind(&key.id).bind(sequence(revision)?).bind(body).execute(&mut **tx).await.map_err(database_error)?;
        Ok(())
    }
    /// Lock a bounded aggregate even when it has not been created yet. The
    /// seed is transactional, so a refused operation leaves no empty record.
    /// PostgreSQL's no-op update locks the row; SQLite already owns its writer.
    async fn lock_record<T: Serialize>(
        &self,
        tx: &mut Transaction<'_, Any>,
        key: &RecordKey,
        initial: &T,
    ) -> Result<Record, RuntimeError> {
        let body = self.serialize(key, initial)?;
        sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) VALUES($1,$2,$3,0,$4) ON CONFLICT(namespace,family,object_id) DO NOTHING")
            .bind(&key.namespace.0).bind(key.family).bind(&key.id).bind(body)
            .execute(&mut **tx).await.map_err(database_error)?;
        sqlx::query("UPDATE taskloom_records SET revision=revision WHERE namespace=$1 AND family=$2 AND object_id=$3")
            .bind(&key.namespace.0).bind(key.family).bind(&key.id)
            .execute(&mut **tx).await.map_err(database_error)?;
        self.read_tx(tx, key)
            .await?
            .ok_or_else(|| storage("locked database record is absent"))
    }
    async fn acquire(&self, key: RecordKey) -> Result<Arc<Lease>, RuntimeError> {
        // A prior SDK operation may just have dropped its final owner. Await
        // its already-enqueued release on this pool, rather than racing it and
        // reporting a spurious occupied record on immediate resume. This never
        // retries an invocation or steals a live/foreign process's lease.
        self.drain_releases().await?;
        self.schema().await?;
        let owner = uuid::Uuid::new_v4().to_string();
        let now = self.0.backend.now();
        let sql=format!("INSERT INTO taskloom_leases(namespace,family,object_id,owner,epoch,expires_ms) VALUES($1,$2,$3,$4,1,{now}+$5) ON CONFLICT(namespace,family,object_id) DO UPDATE SET owner=excluded.owner,epoch=taskloom_leases.epoch+1,expires_ms=excluded.expires_ms WHERE taskloom_leases.expires_ms<={now} AND taskloom_leases.epoch<9223372036854775807 RETURNING epoch");
        let epoch: Option<i64> = sqlx::query_scalar(&sql)
            .bind(&key.namespace.0)
            .bind(key.family)
            .bind(&key.id)
            .bind(&owner)
            .bind(self.0.options.lease_duration.as_millis() as i64)
            .fetch_optional(&self.0.pool)
            .await
            .map_err(database_error)?;
        let epoch = epoch.ok_or_else(|| conflict("database record is owned by another worker"))?;
        let lease = Arc::new(Lease {
            db: self.clone(),
            key,
            owner,
            epoch,
            lost: AtomicBool::new(false),
            heartbeat: Mutex::new(None),
        });
        let weak = Arc::downgrade(&lease);
        let period = self.0.options.lease_duration / 3;
        let task = tokio::spawn(async move {
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let Some(lease) = weak.upgrade() else { return };
                if lease.known_owned().is_err() {
                    return;
                }
                let (db, key, owner, epoch) = (
                    lease.db.clone(),
                    lease.key.clone(),
                    lease.owner.clone(),
                    lease.epoch,
                );
                // No owner Arc is retained across renewal I/O: the caller's
                // final drop can queue its conditional release before close.
                drop(lease);
                if Lease::renew_owned(db, key, owner, epoch).await.is_err() {
                    if let Some(lease) = weak.upgrade() {
                        lease.lost.store(true, Ordering::SeqCst);
                    }
                    return;
                }
            }
        });
        *lease
            .heartbeat
            .lock()
            .map_err(|_| storage("database heartbeat lock poisoned"))? = Some(task);
        Ok(lease)
    }
}

#[derive(Clone)]
struct RecordKey {
    namespace: DatabaseNamespace,
    family: &'static str,
    id: String,
}
impl RecordKey {
    fn new(
        namespace: DatabaseNamespace,
        family: &'static str,
        id: &str,
    ) -> Result<Self, RuntimeError> {
        if id.trim().is_empty() || id.len() > 256 {
            return Err(config("database record ID must contain 1..256 UTF-8 bytes"));
        }
        Ok(Self {
            namespace,
            family,
            id: id.into(),
        })
    }
    fn family(&self, family: &'static str) -> Self {
        Self {
            family,
            ..self.clone()
        }
    }
}
struct Record {
    key: RecordKey,
    revision: u64,
    body: Vec<u8>,
}
impl Record {
    fn parse<T: DeserializeOwned>(&self) -> Result<T, RuntimeError> {
        let envelope: Envelope<T> = serde_json::from_slice(&self.body).map_err(storage)?;
        if envelope.version != 1
            || envelope.namespace != self.key.namespace.0
            || envelope.family != self.key.family
            || envelope.id != self.key.id
        {
            return Err(storage(
                "database envelope version or scoped identity differs",
            ));
        }
        Ok(envelope.value)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    version: u32,
    namespace: String,
    family: String,
    id: String,
    value: T,
}
fn decode_record(row: AnyRow, key: RecordKey, max: u64) -> Result<Record, RuntimeError> {
    let revision: i64 = row.try_get("revision").map_err(database_error)?;
    let body: Vec<u8> = row.try_get("payload").map_err(database_error)?;
    if body.len() as u64 > max {
        return Err(RuntimeError::StorageLimit(
            "stored database record exceeds byte limit".into(),
        ));
    }
    Ok(Record {
        key,
        revision: u64::try_from(revision).map_err(storage)?,
        body,
    })
}
struct Lease {
    db: SqlDatabase,
    key: RecordKey,
    owner: String,
    epoch: i64,
    lost: AtomicBool,
    heartbeat: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Lease {
    fn known_owned(&self) -> Result<(), RuntimeError> {
        if self.lost.load(Ordering::SeqCst) {
            return Err(conflict("database lease ownership was lost"));
        }
        Ok(())
    }
    fn ownership_query(&self) -> String {
        format!("SELECT epoch FROM taskloom_leases WHERE namespace=$1 AND family=$2 AND object_id=$3 AND owner=$4 AND epoch=$5 AND expires_ms>{} AND EXISTS(SELECT 1 FROM taskloom_meta WHERE singleton=1 AND version=1)",self.db.0.backend.now())
    }
    fn renewal_query(&self) -> String {
        renewal_query(&self.db)
    }
    async fn check(&self) -> Result<(), RuntimeError> {
        self.known_owned()?;
        let row: Option<i64> = sqlx::query_scalar(&self.ownership_query())
            .bind(&self.key.namespace.0)
            .bind(self.key.family)
            .bind(&self.key.id)
            .bind(&self.owner)
            .bind(self.epoch)
            .fetch_optional(&self.db.0.pool)
            .await
            .map_err(database_error)?;
        self.checked(row)
    }
    async fn lock(&self, tx: &mut Transaction<'_, Any>) -> Result<(), RuntimeError> {
        self.known_owned()?;
        // Updating locks the ownership row (and SQLite's immediate transaction)
        // and refreshes its bounded TTL without reviving an expired lease.
        let row: Option<i64> = sqlx::query_scalar(&(self.renewal_query() + " RETURNING epoch"))
            .bind(&self.key.namespace.0)
            .bind(self.key.family)
            .bind(&self.key.id)
            .bind(&self.owner)
            .bind(self.epoch)
            .bind(self.db.0.options.lease_duration.as_millis() as i64)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
        self.checked(row)
    }
    fn checked(&self, value: Option<i64>) -> Result<(), RuntimeError> {
        if value != Some(self.epoch) {
            self.lost.store(true, Ordering::SeqCst);
            return Err(conflict("database lease expired or fencing token changed"));
        }
        Ok(())
    }
    async fn commit(&self, mut tx: Transaction<'static, Any>) -> Result<(), RuntimeError> {
        // Validate again after decoding/comparing a possibly large prior record.
        // An expired owner cannot commit merely because it locked the row earlier.
        self.lock(&mut tx).await?;
        tx.commit().await.map_err(database_error)
    }
    async fn renew_owned(
        db: SqlDatabase,
        key: RecordKey,
        owner: String,
        epoch: i64,
    ) -> Result<(), RuntimeError> {
        let result = sqlx::query(&renewal_query(&db))
            .bind(&key.namespace.0)
            .bind(key.family)
            .bind(&key.id)
            .bind(&owner)
            .bind(epoch)
            .bind(db.0.options.lease_duration.as_millis() as i64)
            .execute(&db.0.pool)
            .await
            .map_err(database_error)?;
        if result.rows_affected() != 1 {
            return Err(conflict("database lease renewal refused"));
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut task) = self.heartbeat.lock() {
            if let Some(task) = task.take() {
                task.abort();
            }
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let db = self.db.clone();
            let key = self.key.clone();
            let owner = self.owner.clone();
            let epoch = self.epoch;
            db.0.pending_releases.fetch_add(1, Ordering::SeqCst);
            let pending = PendingRelease(Arc::clone(&db.0));
            runtime.spawn(async move {
                let _pending=pending;
                // Preserve the fencing epoch forever; a delayed release cannot
                // release a successor's ownership. Without a runtime, TTL wins.
                let _=sqlx::query("UPDATE taskloom_leases SET expires_ms=0 WHERE namespace=$1 AND family=$2 AND object_id=$3 AND owner=$4 AND epoch=$5")
                    .bind(&key.namespace.0).bind(key.family).bind(&key.id).bind(owner).bind(epoch).execute(&db.0.pool).await;
            });
        }
    }
}

struct PendingRelease(Arc<DatabaseInner>);
impl Drop for PendingRelease {
    fn drop(&mut self) {
        if self.0.pending_releases.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.release_notify.notify_waiters();
        }
    }
}
fn renewal_query(db: &SqlDatabase) -> String {
    let now = db.0.backend.now();
    format!("UPDATE taskloom_leases SET expires_ms={now}+$6 WHERE namespace=$1 AND family=$2 AND object_id=$3 AND owner=$4 AND epoch=$5 AND expires_ms>{now} AND EXISTS(SELECT 1 FROM taskloom_meta WHERE singleton=1 AND version=1)")
}

#[derive(Clone)]
pub struct SqlRunStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlRunStore {
    /// At most 1000 validated records / 64MiB, ordered by ID.
    pub async fn list(&self) -> Result<Vec<RunState>, RuntimeError> {
        self.db
            .list(&self.namespace, "run", |row| {
                let id = row.key.id.clone();
                parse_run(row, &id)
            })
            .await
    }
    /// Read the scoped reservation journal without taking ownership. A newer
    /// full checkpoint may contain a later settled budget generation.
    pub async fn inspect_budget(&self, id: &str) -> Result<Option<BudgetSnapshot>, RuntimeError> {
        self.db
            .read(&RecordKey::new(self.namespace.clone(), "budget", id)?)
            .await?
            .map(parse_budget)
            .transpose()
    }
}
impl RunStore for SqlRunStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RunLease>, RuntimeError>> {
        Box::pin(async move {
            Ok(Arc::new(RunDatabaseLease(
                self.db
                    .acquire(RecordKey::new(self.namespace.clone(), "run", id)?)
                    .await?,
            )) as Arc<dyn RunLease>)
        })
    }
    fn inspect<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<RunState>, RuntimeError>> {
        Box::pin(async move {
            let key = RecordKey::new(self.namespace.clone(), "run", id)?;
            self.db
                .read(&key)
                .await?
                .map(|r| parse_run(r, id))
                .transpose()
        })
    }
}
struct RunDatabaseLease(Arc<Lease>);
impl RunLease for RunDatabaseLease {
    fn verify_ownership(&self) -> BoxFuture<'_, Result<(), RuntimeError>> {
        Box::pin(self.0.check())
    }
    fn max_bytes(&self) -> Option<u64> {
        Some(self.0.db.0.options.max_record_bytes)
    }
    fn load(&self) -> BoxFuture<'_, Result<Option<RunState>, RuntimeError>> {
        Box::pin(async move {
            self.0.check().await?;
            self.0
                .db
                .read(&self.0.key)
                .await?
                .map(|r| parse_run(r, &self.0.key.id))
                .transpose()
        })
    }
    fn save<'a>(&'a self, state: &'a RunState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            state.validate()?;
            if state.report.run_id != self.0.key.id {
                return Err(storage("checkpoint ID differs from database lease"));
            }
            let body = self.0.db.serialize(&self.0.key, state)?;
            let mut tx = self.0.db.write_transaction().await?;
            self.0.lock(&mut tx).await?;
            if let Some(old) = self.0.db.read_tx(&mut tx, &self.0.key).await? {
                let previous = parse_run(old, &self.0.key.id)?;
                if previous.trace_binding != state.trace_binding {
                    return Err(storage(
                        "immutable trace binding differs from original database checkpoint",
                    ));
                }
                if previous.revision > state.revision {
                    return Err(conflict("a newer database checkpoint revision is stored"));
                }
                if previous.revision == state.revision {
                    return if serde_json::to_value(previous).map_err(storage)?
                        == serde_json::to_value(state).map_err(storage)?
                    {
                        self.0.commit(tx).await
                    } else {
                        Err(storage(
                            "conflicting checkpoint with equal database revision",
                        ))
                    };
                }
            }
            self.0
                .db
                .replace(&mut tx, &self.0.key, state.revision, &body)
                .await?;
            self.0.commit(tx).await
        })
    }
    fn load_budget(&self) -> BoxFuture<'_, Result<Option<BudgetSnapshot>, RuntimeError>> {
        Box::pin(async move {
            self.0.check().await?;
            self.0
                .db
                .read(&self.0.key.family("budget"))
                .await?
                .map(parse_budget)
                .transpose()
        })
    }
    fn save_budget<'a>(
        &'a self,
        value: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let key = self.0.key.family("budget");
            let body = self.0.db.serialize(&key, value)?;
            let mut tx = self.0.db.write_transaction().await?;
            self.0.lock(&mut tx).await?;
            if let Some(old) = self.0.db.read_tx(&mut tx, &key).await? {
                let old = parse_budget(old)?;
                if old.generation > value.generation {
                    return self.0.commit(tx).await;
                }
                if old.generation == value.generation {
                    return if &old == value {
                        self.0.commit(tx).await
                    } else {
                        Err(storage("conflicting database budget generation"))
                    };
                }
            }
            self.0
                .db
                .replace(&mut tx, &key, value.generation, &body)
                .await?;
            self.0.commit(tx).await
        })
    }
}
fn parse_run(row: Record, id: &str) -> Result<RunState, RuntimeError> {
    let state: RunState = row.parse()?;
    state.validate()?;
    if state.report.run_id != id || state.revision != row.revision {
        return Err(storage("stored database run identity/revision differs"));
    }
    Ok(state)
}
fn parse_budget(row: Record) -> Result<BudgetSnapshot, RuntimeError> {
    let value: BudgetSnapshot = row.parse()?;
    if value.generation != row.revision {
        return Err(storage("database budget generation differs from record"));
    }
    Ok(value)
}

#[derive(Clone)]
pub struct SqlWorkflowStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlWorkflowStore {
    pub async fn list(&self) -> Result<Vec<WorkflowState>, RuntimeError> {
        self.db
            .list(&self.namespace, "workflow", |row| {
                let id = row.key.id.clone();
                parse_workflow(row, &id)
            })
            .await
    }
}
impl WorkflowStore for SqlWorkflowStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn WorkflowLease>, RuntimeError>> {
        Box::pin(async move {
            Ok(Arc::new(WorkflowDatabaseLease(
                self.db
                    .acquire(RecordKey::new(self.namespace.clone(), "workflow", id)?)
                    .await?,
            )) as Arc<dyn WorkflowLease>)
        })
    }
    fn inspect<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WorkflowState>, RuntimeError>> {
        Box::pin(async move {
            self.db
                .read(&RecordKey::new(self.namespace.clone(), "workflow", id)?)
                .await?
                .map(|r| parse_workflow(r, id))
                .transpose()
        })
    }
}
struct WorkflowDatabaseLease(Arc<Lease>);
impl WorkflowLease for WorkflowDatabaseLease {
    fn verify_ownership(&self) -> BoxFuture<'_, Result<(), RuntimeError>> {
        Box::pin(self.0.check())
    }
    fn load(&self) -> BoxFuture<'_, Result<Option<WorkflowState>, RuntimeError>> {
        Box::pin(async move {
            self.0.check().await?;
            self.0
                .db
                .read(&self.0.key)
                .await?
                .map(|r| parse_workflow(r, &self.0.key.id))
                .transpose()
        })
    }
    fn save<'a>(&'a self, state: &'a WorkflowState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            state.validate()?;
            if state.id != self.0.key.id {
                return Err(storage("workflow ID differs from database lease"));
            }
            let body = self.0.db.serialize(&self.0.key, state)?;
            let mut tx = self.0.db.write_transaction().await?;
            self.0.lock(&mut tx).await?;
            if let Some(old) = self.0.db.read_tx(&mut tx, &self.0.key).await? {
                let previous = parse_workflow(old, &self.0.key.id)?;
                if previous.revision > state.revision {
                    return Err(conflict("a newer database workflow revision is stored"));
                }
                if previous.revision == state.revision {
                    return if serde_json::to_value(previous).map_err(storage)?
                        == serde_json::to_value(state).map_err(storage)?
                    {
                        self.0.commit(tx).await
                    } else {
                        Err(storage("conflicting database workflow revision"))
                    };
                }
            }
            self.0
                .db
                .replace(&mut tx, &self.0.key, state.revision, &body)
                .await?;
            self.0.commit(tx).await
        })
    }
}
fn parse_workflow(row: Record, id: &str) -> Result<WorkflowState, RuntimeError> {
    let value: WorkflowState = row.parse()?;
    value.validate()?;
    if value.id != id || value.revision != row.revision {
        return Err(storage(
            "stored database workflow identity/revision differs",
        ));
    }
    Ok(value)
}

#[derive(Clone)]
pub struct SqlSessionStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlSessionStore {
    pub async fn list(&self) -> Result<Vec<Session>, RuntimeError> {
        self.db
            .list(&self.namespace, "session", |row| {
                let id = row.key.id.clone();
                parse_session(row, &id)
            })
            .await
    }
}
impl SessionStore for SqlSessionStore {
    fn max_bytes(&self) -> Option<u64> {
        Some(self.db.0.options.max_record_bytes)
    }
    fn load<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Session>, RuntimeError>> {
        Box::pin(async move {
            self.db
                .read(&RecordKey::new(self.namespace.clone(), "session", id)?)
                .await?
                .map(|r| parse_session(r, id))
                .transpose()
        })
    }
    fn save<'a>(
        &'a self,
        value: &'a Session,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            value.validate()?;
            if Some(value.revision) != expected_revision.map_or(Some(1), |r| r.checked_add(1)) {
                return Err(conflict("session revision must advance exactly once"));
            }
            let key = RecordKey::new(self.namespace.clone(), "session", &value.id)?;
            let body = self.db.serialize(&key, value)?;
            // Session CAS is one SQL statement, unlike an invocation-long run lease.
            // Validate append-only history in the same transaction; a concurrent
            // successor changes the expected revision and cannot be overwritten.
            self.db.schema().await?;
            let mut tx = self.db.write_transaction().await?;
            let old = self.db.read_tx(&mut tx, &key).await?;
            if old.as_ref().map(|r| r.revision) != expected_revision {
                return Err(conflict("database session revision changed"));
            }
            if let Some(old) = old {
                let previous = parse_session(old, &value.id)?;
                if value.system_message_count != previous.system_message_count
                    || !value.messages.starts_with(&previous.messages)
                    || !value.turns.starts_with(&previous.turns)
                {
                    return Err(conflict("database session history is append-only"));
                }
            }
            let changed = if let Some(expected) = expected_revision {
                sqlx::query("UPDATE taskloom_records SET revision=$4,body=$5 WHERE namespace=$1 AND family=$2 AND object_id=$3 AND revision=$6")
                .bind(&key.namespace.0).bind(key.family).bind(&key.id).bind(sequence(value.revision)?).bind(body).bind(sequence(expected)?).execute(&mut *tx).await.map_err(database_error)?.rows_affected()
            } else {
                sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) VALUES($1,$2,$3,$4,$5) ON CONFLICT(namespace,family,object_id) DO NOTHING")
                .bind(&key.namespace.0).bind(key.family).bind(&key.id).bind(sequence(value.revision)?).bind(body).execute(&mut *tx).await.map_err(database_error)?.rows_affected()
            };
            if changed != 1 {
                return Err(conflict("database session compare-and-swap failed"));
            }
            tx.commit().await.map_err(database_error)
        })
    }
}
fn parse_session(row: Record, id: &str) -> Result<Session, RuntimeError> {
    let value: Session = row.parse()?;
    value.validate()?;
    if value.id != id || value.revision != row.revision {
        return Err(storage("stored database session identity/revision differs"));
    }
    Ok(value)
}

fn sequence(value: u64) -> Result<i64, RuntimeError> {
    i64::try_from(value).map_err(|_| storage("database sequence exceeds signed BIGINT"))
}
fn sqlite_file_url(path: &Path, query: &str) -> Result<String, RuntimeError> {
    let path = std::path::absolute(path).map_err(storage)?;
    let raw = path
        .to_str()
        .ok_or_else(|| config("SQLite file path must be valid UTF-8"))?;
    if raw.len() > 32768 {
        return Err(config("SQLite file path exceeds 32768 bytes"));
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(raw.len() * 3);
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(b));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(b >> 4)]));
            encoded.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
    Ok(format!("sqlite://{encoded}?{query}"))
}
fn config(value: &str) -> RuntimeError {
    RuntimeError::Configuration(value.into())
}
fn conflict(value: &str) -> RuntimeError {
    RuntimeError::Conflict(value.into())
}
fn storage(value: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Storage(value.to_string())
}
fn dataset_key() -> Result<RecordKey, RuntimeError> {
    RecordKey::new(
        DatabaseNamespace::new("_taskloom_control", "storage")?,
        "metadata",
        "dataset",
    )
}
fn parse_dataset(row: Record) -> Result<String, RuntimeError> {
    let value: String = row.parse()?;
    let uuid =
        uuid::Uuid::parse_str(&value).map_err(|_| storage("invalid database dataset identity"))?;
    if row.revision != 0 || uuid.get_version_num() != 4 || uuid.to_string() != value {
        return Err(storage("invalid database dataset identity"));
    }
    Ok(value)
}
fn database_error(error: sqlx::Error) -> RuntimeError {
    // URLs/passwords, SQL parameters and server error details are not API data.
    let category = match &error {
        sqlx::Error::Database(e) => {
            format!("server code {}", e.code().as_deref().unwrap_or("unknown"))
        }
        sqlx::Error::PoolTimedOut => "pool timeout".into(),
        sqlx::Error::PoolClosed => "pool closed".into(),
        sqlx::Error::RowNotFound => "row missing".into(),
        sqlx::Error::Decode(_) | sqlx::Error::ColumnDecode { .. } => "row decode".into(),
        _ => "connection or query".into(),
    };
    storage(format!("database operation failed ({category})"))
}
