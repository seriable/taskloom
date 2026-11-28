use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use tokio::time::Instant;

use super::{
    types::validate_history, AgentLoop, ApprovalDecision, ApprovalPolicy, ApprovalRecord,
    BudgetJournal, BudgetSnapshot, Message, ModelRequest, RecoveryDecision, RecoveryRecord,
    RunBudget, RunConfig, RunCursor, RunEventKind, RunLease, RunReport, RunState, RunStatus,
    RunStore, RuntimeError, StopReason, ToolEffect, ToolProgress, ToolResult, ToolStep,
    RUN_STATE_VERSION,
};
use crate::{ledger::Usage, CancelToken, TaskError};

/// Checkpointed execution using the same model/tool contracts as [`AgentLoop`].
/// A lease covers the whole invocation. Approval and reconciliation only
/// update state; a separate explicit `resume` performs further work.
#[derive(Clone)]
pub struct DurableAgent<'a, 'model> {
    agent: &'a AgentLoop<'model>,
    store: &'a dyn RunStore,
    approvals: ApprovalPolicy,
    pause: Option<&'a CancelToken>,
    cancel_reason: Option<&'a CancelReason>,
    checkpoint_limit: Option<u64>,
    trace_binding: Option<super::observability::TraceBinding>,
}

/// Who cancelled a live run and why. Record it before firing the worker's
/// cancellation token; the run saves it with its terminal checkpoint, the
/// same way [`DurableAgent::cancel`] does for an unowned run.
#[derive(Debug, Default)]
pub struct CancelReason(std::sync::Mutex<Option<(String, String)>>);

impl CancelReason {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store the operator's actor and note. The first recorded reason wins.
    pub fn record(&self, actor: &str, note: &str) -> Result<(), RuntimeError> {
        validate_actor(actor, note)?;
        let mut reason = self
            .0
            .lock()
            .map_err(|_| corrupt("cancellation reason lock poisoned"))?;
        reason.get_or_insert_with(|| (actor.into(), note.into()));
        Ok(())
    }

    pub fn get(&self) -> Option<(String, String)> {
        self.0.lock().ok().and_then(|reason| reason.clone())
    }
}

impl<'a, 'model> DurableAgent<'a, 'model> {
    pub fn new(agent: &'a AgentLoop<'model>, store: &'a dyn RunStore) -> Self {
        Self {
            agent,
            store,
            approvals: ApprovalPolicy::default(),
            pause: None,
            cancel_reason: None,
            checkpoint_limit: None,
            trace_binding: None,
        }
    }

    pub fn with_approvals(mut self, approvals: ApprovalPolicy) -> Self {
        self.approvals = approvals;
        self
    }
    /// Freeze host-owned parent correlation for a newly created child. This
    /// changes no model input or execution grant. Old unbound runs are never
    /// silently backfilled on resume; existing linked runs cannot be reparented.
    pub fn with_trace_binding(
        mut self,
        binding: super::observability::TraceBinding,
    ) -> Result<Self, RuntimeError> {
        binding.validate()?;
        if self
            .trace_binding
            .as_ref()
            .is_some_and(|old| old != &binding)
        {
            return Err(RuntimeError::Configuration(
                "conflicting host trace bindings".into(),
            ));
        }
        self.trace_binding = Some(binding);
        Ok(self)
    }

    /// A parent may reserve part of its own storage capacity for the run's
    /// transcript. This is an operational admission limit, not a new run input.
    pub(crate) fn with_checkpoint_limit(mut self, bytes: u64) -> Self {
        self.checkpoint_limit = Some(self.checkpoint_limit.map_or(bytes, |old| old.min(bytes)));
        self
    }

