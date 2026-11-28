use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{config::config_error, valid_id, AppRole, Application, Job, JobStatus};
use crate::runtime::workflow::*;
use crate::runtime::*;
use crate::CancelToken;

/// Exactly one of `definition` (a DAG using registered roles) and `peer`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowRequest {
    pub id: String,
    pub input: Value,
    pub definition: Option<WorkflowDefinition>,
    pub peer: Option<PeerSpec>,
    #[serde(default)]
    pub limits: WorkflowLimits,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowLaunch {
    pub request: AppWorkflowRequest,
    pub preview_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppWorkflowPreview {
    pub version: u32,
    pub preview_key: String,
    pub request: AppWorkflowRequest,
    pub plan: WorkflowState,
    pub dispatch_performed: bool,
    pub identity_reserved: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowEnvelope {
    app_workflow_version: u32,
    input: Value,
    peer: Option<PeerSpec>,
}

pub(super) struct RoleAgents<'a> {
    agents: Vec<(&'a AppRole, AgentLoop<'a>)>,
}
impl RoleAgents<'_> {
    pub(super) fn tasks<'a>(
        &'a self,
        runs: &'a dyn RunStore,
    ) -> Result<WorkflowTasks<'a>, RuntimeError> {
        self.tasks_with_cancel_reason(runs, None)
    }
    /// Child runs attribute a live cancellation to the recorded operator.
    pub(super) fn tasks_with_cancel_reason<'a>(
        &'a self,
        runs: &'a dyn RunStore,
        cancel_reason: Option<&Arc<CancelReason>>,
    ) -> Result<WorkflowTasks<'a>, RuntimeError> {
        let mut tasks = WorkflowTasks::new();
        for (role, agent) in &self.agents {
            let mut task =
                AgentWorkflowTask::new(&role.name, agent, runs, &role.system, role.output.clone())?;
            if let Some(reason) = cancel_reason {
                task = task.with_cancel_reason(Arc::clone(reason));
            }
            tasks.register(task)?;
        }
        Ok(tasks)
    }
}

