use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use serde_json::Value;
use tokio::time::Instant;

use super::{
    graph::config, state::invalid, NodeOutcome, NodeOutcomeStatus, NodeRecovery,
    NodeRecoveryRecord, NodeState, NodeStatus, TaskDefinition, TaskRecovery, WorkflowChild,
    WorkflowDefinition, WorkflowEvent, WorkflowEventKind, WorkflowLease, WorkflowLimits,
    WorkflowState, WorkflowStatus, WorkflowStore, WorkflowTasks, WORKFLOW_STATE_VERSION,
};
use crate::runtime::{RunStatus, RuntimeError};
use crate::CancelToken;

type TaskResult = (usize, bool, Result<NodeOutcome, RuntimeError>);
type Pending<'a> = FuturesUnordered<BoxFuture<'a, TaskResult>>;
pub type WorkflowEventSink<'a> = dyn Fn(&WorkflowEvent) + Send + Sync + 'a;

pub struct WorkflowRunner<'a, 'tasks> {
    tasks: &'a WorkflowTasks<'tasks>,
    store: &'a dyn WorkflowStore,
    limits: WorkflowLimits,
    cancel: Option<&'a CancelToken>,
    pause: Option<&'a CancelToken>,
    events: Option<&'a WorkflowEventSink<'a>>,
}