    fn check_capacity(
        &self,
        state: &RunState,
        lease: &Arc<dyn RunLease>,
        additional: usize,
    ) -> Result<(), RuntimeError> {
        let max = match (lease.max_bytes(), self.checkpoint_limit) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return Ok(()),
        };
        let size = serde_json::to_vec(state)
            .map_err(|e| corrupt(&e.to_string()))?
            .len() as u64;
        if size
            .saturating_add(additional as u64)
            .saturating_add(TERMINAL_RECORD_RESERVE)
            > max
        {
            return Err(RuntimeError::StorageLimit(format!(
                "run needs {additional} bytes for its bounded response and terminal record beyond its {size}-byte checkpoint; effective storage limit is {max} bytes; start a new session/run or configure smaller result limits"
            )));
        }
        Ok(())
    }

    /// Graceful pause: finish/checkpoint the current operation, then stop
    /// before the next dispatch. Cancellation still interrupts immediately.
    pub fn with_pause(mut self, token: &'a CancelToken) -> Self {
        self.pause = Some(token);
        self
    }

    /// Attribute a cancellation of this live worker to an operator. See
    /// [`CancelReason`].
    pub fn with_cancel_reason(mut self, reason: &'a CancelReason) -> Self {
        self.cancel_reason = Some(reason);
        self
    }

    pub async fn start(
        &self,
        run_id: &str,
        messages: Vec<Message>,
    ) -> Result<RunState, RuntimeError> {
        let lease = self.store.acquire(run_id).await?;
        let (state, budget, clock) = self.create_owned(run_id, messages, &lease).await?;
        self.execute_owned(state, lease, budget, clock).await
    }

    /// Save a new runnable job without making a model or tool request.
    pub async fn create(
        &self,
        run_id: &str,
        messages: Vec<Message>,
    ) -> Result<RunState, RuntimeError> {
        let lease = self.store.acquire(run_id).await?;
        let (state, _, _) = self.create_owned(run_id, messages, &lease).await?;
        Ok(state)
    }

    pub async fn resume(&self, run_id: &str) -> Result<RunState, RuntimeError> {
        let lease = self.store.acquire(run_id).await?;
        self.resume_owned(run_id, lease).await
    }

    /// Cancel an unowned persisted run without dispatching model/tool work.
    /// A live owner must first receive its cancellation token. Unknown effects
    /// stay uncertain and can still be reconciled after terminal cancellation.
    pub async fn cancel(
        &self,
        run_id: &str,
        actor: &str,
        note: &str,
    ) -> Result<RunState, RuntimeError> {
        validate_actor(actor, note)?;
        let lease = self.store.acquire(run_id).await?;
        // Cancellation dispatches nothing, so it stays available after a host
        // upgrade changed the frozen configuration or tool definitions.
        let (mut state, budget) = self.load_owned(&lease, run_id, false).await?;
        if state.status == RunStatus::Finished {
            return Ok(state);
        }
        if let RunCursor::ModelInFlight {
            started_unix_ms,
            elapsed_before_ms,
        } = state.cursor
        {
            charge_interrupted_time(&mut state, started_unix_ms, elapsed_before_ms)?;
        }
        if let Some(index) = state.current_tool_index() {
            if let ToolProgress::InFlight {
                started_unix_ms,
                elapsed_before_ms,
            } = state.tool_steps[index].progress
            {
                charge_interrupted_time(&mut state, started_unix_ms, elapsed_before_ms)?;
                state.tool_steps[index].progress = ToolProgress::Uncertain {
                    detail:
                        "cancelled after worker interruption; external outcome must be reconciled"
                            .into(),
                    reported_result: None,
                };
                let clock = Clock::new(state.report.elapsed_ms);
                let call_id = state.tool_steps[index].call.id.clone();
                self.event(
                    &mut state,
                    &clock,
                    RunEventKind::RecoveryRequired { call_id },
                );
            }
            if let ToolProgress::AwaitingApproval { request } =
                state.tool_steps[index].progress.clone()
            {
                state.tool_steps[index].approvals.push(ApprovalRecord {
                    request: request.clone(),
                    decision: ApprovalDecision {
                        approved: false,
                        actor: actor.into(),
                        note: note.into(),
                    },
                    decided_unix_ms: unix_ms()?,
                });
                state.tool_steps[index].progress = ToolProgress::Pending;
                let clock = Clock::new(state.report.elapsed_ms);
                self.event(
                    &mut state,
                    &clock,
                    RunEventKind::ApprovalDecided {
                        request_id: request.request_id,
                        approved: false,
                        actor: actor.into(),
                    },
                );
            }
        }
        let clock = Clock::new(state.report.elapsed_ms);
        self.stop(
            &mut state,
            &lease,
            &budget,
            &clock,
            RunStatus::Finished,
            StopReason::Cancelled,
            Some(format!("cancelled by {actor}: {note}")),
        )
        .await?;
        Ok(state)
    }

    /// Idempotent session/workflow entry point. Reusing an id with different
    /// initial messages is rejected before any dispatch.
    pub async fn start_or_resume(
        &self,
        run_id: &str,
        messages: Vec<Message>,
    ) -> Result<RunState, RuntimeError> {
        let lease = self.store.acquire(run_id).await?;
        if let Some(existing) = lease.load().await? {
            if existing.initial_message_count != messages.len()
                || existing.report.messages.get(..messages.len()) != Some(messages.as_slice())
            {
                return Err(RuntimeError::Conflict(
                    "run id is already bound to different initial messages".into(),
                ));
            }
            self.resume_owned(run_id, lease).await
        } else {
            let (state, budget, clock) = self.create_owned(run_id, messages, &lease).await?;
            self.execute_owned(state, lease, budget, clock).await
        }
    }

    async fn resume_owned(
        &self,
        run_id: &str,
        lease: Arc<dyn RunLease>,
    ) -> Result<RunState, RuntimeError> {
        let (mut state, budget) = self.load_owned(&lease, run_id, true).await?;
        if state.status == RunStatus::Finished
            || state.status == RunStatus::WaitingApproval
            || state.status == RunStatus::NeedsRecovery
        {
            return Ok(state);
        }
        // Acquiring an abandoned running job proves its former process no
        // longer owns the OS lock. It does not prove a remote tool failed.
        if state.status == RunStatus::Running {
            match state.cursor.clone() {
                RunCursor::ModelInFlight {
                    started_unix_ms,
                    elapsed_before_ms,
                } => {
                    charge_interrupted_time(&mut state, started_unix_ms, elapsed_before_ms)?;
                    state.cursor = RunCursor::ModelReady;
                    state.status = RunStatus::Ready;
                    let clock = Clock::new(state.report.elapsed_ms);
                    self.event(&mut state, &clock, RunEventKind::RecoveredModel);
                    self.save(&mut state, &lease, &budget, &clock).await?;
                }
                RunCursor::Tools { .. } => {
                    let index = state
                        .current_tool_index()
                        .ok_or_else(|| corrupt("tool cursor has no current step"))?;
                    if let ToolProgress::InFlight {
                        started_unix_ms,
                        elapsed_before_ms,
                    } = state.tool_steps[index].progress
                    {
                        charge_interrupted_time(&mut state, started_unix_ms, elapsed_before_ms)?;
                        let call_id = state.tool_steps[index].call.id.clone();
                        state.tool_steps[index].progress = ToolProgress::Uncertain {
                            detail: "worker stopped while the tool may have been executing".into(),
                            reported_result: None,
                        };
                        let clock = Clock::new(state.report.elapsed_ms);
                        self.event(
                            &mut state,
                            &clock,
                            RunEventKind::RecoveryRequired { call_id },
                        );
                        self.stop(
                            &mut state,
                            &lease,
                            &budget,
                            &clock,
                            RunStatus::NeedsRecovery,
                            StopReason::Uncertain,
                            Some("reconcile the interrupted tool before resuming".into()),
                        )
                        .await?;
                        return Ok(state);
                    }
                }
                _ => {}
            }
        }
        let clock = Clock::new(state.report.elapsed_ms);
        self.execute_owned(state, lease, budget, clock).await
    }

    pub async fn decide(
        &self,
        run_id: &str,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<RunState, RuntimeError> {
        validate_actor(&decision.actor, &decision.note)?;
        let lease = self.store.acquire(run_id).await?;
        // A rejection only terminates the run; an approval leads to dispatch.
        let (mut state, budget) = self.load_owned(&lease, run_id, decision.approved).await?;
        if state.status != RunStatus::WaitingApproval {
            return Err(RuntimeError::Conflict(
                "run is not awaiting approval".into(),
            ));
        }
        let request = state
            .pending_approval()
            .cloned()
            .ok_or_else(|| corrupt("missing approval"))?;
        if request.request_id != request_id {
            return Err(RuntimeError::Conflict(
                "approval id does not match the exact pending request".into(),
            ));
        }
        let index = state.current_tool_index().unwrap();
        // Recompute from the call/definition, so editing the pending arguments
        // cannot accidentally reuse an earlier approval ticket.
        if request
            != state.tool_steps[index].request(run_id, state.tool_steps[index].attempts + 1)?
        {
            return Err(RuntimeError::Conflict("approval input changed".into()));
        }
        let approved = decision.approved;
        let actor = decision.actor.clone();
        state.tool_steps[index].approvals.push(ApprovalRecord {
            request,
            decision,
            decided_unix_ms: unix_ms()?,
        });
        state.tool_steps[index].progress = ToolProgress::Pending;
        let clock = Clock::new(state.report.elapsed_ms);
        self.event(
            &mut state,
            &clock,
            RunEventKind::ApprovalDecided {
                request_id: request_id.into(),
                approved,
                actor,
            },
        );
        self.stop(
            &mut state,
            &lease,
            &budget,
            &clock,
            if approved {
                RunStatus::Paused
            } else {
                RunStatus::Finished
            },
            if approved {
                StopReason::Paused
            } else {
                StopReason::PermissionDenied
            },
            if approved {
                None
            } else {
                Some("human approval rejected the tool request".into())
            },
        )
        .await?;
        Ok(state)
    }

    pub async fn reconcile(
        &self,
        run_id: &str,
        call_id: &str,
        decision: RecoveryDecision,
    ) -> Result<RunState, RuntimeError> {
        validate_actor(decision.actor(), decision.note())?;
        let lease = self.store.acquire(run_id).await?;
        // RecordResult and Abort only record an observation or terminate;
        // RetrySafe prepares another dispatch and needs the frozen contract.
        let strict = matches!(decision, RecoveryDecision::RetrySafe { .. });
        let (mut state, budget) = self.load_owned(&lease, run_id, strict).await?;
        let index = state
            .current_tool_index()
            .ok_or_else(|| RuntimeError::Conflict("run has no uncertain tool".into()))?;
        if state.tool_steps[index].call.id != call_id
            || !matches!(
                state.tool_steps[index].progress,
                ToolProgress::Uncertain { .. }
            )
        {
            return Err(RuntimeError::Conflict(
                "call id does not match the uncertain tool".into(),
            ));
        }
        let terminal = state.status == RunStatus::Finished;
        match &decision {
            RecoveryDecision::RecordResult {
                output, is_error, ..
            } => {
                if serde_json::to_vec(output)
                    .map_err(|e| corrupt(&e.to_string()))?
                    .len()
                    > self.agent.limits.max_tool_result_bytes
                {
                    return Err(RuntimeError::Configuration(
                        "reconciled result exceeds the byte limit".into(),
                    ));
                }
                complete_tool(
                    &mut state,
                    index,
                    ToolResult {
                        call_id: call_id.into(),
                        output: output.clone(),
                        is_error: *is_error,
                    },
                )?;
            }
            RecoveryDecision::RetrySafe { .. } => {
                if terminal
                    || state.tool_steps[index].definition.effect == ToolEffect::NonIdempotent
                {
                    return Err(RuntimeError::Permission(
                        "cannot retry a non-idempotent operation or a terminal run".into(),
                    ));
                }
                state.tool_steps[index].progress = ToolProgress::Pending;
            }
            RecoveryDecision::Abort { actor, note } => {
                // Resolve the call with an explicit unknown outcome, so the
                // protocol is complete and the abort cannot be repeated.
                complete_tool(
                    &mut state,
                    index,
                    ToolResult {
                        call_id: call_id.into(),
                        output: serde_json::json!({
                            "host_error": "aborted by operator; external outcome unknown",
                            "actor": actor,
                            "note": note,
                        }),
                        is_error: true,
                    },
                )?;
            }
        }
        let actor = decision.actor().to_string();
        let action = decision.action().to_string();
        let abort = matches!(decision, RecoveryDecision::Abort { .. });
        state.tool_steps[index].recoveries.push(RecoveryRecord {
            decision,
            decided_unix_ms: unix_ms()?,
        });
        let clock = Clock::new(state.report.elapsed_ms);
        self.event(
            &mut state,
            &clock,
            RunEventKind::ToolReconciled {
                call_id: call_id.into(),
                actor,
                action,
            },
        );
        let reason = if abort {
            StopReason::ToolError
        } else if terminal {
            state.report.stop_reason
        } else {
            StopReason::Paused
        };
        self.stop(
            &mut state,
            &lease,
            &budget,
            &clock,
            if abort || terminal {
                RunStatus::Finished
            } else {
                RunStatus::Paused
            },
            reason,
            if abort {
                Some("operator aborted the uncertain tool".into())
            } else {
                None
            },
        )
        .await?;
        Ok(state)
    }

    /// Explicit settings frozen into a run (provider-owned settings remain
    /// the host application's responsibility).
    pub fn configuration(&self) -> RunConfig {
        RunConfig {
            model: self.agent.model_id.clone(),
            label: self.agent.label.clone(),
            temperature: self.agent.temperature,
            max_output_tokens: self.agent.max_output_tokens,
            limits: self.agent.limits.clone(),
            policy: self.agent.policy.clone(),
            approvals: self.approvals.clone(),
            tools: self.agent.tools.definitions(&self.agent.policy),
            context: self.agent.context_policy.clone(),
            compressor_id: self
                .agent
                .context_policy
                .as_ref()
                .map(|_| self.agent.compressor.id().into()),
            output_policy: self.agent.output_policy.clone(),
            skill_binding: self.agent.skill_binding.clone(),
        }
    }

    async fn create_owned(
        &self,
        run_id: &str,
        messages: Vec<Message>,
        lease: &Arc<dyn RunLease>,
    ) -> Result<(RunState, RunBudget, Clock), RuntimeError> {
        if lease.load().await?.is_some() {
            return Err(RuntimeError::Conflict(format!(
                "run already exists: {run_id}"
            )));
        }
        if lease.load_budget().await?.is_some() {
            return Err(corrupt(
                "orphaned budget journal; refusing to reuse its run id",
            ));
        }
        let clock = Clock::new(0);
        let mut state = RunState {
            version: RUN_STATE_VERSION,
            revision: 0,
            initial_message_count: messages.len(),
            config: self.configuration(),
            trace_binding: self.trace_binding.clone(),
            status: RunStatus::Ready,
            cursor: RunCursor::ModelReady,
            tool_steps: Vec::new(),
            report: RunReport {
                run_id: run_id.into(),
                messages,
                stop_reason: StopReason::Paused,
                detail: None,
                steps: 0,
                tool_calls: 0,
                usage: Usage::default(),
                budget: BudgetSnapshot {
                    limit_micros: self.agent.limits.max_cost_micros,
                    ..Default::default()
                },
                elapsed_ms: 0,
                events: Vec::new(),
                contexts: Vec::new(),
            },
        };
        self.agent.validate(&state.report)?;
        validate_history(&state.report.messages)?;
        if let Some(binding) = &state.trace_binding {
            binding.validate_state(&state)?;
        }
        self.event(&mut state, &clock, RunEventKind::Started);
        if self.agent.output_policy.is_some() {
            self.check_capacity(&state, lease, 0)?;
        }
        let budget = RunBudget::restore(
            state.report.budget.clone(),
            Arc::new(LeaseJournal(Arc::clone(lease))),
        )?;
        self.save(&mut state, lease, &budget, &clock).await?;
        Ok((state, budget, clock))
    }

    async fn load_owned(
        &self,
        lease: &Arc<dyn RunLease>,
        run_id: &str,
        strict_config: bool,
    ) -> Result<(RunState, RunBudget), RuntimeError> {
        let mut state = lease
            .load()
            .await?
            .ok_or_else(|| RuntimeError::Conflict("run does not exist".into()))?;
        state.validate()?;
        if state.report.run_id != run_id {
            return Err(corrupt("store returned a checkpoint for a different run"));
        }
        if strict_config {
            self.agent.validate(&state.report)?;
        } else {
            // Administrative cancellation/rejection must remain possible
            // when a host removes or changes an output-validation tool.
            self.agent
                .clone()
                .without_output_policy()
                .validate(&state.report)?;
        }
        if strict_config && state.config != self.configuration() {
            return Err(RuntimeError::Configuration(
                "resume configuration or tool definitions differ from the saved run".into(),
            ));
        }
        if strict_config
            && self
                .trace_binding
                .as_ref()
                .zip(state.trace_binding.as_ref())
                .is_some_and(|(expected, saved)| expected != saved)
        {
            return Err(RuntimeError::Configuration(
                "host trace binding differs from saved child".into(),
            ));
        }
        if let Some(journal) = lease.load_budget().await? {
            let saved = &state.report.budget;
            if journal.limit_micros != saved.limit_micros
                || journal.generation == saved.generation && journal != *saved
                || journal.generation > saved.generation
                    && (journal.committed_micros < saved.committed_micros
                        || journal.uncertain_micros < saved.uncertain_micros)
            {
                return Err(corrupt("budget journal conflicts with the checkpoint"));
            }
            if journal.generation > saved.generation {
                state.report.budget = journal;
            }
        }
        let budget = RunBudget::restore(
            state.report.budget.clone(),
            Arc::new(LeaseJournal(Arc::clone(lease))),
        )?;
        state.report.budget = budget.snapshot();
        Ok((state, budget))
    }

    async fn execute_owned(
        &self,
        mut state: RunState,
        lease: Arc<dyn RunLease>,
        budget: RunBudget,
        clock: Clock,
    ) -> Result<RunState, RuntimeError> {
        state.status = RunStatus::Running;
        state.report.detail = None;
        self.event(&mut state, &clock, RunEventKind::Resumed);
        self.save(&mut state, &lease, &budget, &clock).await?;
        let cancel = self
            .agent
            .cancel
            .cloned()
            .or_else(crate::cancel::current)
            .unwrap_or_default();
        while state.status == RunStatus::Running {
            tokio::task::yield_now().await;
            let interruption = if cancel.is_cancelled() {
                Some((RunStatus::Finished, StopReason::Cancelled))
            } else if clock.remaining(self.agent.limits.max_duration).is_zero() {
                Some((RunStatus::Finished, StopReason::Deadline))
            } else if self.pause.is_some_and(CancelToken::is_cancelled) {
                Some((RunStatus::Paused, StopReason::Paused))
            } else {
                None
            };
            if let Some((status, reason)) = interruption {
                self.stop(&mut state, &lease, &budget, &clock, status, reason, None)
                    .await?;
                break;
            }
            let result = match state.cursor {
                RunCursor::ModelReady => {
                    self.model_step(&mut state, &lease, &budget, &clock, &cancel)
                        .await
                }
                RunCursor::Tools { .. } => {
                    self.tool_step(&mut state, &lease, &budget, &clock, &cancel)
                        .await
                }
                _ => Err(corrupt("unexpected execution cursor")),
            };
            if let Err(error) = result {
                if let Some(index) = state.current_tool_index() {
                    if matches!(
                        state.tool_steps[index].progress,
                        ToolProgress::InFlight { .. }
                    ) {
                        let call_id = state.tool_steps[index].call.id.clone();
                        state.tool_steps[index].progress = ToolProgress::Uncertain {
                            detail: "checkpoint failed around tool dispatch".into(),
                            reported_result: None,
                        };
                        self.event(
                            &mut state,
                            &clock,
                            RunEventKind::RecoveryRequired { call_id },
                        );
                    }
                }
                let reason = error_reason(&error);
                self.stop(
                    &mut state,
                    &lease,
                    &budget,
                    &clock,
                    RunStatus::Finished,
                    reason,
                    Some(error.to_string()),
                )
                .await?;
            }
        }
        Ok(state)
    }

    async fn model_step(
        &self,
        state: &mut RunState,
        lease: &Arc<dyn RunLease>,
        budget: &RunBudget,
        clock: &Clock,
        cancel: &CancelToken,
    ) -> Result<(), RuntimeError> {
        if state.report.steps >= self.agent.limits.max_steps {
            return self
                .stop(
                    state,
                    lease,
                    budget,
                    clock,
                    RunStatus::Finished,
                    StopReason::StepLimit,
                    None,
                )
                .await;
        }
        // Build the context without emitting events or changing the live state
        // until its checkpoint and the largest legal reply have been admitted.
        let mut candidate = state.clone();
        candidate.report.steps += 1;
        let active_policy = self.agent.active_policy(&candidate.report);
        let tools = self.agent.tools.definitions(&active_policy);
        let messages = self.agent.prepare_context(&mut candidate.report, &tools)?;
        let max_definition = candidate
            .config
            .tools
            .iter()
            .try_fold(0, |largest, definition| {
                serde_json::to_vec(definition).map(|bytes| largest.max(bytes.len()))
            })
            .map_err(|e| corrupt(&e.to_string()))?;
        let batch = if candidate.config.tools.is_empty() {
            0
        } else {
            self.agent
                .limits
                .max_tool_calls
                .saturating_sub(candidate.report.tool_calls)
        };
        // The reply is stored once as the assistant message; its tool calls
        // (bounded by the same reply size) are copied again into the tool
        // steps, hence twice the reply limit. Each step also holds a copy of
        // its tool definition plus fixed metadata (ids, progress, attempts).
        let reserve = self
            .agent
            .limits
            .max_model_result_bytes
            .saturating_mul(2)
            .saturating_add(batch.saturating_mul(max_definition.saturating_add(256)))
            .saturating_add(
                serde_json::to_vec(&self.agent.model_id)
                    .map_err(|e| corrupt(&e.to_string()))?
                    .len(),
            )
            .saturating_add(if self.agent.output_policy.is_some() {
                super::output::OUTPUT_FEEDBACK_RESERVE
            } else {
                0
            });
        self.check_capacity(&candidate, lease, reserve)?;
        *state = candidate;
        self.event(
            state,
            clock,
            RunEventKind::ModelStarted {
                step: state.report.steps,
                model: self.agent.model_id.clone(),
            },
        );
        if let Some(record) = state.report.contexts.last() {
            self.event(
                state,
                clock,
                RunEventKind::ContextPrepared {
                    step: state.report.steps,
                    omitted_messages: record.omitted_messages(),
                    input_bytes: record.input_bytes,
                },
            );
        }
        state.cursor = RunCursor::ModelInFlight {
            started_unix_ms: unix_ms()?,
            elapsed_before_ms: clock.elapsed_ms(),
        };
        self.save(state, lease, budget, clock).await?;
        let request = ModelRequest {
            label: self.agent.label.clone(),
            model: self.agent.model_id.clone(),
            messages,
            tools,
            temperature: self.agent.temperature,
            max_output_tokens: self.agent.max_output_tokens,
        };
        lease.verify_ownership().await?;
        let (output, usage) = crate::ledger::metered(controlled(
            self.agent.model.generate(&request, budget),
            clock,
            self.agent.limits.max_duration,
            cancel,
        ))
        .await;
        state.report.usage.add(usage);
        state.cursor = RunCursor::ModelReady;
        let output = match output {
            Controlled::Done(output) => output?,
            Controlled::Cancelled => {
                return self
                    .stop(
                        state,
                        lease,
                        budget,
                        clock,
                        RunStatus::Finished,
                        StopReason::Cancelled,
                        None,
                    )
                    .await
            }
            Controlled::Deadline => {
                return self
                    .stop(
                        state,
                        lease,
                        budget,
                        clock,
                        RunStatus::Finished,
                        StopReason::Deadline,
                        None,
                    )
                    .await
            }
        };
        if serde_json::to_vec(&output)
            .map_err(|e| corrupt(&e.to_string()))?
            .len()
            > self.agent.limits.max_model_result_bytes
        {
            return Err(RuntimeError::StorageLimit(
                "model reply exceeded max_model_result_bytes; reply not retained, usage charged, no tools dispatched".into(),
            ));
        }
        if let Some(detail) = budget.snapshot().denial {
            return Err(RuntimeError::Budget(detail));
        }
        output.turn.validate()?;
        let calls = output.turn.tool_calls.clone();
        let mut ids = state
            .report
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Assistant { turn } => {
                    Some(turn.tool_calls.iter().map(|call| call.id.clone()))
                }
                _ => None,
            })
            .flatten()
            .collect::<std::collections::BTreeSet<_>>();
        if calls.iter().any(|call| !ids.insert(call.id.clone())) {
            return Err(RuntimeError::Protocol("reused tool call id".into()));
        }
        self.event(
            state,
            clock,
            RunEventKind::ModelFinished {
                step: state.report.steps,
                model: output.model,
            },
        );
        let assistant_message = state.report.messages.len();
        state
            .report
            .messages
            .push(Message::Assistant { turn: output.turn });
        if calls.is_empty() {
            if let Some(policy) = &self.agent.output_policy {
                let validator = policy.identity()?;
                match policy.check(&state.report) {
                    Ok(()) => self.event(
                        state,
                        clock,
                        RunEventKind::OutputValidated {
                            step: state.report.steps,
                            validator,
                        },
                    ),
                    Err(detail) => {
                        let attempt = super::output::rejected_count(&state.report) + 1;
                        let repair = attempt <= policy.max_repairs();
                        let detail = super::output::bounded_detail(&detail);
                        self.event(
                            state,
                            clock,
                            RunEventKind::OutputRejected {
                                step: state.report.steps,
                                validator,
                                attempt,
                                detail: detail.clone(),
                                repair,
                            },
                        );
                        if repair {
                            state.report.messages.push(super::output::feedback(&detail));
                            // Invalid answer, feedback, repair phase and settled
                            // model budget are saved together before more work.
                            return self.save(state, lease, budget, clock).await;
                        }
                        return Err(RuntimeError::Protocol(format!(
                            "final-output validation rejected the answer: {detail}"
                        )));
                    }
                }
            }
            state.cursor = RunCursor::Finished;
            return self
                .stop(
                    state,
                    lease,
                    budget,
                    clock,
                    RunStatus::Finished,
                    StopReason::Completed,
                    None,
                )
                .await;
        }
        if calls.len()
            > self
                .agent
                .limits
                .max_tool_calls
                .saturating_sub(state.report.tool_calls)
        {
            state.cursor = RunCursor::Finished;
            return self
                .stop(
                    state,
                    lease,
                    budget,
                    clock,
                    RunStatus::Finished,
                    StopReason::ToolLimit,
                    None,
                )
                .await;
        }
        let mut batch = Vec::new();
        for call in calls {
            if let Err(error) = self.agent.tools.prepare(&call, &active_policy) {
                state.cursor = RunCursor::Finished;
                return Err(error);
            }
            let definition = self.agent.tools.definition(&call.name).unwrap().clone();
            batch.push(ToolStep {
                call,
                definition,
                attempts: 0,
                progress: ToolProgress::Pending,
                approvals: Vec::new(),
                recoveries: Vec::new(),
            });
        }
        let first = state.tool_steps.len();
        let count = batch.len();
        state.tool_steps.extend(batch);
        state.cursor = RunCursor::Tools {
            first,
            count,
            next: 0,
            assistant_message,
        };
        self.save(state, lease, budget, clock).await
    }

    async fn tool_step(
        &self,
        state: &mut RunState,
        lease: &Arc<dyn RunLease>,
        budget: &RunBudget,
        clock: &Clock,
        cancel: &CancelToken,
    ) -> Result<(), RuntimeError> {
        let index = state
            .current_tool_index()
            .ok_or_else(|| corrupt("missing current tool"))?;
        if matches!(
            state.tool_steps[index].progress,
            ToolProgress::Uncertain { .. }
        ) {
            return self
                .stop(
                    state,
                    lease,
                    budget,
                    clock,
                    RunStatus::NeedsRecovery,
                    StopReason::Uncertain,
                    Some("reconcile the tool before resuming".into()),
                )
                .await;
        }
        if state.report.tool_calls >= self.agent.limits.max_tool_calls {
            return self
                .stop(
                    state,
                    lease,
                    budget,
                    clock,
                    RunStatus::Finished,
                    StopReason::ToolLimit,
                    None,
                )
                .await;
        }
        let step = &state.tool_steps[index];
        let call = step.call.clone();
        let effect = step.definition.effect;
        let prepared = self
            .agent
            .tools
            .prepare(&call, &self.agent.active_policy(&state.report))?;
        let request = step.request(&state.report.run_id, step.attempts + 1)?;
        let request_bytes = serde_json::to_vec(&request)
            .map_err(|e| corrupt(&e.to_string()))?
            .len();
        // A result is stored twice (the step's output and the tool message).
        // The request is stored in the step and its approval or dispatch
        // record, and a cancel while awaiting approval copies it once more
        // into the rejected-approval record.
        self.check_capacity(
            state,
            lease,
            self.agent
                .limits
                .max_tool_result_bytes
                .saturating_mul(2)
                .saturating_add(request_bytes.saturating_mul(3)),
        )?;
        if self.approvals.requires(&step.definition)
            && !step
                .approvals
                .last()
                .is_some_and(|record| record.decision.approved && record.request == request)
        {
            self.event(
                state,
                clock,
                RunEventKind::ApprovalRequested {
                    request_id: request.request_id.clone(),
                    call_id: call.id,
                },
            );
            state.tool_steps[index].progress = ToolProgress::AwaitingApproval { request };
            return self
                .stop(
                    state,
                    lease,
                    budget,
                    clock,
                    RunStatus::WaitingApproval,
                    StopReason::ApprovalRequired,
                    None,
                )
                .await;
        }
        state.tool_steps[index].attempts += 1;
        state.tool_steps[index].progress = ToolProgress::InFlight {
            started_unix_ms: unix_ms()?,
            elapsed_before_ms: clock.elapsed_ms(),
        };
        state.report.tool_calls += 1;
        self.event(
            state,
            clock,
            RunEventKind::ToolStarted {
                call_id: call.id.clone(),
                name: call.name.clone(),
            },
        );
        self.save(state, lease, budget, clock).await?;
        lease.verify_ownership().await?;
        let result = controlled(
            self.agent
                .execute_tool(prepared, call.clone(), state.report.run_id.clone()),
            clock,
            self.agent.limits.max_duration,
            cancel,
        )
        .await;
        match result {
            Controlled::Done(result) if !result.is_error || effect == ToolEffect::ReadOnly => {
                let is_error = result.is_error;
                complete_tool(state, index, result)?;
                self.event(
                    state,
                    clock,
                    RunEventKind::ToolFinished {
                        call_id: call.id,
                        is_error,
                    },
                );
                self.save(state, lease, budget, clock).await
            }
            other => {
                let (status, reason, reported_result) = match other {
                    Controlled::Done(result) => (
                        RunStatus::NeedsRecovery,
                        StopReason::Uncertain,
                        Some(result),
                    ),
                    Controlled::Cancelled => (RunStatus::Finished, StopReason::Cancelled, None),
                    Controlled::Deadline => (RunStatus::Finished, StopReason::Deadline, None),
                };
                state.tool_steps[index].progress = ToolProgress::Uncertain {
                    detail: "tool execution did not yield a durable usable result".into(),
                    reported_result,
                };
                self.event(
                    state,
                    clock,
                    RunEventKind::RecoveryRequired { call_id: call.id },
                );
                self.stop(
                    state,
                    lease,
                    budget,
                    clock,
                    status,
                    reason,
                    Some(
                        "external effects may have occurred; inspect and reconcile the tool".into(),
                    ),
                )
                .await
            }
        }
    }

    fn event(&self, state: &mut RunState, clock: &Clock, event: RunEventKind) {
        self.agent
            .emit_at(&mut state.report, clock.elapsed_ms(), event);
    }

    async fn save(
        &self,
        state: &mut RunState,
        lease: &Arc<dyn RunLease>,
        budget: &RunBudget,
        clock: &Clock,
    ) -> Result<(), RuntimeError> {
        state.report.elapsed_ms = clock.elapsed_ms();
        state.report.budget = budget.snapshot();
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| corrupt("checkpoint revision overflow"))?;
        state.validate()?;
        lease.save(state).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn stop(
        &self,
        state: &mut RunState,
        lease: &Arc<dyn RunLease>,
        budget: &RunBudget,
        clock: &Clock,
        status: RunStatus,
        reason: StopReason,
        detail: Option<String>,
    ) -> Result<(), RuntimeError> {
        let detail = match self.cancel_reason.and_then(CancelReason::get) {
            Some((actor, note)) if reason == StopReason::Cancelled => {
                let by = format!("cancelled by {actor}: {note}");
                Some(detail.map_or(by.clone(), |detail| format!("{by}; {detail}")))
            }
            _ => detail,
        };
        state.status = status;
        state.report.stop_reason = reason;
        state.report.detail = detail;
        self.event(state, clock, RunEventKind::Stopped { reason });
        self.save(state, lease, budget, clock).await
    }
}

