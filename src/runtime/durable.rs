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
pub struct DurableAgent<'a, 'model> {
    agent: &'a AgentLoop<'model>,
    store: &'a dyn RunStore,
    approvals: ApprovalPolicy,
    pause: Option<&'a CancelToken>,
}

impl<'a, 'model> DurableAgent<'a, 'model> {
    pub fn new(agent: &'a AgentLoop<'model>, store: &'a dyn RunStore) -> Self {
        Self {
            agent,
            store,
            approvals: ApprovalPolicy::default(),
            pause: None,
        }
    }

    pub fn with_approvals(mut self, approvals: ApprovalPolicy) -> Self {
        self.approvals = approvals;
        self
    }

    /// Graceful pause: finish/checkpoint the current operation, then stop
    /// before the next dispatch. Cancellation still interrupts immediately.
    pub fn with_pause(mut self, token: &'a CancelToken) -> Self {
        self.pause = Some(token);
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
        let (mut state, budget) = self.load_owned(&lease, run_id).await?;
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
        let (mut state, budget) = self.load_owned(&lease, run_id).await?;
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
        let (mut state, budget) = self.load_owned(&lease, run_id).await?;
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
            RecoveryDecision::Abort { .. } => {}
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

    fn config(&self) -> RunConfig {
        RunConfig {
            model: self.agent.model_id.clone(),
            label: self.agent.label.clone(),
            temperature: self.agent.temperature,
            max_output_tokens: self.agent.max_output_tokens,
            limits: self.agent.limits.clone(),
            policy: self.agent.policy.clone(),
            approvals: self.approvals.clone(),
            tools: self.agent.tools.definitions(&self.agent.policy),
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
            config: self.config(),
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
            },
        };
        self.agent.validate(&state.report)?;
        validate_history(&state.report.messages)?;
        self.event(&mut state, &clock, RunEventKind::Started);
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
    ) -> Result<(RunState, RunBudget), RuntimeError> {
        let mut state = lease
            .load()
            .await?
            .ok_or_else(|| RuntimeError::Conflict("run does not exist".into()))?;
        state.validate()?;
        if state.report.run_id != run_id {
            return Err(corrupt("store returned a checkpoint for a different run"));
        }
        self.agent.validate(&state.report)?;
        if state.config != self.config() {
            return Err(RuntimeError::Configuration(
                "resume configuration or tool definitions differ from the saved run".into(),
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
        state.report.steps += 1;
        state.cursor = RunCursor::ModelInFlight {
            started_unix_ms: unix_ms()?,
            elapsed_before_ms: clock.elapsed_ms(),
        };
        self.event(
            state,
            clock,
            RunEventKind::ModelStarted {
                step: state.report.steps,
                model: self.agent.model_id.clone(),
            },
        );
        self.save(state, lease, budget, clock).await?;
        let request = ModelRequest {
            label: self.agent.label.clone(),
            model: self.agent.model_id.clone(),
            messages: state.report.messages.clone(),
            tools: state.config.tools.clone(),
            temperature: self.agent.temperature,
            max_output_tokens: self.agent.max_output_tokens,
        };
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
            if let Err(error) = self.agent.tools.prepare(&call, &self.agent.policy) {
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
        let prepared = self.agent.tools.prepare(&call, &self.agent.policy)?;
        let request = step.request(&state.report.run_id, step.attempts + 1)?;
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
        state.status = status;
        state.report.stop_reason = reason;
        state.report.detail = detail;
        self.event(state, clock, RunEventKind::Stopped { reason });
        self.save(state, lease, budget, clock).await
    }
}

struct LeaseJournal(Arc<dyn RunLease>);
impl BudgetJournal for LeaseJournal {
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

fn validate_actor(actor: &str, note: &str) -> Result<(), RuntimeError> {
    if actor.trim().is_empty() || actor.len() > 256 || note.len() > 4096 {
        return Err(RuntimeError::Configuration("decisions require a nonempty actor (at most 256 bytes) and a note of at most 4096 bytes".into()));
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
    }
}

fn corrupt(detail: &str) -> RuntimeError {
    RuntimeError::Storage(detail.into())
}