impl<'a, 'tasks> WorkflowRunner<'a, 'tasks> {
    pub fn new(tasks: &'a WorkflowTasks<'tasks>, store: &'a dyn WorkflowStore) -> Self {
        Self {
            tasks,
            store,
            limits: WorkflowLimits::default(),
            cancel: None,
            pause: None,
            events: None,
        }
    }
    pub fn with_limits(mut self, limits: WorkflowLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn with_cancel(mut self, cancel: &'a CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
    pub fn with_pause(mut self, pause: &'a CancelToken) -> Self {
        self.pause = Some(pause);
        self
    }
    pub fn with_events(mut self, events: &'a WorkflowEventSink<'a>) -> Self {
        self.events = Some(events);
        self
    }

    /// Compile the exact initial plan without acquiring a lease, saving state,
    /// emitting an event, reserving spending or invoking a task. This does not
    /// reserve an ID or prove live service/storage availability.
    pub fn preview(
        &self,
        id: &str,
        definition: WorkflowDefinition,
        input: Value,
    ) -> Result<WorkflowState, RuntimeError> {
        let state = self.initial(id, definition, input)?;
        state.validate()?;
        Ok(state)
    }

    pub async fn start(
        &self,
        id: &str,
        definition: WorkflowDefinition,
        input: Value,
    ) -> Result<WorkflowState, RuntimeError> {
        let (state, lease) = self.create_leased(id, definition, input).await?;
        self.execute(state, lease).await
    }

    /// Persist a validated workflow without dispatching any tasks. Hosts can
    /// acknowledge creation before admitting an asynchronous worker.
    pub async fn create(
        &self,
        id: &str,
        definition: WorkflowDefinition,
        input: Value,
    ) -> Result<WorkflowState, RuntimeError> {
        Ok(self.create_leased(id, definition, input).await?.0)
    }

    async fn create_leased(
        &self,
        id: &str,
        definition: WorkflowDefinition,
        input: Value,
    ) -> Result<(WorkflowState, Arc<dyn WorkflowLease>), RuntimeError> {
        let lease = self.store.acquire(id).await?;
        if lease.load().await?.is_some() {
            return Err(RuntimeError::Conflict("workflow id already exists".into()));
        }
        let mut state = self.initial(id, definition, input)?;
        let clock = Clock::new(0);
        self.event(&mut state, &clock, WorkflowEventKind::Started);
        self.save(&mut state, &lease, &clock).await?;
        Ok((state, lease))
    }

    pub async fn resume(&self, id: &str) -> Result<WorkflowState, RuntimeError> {
        let lease = self.store.acquire(id).await?;
        let state = self.load(&lease, id).await?;
        if state.status.is_terminal()
            || state
                .nodes
                .iter()
                .any(|node| node.status == NodeStatus::Uncertain)
        {
            return Ok(state);
        }
        self.execute(state, lease).await
    }

    /// A stored `Running` status means the previous owner died: its lease is
    /// gone but its invocation never stopped. Charge the wall-clock gap and
    /// classify every node it left running before any other transition.
    async fn recover_crash(
        &self,
        state: &mut WorkflowState,
        lease: &Arc<dyn WorkflowLease>,
    ) -> Result<(), RuntimeError> {
        if state.status != WorkflowStatus::Running {
            return Ok(());
        }
        let started = state
            .invocation_started_unix_ms
            .ok_or_else(|| invalid("running workflow has no wall clock"))?;
        let gap = unix_ms()?
            .checked_sub(started)
            .ok_or_else(|| invalid("wall clock moved backwards during workflow interruption"))?;
        state.elapsed_ms = state
            .elapsed_ms
            .max(state.invocation_base_elapsed_ms.saturating_add(gap));
        let clock = Clock::new(state.elapsed_ms);
        for index in 0..state.nodes.len() {
            if state.nodes[index].status != NodeStatus::Running {
                continue;
            }
            let task = self.task_definition(state, index)?;
            if task.recovery == TaskRecovery::Manual {
                mark_unknown(
                    &mut state.nodes[index],
                    &task,
                    "worker stopped during a task with unverified effects",
                );
                let node = state.nodes[index].id.clone();
                self.event(state, &clock, WorkflowEventKind::NodeUncertain { node });
            } else {
                state.nodes[index].status = NodeStatus::Pending;
                let node = state.nodes[index].id.clone();
                self.event(state, &clock, WorkflowEventKind::Recovered { node });
            }
        }
        state.status = if state
            .nodes
            .iter()
            .any(|node| node.status == NodeStatus::Uncertain)
        {
            WorkflowStatus::NeedsRecovery
        } else {
            WorkflowStatus::Paused
        };
        state.invocation_started_unix_ms = None;
        self.save(state, lease, &clock).await
    }

    pub async fn reconcile(
        &self,
        id: &str,
        node_id: &str,
        decision: NodeRecovery,
    ) -> Result<WorkflowState, RuntimeError> {
        if decision.actor().trim().is_empty()
            || decision.actor().len() > 256
            || decision.note().len() > 4096
        {
            return Err(config("recovery needs a bounded actor and note"));
        }
        let lease = self.store.acquire(id).await?;
        let mut state = self.load(&lease, id).await?;
        let index = state
            .nodes
            .iter()
            .position(|node| node.id == node_id)
            .ok_or_else(|| config("workflow node does not exist"))?;
        if state.nodes[index].status != NodeStatus::Uncertain {
            return Err(RuntimeError::Conflict(
                "workflow node is not awaiting reconciliation".into(),
            ));
        }
        let task = self.task_definition(&state, index)?;
        if task.recovery == TaskRecovery::DurableChild {
            return Err(RuntimeError::Permission(
                "reconcile the durable child's tool state through its own controller".into(),
            ));
        }
        let terminal = state.status.is_terminal();
        match &decision {
            NodeRecovery::RecordOutput {
                output,
                cost_micros,
                usage,
                ..
            } => {
                if serde_json::to_vec(output)
                    .map_err(|e| config(&e.to_string()))?
                    .len()
                    > state.limits.max_output_bytes
                {
                    return Err(config("reconciled workflow output exceeds byte limit"));
                }
                if task.recovery == TaskRecovery::Pure
                    && (*cost_micros != Some(0) || *usage != crate::ledger::Usage::default())
                {
                    return Err(config("pure task recovery cannot report paid work"));
                }
                set_spend(&mut state.nodes[index], &task, *cost_micros, 0);
                state.nodes[index].usage = *usage;
                state.nodes[index].output = Some(output.clone());
                state.nodes[index].status = NodeStatus::Complete;
                state.nodes[index].needs_recovery = false;
                state.nodes[index].allocated = false;
                if task
                    .max_cost_micros
                    .is_some_and(|cap| state.nodes[index].cost_micros > cap)
                {
                    state.nodes[index].status = NodeStatus::Failed;
                    state.nodes[index].output = None;
                    state.nodes[index].detail =
                        Some("reconciled cost exceeded the declared ceiling".into());
                }
            }
            NodeRecovery::RetryPure { .. } => {
                if terminal || task.recovery != TaskRecovery::Pure {
                    return Err(RuntimeError::Permission(
                        "only pure tasks in nonterminal workflows can be retried".into(),
                    ));
                }
                state.nodes[index].status = NodeStatus::Pending;
                state.nodes[index].allocated = false;
                state.nodes[index].needs_recovery = false;
            }
            NodeRecovery::Abort { .. } => {
                state.nodes[index].status = NodeStatus::Failed;
                state.nodes[index].needs_recovery = false;
            }
        }
        let actor = decision.actor().to_string();
        state.nodes[index].recoveries.push(NodeRecoveryRecord {
            decision,
            decided_unix_ms: unix_ms()?,
        });
        let clock = Clock::new(state.elapsed_ms);
        self.event(
            &mut state,
            &clock,
            WorkflowEventKind::NodeReconciled {
                node: node_id.into(),
                actor,
            },
        );
        let status = if terminal {
            state.status
        } else if state
            .nodes
            .iter()
            .any(|node| node.status == NodeStatus::Failed)
        {
            WorkflowStatus::Failed
        } else if state
            .nodes
            .iter()
            .any(|node| node.status == NodeStatus::Uncertain)
        {
            WorkflowStatus::NeedsRecovery
        } else {
            WorkflowStatus::Paused
        };
        self.stop(&mut state, &lease, &clock, status, None).await?;
        Ok(state)
    }

    fn initial(
        &self,
        id: &str,
        definition: WorkflowDefinition,
        input: Value,
    ) -> Result<WorkflowState, RuntimeError> {
        if id.trim().is_empty() {
            return Err(config("workflow id must be nonempty"));
        }
        self.limits.validate()?;
        definition.validate(&self.limits)?;
        let tasks = self.definitions(&definition)?;
        if self.limits.max_cost_micros.is_some()
            && tasks.iter().any(|task| task.max_cost_micros.is_none())
        {
            return Err(config(
                "workflow spending limits require every task to declare a paid-cost ceiling",
            ));
        }
        let nodes = definition
            .nodes
            .iter()
            .map(|node| NodeState::new(node.id.clone()))
            .collect();
        Ok(WorkflowState {
            version: WORKFLOW_STATE_VERSION,
            revision: 0,
            id: id.into(),
            definition,
            tasks,
            limits: self.limits.clone(),
            input,
            status: WorkflowStatus::Ready,
            nodes,
            dispatches: 0,
            elapsed_ms: 0,
            invocation_started_unix_ms: None,
            invocation_base_elapsed_ms: 0,
            events: Vec::new(),
            detail: None,
        })
    }

    fn definitions(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<Vec<TaskDefinition>, RuntimeError> {
        definition
            .nodes
            .iter()
            .map(|node| node.task.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|name| {
                self.tasks
                    .tasks
                    .get(name)
                    .map(|task| task.definition.clone())
                    .ok_or_else(|| config(&format!("unregistered workflow task: {name}")))
            })
            .collect()
    }
    fn task_definition(
        &self,
        state: &WorkflowState,
        index: usize,
    ) -> Result<TaskDefinition, RuntimeError> {
        state
            .tasks
            .iter()
            .find(|task| task.name == state.definition.nodes[index].task)
            .cloned()
            .ok_or_else(|| invalid("missing task definition"))
    }
    async fn load(
        &self,
        lease: &Arc<dyn WorkflowLease>,
        id: &str,
    ) -> Result<WorkflowState, RuntimeError> {
        let mut state = lease
            .load()
            .await?
            .ok_or_else(|| RuntimeError::Conflict("workflow does not exist".into()))?;
        state.validate()?;
        if state.id != id
            || state.limits != self.limits
            || state.tasks != self.definitions(&state.definition)?
        {
            return Err(config(
                "workflow identity, limits or registered task configuration changed",
            ));
        }
        self.recover_crash(&mut state, lease).await?;
        Ok(state)
    }

    async fn execute(
        &self,
        mut state: WorkflowState,
        lease: Arc<dyn WorkflowLease>,
    ) -> Result<WorkflowState, RuntimeError> {
        let clock = Clock::new(state.elapsed_ms);
        let prior = state.status;
        state.status = WorkflowStatus::Running;
        state.invocation_base_elapsed_ms = state.elapsed_ms;
        state.invocation_started_unix_ms = Some(unix_ms()?);
        state.detail = None;
        // Waiting children are probed once per explicit resume; a still-waiting
        // result is not dispatched repeatedly in this invocation.
        for node in &mut state.nodes {
            if node.status == NodeStatus::Waiting {
                node.status = NodeStatus::Pending;
            }
        }
        self.event(&mut state, &clock, WorkflowEventKind::Resumed);
        self.save(&mut state, &lease, &clock).await?;
        let external = self
            .cancel
            .cloned()
            .or_else(crate::cancel::current)
            .unwrap_or_default();
        let workers = CancelToken::new();
        let mut pending: Pending<'_> = FuturesUnordered::new();
        loop {
            tokio::task::yield_now().await;
            let interrupt = if external.is_cancelled() {
                Some(WorkflowStatus::Cancelled)
            } else if clock.remaining(state.limits.max_duration).is_zero() {
                Some(WorkflowStatus::Deadline)
            } else if state
                .nodes
                .iter()
                .any(|node| node.status == NodeStatus::Failed)
            {
                Some(WorkflowStatus::Failed)
            } else {
                None
            };
            if let Some(status) = interrupt {
                workers.cancel();
                self.drain(&mut state, &lease, &clock, &mut pending).await?;
                self.finish(&mut state, &lease, &clock, status, prior)
                    .await?;
                return Ok(state);
            }
            let pausing = self.pause.is_some_and(CancelToken::is_cancelled);
            let recovery_boundary = state.nodes.iter().any(|node| {
                node.status == NodeStatus::Uncertain
                    || node.status == NodeStatus::Waiting && node.needs_recovery
            });
            let mut changed = false;
            let mut blocked_by_budget = false;
            let mut blocked_by_dispatch = false;
            if !pausing && !recovery_boundary {
                for index in 0..state.nodes.len() {
                    if external.is_cancelled()
                        || clock.remaining(state.limits.max_duration).is_zero()
                        || self.pause.is_some_and(CancelToken::is_cancelled)
                    {
                        break;
                    }
                    if state.nodes[index].status != NodeStatus::Pending {
                        continue;
                    }
                    let spec = state.definition.nodes[index].clone();
                    if !spec.depends_on.iter().all(|id| {
                        state.node(id).is_some_and(|node| {
                            matches!(node.status, NodeStatus::Complete | NodeStatus::Skipped)
                        })
                    }) {
                        continue;
                    }
                    let outputs = spec
                        .depends_on
                        .iter()
                        .filter_map(|id| {
                            state.output(id).map(|output| (id.clone(), output.clone()))
                        })
                        .collect::<BTreeMap<_, _>>();
                    if !spec.allow_skipped_dependencies
                        && spec.depends_on.iter().any(|id| {
                            state
                                .node(id)
                                .is_some_and(|node| node.status == NodeStatus::Skipped)
                        })
                        || !spec.condition.evaluate(&outputs)
                    {
                        state.nodes[index].status = NodeStatus::Skipped;
                        self.event(
                            &mut state,
                            &clock,
                            WorkflowEventKind::NodeSkipped { node: spec.id },
                        );
                        self.save(&mut state, &lease, &clock).await?;
                        changed = true;
                        continue;
                    }
                    if pending.len() >= state.limits.max_parallel {
                        break;
                    }
                    if state.dispatches >= state.limits.max_dispatches {
                        blocked_by_dispatch = true;
                        continue;
                    }
                    let definition = self.task_definition(&state, index)?;
                    if !state.nodes[index].allocated {
                        let budget = state.budget()?;
                        let required = budget
                            .committed_micros
                            .checked_add(budget.reserved_micros)
                            .and_then(|n| {
                                n.checked_add(
                                    definition
                                        .max_cost_micros
                                        .unwrap_or(0)
                                        .saturating_sub(state.nodes[index].cost_micros),
                                )
                            });
                        if required.is_none()
                            || state
                                .limits
                                .max_cost_micros
                                .is_some_and(|cap| required.unwrap() > cap)
                        {
                            blocked_by_budget = true;
                            continue;
                        }
                    }
                    let context = match state.context_for(&spec.id) {
                        Ok(context) => context,
                        Err(error) => {
                            state.nodes[index].status = NodeStatus::Failed;
                            state.nodes[index].detail = Some(error.to_string());
                            self.event(
                                &mut state,
                                &clock,
                                WorkflowEventKind::NodeFailed { node: spec.id },
                            );
                            self.save(&mut state, &lease, &clock).await?;
                            break;
                        }
                    };
                    state.nodes[index].context = Some(context.clone());
                    state.nodes[index].status = NodeStatus::Running;
                    state.nodes[index].allocated = true;
                    state.nodes[index].needs_recovery = false;
                    state.nodes[index].dispatches += 1;
                    state.dispatches += 1;
                    let dispatch = state.nodes[index].dispatches;
                    self.event(
                        &mut state,
                        &clock,
                        WorkflowEventKind::NodeStarted {
                            node: spec.id,
                            dispatch,
                        },
                    );
                    self.save(&mut state, &lease, &clock).await?;
                    let handler = &self
                        .tasks
                        .tasks
                        .get(&spec.task)
                        .ok_or_else(|| config("handler disappeared"))?
                        .task;
                    let token = workers.clone();
                    let owner = Arc::clone(&lease);
                    let deadline = Instant::now() + clock.remaining(state.limits.max_duration);
                    pending.push(Box::pin(async move {
                        if token.is_cancelled() || Instant::now() >= deadline {
                            (
                                index,
                                false,
                                Err(RuntimeError::Model(crate::TaskError::Cancelled)),
                            )
                        } else if let Err(error) = owner.verify_ownership().await {
                            (index, false, Err(error))
                        } else if token.is_cancelled() || Instant::now() >= deadline {
                            (
                                index,
                                false,
                                Err(RuntimeError::Model(crate::TaskError::Cancelled)),
                            )
                        } else {
                            (index, true, handler.run(context, &token).await)
                        }
                    }));
                    changed = true;
                }
            }
            if state
                .nodes
                .iter()
                .any(|node| node.status == NodeStatus::Failed)
            {
                continue;
            }
            if pending.is_empty() {
                if state
                    .nodes
                    .iter()
                    .all(|node| matches!(node.status, NodeStatus::Complete | NodeStatus::Skipped))
                {
                    self.finish(&mut state, &lease, &clock, WorkflowStatus::Completed, prior)
                        .await?;
                    return Ok(state);
                }
                if pausing {
                    self.finish(&mut state, &lease, &clock, WorkflowStatus::Paused, prior)
                        .await?;
                    return Ok(state);
                }
                if changed {
                    continue;
                }
                let status = if recovery_boundary {
                    WorkflowStatus::NeedsRecovery
                } else if state
                    .nodes
                    .iter()
                    .any(|node| node.status == NodeStatus::Waiting)
                {
                    WorkflowStatus::Waiting
                } else if blocked_by_dispatch {
                    WorkflowStatus::DispatchLimit
                } else if blocked_by_budget {
                    WorkflowStatus::BudgetLimit
                } else {
                    return Err(invalid(
                        "workflow cannot make progress despite an acyclic definition",
                    ));
                };
                self.finish(&mut state, &lease, &clock, status, prior)
                    .await?;
                return Ok(state);
            }
            let remaining = clock.remaining(state.limits.max_duration);
            let result = tokio::select! {
                biased;
                ()=external.cancelled()=>None,
                ()=tokio::time::sleep(remaining)=>None,
                result=pending.next()=>result,
            };
            if let Some((index, started, result)) = result {
                self.apply(&mut state, index, started, result, &clock)?;
                self.save(&mut state, &lease, &clock).await?;
            }
        }
    }

    async fn drain(
        &self,
        state: &mut WorkflowState,
        lease: &Arc<dyn WorkflowLease>,
        clock: &Clock,
        pending: &mut Pending<'_>,
    ) -> Result<(), RuntimeError> {
        let until = Instant::now() + state.limits.shutdown_grace;
        while !pending.is_empty() {
            let next = tokio::time::timeout_at(until, pending.next()).await;
            match next {
                Ok(Some((index, started, result))) => {
                    self.apply(state, index, started, result, clock)?;
                    self.save(state, lease, clock).await?;
                }
                _ => break,
            }
        }
        pending.clear();
        for index in 0..state.nodes.len() {
            if state.nodes[index].status == NodeStatus::Running {
                let definition = self.task_definition(state, index)?;
                let node = state.nodes[index].id.clone();
                if definition.recovery == TaskRecovery::DurableChild {
                    // The child owns effect reconciliation. Keep its allocation
                    // until its durable journal can be observed on reentry.
                    let entry = &mut state.nodes[index];
                    entry.status = NodeStatus::Waiting;
                    entry.needs_recovery = true;
                    entry.child = Some(WorkflowChild {
                        run_id: entry.context.as_ref().unwrap().run_id.clone(),
                        status: None,
                        stop_reason: None,
                    });
                    entry.detail = Some("child did not finish checkpointing during shutdown grace; inspect its durable state".into());
                    self.event(
                        state,
                        clock,
                        WorkflowEventKind::NodeWaiting {
                            node,
                            needs_recovery: true,
                        },
                    );
                } else {
                    mark_unknown(
                        &mut state.nodes[index],
                        &definition,
                        "task did not finish checkpointing during shutdown grace",
                    );
                    self.event(state, clock, WorkflowEventKind::NodeUncertain { node });
                }
            }
        }
        Ok(())
    }

    fn apply(
        &self,
        state: &mut WorkflowState,
        index: usize,
        started: bool,
        result: Result<NodeOutcome, RuntimeError>,
        clock: &Clock,
    ) -> Result<(), RuntimeError> {
        let definition = self.task_definition(state, index)?;
        let id = state.nodes[index].id.clone();
        if !started {
            let node = &mut state.nodes[index];
            node.detail = Some("workflow stopped before handler dispatch".into());
            let kind = if definition.recovery == TaskRecovery::DurableChild && node.dispatches > 1 {
                node.status = NodeStatus::Waiting;
                node.child.get_or_insert_with(|| WorkflowChild {
                    run_id: node.context.as_ref().unwrap().run_id.clone(),
                    status: None,
                    stop_reason: None,
                });
                node.allocated = !node
                    .child
                    .as_ref()
                    .is_some_and(|child| child.status == Some(RunStatus::Finished));
                node.needs_recovery = true;
                WorkflowEventKind::NodeWaiting {
                    node: id,
                    needs_recovery: true,
                }
            } else {
                node.status = NodeStatus::Failed;
                node.allocated = false;
                node.needs_recovery = false;
                WorkflowEventKind::NodeFailed { node: id }
            };
            self.event(state, clock, kind);
            return Ok(());
        }
        let kind = match result {
            Err(error) if definition.recovery == TaskRecovery::DurableChild => {
                let busy = matches!(error, RuntimeError::Conflict(_));
                let node = &mut state.nodes[index];
                node.status = NodeStatus::Waiting;
                node.detail = Some(error.to_string());
                node.needs_recovery = !busy;
                node.child = Some(WorkflowChild {
                    run_id: node.context.as_ref().unwrap().run_id.clone(),
                    status: None,
                    stop_reason: None,
                });
                WorkflowEventKind::NodeWaiting {
                    node: id,
                    needs_recovery: !busy,
                }
            }
            Err(error) if definition.recovery == TaskRecovery::Manual => {
                mark_unknown(&mut state.nodes[index], &definition, &error.to_string());
                WorkflowEventKind::NodeUncertain { node: id }
            }
            Err(error) => {
                let node = &mut state.nodes[index];
                node.status = NodeStatus::Failed;
                node.allocated = false;
                node.detail = Some(error.to_string());
                WorkflowEventKind::NodeFailed { node: id }
            }
            Ok(outcome) => {
                let invalid_wait = matches!(
                    outcome.status,
                    NodeOutcomeStatus::Waiting | NodeOutcomeStatus::NeedsRecovery
                ) && definition.recovery != TaskRecovery::DurableChild;
                let invalid_child = outcome.child.as_ref().is_some_and(|child| {
                    definition.recovery != TaskRecovery::DurableChild
                        || child.run_id != state.nodes[index].context.as_ref().unwrap().run_id
                });
                let missing_child =
                    definition.recovery == TaskRecovery::DurableChild && outcome.child.is_none();
                let invalid_pure = definition.recovery == TaskRecovery::Pure
                    && (outcome.cost_micros != Some(0)
                        || outcome.uncertain_micros != 0
                        || outcome.usage != crate::ledger::Usage::default());
                if invalid_wait || invalid_child || missing_child || invalid_pure {
                    mark_unknown(
                        &mut state.nodes[index],
                        &definition,
                        "task violated its recovery or spend declaration",
                    );
                    if definition.recovery == TaskRecovery::Pure {
                        state.nodes[index].status = NodeStatus::Failed;
                        state.nodes[index].needs_recovery = false;
                    }
                    self.event(state, clock, WorkflowEventKind::NodeFailed { node: id });
                    return Ok(());
                }
                let node = &mut state.nodes[index];
                node.needs_recovery = outcome.status == NodeOutcomeStatus::NeedsRecovery;
                set_spend(
                    node,
                    &definition,
                    outcome.cost_micros,
                    outcome.uncertain_micros,
                );
                node.usage = outcome.usage;
                node.child = outcome.child;
                node.detail = outcome.detail;
                let waiting = matches!(
                    outcome.status,
                    NodeOutcomeStatus::Waiting | NodeOutcomeStatus::NeedsRecovery
                );
                node.allocated = waiting
                    && !node
                        .child
                        .as_ref()
                        .is_some_and(|child| child.status == Some(RunStatus::Finished));
                let overspent = definition
                    .max_cost_micros
                    .is_some_and(|cap| node.cost_micros > cap);
                let output_invalid = outcome.status == NodeOutcomeStatus::Complete
                    && outcome.output.as_ref().is_none_or(|value| {
                        serde_json::to_vec(value)
                            .map_or(true, |bytes| bytes.len() > state.limits.max_output_bytes)
                    });
                if overspent || output_invalid {
                    node.status = NodeStatus::Failed;
                    node.allocated = false;
                    node.detail = Some(
                        if overspent {
                            "task exceeded its declared spend ceiling"
                        } else {
                            "task produced no output or exceeded the output byte limit"
                        }
                        .into(),
                    );
                    WorkflowEventKind::NodeFailed { node: id }
                } else {
                    match outcome.status {
                        NodeOutcomeStatus::Complete => {
                            node.status = NodeStatus::Complete;
                            node.output = outcome.output;
                            WorkflowEventKind::NodeCompleted { node: id }
                        }
                        NodeOutcomeStatus::Failed => {
                            node.status = NodeStatus::Failed;
                            WorkflowEventKind::NodeFailed { node: id }
                        }
                        NodeOutcomeStatus::Waiting | NodeOutcomeStatus::NeedsRecovery => {
                            node.status = NodeStatus::Waiting;
                            WorkflowEventKind::NodeWaiting {
                                node: id,
                                needs_recovery: outcome.status == NodeOutcomeStatus::NeedsRecovery,
                            }
                        }
                    }
                }
            }
        };
        self.event(state, clock, kind);
        Ok(())
    }

    fn event(&self, state: &mut WorkflowState, clock: &Clock, kind: WorkflowEventKind) {
        let event = WorkflowEvent {
            unix_ms: crate::runtime::observability::unix_ms(),
            sequence: state.events.len(),
            elapsed_ms: clock.elapsed_ms(),
            kind,
        };
        tracing::debug!(workflow=%state.id,sequence=event.sequence,kind=?event.kind,"workflow event");
        // `Resumed` reaches the sink together with the first event after it,
        // so a resume that changes nothing leaves no trace (see `finish`).
        if let Some(sink) = self
            .events
            .filter(|_| event.kind != WorkflowEventKind::Resumed)
        {
            if let Some(resumed) = state
                .events
                .last()
                .filter(|last| last.kind == WorkflowEventKind::Resumed)
            {
                sink(resumed);
            }
            sink(&event);
        }
        state.events.push(event);
    }
    async fn save(
        &self,
        state: &mut WorkflowState,
        lease: &Arc<dyn WorkflowLease>,
        clock: &Clock,
    ) -> Result<(), RuntimeError> {
        state.elapsed_ms = clock.elapsed_ms();
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("workflow revision overflow"))?;
        state.validate()?;
        lease.save(state).await
    }
    /// Stop an `execute` invocation. If it recorded nothing after its
    /// `Resumed` event and leaves the status unchanged, drop that event
    /// instead of logging a `Resumed`/`Stopped` pair, so idle resumes do not
    /// grow the persisted event log.
    async fn finish(
        &self,
        state: &mut WorkflowState,
        lease: &Arc<dyn WorkflowLease>,
        clock: &Clock,
        status: WorkflowStatus,
        prior: WorkflowStatus,
    ) -> Result<(), RuntimeError> {
        if status != prior
            || state
                .events
                .last()
                .is_none_or(|last| last.kind != WorkflowEventKind::Resumed)
        {
            return self.stop(state, lease, clock, status, None).await;
        }
        state.events.pop();
        state.status = status;
        state.detail = None;
        state.invocation_started_unix_ms = None;
        self.save(state, lease, clock).await
    }
    async fn stop(
        &self,
        state: &mut WorkflowState,
        lease: &Arc<dyn WorkflowLease>,
        clock: &Clock,
        status: WorkflowStatus,
        detail: Option<String>,
    ) -> Result<(), RuntimeError> {
        state.status = status;
        state.detail = detail;
        state.invocation_started_unix_ms = None;
        self.event(state, clock, WorkflowEventKind::Stopped { status });
        self.save(state, lease, clock).await
    }
}

fn set_spend(node: &mut NodeState, definition: &TaskDefinition, cost: Option<u64>, uncertain: u64) {
    node.cost_micros = cost.unwrap_or_else(|| definition.max_cost_micros.unwrap_or(0));
    node.uncertain_micros = if cost.is_none() {
        node.cost_micros
    } else {
        uncertain.min(node.cost_micros)
    };
    node.cost_unknown = cost.is_none() && definition.max_cost_micros.is_none();
}
fn mark_unknown(node: &mut NodeState, definition: &TaskDefinition, detail: &str) {
    node.status = NodeStatus::Uncertain;
    node.allocated = false;
    node.output = None;
    node.detail = Some(detail.chars().take(4096).collect());
    node.needs_recovery = true;
    set_spend(node, definition, None, 0);
}
struct Clock {
    base: u64,
    start: Instant,
}
impl Clock {
    fn new(base: u64) -> Self {
        Self {
            base,
            start: Instant::now(),
        }
    }
    fn elapsed_ms(&self) -> u64 {
        self.base.saturating_add(
            self.start
                .elapsed()
                .as_nanos()
                .div_ceil(1_000_000)
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }
    fn remaining(&self, limit: Duration) -> Duration {
        limit.saturating_sub(Duration::from_millis(self.base).saturating_add(self.start.elapsed()))
    }
}
fn unix_ms() -> Result<u64, RuntimeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| invalid(&e.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| invalid("workflow timestamp overflow"))
}