struct LeaseJournal(Arc<dyn RunLease>);
impl BudgetJournal for LeaseJournal {
    fn requires_cost_ceiling(&self) -> bool {
        self.0.requires_cost_ceiling()
    }
    fn persist<'a>(
        &'a self,
        snapshot: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        self.0.save_budget(snapshot)
    }
}

fn complete_tool(
    state: &mut RunState,
    index: usize,
    result: ToolResult,
) -> Result<(), RuntimeError> {
    state.tool_steps[index].progress = ToolProgress::Complete {
        result: result.clone(),
    };
    state.report.messages.push(Message::Tool { result });
    let RunCursor::Tools {
        first,
        count,
        next,
        assistant_message,
    } = state.cursor
    else {
        return Err(corrupt("completed tool has no batch"));
    };
    state.cursor = if next + 1 == count {
        RunCursor::ModelReady
    } else {
        RunCursor::Tools {
            first,
            count,
            next: next + 1,
            assistant_message,
        }
    };
    Ok(())
}

struct Clock {
    base_ms: u64,
    started: Instant,
}
impl Clock {
    fn new(base_ms: u64) -> Self {
        Self {
            base_ms,
            started: Instant::now(),
        }
    }
    fn elapsed_ms(&self) -> u64 {
        let nanos = self.started.elapsed().as_nanos();
        self.base_ms
            .saturating_add(nanos.div_ceil(1_000_000).try_into().unwrap_or(u64::MAX))
    }
    fn remaining(&self, limit: Duration) -> Duration {
        limit.saturating_sub(
            Duration::from_millis(self.base_ms).saturating_add(self.started.elapsed()),
        )
    }
}

