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
        runs: &'a FileRunStore,
    ) -> Result<WorkflowTasks<'a>, RuntimeError> {
        let mut tasks = WorkflowTasks::new();
        for (role, agent) in &self.agents {
            tasks.register(AgentWorkflowTask::new(
                &role.name,
                agent,
                runs,
                &role.system,
                role.output.clone(),
            )?)?;
        }
        Ok(tasks)
    }
}

impl Application {
    pub(super) fn role_agents(&self) -> RoleAgents<'_> {
        RoleAgents {
            agents: self
                .config
                .roles
                .iter()
                .map(|role| {
                    let mut policy = self.policy.clone();
                    policy
                        .allowed_tools
                        .retain(|name| role.tools.contains(name));
                    (role, self.agent().with_policy(policy))
                })
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
    pub async fn start_workflow(
        self: &Arc<Self>,
        input: AppWorkflowRequest,
    ) -> Result<WorkflowState, RuntimeError> {
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
        let roles = self.role_agents();
        let mut tasks = roles.tasks(&self.runs)?;
        let definition = match (&input.definition, &input.peer) {
            (Some(definition), None) => definition.clone(),
            (None, Some(peer)) => peer.install(&mut tasks)?,
            _ => return Err(config_error("supply exactly one of definition or peer")),
        };
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
                let mut tasks = roles.tasks(&app.runs)?;
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
    fn finish_job(&self, key: &str, result: Result<(), RuntimeError>) {
        if let Ok(mut jobs) = self.jobs() {
            if let Some(job) = jobs.get_mut(key) {
                job.active = false;
                job.error = result.err().map(|e| e.to_string());
            }
        }
    }
    fn signal_parent(&self, key: &str, cancel: bool) -> Result<bool, RuntimeError> {
        let jobs = self.jobs()?;
        if let Some(job) = jobs.get(key).filter(|j| j.active) {
            if cancel {
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
        if self.signal_parent(&format!("workflow:{id}"), false)? {
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
        if self.signal_parent(&format!("workflow:{id}"), true)? {
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
            .config
            .roles
            .iter()
            .find(|r| r.name == spec.task)
            .ok_or_else(|| config_error("node's original role is unavailable"))?;
        let mut policy = self.policy.clone();
        policy
            .allowed_tools
            .retain(|name| role.tools.contains(name));
        let agent = self.agent().with_policy(policy);
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
    fn spawn_session(self: &Arc<Self>, id: &str, turn: &str, job: Job) {
        let app = Arc::clone(self);
        let id = id.to_owned();
        let turn = turn.to_owned();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(async {
                let agent = app.agent().with_cancel(&job.cancel);
                let durable = DurableAgent::new(&agent, &app.runs).with_pause(&job.pause);
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
        if self.signal_parent(&format!("session:{id}"), false)? {
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
        if !self.signal_parent(&format!("session:{id}"), true)? {
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
        let mut summaries = list_states::<WorkflowState, _>(self.workflows.root().into(), ".workflow.json", |s| {
            Ok(json!({"id":s.id,"status":s.status,"revision":s.revision,"nodes":s.nodes.len(),"budget":s.budget()?}))
        }).await?;
        for summary in &mut summaries {
            summary["job"] = json!(self.workflow_job(summary["id"].as_str().unwrap())?);
        }
        Ok(summaries)
    }
    pub async fn list_sessions(&self) -> Result<Vec<Value>, RuntimeError> {
        let mut summaries = list_states::<Session, _>(self.sessions.root().into(), ".session.json", |s| {
            Ok(json!({"id":s.id,"revision":s.revision,"turns":s.turns.len(),"active":s.active.map(|b|json!({"turn_id":b.turn_id,"run_id":b.run_id}))}))
        }).await?;
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
trait ListedState: serde::de::DeserializeOwned + Send + 'static {
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
async fn list_states<T: ListedState, F: Fn(T) -> Result<Value, RuntimeError> + Send + 'static>(
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
