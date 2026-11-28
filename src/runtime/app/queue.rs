//! Explicit durable service execution. Queue input selects registered host
//! roles; workers still validate exact saved runtime contracts before dispatch.
use super::{AppWorkflowLaunch, Application};
use crate::runtime::{database::*, workflow::*, *};
use crate::CancelToken;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueuedTask {
    Run {
        id: String,
        prompt: String,
    },
    Workflow {
        launch: Box<AppWorkflowLaunch>,
    },
    SessionTurn {
        id: String,
        turn: String,
        prompt: String,
    },
}
impl QueuedTask {
    fn keys(&self) -> Result<(String, String), RuntimeError> {
        let (job, resource) = match self {
            Self::Run { id, .. } => {
                super::valid_id(id)?;
                (format!("run-{id}"), format!("run:{id}"))
            }
            Self::Workflow { launch } => {
                super::valid_id(&launch.request.id)?;
                (
                    format!("workflow-{}", launch.request.id),
                    format!("workflow:{}", launch.request.id),
                )
            }
            Self::SessionTurn { id, turn, .. } => {
                super::valid_id(id)?;
                super::valid_id(turn)?;
                (
                    format!("turn-{}", crate::runtime::state::fingerprint(&(id, turn))?),
                    format!("session:{id}"),
                )
            }
        };
        Ok((job, resource))
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSubmission {
    pub task: QueuedTask,
    pub actor: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueResume {
    pub expected_revision: u64,
    pub actor: String,
    pub note: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSignal {
    pub expected_revision: u64,
    pub action: QueueSignalAction,
    pub actor: String,
    pub note: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueSignalAction {
    Pause,
    Cancel,
    Abandon,
}

impl Application {
    pub fn queue_store(&self) -> Result<SqlQueueStore, RuntimeError> {
        let (db, ns) = self.stores.sql.as_ref().ok_or_else(|| {
            RuntimeError::Configuration(
                "durable queues require explicit SQL Application storage".into(),
            )
        })?;
        Ok(db.queue(ns.clone()))
    }
    /// No secrets or per-process state paths enter this worker contract.
    /// Custom models remain trusted host implementations of the declared config.
    pub fn queue_binding(&self) -> Result<String, RuntimeError> {
        let mut config =
            serde_json::to_value(&self.config).map_err(crate::runtime::store::storage)?;
        config.as_object_mut().unwrap().remove("state_dir");
        config["database"] = serde_json::json!({"dataset_project_identity":self.stores.identity()});
        let roles = self
            .registered_roles()
            .map(|role| {
                let agent = self.agent_for_role(role);
                (role, DurableAgent::new(&agent, &self.runs).configuration())
            })
            .collect::<Vec<_>>();
        crate::runtime::state::fingerprint(&(
            "app-queue-v1",
            config,
            DurableAgent::new(&self.agent(), &self.runs).configuration(),
            roles,
            self.retriever.identity(),
        ))
    }
    pub async fn configure_queue(
        &self,
        limits: Option<QueueLimits>,
        expected: u64,
        operator: &str,
        note: &str,
    ) -> Result<QueuePolicy, RuntimeError> {
        self.queue_store()?
            .configure(limits, &self.queue_binding()?, expected, operator, note)
            .await
    }
    pub async fn enqueue_task(
        &self,
        task: QueuedTask,
        actor: &str,
    ) -> Result<QueueJob, RuntimeError> {
        let (id, resource) = task.keys()?;
        match &task {
            QueuedTask::Run { prompt, .. } | QueuedTask::SessionTurn { prompt, .. } => {
                if prompt.trim().is_empty() || prompt.len() > 128 * 1024 {
                    return Err(RuntimeError::Configuration(
                        "queued prompt must contain 1..131072 bytes".into(),
                    ));
                }
            }
            QueuedTask::Workflow { launch } => {
                let preview = self.preview_workflow(launch.request.clone())?;
                if preview.preview_key != launch.preview_key {
                    return Err(RuntimeError::Conflict(
                        "queue workflow preview or host contracts changed".into(),
                    ));
                }
            }
        }
        if let QueuedTask::SessionTurn { id, .. } = &task {
            self.session(id).await?;
        }
        self.queue_store()?
            .enqueue(
                &id,
                &resource,
                serde_json::to_value(task).map_err(crate::runtime::store::storage)?,
                &self.queue_binding()?,
                actor,
            )
            .await
    }
    pub async fn resume_queue_job(
        &self,
        id: &str,
        input: QueueResume,
    ) -> Result<QueueJob, RuntimeError> {
        self.queue_store()?
            .resume(
                id,
                input.expected_revision,
                &self.queue_binding()?,
                &input.actor,
                &input.note,
            )
            .await
    }
    pub async fn signal_queue_job(
        &self,
        id: &str,
        input: QueueSignal,
    ) -> Result<QueueJob, RuntimeError> {
        let control = match input.action {
            QueueSignalAction::Abandon => {
                return self
                    .queue_store()?
                    .abandon(id, input.expected_revision, &input.actor, &input.note)
                    .await
            }
            QueueSignalAction::Pause => QueueControl::Pause {
                actor: input.actor,
                note: input.note,
            },
            QueueSignalAction::Cancel => QueueControl::Cancel {
                actor: input.actor,
                note: input.note,
            },
        };
        self.queue_store()?
            .control(id, input.expected_revision, control)
            .await
    }
    /// One claimed attempt. Start multiple independent processes for parallel
    /// execution; global queue capacity and SQL leases arbitrate admission.
    /// Shutdown requests pause at a durable boundary; a hard death is recovered
    /// only after both queue and runtime ownership expire.
    pub async fn work_queue_once(
        &self,
        worker: &str,
        shutdown: &CancelToken,
    ) -> Result<Option<QueueJob>, RuntimeError> {
        if shutdown.is_cancelled() {
            return Ok(None);
        }
        let queue = self.queue_store()?;
        let Some(owner) = queue.claim(&self.queue_binding()?, worker).await? else {
            return Ok(None);
        };
        let claimed = owner.snapshot().await?;
        let prepared = serde_json::from_value::<QueuedTask>(claimed.payload.clone())
            .map_err(crate::runtime::store::storage)
            .and_then(|task| {
                let (key, resource) = task.keys()?;
                if key != claimed.id || resource != claimed.resource {
                    return Err(RuntimeError::Storage(
                        "queued task identity differs from scheduler binding".into(),
                    ));
                }
                Ok(task)
            });
        let task = match prepared {
            Ok(task) => task,
            Err(error) => {
                return Ok(Some(
                    owner
                        .finish(QueueState::Failed, &failure_detail(&error))
                        .await?,
                ))
            }
        };
        let cancel = CancelToken::new();
        let pause = CancelToken::new();
        let reason = Arc::new(CancelReason::new());
        apply_control(claimed.control.as_ref(), &cancel, &pause, &reason)?;
        if shutdown.is_cancelled() {
            pause.cancel();
        }
        let done = CancelToken::new();
        let lost = Arc::new(AtomicBool::new(false));
        let monitor = async {
            loop {
                tokio::select! {
                    _=done.cancelled()=>return,
                    _=tokio::time::sleep(Duration::from_millis(100))=>(),
                }
                if shutdown.is_cancelled() {
                    pause.cancel();
                }
                match owner.snapshot().await {
                    Ok(job) => {
                        if apply_control(job.control.as_ref(), &cancel, &pause, &reason).is_err() {
                            lost.store(true, Ordering::SeqCst);
                            cancel.cancel();
                            return;
                        }
                    }
                    Err(_) => {
                        lost.store(true, Ordering::SeqCst);
                        cancel.cancel();
                        return;
                    }
                }
            }
        };
        let execute = async {
            let result=std::panic::AssertUnwindSafe(self.execute_queued(&owner,task,&cancel,&pause,&reason)).catch_unwind().await.unwrap_or_else(|_|Err(RuntimeError::Storage("queue worker panicked; inspect the original runtime intent before explicit recovery".into())));
            done.cancel();
            result
        };
        let (result, ()) = tokio::join!(execute, monitor);
        if lost.load(Ordering::SeqCst) {
            return Err(RuntimeError::Conflict("queue ownership became unverified; no outcome acknowledgement, inspect/recover persisted runtime state".into()));
        }
        let (state, detail) = match result {
            Ok(outcome) => (
                outcome,
                "runtime reached an inactive durable boundary".to_string(),
            ),
            Err(error) => (QueueState::Failed, failure_detail(&error)),
        };
        let note = if detail.trim().is_empty() {
            "queue execution failed"
        } else {
            &detail
        };
        Ok(Some(owner.finish(state, note).await?))
    }
    async fn execute_queued(
        &self,
        owner: &SqlQueueLease,
        task: QueuedTask,
        cancel: &CancelToken,
        pause: &CancelToken,
        reason: &Arc<CancelReason>,
    ) -> Result<QueueState, RuntimeError> {
        let (db, ns) =
            self.stores.sql.as_ref().ok_or_else(|| {
                RuntimeError::Configuration("queued execution requires SQL".into())
            })?;
        let runs = db.runs(ns.clone()).with_queue_lease(owner)?;
        match task {
            QueuedTask::Run { id, prompt } => {
                let agent = self.agent().with_cancel(cancel);
                let state = DurableAgent::new(&agent, &runs)
                    .with_pause(pause)
                    .with_cancel_reason(reason)
                    .start_or_resume(
                        &id,
                        vec![Message::system(&self.config.system), Message::user(prompt)],
                    )
                    .await?;
                Ok(run_outcome(&state))
            }
            QueuedTask::Workflow { launch } => {
                let preview = self.preview_workflow(launch.request.clone())?;
                if preview.preview_key != launch.preview_key {
                    return Err(RuntimeError::Conflict(
                        "worker workflow contracts differ from queued preview".into(),
                    ));
                }
                let roles = self.role_agents();
                let mut tasks = roles.tasks_with_cancel_reason(&runs, Some(reason))?;
                if let Some(peer) = &launch.request.peer {
                    peer.install(&mut tasks)?;
                }
                let workflows = db.workflows(ns.clone()).with_queue_lease(owner)?;
                let runner = WorkflowRunner::new(&tasks, &workflows)
                    .with_limits(launch.request.limits)
                    .with_cancel(cancel)
                    .with_pause(pause);
                let state = if let Some(saved) = workflows.inspect(&launch.request.id).await? {
                    if saved.definition != preview.plan.definition
                        || saved.input != preview.plan.input
                        || saved.limits != preview.plan.limits
                        || saved.tasks != preview.plan.tasks
                    {
                        return Err(RuntimeError::Conflict(
                            "workflow identity is already bound to other exact inputs or tasks"
                                .into(),
                        ));
                    }
                    runner.resume(&saved.id).await?
                } else {
                    runner
                        .start(
                            &preview.plan.id,
                            preview.plan.definition,
                            preview.plan.input,
                        )
                        .await?
                };
                // Parent cancellation can leave an interrupted child requiring
                // operator reconciliation. Keep the resource occupied then.
                let mut uncertain = false;
                for node in &state.nodes {
                    if let Some(child) = &node.child {
                        if let Some(run) = runs.inspect(&child.run_id).await? {
                            uncertain |= has_uncertain_tools(&run);
                        }
                    }
                }
                Ok(if uncertain {
                    QueueState::Waiting
                } else if state.status == WorkflowStatus::Completed {
                    QueueState::Completed
                } else if state.status == WorkflowStatus::Paused {
                    QueueState::Paused
                } else if state.status.is_terminal() {
                    QueueState::Stopped
                } else {
                    QueueState::Waiting
                })
            }
            QueuedTask::SessionTurn { id, turn, prompt } => {
                let sessions = db.sessions(ns.clone()).with_queue_lease(owner)?;
                let agent = self.agent().with_cancel(cancel);
                let durable = DurableAgent::new(&agent, &runs)
                    .with_pause(pause)
                    .with_cancel_reason(reason);
                let result = SessionManager::new(&sessions)
                    .run_turn(&durable, &id, &turn, &prompt)
                    .await?;
                Ok(if result.session.active.is_some() {
                    if result.run.status == RunStatus::Paused {
                        QueueState::Paused
                    } else {
                        QueueState::Waiting
                    }
                } else {
                    run_outcome(&result.run)
                })
            }
        }
    }
}
fn failure_detail(error: &RuntimeError) -> String {
    let mut detail = String::new();
    for ch in error.to_string().chars().filter(|ch| !ch.is_control()) {
        if detail.len() + ch.len_utf8() > 4096 {
            break;
        }
        detail.push(ch);
    }
    if detail.trim().is_empty() {
        "queue execution failed".into()
    } else {
        detail
    }
}
fn apply_control(
    control: Option<&QueueControl>,
    cancel: &CancelToken,
    pause: &CancelToken,
    reason: &CancelReason,
) -> Result<(), RuntimeError> {
    match control {
        Some(QueueControl::Cancel { actor, note }) => {
            if reason.get().is_none() {
                reason.record(actor, note)?;
            }
            cancel.cancel();
        }
        Some(QueueControl::Pause { .. }) => pause.cancel(),
        None => (),
    };
    Ok(())
}
fn has_uncertain_tools(state: &RunState) -> bool {
    state.tool_steps.iter().any(|s| {
        matches!(
            s.progress,
            ToolProgress::InFlight { .. } | ToolProgress::Uncertain { .. }
        )
    })
}
fn run_outcome(state: &RunState) -> QueueState {
    if has_uncertain_tools(state)
        || matches!(
            state.status,
            RunStatus::WaitingApproval | RunStatus::NeedsRecovery
        )
    {
        QueueState::Waiting
    } else if state.status == RunStatus::Paused {
        QueueState::Paused
    } else if state.status == RunStatus::Finished {
        if state.report.stop_reason == StopReason::Completed {
            QueueState::Completed
        } else {
            QueueState::Stopped
        }
    } else {
        QueueState::Failed
    }
}

pub(super) fn routes() -> axum::Router<Arc<Application>> {
    use super::service::ApiError;
    use axum::{
        extract::{Path, State},
        routing::get,
        Json,
    };
    use serde_json::{json, Value};
    type ResultJson = Result<Json<Value>, ApiError>;
    async fn policy(State(app): State<Arc<Application>>) -> ResultJson {
        Ok(Json(
            json!({"policy":app.queue_store()?.policy().await?,"host_binding":app.queue_binding()?}),
        ))
    }
    async fn jobs(State(app): State<Arc<Application>>) -> ResultJson {
        Ok(Json(
            json!({"jobs":app.queue_store()?.list().await?,"limit":1000}),
        ))
    }
    async fn job(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ResultJson {
        let queue = app.queue_store()?;
        Ok(Json(
            json!({"job":queue.inspect(&id).await?,"runtime_bindings":queue.bindings(&id).await?}),
        ))
    }
    async fn submit(
        State(app): State<Arc<Application>>,
        Json(input): Json<QueueSubmission>,
    ) -> ResultJson {
        Ok(Json(
            json!({"job":app.enqueue_task(input.task,&input.actor).await?}),
        ))
    }
    async fn resume(
        State(app): State<Arc<Application>>,
        Path(id): Path<String>,
        Json(input): Json<QueueResume>,
    ) -> ResultJson {
        Ok(Json(json!({"job":app.resume_queue_job(&id,input).await?})))
    }
    async fn signal(
        State(app): State<Arc<Application>>,
        Path(id): Path<String>,
        Json(input): Json<QueueSignal>,
    ) -> ResultJson {
        Ok(Json({
            let abandoned = matches!(input.action, QueueSignalAction::Abandon);
            json!({"job":app.signal_queue_job(&id,input).await?,"control_completion":if abandoned{"scheduling permanently abandoned; runtime and external outcomes unchanged"}else{"request persisted; inspect the worker outcome for completion"}})
        }))
    }
    axum::Router::new()
        .route("/queue", get(policy))
        .route("/queue/jobs", get(jobs).post(submit))
        .route("/queue/jobs/{id}", get(job))
        .route("/queue/jobs/{id}/resume", axum::routing::post(resume))
        .route("/queue/jobs/{id}/control", axum::routing::post(signal))
}
