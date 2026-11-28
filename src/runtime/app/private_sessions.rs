//! Explicit no-tools private conversations. Storage and every model admission
//! are owner-bound; project generation accounting remains shared and atomic.
use super::{multiuser::PrivateMemoryPolicy, Application, MemoryExtract};
use crate::runtime::{database::*, memory::*, observability::TraceSnapshot, *};
use crate::CancelToken;
use axum::{
    body::to_bytes,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::{future::BoxFuture, FutureExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Trusted host opt-in, distinct from memory CRUD. It grants no tools, writes,
/// approval authority, shared-queue capacity or implicit extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivateSessionPolicy {
    pub max_active_runs: usize,
    pub max_retained_jobs: usize,
}
impl Default for PrivateSessionPolicy {
    fn default() -> Self {
        Self {
            max_active_runs: 2,
            max_retained_jobs: 256,
        }
    }
}
impl PrivateSessionPolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if !(1..=32).contains(&self.max_active_runs)
            || !(1..=1000).contains(&self.max_retained_jobs)
            || self.max_retained_jobs < self.max_active_runs
        {
            return Err(RuntimeError::Configuration("private execution requires 1..32 active and 1..1000 retained jobs, retained >= active".into()));
        }
        Ok(())
    }
}

struct Job {
    active: bool,
    cancel: CancelToken,
    error: Option<String>,
    run_id: Option<String>,
    sequence: u64,
}
struct Jobs {
    entries: BTreeMap<String, Job>,
    sequence: u64,
    draining: bool,
}
pub struct PrivateSessionController {
    app: Arc<Application>,
    identities: SqlIdentityStore,
    tenant: String,
    project: String,
    namespace: DatabaseNamespace,
    memory: PrivateMemoryPolicy,
    policy: PrivateSessionPolicy,
    jobs: Mutex<Jobs>,
}
impl PrivateSessionController {
    pub fn new(
        app: Arc<Application>,
        identities: SqlIdentityStore,
        tenant: String,
        project: String,
        memory: PrivateMemoryPolicy,
        policy: PrivateSessionPolicy,
    ) -> Result<Arc<Self>, RuntimeError> {
        policy.validate()?;
        memory.validate()?;
        let namespace = DatabaseNamespace::new(&tenant, &project)?;
        if app
            .stores
            .sql
            .as_ref()
            .is_none_or(|(_, scope)| scope != &namespace)
            || app.config.memory.is_some()
        {
            return Err(RuntimeError::Configuration(
                "private execution requires matching SQL project and no fixed shared memory".into(),
            ));
        }
        if app.config.limits.max_cost_micros.is_none()
            || !app
                .config
                .prices
                .iter()
                .any(|price| app.config.model.starts_with(&price.prefix))
        {
            return Err(RuntimeError::Configuration("private execution requires explicit run spending ceiling and configured model prices".into()));
        }
        Ok(Arc::new(Self {
            app,
            identities,
            tenant,
            project,
            namespace,
            memory,
            policy,
            jobs: Mutex::new(Jobs {
                entries: BTreeMap::new(),
                sequence: 0,
                draining: false,
            }),
        }))
    }
    fn job_key(owner: &str, resource: &str) -> Result<String, RuntimeError> {
        crate::runtime::state::fingerprint(&("private-job-v1", owner, resource))
    }
    fn admit(&self, owner: &str, resource: &str) -> Result<CancelToken, RuntimeError> {
        let key = Self::job_key(owner, resource)?;
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| RuntimeError::Storage("private job controller lock poisoned".into()))?;
        if jobs.draining
            || jobs.entries.get(&key).is_some_and(|job| job.active)
            || jobs.entries.values().filter(|job| job.active).count() >= self.policy.max_active_runs
        {
            return Err(RuntimeError::Conflict("private controller is draining, occupied or at its explicit private concurrency cap".into()));
        }
        if !jobs.entries.contains_key(&key) && jobs.entries.len() >= self.policy.max_retained_jobs {
            let expired = jobs
                .entries
                .iter()
                .filter(|(_, job)| !job.active)
                .min_by_key(|(_, job)| job.sequence)
                .map(|(key, _)| key.clone())
                .ok_or_else(|| RuntimeError::Conflict("private job capacity is occupied".into()))?;
            jobs.entries.remove(&expired);
        }
        jobs.sequence = jobs
            .sequence
            .checked_add(1)
            .ok_or_else(|| RuntimeError::Storage("private job sequence overflow".into()))?;
        let sequence = jobs.sequence;
        let cancel = CancelToken::new();
        jobs.entries.insert(
            key,
            Job {
                active: true,
                cancel: cancel.clone(),
                error: None,
                run_id: None,
                sequence,
            },
        );
        Ok(cancel)
    }
    fn finish(&self, owner: &str, resource: &str, result: Result<String, RuntimeError>) {
        if let (Ok(key), Ok(mut jobs)) = (Self::job_key(owner, resource), self.jobs.lock()) {
            if let Some(job) = jobs.entries.get_mut(&key) {
                job.active = false;
                match result {
                    Ok(id) => job.run_id = Some(id),
                    Err(error) => job.error = Some(error.to_string().chars().take(4096).collect()),
                }
            }
        }
    }
    fn status(&self, owner: &str, resource: &str) -> Result<Value, RuntimeError> {
        let key = Self::job_key(owner, resource)?;
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| RuntimeError::Storage("private job controller lock poisoned".into()))?;
        Ok(jobs.entries.get(&key).map_or(json!({"active":false,"error":null,"run_id":null,"observation":"no process-local job; inspect durable private session/run after restart"}),|job|json!({"active":job.active,"error":job.error,"run_id":job.run_id})))
    }
    pub async fn shutdown(&self, grace: Duration) -> Result<(), RuntimeError> {
        {
            let mut jobs = self.jobs.lock().map_err(|_| {
                RuntimeError::Storage("private job controller lock poisoned".into())
            })?;
            jobs.draining = true;
            for job in jobs.entries.values().filter(|job| job.active) {
                job.cancel.cancel();
            }
        }
        let end = tokio::time::Instant::now() + grace;
        loop {
            if self
                .jobs
                .lock()
                .map_err(|_| RuntimeError::Storage("private job controller lock poisoned".into()))?
                .entries
                .values()
                .all(|job| !job.active)
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= end {
                return Err(RuntimeError::Conflict("private shutdown grace expired; retain unknown reservations and private journals".into()));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn stores(&self, owner: &str, create: bool) -> Result<SqlPrivateStores, RuntimeError> {
        let (database, _) =
            self.app.stores.sql.as_ref().ok_or_else(|| {
                RuntimeError::Configuration("SQL private stores unavailable".into())
            })?;
        if create {
            database
                .private_stores(&self.tenant, &self.project, owner)
                .await
        } else {
            database
                .open_private_stores(&self.tenant, &self.project, owner)
                .await
        }
    }
    fn access(&self, owner: &str, curate: bool) -> Result<MemoryAccess, RuntimeError> {
        let scope = MemoryScope {
            tenant: self.tenant.clone(),
            project: self.project.clone(),
            owner: owner.into(),
        };
        if curate {
            MemoryAccess::curator(scope, owner)
        } else {
            MemoryAccess::reader(scope, owner)
        }
    }
    fn label(&self, owner: &str) -> Result<String, RuntimeError> {
        Ok(format!(
            "{}:private-no-tools-v1:{}",
            self.app.label,
            crate::runtime::state::fingerprint(&(&self.tenant, &self.project, owner))?
        ))
    }
    fn spawn(
        self: &Arc<Self>,
        actor: AuthenticatedActor,
        work: Work,
        resource: String,
    ) -> Result<(), RuntimeError> {
        let owner = actor.user_id().to_owned();
        let cancel = self.admit(&owner, &resource)?;
        let controller = Arc::clone(self);
        tokio::spawn(async move {
            let result=std::panic::AssertUnwindSafe(Box::pin(controller.drive(actor,work,&cancel))).catch_unwind().await.unwrap_or_else(|_|Err(RuntimeError::Storage("private controller task panicked; inspect durable private run before explicit recovery".into())));
            controller.finish(&owner, &resource, result);
        });
        Ok(())
    }
    async fn drive(
        &self,
        actor: AuthenticatedActor,
        work: Work,
        cancel: &CancelToken,
    ) -> Result<String, RuntimeError> {
        let owner = actor.user_id();
        self.identities
            .authorize(&actor, &self.namespace, ProjectPermission::Submit)
            .await?;
        // The user explicitly created this private session beforehand. Opening
        // its immutable scope also permits inspection/known settlement after
        // quota disable; every new reservation still checks the live policy.
        let stores = self.stores(owner, false).await?;
        let runs = stores.runs();
        let sessions = stores.sessions();
        let model = AuthorizedModel {
            model: Arc::clone(&self.app.model),
            identities: self.identities.clone(),
            actor: actor.clone(),
            namespace: self.namespace.clone(),
        };
        let tools = ToolRegistry::new();
        let mut agent = AgentLoop::new(&model, &tools, &self.app.config.model)
            .with_policy(ToolPolicy::default())
            .with_limits(self.app.config.limits.clone())
            .with_context(self.app.config.context.clone())
            .with_output_tokens(self.app.config.output_tokens)
            .with_label(self.label(owner)?)
            .with_cancel(cancel);
        if let Some(policy) = &self.app.config.final_output {
            agent = agent.with_output_policy(policy.clone());
        }
        let durable = DurableAgent::new(&agent, &runs);
        let manager = SessionManager::new(&sessions);
        let result = match work {
            Work::Turn {
                session,
                turn,
                prompt,
                recall,
            } => {
                if recall {
                    let prepared = MemoryManager::new(&self.app.memory)
                        .begin_turn_with_recall(
                            &self.access(owner, false)?,
                            &manager,
                            &session,
                            &turn,
                            &prompt,
                            &self.memory.recall,
                        )
                        .await?;
                    // Begin uses the actual private session and freezes recall.
                    // Resume drives that exact bound input without re-querying memory.
                    let _ = prepared;
                    manager.resume_turn(&durable, &session, &turn).await?.run
                } else {
                    manager
                        .run_turn(&durable, &session, &turn, &prompt)
                        .await?
                        .run
                }
            }
            Work::Resume { session, turn } => {
                manager.resume_turn(&durable, &session, &turn).await?.run
            }
            Work::Extract(input) => {
                if !self.memory.curate {
                    return Err(RuntimeError::Permission(
                        "private memory extraction requires explicit host curation".into(),
                    ));
                }
                let source = manager.load(&input.session_id).await?;
                MemoryManager::new(&self.app.memory)
                    .extract_from_turn(
                        &self.access(owner, true)?,
                        &source,
                        &MemoryExtractionRequest {
                            expected_revision: input.expected_revision,
                            turn_id: input.turn_id,
                            request_id: input.request_id,
                        },
                        &agent,
                        &runs,
                    )
                    .await?
                    .run
            }
        };
        Ok(result.report.run_id)
    }

    /// Call only after the project's trusted authentication middleware selected
    /// this controller; authorization is checked again here and before generate.
    pub async fn request(
        self: &Arc<Self>,
        actor: AuthenticatedActor,
        _role: ProjectRole,
        request: Request,
    ) -> Response {
        match Box::pin(self.dispatch(actor, request)).await {
            Ok(value) => value,
            Err(error) => super::service::ApiError::from(error).into_response(),
        }
    }
    async fn dispatch(
        self: &Arc<Self>,
        actor: AuthenticatedActor,
        request: Request,
    ) -> Result<Response, RuntimeError> {
        let method = request.method().clone();
        let path = request
            .uri()
            .path()
            .strip_prefix("/api/private/")
            .ok_or_else(|| RuntimeError::Permission("invalid private route".into()))?
            .to_owned();
        if request.uri().query().is_some() {
            return Err(RuntimeError::Configuration(
                "private owner scope is server-selected; query identity is forbidden".into(),
            ));
        }
        let parts = path.split('/').collect::<Vec<_>>();
        let mutation = method == Method::POST;
        let permission = if mutation {
            ProjectPermission::Submit
        } else {
            ProjectPermission::Read
        };
        let role = self
            .identities
            .authorize(&actor, &self.namespace, permission)
            .await?;
        let owner = actor.user_id().to_owned();
        match (method.as_str(),parts.as_slice()) {
            ("GET",["sessions"])=> {
                let (database,_)=self.app.stores.sql.as_ref().ok_or_else(||RuntimeError::Configuration("private SQL unavailable".into()))?;
                let sessions=match database.private_stores_if_exists(&self.tenant,&self.project,&owner).await? {Some(stores)=>stores.sessions().list().await?,None=>Vec::new()};
                Ok(Json(json!({"sessions":sessions})).into_response())
            }
            ("POST",["sessions"])=> {
                let input:Create=body(request).await?; super::valid_id(&input.id)?;
                let stores=self.stores(&owner,true).await?;
                let value=SessionManager::new(&stores.sessions()).create(&input.id,vec![self.app.config.system.clone()]).await?;
                Ok((StatusCode::CREATED,Json(value)).into_response())
            }
            ("GET",["sessions",id])=> {super::valid_id(id)?;Ok(Json(SessionManager::new(&self.stores(&owner,false).await?.sessions()).load(id).await?).into_response())}
            ("POST",["sessions",id,operation @ ("turns"|"memory-turns")])=> {
                super::valid_id(id)?;let input:Turn=body(request).await?;super::valid_id(&input.turn)?;
                if input.prompt.is_empty()||input.prompt.len()>128*1024 {return Err(RuntimeError::Configuration("private prompt must contain 1..128KiB".into()));}
                self.spawn(actor,Work::Turn {session:(*id).into(),turn:input.turn,prompt:input.prompt,recall:*operation=="memory-turns"},format!("session:{id}"))?;
                Ok((StatusCode::ACCEPTED,Json(json!({"accepted":true,"job":"session","id":id,"scope":"authenticated owner; no-tools-v1"}))).into_response())
            }
            ("POST",["sessions",id,"resume"])=> {super::valid_id(id)?;let input:Resume=body(request).await?;super::valid_id(&input.turn)?;self.spawn(actor,Work::Resume {session:(*id).into(),turn:input.turn},format!("session:{id}"))?;Ok((StatusCode::ACCEPTED,Json(json!({"accepted":true}))).into_response())}
            ("GET",["sessions",id,"status"])=> {super::valid_id(id)?;Ok(Json(self.status(&owner,&format!("session:{id}"))?).into_response())}
            ("POST",["sessions",id,"cancel"])=> {
                super::valid_id(id)?;let _:Empty=body(request).await?;
                let key=Self::job_key(&owner,&format!("session:{id}"))?;
                let jobs=self.jobs.lock().map_err(|_|RuntimeError::Storage("private controller lock poisoned".into()))?;
                let job=jobs.entries.get(&key).filter(|job|job.active).ok_or_else(||RuntimeError::Conflict("no active private process-local job; inspect/resume original private turn after restart".into()))?;
                job.cancel.cancel();Ok(Json(json!({"cancellation_requested":true,"completed":false})).into_response())
            }
            ("POST",["sessions",id,"acknowledge-failure"])=> {
                super::valid_id(id)?;let input:Acknowledge=body(request).await?;super::valid_id(&input.turn)?;
                let stores=self.stores(&owner,false).await?;let sessions=stores.sessions();let manager=SessionManager::new(&sessions);let session=manager.load(id).await?;
                let binding=session.active.as_ref().filter(|binding|binding.turn_id==input.turn).ok_or_else(||RuntimeError::Conflict("private turn is not active".into()))?;
                let run=stores.runs().inspect(&binding.run_id).await?.ok_or_else(||RuntimeError::Conflict("private turn run absent".into()))?;
                Ok(Json(manager.abandon_turn(id,&input.turn,&run,&owner,&input.note).await?).into_response())
            }
            ("GET",["sessions",id,"diagnostics"])=> {
                super::valid_id(id)?;let stores=self.stores(&owner,false).await?;let session=SessionManager::new(&stores.sessions()).load(id).await?;let mut children=Vec::new();let mut bytes=0usize;
                for binding in session.turns.iter().map(|turn|&turn.binding).chain(session.active.iter()) {
                    if children.len()>=256 {return Err(RuntimeError::StorageLimit("private session diagnostic child bound exceeded".into()));}
                    if let Some(run)=stores.runs().inspect(&binding.run_id).await? {bytes=bytes.saturating_add(serde_json::to_vec(&run).map_err(crate::runtime::store::storage)?.len());if bytes>64*1024*1024{return Err(RuntimeError::StorageLimit("private diagnostic inputs exceed 64MiB".into()));}children.push(run);}
                }
                Ok(Json(TraceSnapshot::session(&self.label(&owner)?,&session,&children)?).into_response())
            }
            ("GET",["runs"])=> {
                let (database,_)=self.app.stores.sql.as_ref().ok_or_else(||RuntimeError::Configuration("private SQL unavailable".into()))?;
                let runs=match database.private_stores_if_exists(&self.tenant,&self.project,&owner).await? {Some(stores)=>stores.runs().list().await?,None=>Vec::new()};
                Ok(Json(json!({"runs":runs})).into_response())
            }
            ("GET",["runs",id])=> {super::valid_read_id(id)?;let run=self.stores(&owner,false).await?.runs().inspect(id).await?.ok_or_else(||RuntimeError::Configuration("private run not found".into()))?;Ok(Json(run).into_response())}
            ("GET",["runs",id,"diagnostics"])=> {super::valid_read_id(id)?;let run=self.stores(&owner,false).await?.runs().inspect(id).await?.ok_or_else(||RuntimeError::Configuration("private run not found".into()))?;Ok(Json(TraceSnapshot::run(&self.label(&owner)?,&run)?).into_response())}
            ("POST",["memory","extract"])=> {
                if !self.memory.curate||!matches!(role,ProjectRole::Runner|ProjectRole::Operator){return Err(RuntimeError::Permission("explicit private memory curation is required".into()));}
                let input:MemoryExtract=body(request).await?;super::valid_id(&input.request_id)?;super::valid_id(&input.session_id)?;super::valid_id(&input.turn_id)?;let id=input.request_id.clone();self.spawn(actor,Work::Extract(input),format!("extract:{id}"))?;Ok((StatusCode::ACCEPTED,Json(json!({"accepted":true,"request_id":id}))).into_response())
            }
            ("GET",["memory","extractions",id])=> {super::valid_id(id)?;Ok(Json(self.status(&owner,&format!("extract:{id}"))?).into_response())}
            _=>Err(RuntimeError::Permission("private route is unavailable: no tools, write approvals, shared queue, cross-owner access or implicit effect replay".into())),
        }
    }
}

struct AuthorizedModel {
    model: Arc<dyn Model>,
    identities: SqlIdentityStore,
    actor: AuthenticatedActor,
    namespace: DatabaseNamespace,
}
impl Model for AuthorizedModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            self.identities
                .authorize(&self.actor, &self.namespace, ProjectPermission::Submit)
                .await?;
            self.model.generate(request, budget).await
        })
    }
}
enum Work {
    Turn {
        session: String,
        turn: String,
        prompt: String,
        recall: bool,
    },
    Resume {
        session: String,
        turn: String,
    },
    Extract(MemoryExtract),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    turn: String,
    prompt: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resume {
    turn: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledge {
    turn: String,
    note: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn body<T: DeserializeOwned>(request: Request) -> Result<T, RuntimeError> {
    let bytes = to_bytes(request.into_body(), 256 * 1024)
        .await
        .map_err(|_| RuntimeError::Configuration("private body exceeds 256KiB".into()))?;
    serde_json::from_slice(&bytes).map_err(|_| {
        RuntimeError::Configuration(
            "private body shape invalid; identity/grants are server-selected".into(),
        )
    })
}