impl Application {
    pub(super) fn role_agents(&self) -> RoleAgents<'_> {
        RoleAgents {
            agents: self
                .registered_roles()
                .map(|role| (role, self.agent_for_role(role)))
                .collect(),
        }
    }

    pub fn workflow_job(&self, id: &str) -> Result<JobStatus, RuntimeError> {
        self.job_key(&format!("workflow:{id}"))
    }
    pub fn session_job(&self, id: &str) -> Result<JobStatus, RuntimeError> {
        self.job_key(&format!("session:{id}"))
    }
    pub async fn workflow(&self, id: &str) -> Result<WorkflowState, RuntimeError> {
        valid_id(id)?;
        self.workflows
            .inspect(id)
            .await?
            .ok_or_else(|| RuntimeError::Conflict("workflow does not exist".into()))
    }
    pub async fn session(&self, id: &str) -> Result<Session, RuntimeError> {
        valid_id(id)?;
        SessionManager::new(&self.sessions).load(id).await
    }
    fn compile_workflow(
        &self,
        input: &AppWorkflowRequest,
        tasks: &mut WorkflowTasks<'_>,
    ) -> Result<WorkflowDefinition, RuntimeError> {
        valid_id(&input.id)?;
        // API callers may tighten execution limits but cannot allocate unbounded
        // workers, memory, dispatches or wall time in the host process.
        if input.limits.max_nodes > 256
            || input.limits.max_parallel > 32
            || input.limits.max_dispatches > 4096
            || input.limits.max_output_bytes > 1024 * 1024
            || input.limits.max_duration > std::time::Duration::from_secs(86400)
            || input.limits.shutdown_grace > std::time::Duration::from_secs(30)
        {
            return Err(config_error("workflow limits exceed application ceilings"));
        }
        let definition = match (&input.definition, &input.peer) {
            (Some(definition), None) => definition.clone(),
            (None, Some(peer)) => peer.install(tasks)?,
            _ => return Err(config_error("supply exactly one of definition or peer")),
        };
        if definition
            .nodes
            .iter()
            .any(|n| self.recovery_roles.contains(&n.task))
        {
            return Err(RuntimeError::Permission(
                "recovery-only skill roles cannot start new workflows".into(),
            ));
        }
        Ok(definition)
    }

    pub fn preview_workflow(
        &self,
        input: AppWorkflowRequest,
    ) -> Result<AppWorkflowPreview, RuntimeError> {
        let roles = self.role_agents();
        let mut tasks = roles.tasks(&self.runs)?;
        let definition = self.compile_workflow(&input, &mut tasks)?;
        let envelope = serde_json::to_value(WorkflowEnvelope {
            app_workflow_version: 1,
            input: input.input.clone(),
            peer: input.peer.clone(),
        })
        .map_err(crate::runtime::store::storage)?;
        let plan = WorkflowRunner::new(&tasks, &self.workflows)
            .with_limits(input.limits.clone())
            .preview(&input.id, definition, envelope)?;
        Ok(AppWorkflowPreview {
            version: 1,
            preview_key: crate::runtime::state::fingerprint(&plan)?,
            request: input,
            plan,
            dispatch_performed: false,
            identity_reserved: false,
        })
    }

    pub async fn launch_workflow(
        self: &Arc<Self>,
        input: AppWorkflowLaunch,
    ) -> Result<WorkflowState, RuntimeError> {
        let preview = self.preview_workflow(input.request)?;
        if preview.preview_key != input.preview_key {
            return Err(RuntimeError::Conflict(
                "workflow input or frozen host contracts changed; preview again before launch"
                    .into(),
            ));
        }
        self.start_workflow(preview.request).await
    }

    pub async fn start_workflow(
        self: &Arc<Self>,
        input: AppWorkflowRequest,
    ) -> Result<WorkflowState, RuntimeError> {
        let roles = self.role_agents();
        let mut tasks = roles.tasks(&self.runs)?;
        let definition = self.compile_workflow(&input, &mut tasks)?;
        let key = format!("workflow:{}", input.id);
        let job = self.admit_key(&key)?;
        let result = WorkflowRunner::new(&tasks, &self.workflows)
            .with_limits(input.limits)
            .create(
                &input.id,
                definition,
                serde_json::to_value(WorkflowEnvelope {
                    app_workflow_version: 1,
                    input: input.input,
                    peer: input.peer,
                })
                .map_err(crate::runtime::store::storage)?,
            )
            .await;
        match result {
            Ok(state) => {
                self.spawn_workflow(&input.id, job);
                Ok(state)
            }
            Err(error) => {
                self.jobs()?.remove(&key);
                Err(error)
            }
        }
    }
    pub async fn resume_workflow(self: &Arc<Self>, id: &str) -> Result<(), RuntimeError> {
        self.workflow(id).await?;
        let job = self.admit_key(&format!("workflow:{id}"))?;
        self.spawn_workflow(id, job);
        Ok(())
    }
    fn spawn_workflow(self: &Arc<Self>, id: &str, job: Job) {
        let app = Arc::clone(self);
        let id = id.to_owned();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                let saved = app.workflow(&id).await?;
                let roles = app.role_agents();
                let mut tasks =
                    roles.tasks_with_cancel_reason(&app.runs, Some(&job.cancel_reason))?;
                restore_peer(&saved, &mut tasks)?;
                WorkflowRunner::new(&tasks, &app.workflows)
                    .with_limits(saved.limits)
                    .with_cancel(&job.cancel)
                    .with_pause(&job.pause)
                    .resume(&id)
                    .await?;
                Ok::<_, RuntimeError>(())
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(RuntimeError::Storage(
                    "workflow worker panicked; inspect persisted child state".into(),
                ))
            });
            app.finish_job(&format!("workflow:{id}"), result);
        });
    }
    pub(super) fn finish_job(&self, key: &str, result: Result<(), RuntimeError>) {
        if let Ok(mut jobs) = self.jobs() {
            if let Some(job) = jobs.get_mut(key) {
                job.active = false;
                job.error = result.err().map(|e| e.to_string());
            }
        }
    }
    /// `cancel` carries the operator's actor and note; `None` pauses.
    fn signal_parent(&self, key: &str, cancel: Option<(&str, &str)>) -> Result<bool, RuntimeError> {
        let jobs = self.jobs()?;
        if let Some(job) = jobs.get(key).filter(|j| j.active) {
            if let Some((actor, note)) = cancel {
                // Record who and why before the worker observes the token.
                job.cancel_reason.record(actor, note)?;
                job.cancel.cancel();
            } else {
                job.pause.cancel();
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn pause_workflow(&self, id: &str) -> Result<(), RuntimeError> {
        valid_id(id)?;
        if self.signal_parent(&format!("workflow:{id}"), None)? {
            Ok(())
        } else {
            Err(RuntimeError::Conflict(
                "workflow has no active local worker".into(),
            ))
        }
    }
    pub async fn cancel_workflow(
        &self,
        id: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), RuntimeError> {
        validate_actor(actor, note)?;
        let state = self.workflow(id).await?;
        if self.signal_parent(&format!("workflow:{id}"), Some((actor, note)))? {
            return Ok(());
        }
        let roles = self.role_agents();
        let mut tasks = roles.tasks(&self.runs)?;
        restore_peer(&state, &mut tasks)?;
        // Revoke pending child approvals through each child's original role.
        // Uncertain effects remain in child journals for explicit reconciliation.
        for node in &state.nodes {
            if node.child.is_some() {
                let (agent, run_id) = self.workflow_child(id, &node.id).await?;
                DurableAgent::new(&agent, &self.runs)
                    .cancel(&run_id, actor, note)
                    .await?;
            }
        }
        let cancel = CancelToken::new();
        cancel.cancel();
        WorkflowRunner::new(&tasks, &self.workflows)
            .with_limits(state.limits)
            .with_cancel(&cancel)
            .resume(id)
            .await?;
        Ok(())
    }
    async fn workflow_child(
        &self,
        id: &str,
        node: &str,
    ) -> Result<(AgentLoop<'_>, String), RuntimeError> {
        let state = self.workflow(id).await?;
        let spec = state
            .definition
            .nodes
            .iter()
            .find(|n| n.id == node)
            .ok_or_else(|| config_error("unknown workflow node"))?;
        let entry = state
            .nodes
            .iter()
            .find(|n| n.id == node)
            .ok_or_else(|| config_error("workflow state has no entry for node"))?;
        let child = entry
            .child
            .as_ref()
            .ok_or_else(|| RuntimeError::Conflict("node has no durable child yet".into()))?;
        let role = self
            .registered_roles()
            .find(|r| r.name == spec.task)
            .ok_or_else(|| config_error("node's original role is unavailable"))?;
        let agent = self.agent_for_role(role);
        let current = AgentWorkflowTask::new(
            &role.name,
            &agent,
            &self.runs,
            &role.system,
            role.output.clone(),
        )?
        .definition()?;
        if state.tasks.iter().find(|t| t.name == spec.task) != Some(&current) {
            return Err(config_error("node's original role configuration changed"));
        }
        Ok((agent, child.run_id.clone()))
    }
    pub async fn decide_workflow(
        &self,
        id: &str,
        node: &str,
        ticket: &str,
        decision: ApprovalDecision,
    ) -> Result<RunState, RuntimeError> {
        let (agent, run_id) = self.workflow_child(id, node).await?;
        DurableAgent::new(&agent, &self.runs)
            .decide(&run_id, ticket, decision)
            .await
    }
    pub async fn reconcile_workflow(
        &self,
        id: &str,
        node: &str,
        call: &str,
        decision: RecoveryDecision,
    ) -> Result<RunState, RuntimeError> {
        let (agent, run_id) = self.workflow_child(id, node).await?;
        DurableAgent::new(&agent, &self.runs)
            .reconcile(&run_id, call, decision)
            .await
    }
    pub async fn create_session(&self, id: &str) -> Result<Session, RuntimeError> {
        valid_id(id)?;
        SessionManager::new(&self.sessions)
            .create(id, vec![self.config.system.clone()])
            .await
    }
    pub async fn start_session_turn(
        self: &Arc<Self>,
        id: &str,
        turn: &str,
        prompt: &str,
    ) -> Result<RunState, RuntimeError> {
        valid_id(id)?;
        valid_id(turn)?;
        if prompt.trim().is_empty() || prompt.len() > 128 * 1024 {
            return Err(config_error("prompt must be 1..=131072 bytes"));
        }
        let key = format!("session:{id}");
        let job = self.admit_key(&key)?;
        let result = async {
            let prepared = SessionManager::new(&self.sessions)
                .begin_turn(id, turn, prompt)
                .await?;
            // begin_turn is idempotent; do not recreate a saved or closed child.
            if let Some(state) = self.runs.inspect(&prepared.run_id).await? {
                return Ok(state);
            }
            DurableAgent::new(&self.agent(), &self.runs)
                .with_trace_binding(crate::runtime::observability::TraceBinding::session(
                    id, turn,
                ))?
                .create(&prepared.run_id, prepared.messages)
                .await
        }
        .await;
        match result {
            Ok(state) => {
                self.spawn_session(id, turn, job);
                Ok(state)
            }
            Err(error) => {
                self.jobs()?.remove(&key);
                Err(error)
            }
        }
    }
    pub async fn resume_session(self: &Arc<Self>, id: &str) -> Result<(), RuntimeError> {
        let session = self.session(id).await?;
        let binding = session
            .active
            .ok_or_else(|| RuntimeError::Conflict("session has no active turn".into()))?;
        let job = self.admit_key(&format!("session:{id}"))?;
        self.spawn_session(id, &binding.turn_id, job);
        Ok(())
    }
    pub(super) fn spawn_session(self: &Arc<Self>, id: &str, turn: &str, job: Job) {
        let app = Arc::clone(self);
        let id = id.to_owned();
        let turn = turn.to_owned();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                let agent = app.agent().with_cancel(&job.cancel);
                let durable = DurableAgent::new(&agent, &app.runs)
                    .with_pause(&job.pause)
                    .with_cancel_reason(&job.cancel_reason);
                SessionManager::new(&app.sessions)
                    .resume_turn(&durable, &id, &turn)
                    .await?;
                Ok::<_, RuntimeError>(())
            })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                Err(RuntimeError::Storage(
                    "session worker panicked; inspect persisted child state".into(),
                ))
            });
            app.finish_job(&format!("session:{id}"), result);
        });
    }
    async fn session_child(&self, id: &str) -> Result<String, RuntimeError> {
        self.session(id)
            .await?
            .active
            .map(|b| b.run_id)
            .ok_or_else(|| RuntimeError::Conflict("session has no active turn".into()))
    }
    pub fn pause_session(&self, id: &str) -> Result<(), RuntimeError> {
        valid_id(id)?;
        if self.signal_parent(&format!("session:{id}"), None)? {
            Ok(())
        } else {
            Err(RuntimeError::Conflict(
                "session has no active local worker".into(),
            ))
        }
    }
    pub async fn cancel_session(
        &self,
        id: &str,
        actor: &str,
        note: &str,
    ) -> Result<(), RuntimeError> {
        validate_actor(actor, note)?;
        let run = self.session_child(id).await?;
        if !self.signal_parent(&format!("session:{id}"), Some((actor, note)))? {
            DurableAgent::new(&self.agent(), &self.runs)
                .cancel(&run, actor, note)
                .await?;
        }
        Ok(())
    }
    pub async fn decide_session(
        &self,
        id: &str,
        ticket: &str,
        decision: ApprovalDecision,
    ) -> Result<RunState, RuntimeError> {
        let run = self.session_child(id).await?;
        DurableAgent::new(&self.agent(), &self.runs)
            .decide(&run, ticket, decision)
            .await
    }
    pub async fn reconcile_session(
        &self,
        id: &str,
        call: &str,
        decision: RecoveryDecision,
    ) -> Result<RunState, RuntimeError> {
        let run = self.session_child(id).await?;
        DurableAgent::new(&self.agent(), &self.runs)
            .reconcile(&run, call, decision)
            .await
    }
    pub async fn abandon_session(
        &self,
        id: &str,
        actor: &str,
        note: &str,
    ) -> Result<Session, RuntimeError> {
        let session = self.session(id).await?;
        let binding = session
            .active
            .ok_or_else(|| RuntimeError::Conflict("session has no active turn".into()))?;
        let state = self.inspect(&binding.run_id).await?;
        SessionManager::new(&self.sessions)
            .abandon_turn(id, &binding.turn_id, &state, actor, note)
            .await
    }
    pub async fn list_workflows(&self) -> Result<Vec<Value>, RuntimeError> {
        let mut summaries = self.workflows.list().await?;
        for summary in &mut summaries {
            summary["job"] = json!(self.workflow_job(summary["id"].as_str().unwrap())?);
        }
        Ok(summaries)
    }
    pub async fn list_sessions(&self) -> Result<Vec<Value>, RuntimeError> {
        let mut summaries = self.sessions.list().await?;
        for summary in &mut summaries {
            summary["job"] = json!(self.session_job(summary["id"].as_str().unwrap())?);
        }
        Ok(summaries)
    }
}

