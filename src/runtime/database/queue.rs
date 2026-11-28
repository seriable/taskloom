//! Host-selected project queues. Runtime checkpoints, rather than job retries,
//! decide whether an interrupted operation is safe to continue.
use super::*;
use serde_json::Value;

const MAX_EVENTS: usize = 2048;
const MAX_PAYLOAD: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueLimits {
    /// Includes pending, running, paused, waiting and failed jobs.
    pub max_open_jobs: u32,
    pub max_running: u32,
}
impl QueueLimits {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.max_open_jobs == 0
            || self.max_open_jobs > 100_000
            || self.max_running == 0
            || self.max_running > 256
            || self.max_running > self.max_open_jobs
        {
            return Err(config("queue limits require 1..100000 open jobs and 1..256 running jobs within open capacity"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueEvent {
    pub revision: u64,
    pub unix_ms: i64,
    pub action: String,
    pub actor: String,
    pub note: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuePolicy {
    pub revision: u64,
    pub host_binding: Option<String>,
    pub limits: Option<QueueLimits>,
    pub events: Vec<QueueEvent>,
}
impl QueuePolicy {
    fn validate(&self) -> Result<(), RuntimeError> {
        validate_events(self.revision, &self.events)?;
        if let Some(binding) = &self.host_binding {
            digest(binding)?;
        }
        if let Some(limits) = &self.limits {
            limits.validate()?;
            if self.host_binding.is_none() {
                return Err(storage("enabled queue has no frozen host binding"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    Pending,
    Running,
    Paused,
    Waiting,
    Failed,
    Completed,
    Stopped,
    Abandoned,
}
impl QueueState {
    pub fn is_open(self) -> bool {
        !matches!(self, Self::Completed | Self::Stopped | Self::Abandoned)
    }
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
            Self::Abandoned => "abandoned",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueControl {
    Pause { actor: String, note: String },
    Cancel { actor: String, note: String },
}
impl QueueControl {
    pub fn actor(&self) -> &str {
        match self {
            Self::Pause { actor, .. } | Self::Cancel { actor, .. } => actor,
        }
    }
    pub fn note(&self) -> &str {
        match self {
            Self::Pause { note, .. } | Self::Cancel { note, .. } => note,
        }
    }
    fn validate(&self) -> Result<(), RuntimeError> {
        text(self.actor(), 256)?;
        text(self.note(), 4096)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueJob {
    pub revision: u64,
    pub id: String,
    pub resource: String,
    pub host_binding: String,
    pub payload: Value,
    pub payload_fingerprint: String,
    pub submitted_by: String,
    pub created_ms: i64,
    pub state: QueueState,
    pub attempts: u64,
    pub claim_epoch: u64,
    pub worker: Option<String>,
    pub control: Option<QueueControl>,
    pub detail: Option<String>,
    pub events: Vec<QueueEvent>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueRuntimeBinding {
    pub family: String,
    pub id: String,
}
impl QueueJob {
    fn validate(&self) -> Result<(), RuntimeError> {
        text(&self.id, 256)?;
        text(&self.resource, 256)?;
        text(&self.submitted_by, 256)?;
        digest(&self.host_binding)?;
        digest(&self.payload_fingerprint)?;
        if self.payload_fingerprint != super::super::state::fingerprint(&self.payload)?
            || serde_json::to_vec(&self.payload).map_err(storage)?.len() > MAX_PAYLOAD
            || self.created_ms <= 0
            || self.state == QueueState::Running
                && (self.worker.is_none() || self.claim_epoch == 0 || self.attempts == 0)
        {
            return Err(storage("queue payload, identity or claim is invalid"));
        }
        if let Some(worker) = &self.worker {
            text(worker, 128)?;
        }
        if let Some(control) = &self.control {
            control.validate()?;
        }
        if let Some(detail) = &self.detail {
            text(detail, 4096)?;
        }
        validate_events(self.revision, &self.events)
    }
    fn event(
        &mut self,
        now: i64,
        action: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), RuntimeError> {
        self.revision = next(self.revision)?;
        self.events.push(QueueEvent {
            revision: self.revision,
            unix_ms: now,
            action: action.into(),
            actor: actor.into(),
            note: note.into(),
        });
        self.validate()
    }
}

#[derive(Clone)]
pub struct SqlQueueStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlQueueStore {
    pub(super) fn new(db: SqlDatabase, namespace: DatabaseNamespace) -> Self {
        Self { db, namespace }
    }
    fn policy_key(&self) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.namespace.clone(), "queue_control", "policy")
    }
    fn key(&self, id: &str) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.namespace.clone(), "queue_job", id)
    }
    async fn lock(&self, tx: &mut Transaction<'_, Any>) -> Result<QueuePolicy, RuntimeError> {
        parse_policy(
            self.db
                .lock_record(tx, &self.policy_key()?, &QueuePolicy::default())
                .await?,
        )
    }
    pub async fn policy(&self) -> Result<QueuePolicy, RuntimeError> {
        self.db
            .read(&self.policy_key()?)
            .await?
            .map(parse_policy)
            .transpose()
            .map(Option::unwrap_or_default)
    }
    pub async fn configure(
        &self,
        limits: Option<QueueLimits>,
        host_binding: &str,
        expected: u64,
        operator: &str,
        note: &str,
    ) -> Result<QueuePolicy, RuntimeError> {
        digest(host_binding)?;
        text(operator, 256)?;
        text(note, 4096)?;
        if let Some(limits) = &limits {
            limits.validate()?;
        }
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let mut policy = self.lock(&mut tx).await?;
        if policy.revision != expected {
            return Err(conflict("queue policy revision changed"));
        }
        if policy
            .host_binding
            .as_deref()
            .is_some_and(|binding| binding != host_binding)
            && self.count(&mut tx, None).await? > 0
        {
            return Err(conflict(
                "cannot change queue host contracts with open jobs",
            ));
        }
        policy.host_binding = Some(host_binding.into());
        policy.limits = limits;
        policy.revision = next(policy.revision)?;
        policy.events.push(QueueEvent {
            revision: policy.revision,
            unix_ms: self.now(&mut tx).await?,
            action: "configure".into(),
            actor: operator.into(),
            note: note.into(),
        });
        policy.validate()?;
        let key = self.policy_key()?;
        self.db
            .replace(
                &mut tx,
                &key,
                policy.revision,
                &self.db.serialize(&key, &policy)?,
            )
            .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(policy)
    }
    fn admit<'a>(
        &self,
        policy: &'a QueuePolicy,
        binding: &str,
    ) -> Result<&'a QueueLimits, RuntimeError> {
        if policy.host_binding.as_deref() != Some(binding) {
            return Err(config("queue frozen host contracts differ"));
        }
        policy.limits.as_ref().ok_or_else(|| {
            RuntimeError::Permission("queue admission is disabled by the host".into())
        })
    }
    async fn count(
        &self,
        tx: &mut Transaction<'_, Any>,
        state: Option<&str>,
    ) -> Result<i64, RuntimeError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM taskloom_jobs WHERE namespace=$1 AND is_open=1 AND ($2='' OR state=$2)").bind(&self.namespace.0).bind(state.unwrap_or("")).fetch_one(&mut **tx).await.map_err(database_error)
    }
    async fn now(&self, tx: &mut Transaction<'_, Any>) -> Result<i64, RuntimeError> {
        sqlx::query_scalar(&format!("SELECT {}", self.db.0.backend.now()))
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)
    }
    async fn read_tx(
        &self,
        tx: &mut Transaction<'_, Any>,
        id: &str,
    ) -> Result<Option<QueueJob>, RuntimeError> {
        // One statement sees a consistent index/journal even while a remote
        // operator commits a control request (PostgreSQL READ COMMITTED).
        let body = match self.db.0.backend {
            Backend::Sqlite => "substr(CAST(r.body AS BLOB),1,$4)",
            Backend::Postgres => "substring(convert_to(r.body,'UTF8') from 1 for $4)",
        };
        let query=format!("SELECT r.revision,{body} AS payload,j.resource AS indexed_resource,j.state AS indexed_state,j.is_open,j.created_ms,j.revision AS indexed_revision FROM taskloom_records r LEFT JOIN taskloom_jobs j ON j.namespace=r.namespace AND j.job_id=r.object_id WHERE r.namespace=$1 AND r.family=$2 AND r.object_id=$3");
        let row = sqlx::query(&query)
            .bind(&self.namespace.0)
            .bind("queue_job")
            .bind(id)
            .bind((self.db.0.options.max_record_bytes + 1) as i32)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
        let Some(row) = row else {
            let indexed: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM taskloom_jobs WHERE namespace=$1 AND job_id=$2",
            )
            .bind(&self.namespace.0)
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?;
            if indexed != 0 {
                return Err(storage("queue index references absent journal"));
            }
            return Ok(None);
        };
        let index = (
            row.try_get::<Option<String>, _>("indexed_resource")
                .map_err(database_error)?,
            row.try_get::<Option<String>, _>("indexed_state")
                .map_err(database_error)?,
            row.try_get::<Option<i64>, _>("is_open")
                .map_err(database_error)?,
            row.try_get::<Option<i64>, _>("created_ms")
                .map_err(database_error)?,
            row.try_get::<Option<i64>, _>("indexed_revision")
                .map_err(database_error)?,
        );
        let job = parse_job(decode_record(
            row,
            self.key(id)?,
            self.db.0.options.max_record_bytes,
        )?)?;
        if index
            != (
                Some(job.resource.clone()),
                Some(job.state.name().into()),
                Some(i64::from(job.state.is_open())),
                Some(job.created_ms),
                Some(sequence(job.revision)?),
            )
        {
            return Err(storage("queue index differs from its bound journal"));
        }
        Ok(Some(job))
    }
    async fn write(
        &self,
        tx: &mut Transaction<'_, Any>,
        job: &QueueJob,
    ) -> Result<(), RuntimeError> {
        job.validate()?;
        let key = self.key(&job.id)?;
        self.db
            .replace(tx, &key, job.revision, &self.db.serialize(&key, job)?)
            .await?;
        sqlx::query("INSERT INTO taskloom_jobs(namespace,job_id,resource,state,is_open,created_ms,revision) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(namespace,job_id) DO UPDATE SET state=excluded.state,is_open=excluded.is_open,revision=excluded.revision")
            .bind(&self.namespace.0).bind(&job.id).bind(&job.resource).bind(job.state.name()).bind(i64::from(job.state.is_open())).bind(job.created_ms).bind(sequence(job.revision)?).execute(&mut **tx).await.map_err(database_error)?;
        Ok(())
    }
    /// Persist immutable input before any worker may dispatch. Same ID/input is
    /// idempotent, while a new payload or resource cannot reuse that identity.
    pub async fn enqueue(
        &self,
        id: &str,
        resource: &str,
        payload: Value,
        host_binding: &str,
        actor: &str,
    ) -> Result<QueueJob, RuntimeError> {
        text(id, 256)?;
        text(resource, 256)?;
        text(actor, 256)?;
        digest(host_binding)?;
        if serde_json::to_vec(&payload).map_err(storage)?.len() > MAX_PAYLOAD {
            return Err(RuntimeError::StorageLimit(
                "queue input exceeds 512KiB".into(),
            ));
        }
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let policy = self.lock(&mut tx).await?;
        let limits = self.admit(&policy, host_binding)?;
        if let Some(old) = self.read_tx(&mut tx, id).await? {
            if old.payload != payload
                || old.resource != resource
                || old.host_binding != host_binding
            {
                return Err(conflict(
                    "queue identity is bound to different exact input or host contracts",
                ));
            }
            tx.commit().await.map_err(database_error)?;
            return Ok(old);
        }
        if self.count(&mut tx, None).await? >= i64::from(limits.max_open_jobs) {
            return Err(conflict("project queue open-job capacity reached"));
        }
        let occupied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM taskloom_jobs WHERE namespace=$1 AND resource=$2 AND is_open=1",
        )
        .bind(&self.namespace.0)
        .bind(resource)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if occupied != 0 {
            return Err(conflict(
                "queue resource is occupied, including waiting approvals/recovery",
            ));
        }
        let now = self.now(&mut tx).await?;
        let mut job = QueueJob {
            revision: 0,
            id: id.into(),
            resource: resource.into(),
            host_binding: host_binding.into(),
            payload_fingerprint: super::super::state::fingerprint(&payload)?,
            payload,
            submitted_by: actor.into(),
            created_ms: now,
            state: QueueState::Pending,
            attempts: 0,
            claim_epoch: 0,
            worker: None,
            control: None,
            detail: None,
            events: vec![],
        };
        job.event(now, "enqueue", actor, "exact immutable input accepted")?;
        self.write(&mut tx, &job).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(job)
    }
    /// Consistent inspection of journal and scheduler index; no model service.
    pub async fn inspect(&self, id: &str) -> Result<Option<QueueJob>, RuntimeError> {
        self.db.schema().await?;
        // A read transaction sees either side of a commit, never a mixed index.
        // PostgreSQL's default read-committed needs the same row lock as writers.
        let mut tx = self.db.write_transaction().await?;
        self.lock(&mut tx).await?;
        let value = self.read_tx(&mut tx, id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(value)
    }
    /// Current runtime/child associations, useful for operator inspection and
    /// validating a complete SQL backup. A later closed conversation turn can
    /// rebind its session record; its historical child run remains associated.
    pub async fn bindings(&self, id: &str) -> Result<Vec<QueueRuntimeBinding>, RuntimeError> {
        self.key(id)?;
        self.db.schema().await?;
        let rows=sqlx::query("SELECT substr(family,1,33) AS family,substr(object_id,1,257) AS object_id FROM taskloom_job_bindings WHERE namespace=$1 AND job_id=$2 ORDER BY family,object_id LIMIT 8193").bind(&self.namespace.0).bind(id).fetch_all(&self.db.0.pool).await.map_err(database_error)?;
        if rows.len() > 8192 {
            return Err(RuntimeError::StorageLimit(
                "queue runtime binding listing exceeds 8192 entries".into(),
            ));
        }
        rows.into_iter()
            .map(|row| {
                let family: String = row.try_get("family").map_err(database_error)?;
                let id: String = row.try_get("object_id").map_err(database_error)?;
                if !matches!(family.as_str(), "run" | "workflow" | "session")
                    || id.is_empty()
                    || id.len() > 256
                {
                    return Err(storage("invalid queue runtime association"));
                }
                Ok(QueueRuntimeBinding { family, id })
            })
            .collect()
    }
    pub async fn list(&self) -> Result<Vec<QueueJob>, RuntimeError> {
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        self.lock(&mut tx).await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT job_id FROM taskloom_jobs WHERE namespace=$1 ORDER BY created_ms,job_id LIMIT 1000").bind(&self.namespace.0).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut jobs = Vec::new();
        let mut bytes = 0usize;
        for id in ids {
            let job = self
                .read_tx(&mut tx, &id)
                .await?
                .ok_or_else(|| storage("queue index references missing job"))?;
            bytes = bytes.saturating_add(serde_json::to_vec(&job).map_err(storage)?.len());
            if bytes > MAX_BYTES as usize {
                return Err(RuntimeError::StorageLimit(
                    "queue list exceeds 64MiB; inspect individual jobs".into(),
                ));
            }
            jobs.push(job);
        }
        tx.commit().await.map_err(database_error)?;
        Ok(jobs)
    }
    /// Explicit resume only. Paused, failed and human-waiting jobs never poll
    /// themselves back into execution, even when another worker is idle.
    pub async fn resume(
        &self,
        id: &str,
        expected: u64,
        binding: &str,
        actor: &str,
        note: &str,
    ) -> Result<QueueJob, RuntimeError> {
        text(actor, 256)?;
        text(note, 4096)?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let policy = self.lock(&mut tx).await?;
        self.admit(&policy, binding)?;
        let mut job = self
            .read_tx(&mut tx, id)
            .await?
            .ok_or_else(|| conflict("queue job does not exist"))?;
        if job.revision != expected
            || job.host_binding != binding
            || !matches!(
                job.state,
                QueueState::Waiting | QueueState::Paused | QueueState::Failed
            )
        {
            return Err(conflict(
                "queue resume requires the exact inactive open job revision and contracts",
            ));
        }
        if job.events.len() > MAX_EVENTS - 16 {
            return Err(RuntimeError::StorageLimit(
                "queue audit capacity reached".into(),
            ));
        }
        job.state = QueueState::Pending;
        job.detail = None;
        if matches!(job.control, Some(QueueControl::Pause { .. })) {
            job.control = None;
        }
        job.event(self.now(&mut tx).await?, "resume", actor, note)?;
        self.write(&mut tx, &job).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(job)
    }
    /// Acknowledges durable request delivery, not completion of cancellation.
    /// The executing host cooperatively observes it; unknown effects survive.
    pub async fn control(
        &self,
        id: &str,
        expected: u64,
        control: QueueControl,
    ) -> Result<QueueJob, RuntimeError> {
        control.validate()?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        self.lock(&mut tx).await?;
        let mut job = self
            .read_tx(&mut tx, id)
            .await?
            .ok_or_else(|| conflict("queue job does not exist"))?;
        if job.revision != expected || !job.state.is_open() {
            return Err(conflict(
                "queue control requires the exact open job revision",
            ));
        }
        if matches!(job.control, Some(QueueControl::Cancel { .. }))
            && job.control.as_ref() != Some(&control)
        {
            return Err(conflict(
                "the original queue cancellation request is immutable",
            ));
        }
        if job.events.len() > MAX_EVENTS - 8 {
            return Err(RuntimeError::StorageLimit(
                "queue audit capacity reached".into(),
            ));
        }
        if job.state != QueueState::Running {
            job.state = if matches!(control, QueueControl::Cancel { .. }) {
                QueueState::Pending
            } else {
                QueueState::Paused
            };
        }
        job.control = Some(control.clone());
        job.event(
            self.now(&mut tx).await?,
            "control",
            control.actor(),
            control.note(),
        )?;
        self.write(&mut tx, &job).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(job)
    }
    /// Permanently abandon scheduling, not the external outcome. No original
    /// model/tool credential is required; runtime/budget/uncertainty stay intact.
    pub async fn abandon(
        &self,
        id: &str,
        expected: u64,
        actor: &str,
        note: &str,
    ) -> Result<QueueJob, RuntimeError> {
        text(actor, 256)?;
        text(note, 4096)?;
        self.db.drain_releases().await?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        self.lock(&mut tx).await?;
        let mut job = self
            .read_tx(&mut tx, id)
            .await?
            .ok_or_else(|| conflict("queue job does not exist"))?;
        if job.revision != expected || !job.state.is_open() {
            return Err(conflict(
                "abandon requires the exact open job revision with no live owners",
            ));
        }
        let now = self.db.0.backend.now();
        let query=format!("SELECT COUNT(*) FROM taskloom_leases l WHERE l.namespace=$1 AND l.expires_ms>{now} AND ((l.family='queue_job' AND l.object_id=$2) OR EXISTS(SELECT 1 FROM taskloom_job_bindings b WHERE b.namespace=l.namespace AND b.family=l.family AND b.object_id=l.object_id AND b.job_id=$2))");
        let live: i64 = sqlx::query_scalar(&query)
            .bind(&self.namespace.0)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        if live != 0 {
            return Err(conflict(
                "cannot abandon while queue or runtime ownership remains live",
            ));
        }
        if job.events.len() > MAX_EVENTS - 2 {
            return Err(RuntimeError::StorageLimit(
                "queue abandonment audit capacity reached".into(),
            ));
        }
        job.state = QueueState::Abandoned;
        job.detail = Some(
            "host abandoned scheduling; original runtime outcomes and uncertainty are retained"
                .into(),
        );
        job.event(self.now(&mut tx).await?, "abandon", actor, note)?;
        self.write(&mut tx, &job).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(job)
    }
    /// Claim and its persisted Running transition commit atomically. Crashed
    /// jobs are eligible only after queue AND every bound runtime owner expire.
    pub async fn claim(
        &self,
        binding: &str,
        worker: &str,
    ) -> Result<Option<SqlQueueLease>, RuntimeError> {
        text(worker, 128)?;
        digest(binding)?;
        self.db.drain_releases().await?;
        self.db.schema().await?;
        let mut tx = self.db.write_transaction().await?;
        let policy = self.lock(&mut tx).await?;
        let limits = self.admit(&policy, binding)?;
        let now = self.db.0.backend.now();
        let query=format!("SELECT j.job_id FROM taskloom_jobs j WHERE j.namespace=$1 AND j.state IN ('pending','running') AND NOT EXISTS(SELECT 1 FROM taskloom_leases l WHERE l.namespace=j.namespace AND l.family='queue_job' AND l.object_id=j.job_id AND l.expires_ms>{now}) AND NOT EXISTS(SELECT 1 FROM taskloom_job_bindings b JOIN taskloom_leases l ON l.namespace=b.namespace AND l.family=b.family AND l.object_id=b.object_id WHERE b.namespace=j.namespace AND b.job_id=j.job_id AND l.expires_ms>{now}) ORDER BY CASE j.state WHEN 'running' THEN 0 ELSE 1 END,j.created_ms,j.job_id LIMIT 1");
        let Some(id) = sqlx::query_scalar::<_, String>(&query)
            .bind(&self.namespace.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?
        else {
            tx.commit().await.map_err(database_error)?;
            return Ok(None);
        };
        let mut job = self
            .read_tx(&mut tx, &id)
            .await?
            .ok_or_else(|| storage("claim references absent queue journal"))?;
        if job.host_binding != binding {
            return Err(config("queued job host contracts differ from worker"));
        }
        // Expired claims without live runtimes are recovery candidates, not
        // active slots. A lowered cap must constrain recovery as well as new
        // jobs, without deadlocking a backlog of expired Running journals.
        let active=format!("SELECT COUNT(*) FROM taskloom_jobs j WHERE j.namespace=$1 AND j.state='running' AND (EXISTS(SELECT 1 FROM taskloom_leases l WHERE l.namespace=j.namespace AND l.family='queue_job' AND l.object_id=j.job_id AND l.expires_ms>{now}) OR EXISTS(SELECT 1 FROM taskloom_job_bindings b JOIN taskloom_leases l ON l.namespace=b.namespace AND l.family=b.family AND l.object_id=b.object_id WHERE b.namespace=j.namespace AND b.job_id=j.job_id AND l.expires_ms>{now}))");
        let active: i64 = sqlx::query_scalar(&active)
            .bind(&self.namespace.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
        if active >= i64::from(limits.max_running) {
            tx.commit().await.map_err(database_error)?;
            return Ok(None);
        }
        if job.events.len() > MAX_EVENTS - 16 || job.attempts >= 1024 {
            return Err(RuntimeError::StorageLimit(
                "queue attempt/audit capacity reached; explicit administration required".into(),
            ));
        }
        let lease = self.db.acquire_tx(&mut tx, self.key(&id)?).await?;
        job.state = QueueState::Running;
        job.attempts = next(job.attempts)?;
        job.claim_epoch = lease.epoch as u64;
        job.worker = Some(worker.into());
        job.detail = None;
        job.event(
            self.now(&mut tx).await?,
            "claim",
            worker,
            "exclusive fenced queue ownership acquired",
        )?;
        self.write(&mut tx, &job).await?;
        lease.commit(tx).await?;
        self.db.start_heartbeat(&lease)?;
        Ok(Some(SqlQueueLease {
            lease,
            store: self.clone(),
            worker: worker.into(),
        }))
    }
}

#[derive(Clone)]
pub struct SqlQueueLease {
    lease: Arc<Lease>,
    store: SqlQueueStore,
    worker: String,
}
impl SqlQueueLease {
    pub async fn verify_ownership(&self) -> Result<(), RuntimeError> {
        self.snapshot().await.map(|_| ())
    }
    pub async fn snapshot(&self) -> Result<QueueJob, RuntimeError> {
        let mut tx = self.store.db.write_transaction().await?;
        self.lease.lock(&mut tx).await?;
        let job = self.current(&mut tx).await?;
        self.lease.commit(tx).await?;
        Ok(job)
    }
    async fn current(&self, tx: &mut Transaction<'_, Any>) -> Result<QueueJob, RuntimeError> {
        let job = self
            .store
            .read_tx(tx, &self.lease.key.id)
            .await?
            .ok_or_else(|| storage("owned queue job disappeared"))?;
        if job.state != QueueState::Running
            || job.claim_epoch != self.lease.epoch as u64
            || job.worker.as_deref() != Some(&self.worker)
        {
            return Err(conflict("queue claim state or fencing epoch differs"));
        }
        Ok(job)
    }
    pub async fn finish(&self, state: QueueState, note: &str) -> Result<QueueJob, RuntimeError> {
        if matches!(
            state,
            QueueState::Running | QueueState::Pending | QueueState::Abandoned
        ) {
            return Err(config("queue finish requires an inactive outcome"));
        }
        text(note, 4096)?;
        let mut tx = self.store.db.write_transaction().await?;
        self.store.lock(&mut tx).await?;
        self.lease.lock(&mut tx).await?;
        let mut job = self.current(&mut tx).await?;
        job.state = state;
        job.detail = Some(note.into());
        job.event(self.store.now(&mut tx).await?, "finish", &self.worker, note)?;
        self.store.write(&mut tx, &job).await?;
        self.lease.commit(tx).await?;
        Ok(job)
    }
    /// This capability can guard only this exact database pool and namespace.
    pub(super) fn matches(
        &self,
        db: &SqlDatabase,
        ns: &DatabaseNamespace,
    ) -> Result<(), RuntimeError> {
        if !Arc::ptr_eq(&db.0, &self.store.db.0) || ns != &self.store.namespace {
            return Err(config(
                "queue capability belongs to another database or namespace",
            ));
        }
        Ok(())
    }
    pub(super) async fn lock_runtime(
        &self,
        tx: &mut Transaction<'_, Any>,
        key: &RecordKey,
    ) -> Result<(), RuntimeError> {
        self.matches(&self.store.db, &key.namespace)?;
        self.lease.lock(tx).await?;
        self.current(tx).await?;
        let previous:Option<String>=sqlx::query_scalar("SELECT job_id FROM taskloom_job_bindings WHERE namespace=$1 AND family=$2 AND object_id=$3").bind(&key.namespace.0).bind(key.family).bind(&key.id).fetch_optional(&mut **tx).await.map_err(database_error)?;
        if let Some(previous) = previous {
            if previous != self.lease.key.id {
                // A conversation can accept another explicit turn after the
                // former queue job closes; run/workflow identities never move.
                let old = self
                    .store
                    .read_tx(tx, &previous)
                    .await?
                    .ok_or_else(|| storage("runtime queue binding references absent job"))?;
                let current = self.current(tx).await?;
                if key.family != "session"
                    || old.state.is_open()
                    || old.resource != current.resource
                {
                    return Err(conflict("runtime identity belongs to another queue job"));
                }
            }
        }
        sqlx::query("INSERT INTO taskloom_job_bindings(namespace,family,object_id,job_id) VALUES($1,$2,$3,$4) ON CONFLICT(namespace,family,object_id) DO UPDATE SET job_id=excluded.job_id")
            .bind(&key.namespace.0).bind(key.family).bind(&key.id).bind(&self.lease.key.id).execute(&mut **tx).await.map_err(database_error)?;
        Ok(())
    }
}
pub(super) async fn dispatch_guard(
    lease: &Arc<Lease>,
    guard: Option<&SqlQueueLease>,
) -> Result<(), RuntimeError> {
    let db = &lease.db;
    let key = &lease.key;
    let mut tx = db.write_transaction().await?;
    lease.lock(&mut tx).await?;
    if let Some(guard) = guard {
        guard.matches(db, &key.namespace)?;
        guard.lock_runtime(&mut tx, key).await?;
        lease.commit_with_queue(tx, Some(guard)).await
    } else {
        let bound:i64=sqlx::query_scalar("SELECT COUNT(*) FROM taskloom_job_bindings WHERE namespace=$1 AND family=$2 AND object_id=$3").bind(&key.namespace.0).bind(key.family).bind(&key.id).fetch_one(&mut *tx).await.map_err(database_error)?;
        if bound != 0 {
            return Err(RuntimeError::Permission(
                "queued runtime dispatch requires its current fenced queue owner".into(),
            ));
        }
        lease.commit(tx).await
    }
}
pub(super) async fn refuse_unowned_admission(
    tx: &mut Transaction<'_, Any>,
    key: &RecordKey,
) -> Result<(), RuntimeError> {
    let bound:i64=sqlx::query_scalar("SELECT COUNT(*) FROM taskloom_job_bindings WHERE namespace=$1 AND family=$2 AND object_id=$3").bind(&key.namespace.0).bind(key.family).bind(&key.id).fetch_one(&mut **tx).await.map_err(database_error)?;
    if bound != 0 {
        return Err(RuntimeError::Permission(
            "queued model admission requires its fenced queue owner".into(),
        ));
    }
    Ok(())
}
fn parse_policy(row: Record) -> Result<QueuePolicy, RuntimeError> {
    let policy: QueuePolicy = row.parse()?;
    if policy.revision != row.revision {
        return Err(storage("queue policy revision differs"));
    }
    policy.validate()?;
    Ok(policy)
}
fn parse_job(row: Record) -> Result<QueueJob, RuntimeError> {
    let job: QueueJob = row.parse()?;
    if job.id != row.key.id || job.revision != row.revision {
        return Err(storage("queue journal identity/revision differs"));
    }
    job.validate()?;
    Ok(job)
}
fn validate_events(revision: u64, events: &[QueueEvent]) -> Result<(), RuntimeError> {
    if events.len() > MAX_EVENTS || revision != events.len() as u64 {
        return Err(storage("queue audit capacity/revision differs"));
    }
    for (i, event) in events.iter().enumerate() {
        if event.revision != i as u64 + 1 || event.unix_ms <= 0 {
            return Err(storage("queue audit revision/time invalid"));
        }
        text(&event.action, 64)?;
        text(&event.actor, 256)?;
        text(&event.note, 4096)?;
    }
    Ok(())
}
fn next(value: u64) -> Result<u64, RuntimeError> {
    value
        .checked_add(1)
        .ok_or_else(|| storage("queue revision overflow"))
}
fn digest(value: &str) -> Result<(), RuntimeError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Err(config(
            "queue binding requires a 64-digit hex host fingerprint",
        ))
    } else {
        Ok(())
    }
}
fn text(value: &str, max: usize) -> Result<(), RuntimeError> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        Err(config(
            "queue metadata must be bounded nonempty text without control characters",
        ))
    } else {
        Ok(())
    }
}
