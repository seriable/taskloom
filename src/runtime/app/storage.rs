//! One explicit persistence selection shared by execution and cold inspection.
use super::{config::config_error, AppConfig, RunSummary};
#[cfg(feature = "database")]
use super::{config::secret, JobStatus};
#[cfg(feature = "database")]
use crate::runtime::database::*;
use crate::runtime::{memory::*, rag::*, skills::*, workflow::*, *};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(feature = "database")]
use std::time::Duration;
use std::{path::Path, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppDatabase {
    pub url_env: String,
    pub tenant: String,
    pub project: String,
    #[serde(default = "connections")]
    pub max_connections: u32,
    #[serde(default = "record_bytes")]
    pub max_record_bytes: u64,
    #[serde(default = "lease_ms")]
    pub lease_ms: u64,
}
fn connections() -> u32 {
    4
}
fn record_bytes() -> u64 {
    64 * 1024 * 1024
}
fn lease_ms() -> u64 {
    10_000
}
impl AppDatabase {
    /// Explicit schema/dataset migration; only the database credential env is
    /// resolved. No model, tool, embedding or telemetry service is initialized.
    pub async fn migrate(&self) -> Result<String, RuntimeError> {
        self.validate()?;
        #[cfg(feature = "database")]
        {
            let db = SqlDatabase::connect(
                &secret(&self.url_env)?,
                DatabaseOptions {
                    max_connections: self.max_connections,
                    max_record_bytes: self.max_record_bytes,
                    lease_duration: Duration::from_millis(self.lease_ms),
                },
            )
            .await?;
            let result = async {
                db.migrate().await?;
                db.identity().await
            }
            .await;
            db.close().await;
            result
        }
        #[cfg(not(feature = "database"))]
        Err(config_error(
            "SQL persistence requires the database Cargo feature",
        ))
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.url_env.is_empty()
            || self.url_env.len() > 128
            || !self
                .url_env
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit())
            || !(2..=32).contains(&self.max_connections)
            || !(1024..=64 * 1024 * 1024).contains(&self.max_record_bytes)
            || !(100..=300_000).contains(&self.lease_ms)
        {
            return Err(config_error(
                "invalid SQL environment name or connection/record/lease bounds",
            ));
        }
        for name in [&self.tenant, &self.project] {
            if name.is_empty()
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            {
                return Err(config_error(
                    "SQL tenant/project must be 1..64 ASCII identifier characters",
                ));
            }
        }
        if !cfg!(feature = "database") {
            return Err(config_error(
                "SQL persistence requires the database Cargo feature",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct AppStores {
    pub runs: AppRunStore,
    pub workflows: AppWorkflowStore,
    pub sessions: AppSessionStore,
    pub corpus: AppCorpusStore,
    pub memory: AppMemoryStore,
    pub skills: AppSkillStore,
    identity: String,
    #[cfg(feature = "database")]
    pub(super) sql: Option<(SqlDatabase, DatabaseNamespace)>,
}
impl AppStores {
    /// Does not initialize model, MCP, embedding or telemetry credentials.
    /// SQL schema/dataset identity must have been explicitly migrated first.
    pub async fn open(config: &AppConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        #[cfg(feature = "database")]
        if let Some(cfg) = &config.database {
            let db = SqlDatabase::connect(
                &secret(&cfg.url_env)?,
                DatabaseOptions {
                    max_connections: cfg.max_connections,
                    max_record_bytes: cfg.max_record_bytes,
                    lease_duration: Duration::from_millis(cfg.lease_ms),
                },
            )
            .await?;
            let ns = DatabaseNamespace::new(&cfg.tenant, &cfg.project)?;
            let identity = crate::runtime::state::fingerprint(&(
                "app-sql-dataset-v1",
                db.identity().await?,
                &cfg.tenant,
                &cfg.project,
            ))?;
            return Ok(Self {
                runs: AppRunStore::Sql(db.runs(ns.clone())),
                workflows: AppWorkflowStore::Sql(db.workflows(ns.clone())),
                sessions: AppSessionStore::Sql(db.sessions(ns.clone())),
                corpus: AppCorpusStore::Sql(db.corpus(ns.clone())),
                memory: AppMemoryStore::Sql(db.memory(ns.clone())),
                skills: AppSkillStore::Sql(db.skills(ns.clone())),
                identity,
                sql: Some((db, ns)),
            });
        }
        let runs = FileRunStore::new(config.state_dir.join("runs"));
        use sha2::{Digest, Sha256};
        let identity = hex::encode(Sha256::digest(runs.root().to_string_lossy().as_bytes()));
        Ok(Self {
            runs: AppRunStore::File(runs),
            workflows: AppWorkflowStore::File(FileWorkflowStore::new(
                config.state_dir.join("workflows"),
            )),
            sessions: AppSessionStore::File(FileSessionStore::new(
                config.state_dir.join("sessions"),
            )),
            corpus: AppCorpusStore::File(FileCorpusStore::new(config.state_dir.join("knowledge"))),
            memory: AppMemoryStore::File(FileMemoryStore::new(config.state_dir.join("memory"))),
            skills: AppSkillStore::File(FileSkillStore::new(config.state_dir.join("skills"))),
            identity,
            #[cfg(feature = "database")]
            sql: None,
        })
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub(super) fn label(&self, config: &AppConfig) -> Result<String, RuntimeError> {
        let label = config.label()?;
        if config.database.is_some() {
            Ok(format!("{label}:sql:{}", self.identity))
        } else {
            Ok(label)
        }
    }
    pub async fn close(&self) {
        #[cfg(feature = "database")]
        if let Some((db, _)) = &self.sql {
            db.close().await;
        }
    }
}

macro_rules! delegate {($self:expr,$method:ident($($arg:expr),*))=> {match $self {Self::File(s)=>s.$method($($arg),*),#[cfg(feature="database")]Self::Sql(s)=>s.$method($($arg),*)}};}
macro_rules! store {
    ($name:ident,$file:ty,$sql:ty) => {
        #[derive(Clone)]
        pub enum $name {
            File($file),
            #[cfg(feature = "database")]
            Sql($sql),
        }
        impl $name {
            pub fn file_root(&self) -> Option<&Path> {
                match self {
                    Self::File(s) => Some(s.root()),
                    #[cfg(feature = "database")]
                    Self::Sql(_) => None,
                }
            }
        }
    };
}
store!(AppRunStore, FileRunStore, SqlRunStore);
store!(AppWorkflowStore, FileWorkflowStore, SqlWorkflowStore);
store!(AppSessionStore, FileSessionStore, SqlSessionStore);
store!(AppCorpusStore, FileCorpusStore, SqlCorpusStore);
store!(AppMemoryStore, FileMemoryStore, SqlMemoryStore);
store!(AppSkillStore, FileSkillStore, SqlSkillStore);
impl AppSkillStore {
    pub async fn catalog(&self) -> Result<SkillCatalog, RuntimeError> {
        match self {
            Self::File(s) => s.catalog().await,
            #[cfg(feature = "database")]
            Self::Sql(s) => s.catalog().await,
        }
    }
    pub async fn install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> Result<SkillCatalog, RuntimeError> {
        match self {
            Self::File(s) => s.install(package, expected).await,
            #[cfg(feature = "database")]
            Self::Sql(s) => s.install(package, expected).await,
        }
    }
    pub async fn retire(
        &self,
        reference: &SkillRef,
        expected: u64,
    ) -> Result<SkillCatalog, RuntimeError> {
        match self {
            Self::File(s) => s.retire(reference, expected).await,
            #[cfg(feature = "database")]
            Self::Sql(s) => s.retire(reference, expected).await,
        }
    }
    pub async fn inspect(&self, reference: &SkillRef) -> Result<SkillPackage, RuntimeError> {
        match self {
            Self::File(s) => s.inspect(reference).await,
            #[cfg(feature = "database")]
            Self::Sql(s) => s.inspect(reference).await,
        }
    }
    pub async fn activate(
        &self,
        reference: &SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> Result<ActivatedSkill, RuntimeError> {
        match self {
            Self::File(s) => s.activate(reference, grant, host, mode).await,
            #[cfg(feature = "database")]
            Self::Sql(s) => s.activate(reference, grant, host, mode).await,
        }
    }
}
#[cfg(feature = "skill-registry")]
impl RegistrySkillStore for AppSkillStore {
    fn registry_catalog(&self) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
        Box::pin(self.catalog())
    }
    fn registry_install(
        &self,
        package: SkillPackage,
        expected: u64,
    ) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
        Box::pin(self.install(package, expected))
    }
    fn registry_inspect<'a>(
        &'a self,
        reference: &'a SkillRef,
    ) -> BoxFuture<'a, Result<SkillPackage, RuntimeError>> {
        Box::pin(self.inspect(reference))
    }
    fn registry_activate<'a>(
        &'a self,
        reference: &'a SkillRef,
        grant: SkillGrant,
        host: SkillHost,
        mode: SkillActivationMode,
    ) -> BoxFuture<'a, Result<ActivatedSkill, RuntimeError>> {
        Box::pin(self.activate(reference, grant, host, mode))
    }
}
impl RunStore for AppRunStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RunLease>, RuntimeError>> {
        delegate!(self, acquire(id))
    }
    fn inspect<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<RunState>, RuntimeError>> {
        delegate!(self, inspect(id))
    }
}
impl AppRunStore {
    pub async fn list(&self) -> Result<Vec<RunSummary>, RuntimeError> {
        match self {
            Self::File(s) => super::list_persisted(s).await,
            #[cfg(feature = "database")]
            Self::Sql(s) => Ok(s
                .list()
                .await?
                .into_iter()
                .map(|s| RunSummary {
                    id: s.report.run_id,
                    status: s.status,
                    stop_reason: s.report.stop_reason,
                    revision: s.revision,
                    steps: s.report.steps,
                    tool_calls: s.report.tool_calls,
                    elapsed_ms: s.report.elapsed_ms,
                    job: JobStatus {
                        active: false,
                        error: None,
                    },
                })
                .collect()),
        }
    }
}
impl WorkflowStore for AppWorkflowStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn WorkflowLease>, RuntimeError>> {
        delegate!(self, acquire(id))
    }
    fn inspect<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WorkflowState>, RuntimeError>> {
        delegate!(self, inspect(id))
    }
}
impl AppWorkflowStore {
    pub async fn list(&self) -> Result<Vec<Value>, RuntimeError> {
        fn summary(s: WorkflowState) -> Result<Value, RuntimeError> {
            Ok(
                json!({"id":s.id,"status":s.status,"revision":s.revision,"nodes":s.nodes.len(),"budget":s.budget()?}),
            )
        }
        match self {
            Self::File(s) => {
                super::orchestration::list_states(s.root().into(), ".workflow.json", summary).await
            }
            #[cfg(feature = "database")]
            Self::Sql(s) => s.list().await?.into_iter().map(summary).collect(),
        }
    }
}
impl SessionStore for AppSessionStore {
    fn load<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Session>, RuntimeError>> {
        delegate!(self, load(id))
    }
    fn save<'a>(
        &'a self,
        session: &'a Session,
        expected: Option<u64>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        delegate!(self, save(session, expected))
    }
}
impl AppSessionStore {
    pub async fn list(&self) -> Result<Vec<Value>, RuntimeError> {
        fn summary(s: Session) -> Result<Value, RuntimeError> {
            Ok(
                json!({"id":s.id,"revision":s.revision,"turns":s.turns.len(),"active":s.active.map(|b|json!({"turn_id":b.turn_id,"run_id":b.run_id}))}),
            )
        }
        match self {
            Self::File(s) => {
                super::orchestration::list_states(s.root().into(), ".session.json", summary).await
            }
            #[cfg(feature = "database")]
            Self::Sql(s) => s.list().await?.into_iter().map(summary).collect(),
        }
    }
}
impl CorpusStore for AppCorpusStore {
    fn snapshot(&self) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>> {
        delegate!(self, snapshot())
    }
    fn replace(
        &self,
        document: Document,
        chunks: Vec<Chunk>,
        expected: Option<u64>,
    ) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>> {
        delegate!(self, replace(document, chunks, expected))
    }
    fn remove<'a>(
        &'a self,
        id: &'a str,
        expected: Option<u64>,
    ) -> BoxFuture<'a, Result<CorpusSnapshot, RuntimeError>> {
        delegate!(self, remove(id, expected))
    }
}
impl MemoryStore for AppMemoryStore {
    fn load<'a>(
        &'a self,
        scope: &'a MemoryScope,
    ) -> BoxFuture<'a, Result<MemorySnapshot, RuntimeError>> {
        delegate!(self, load(scope))
    }
    fn save<'a>(
        &'a self,
        snapshot: &'a MemorySnapshot,
        expected: u64,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        delegate!(self, save(snapshot, expected))
    }
}