enum Controlled<T> {
    Done(T),
    Cancelled,
    Deadline,
}
async fn controlled<F: Future>(
    future: F,
    clock: &Clock,
    limit: Duration,
    cancel: &CancelToken,
) -> Controlled<F::Output> {
    let remaining = clock.remaining(limit);
    if remaining.is_zero() {
        return Controlled::Deadline;
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => Controlled::Cancelled,
        () = tokio::time::sleep(remaining) => Controlled::Deadline,
        result = Box::pin(future) => Controlled::Done(result),
    }
}

fn charge_interrupted_time(
    state: &mut RunState,
    started_unix_ms: u64,
    elapsed_before_ms: u64,
) -> Result<(), RuntimeError> {
    let gap = unix_ms()?
        .checked_sub(started_unix_ms)
        .ok_or_else(|| corrupt("clock moved backwards during interrupted execution"))?;
    state.report.elapsed_ms = state
        .report
        .elapsed_ms
        .max(elapsed_before_ms.saturating_add(gap));
    Ok(())
}

fn unix_ms() -> Result<u64, RuntimeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| corrupt(&e.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| corrupt("wall clock exceeds the persistent timestamp range"))
}

/// Longest accepted decision actor and note, in bytes.
const MAX_ACTOR_BYTES: usize = 256;
const MAX_NOTE_BYTES: usize = 4096;