fn restore_peer(state: &WorkflowState, tasks: &mut WorkflowTasks<'_>) -> Result<(), RuntimeError> {
    let envelope: WorkflowEnvelope = serde_json::from_value(state.input.clone())
        .map_err(|_| config_error("not an application workflow"))?;
    if envelope.app_workflow_version != 1 {
        return Err(config_error("unsupported application workflow version"));
    }
    if let Some(peer) = envelope.peer {
        if peer.install(tasks)? != state.definition {
            return Err(config_error(
                "saved PEER definition differs from its specification",
            ));
        }
    }
    Ok(())
}
fn validate_actor(actor: &str, note: &str) -> Result<(), RuntimeError> {
    if actor.trim().is_empty() || actor.len() > 256 || note.len() > 4096 {
        Err(config_error("invalid operator or note"))
    } else {
        Ok(())
    }
}
pub(super) trait ListedState: serde::de::DeserializeOwned + Send + 'static {
    fn id(&self) -> &str;
    fn validate(&self) -> Result<(), RuntimeError>;
}
impl ListedState for WorkflowState {
    fn id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<(), RuntimeError> {
        WorkflowState::validate(self)
    }
}
impl ListedState for Session {
    fn id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<(), RuntimeError> {
        Session::validate(self)
    }
}
pub(super) async fn list_states<
    T: ListedState,
    F: Fn(T) -> Result<Value, RuntimeError> + Send + 'static,
>(
    root: PathBuf,
    suffix: &'static str,
    project: F,
) -> Result<Vec<Value>, RuntimeError> {
    crate::runtime::store::blocking(move || {
        let mut states = Vec::new();
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(states),
            Err(e) => return Err(crate::runtime::store::storage(e)),
        };
        for entry in entries.take(20_000) {
            let path = entry.map_err(crate::runtime::store::storage)?.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.len() != 64 + suffix.len()
                || !name.ends_with(suffix)
                || !name.as_bytes()[..64].iter().all(u8::is_ascii_hexdigit)
            {
                continue;
            }
            if let Some(state) = crate::runtime::store::read_json::<T>(&path, 64 * 1024 * 1024)? {
                state.validate()?;
                use sha2::{Digest, Sha256};
                if hex::encode(Sha256::digest(state.id().as_bytes())) != name[..64] {
                    return Err(crate::runtime::store::storage(
                        "listed state identity differs from filename",
                    ));
                }
                states.push(project(state)?);
            }
            if states.len() >= 1000 {
                break;
            }
        }
        states.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(states)
    })
    .await
}