/// Headroom every admission leaves for an operator's terminal record, so a
/// run admitted to work can always still be cancelled, aborted or stopped
/// under the store's size cap. A cancel stores the actor and note twice (the
/// rejected-approval record and the stop detail); `validate_actor` refuses
/// control characters, so JSON escaping at most doubles them. The rest covers
/// the status, cursor, events and timestamps. A copy of a pending approval
/// request is reserved by the tool admission itself.
const TERMINAL_RECORD_RESERVE: u64 = (2 * 2 * (MAX_ACTOR_BYTES + MAX_NOTE_BYTES) + 4096) as u64;

fn validate_actor(actor: &str, note: &str) -> Result<(), RuntimeError> {
    let control = |text: &str| {
        text.chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    };
    if actor.trim().is_empty()
        || actor.len() > MAX_ACTOR_BYTES
        || note.len() > MAX_NOTE_BYTES
        || control(actor)
        || control(note)
    {
        return Err(RuntimeError::Configuration("decisions require a nonempty actor (at most 256 bytes) and a note of at most 4096 bytes, without control characters other than newline and tab".into()));
    }
    Ok(())
}

fn error_reason(error: &RuntimeError) -> StopReason {
    match error {
        RuntimeError::Model(TaskError::Cancelled) => StopReason::Cancelled,
        RuntimeError::Model(TaskError::Deadline(_)) => StopReason::Deadline,
        RuntimeError::Model(_) => StopReason::ModelError,
        RuntimeError::Budget(_) => StopReason::Budget,
        RuntimeError::Protocol(_) => StopReason::ProtocolError,
        RuntimeError::Configuration(_) => StopReason::ConfigurationError,
        RuntimeError::Tool(_) => StopReason::ToolError,
        RuntimeError::Permission(_) => StopReason::PermissionDenied,
        RuntimeError::Storage(_) | RuntimeError::Conflict(_) => StopReason::StorageError,
        RuntimeError::Context(_) => StopReason::ContextLimit,
        RuntimeError::StorageLimit(_) => StopReason::StorageLimit,
    }
}

fn corrupt(detail: &str) -> RuntimeError {
    RuntimeError::Storage(detail.into())
}
