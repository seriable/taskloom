use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    NodeContext, TaskDefinition, TaskRecovery, WorkflowChild, WorkflowDefinition, WorkflowLimits,
};
use crate::ledger::Usage;
use crate::runtime::{state::fingerprint, RuntimeError};

pub const WORKFLOW_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Ready,
    Running,
    Paused,
    Waiting,
    NeedsRecovery,
    Completed,
    Failed,
    Cancelled,
    Deadline,
    DispatchLimit,
    BudgetLimit,
}
impl WorkflowStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Deadline
                | Self::DispatchLimit
                | Self::BudgetLimit
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Waiting,
    Complete,
    Skipped,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeRecovery {
    RecordOutput {
        output: Value,
        cost_micros: Option<u64>,
        usage: Usage,
        actor: String,
        note: String,
    },
    RetryPure {
        actor: String,
        note: String,
    },
    Abort {
        actor: String,
        note: String,
    },
}
impl NodeRecovery {
    pub fn actor(&self) -> &str {
        match self {
            Self::RecordOutput { actor, .. }
            | Self::RetryPure { actor, .. }
            | Self::Abort { actor, .. } => actor,
        }
    }
    pub fn note(&self) -> &str {
        match self {
            Self::RecordOutput { note, .. }
            | Self::RetryPure { note, .. }
            | Self::Abort { note, .. } => note,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecoveryRecord {
    pub decision: NodeRecovery,
    pub decided_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    pub id: String,
    pub status: NodeStatus,
    pub context: Option<NodeContext>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_value"
    )]
    pub output: Option<Value>,
    pub child: Option<WorkflowChild>,
    pub detail: Option<String>,
    pub needs_recovery: bool,
    pub dispatches: usize,
    pub allocated: bool,
    pub cost_micros: u64,
    pub uncertain_micros: u64,
    pub cost_unknown: bool,
    pub usage: Usage,
    pub recoveries: Vec<NodeRecoveryRecord>,
}
impl NodeState {
    pub(super) fn new(id: String) -> Self {
        Self {
            id,
            status: NodeStatus::Pending,
            context: None,
            output: None,
            child: None,
            detail: None,
            needs_recovery: false,
            dispatches: 0,
            allocated: false,
            cost_micros: 0,
            uncertain_micros: 0,
            cost_unknown: false,
            usage: Usage::default(),
            recoveries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowEventKind {
    Started,
    Resumed,
    NodeStarted { node: String, dispatch: usize },
    NodeCompleted { node: String },
    NodeSkipped { node: String },
    NodeWaiting { node: String, needs_recovery: bool },
    NodeFailed { node: String },
    NodeUncertain { node: String },
    NodeReconciled { node: String, actor: String },
    Recovered { node: String },
    Stopped { status: WorkflowStatus },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEvent {
    pub sequence: usize,
    pub elapsed_ms: u64,
    pub kind: WorkflowEventKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowBudget {
    pub limit_micros: Option<u64>,
    pub committed_micros: u64,
    pub reserved_micros: u64,
    pub uncertain_micros: u64,
    pub unknown_nodes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub version: u32,
    pub revision: u64,
    pub id: String,
    pub definition: WorkflowDefinition,
    pub tasks: Vec<TaskDefinition>,
    pub limits: WorkflowLimits,
    pub input: Value,
    pub status: WorkflowStatus,
    pub nodes: Vec<NodeState>,
    pub dispatches: usize,
    pub elapsed_ms: u64,
    /// Wall clock interval is charged conservatively if a running worker dies.
    pub invocation_started_unix_ms: Option<u64>,
    pub invocation_base_elapsed_ms: u64,
    pub events: Vec<WorkflowEvent>,
    pub detail: Option<String>,
}

impl WorkflowState {
    pub fn node(&self, id: &str) -> Option<&NodeState> {
        self.nodes.iter().find(|node| node.id == id)
    }
    pub fn output(&self, id: &str) -> Option<&Value> {
        self.node(id)
            .filter(|node| node.status == NodeStatus::Complete)
            .and_then(|node| node.output.as_ref())
    }
    pub fn usage(&self) -> Usage {
        let mut usage = Usage::default();
        for node in &self.nodes {
            usage.add(node.usage);
        }
        usage
    }
    pub fn budget(&self) -> Result<WorkflowBudget, RuntimeError> {
        let mut budget = WorkflowBudget {
            limit_micros: self.limits.max_cost_micros,
            ..Default::default()
        };
        for (spec, node) in self.definition.nodes.iter().zip(&self.nodes) {
            let task = self
                .tasks
                .iter()
                .find(|task| task.name == spec.task)
                .ok_or_else(|| invalid("missing task definition"))?;
            budget.committed_micros = budget
                .committed_micros
                .checked_add(node.cost_micros)
                .ok_or_else(|| invalid("workflow cost overflow"))?;
            budget.uncertain_micros = budget
                .uncertain_micros
                .checked_add(node.uncertain_micros)
                .ok_or_else(|| invalid("workflow uncertainty overflow"))?;
            budget.unknown_nodes += usize::from(node.cost_unknown);
            if node.allocated {
                budget.reserved_micros = budget
                    .reserved_micros
                    .checked_add(
                        task.max_cost_micros
                            .unwrap_or(0)
                            .saturating_sub(node.cost_micros),
                    )
                    .ok_or_else(|| invalid("workflow reservation overflow"))?;
            }
        }
        Ok(budget)
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.version != WORKFLOW_STATE_VERSION
            || self.revision >= u64::MAX - 1024
            || self.id.trim().is_empty()
        {
            return Err(invalid(
                "unsupported workflow version, revision or identity",
            ));
        }
        self.limits.validate()?;
        self.definition.validate(&self.limits)?;
        if self.nodes.len() != self.definition.nodes.len() {
            return Err(invalid("workflow node state count differs"));
        }
        let task_names = self
            .tasks
            .iter()
            .map(|task| task.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if task_names.len() != self.tasks.len()
            || self.tasks.iter().any(|task| {
                !super::graph::valid_name(&task.name)
                    || task.version.trim().is_empty()
                    || task.recovery == TaskRecovery::Pure && task.max_cost_micros != Some(0)
                    || self.limits.max_cost_micros.is_some() && task.max_cost_micros.is_none()
            })
        {
            return Err(invalid("invalid task definitions or spend ceilings"));
        }
        let mut dispatches = 0usize;
        for (spec, node) in self.definition.nodes.iter().zip(&self.nodes) {
            let task = self
                .tasks
                .iter()
                .find(|task| task.name == spec.task)
                .ok_or_else(|| invalid("missing workflow handler"))?;
            if node.id != spec.id
                || node.uncertain_micros > node.cost_micros
                || node.dispatches > self.limits.max_dispatches
            {
                return Err(invalid("invalid node identity, cost or count"));
            }
            dispatches = dispatches
                .checked_add(node.dispatches)
                .ok_or_else(|| invalid("dispatch count overflow"))?;
            if node.dispatches > 0 {
                let context = node
                    .context
                    .as_ref()
                    .ok_or_else(|| invalid("dispatched node has no exact input"))?;
                if *context != self.context_for(&spec.id)? {
                    return Err(invalid("node input differs from frozen dependency handoff"));
                }
            } else if node.context.is_some()
                || node.child.is_some()
                || node.cost_micros != 0
                || node.allocated
            {
                return Err(invalid("undispatched node has execution data"));
            }
            if let Some(child) = &node.child {
                if task.recovery != TaskRecovery::DurableChild
                    || node
                        .context
                        .as_ref()
                        .is_none_or(|context| child.run_id != context.run_id)
                {
                    return Err(invalid("child run is not bound to its workflow node"));
                }
            }
            if node.status == NodeStatus::Waiting
                && (task.recovery != TaskRecovery::DurableChild || node.child.is_none())
            {
                return Err(invalid("waiting task has no durable child"));
            }
            if node.status == NodeStatus::Complete {
                let output = node
                    .output
                    .as_ref()
                    .ok_or_else(|| invalid("completed node has no output"))?;
                if node.allocated
                    || serde_json::to_vec(output)
                        .map_err(|e| invalid(&e.to_string()))?
                        .len()
                        > self.limits.max_output_bytes
                {
                    return Err(invalid("invalid completed node output"));
                }
            } else if node.output.is_some() {
                return Err(invalid("unfinished node exposes output"));
            }
            if node.status == NodeStatus::Skipped && (node.dispatches != 0 || node.allocated) {
                return Err(invalid("skipped node was dispatched"));
            }
            if task.recovery == TaskRecovery::Pure
                && (node.cost_micros != 0 || node.uncertain_micros != 0 || node.child.is_some())
            {
                return Err(invalid("pure task reported paid work or a child run"));
            }
        }
        if dispatches != self.dispatches || dispatches > self.limits.max_dispatches {
            return Err(invalid("workflow dispatch count differs"));
        }
        let mut counts = std::collections::BTreeMap::new();
        let mut previous = 0;
        for (index, event) in self.events.iter().enumerate() {
            if event.sequence != index
                || event.elapsed_ms < previous
                || event.elapsed_ms > self.elapsed_ms
            {
                return Err(invalid("invalid workflow event order"));
            }
            previous = event.elapsed_ms;
            if let WorkflowEventKind::NodeStarted { node, dispatch } = &event.kind {
                let count = counts.entry(node.as_str()).or_insert(0usize);
                *count += 1;
                if *dispatch != *count {
                    return Err(invalid("node dispatch event is out of order"));
                }
            }
        }
        if self
            .nodes
            .iter()
            .any(|node| counts.get(node.id.as_str()).copied().unwrap_or(0) != node.dispatches)
            || counts.keys().any(|id| self.node(id).is_none())
        {
            return Err(invalid("dispatch events differ from counters"));
        }
        if self.status == WorkflowStatus::Completed
            && self
                .nodes
                .iter()
                .any(|node| !matches!(node.status, NodeStatus::Complete | NodeStatus::Skipped))
        {
            return Err(invalid("completed workflow has unfinished nodes"));
        }
        if (self.status == WorkflowStatus::Running) != self.invocation_started_unix_ms.is_some() {
            return Err(invalid("workflow ownership clock does not match status"));
        }
        self.budget()?;
        Ok(())
    }

    pub(super) fn context_for(&self, id: &str) -> Result<NodeContext, RuntimeError> {
        let spec = self
            .definition
            .nodes
            .iter()
            .find(|node| node.id == id)
            .ok_or_else(|| invalid("missing node spec"))?;
        let mut dependencies = std::collections::BTreeMap::new();
        for dependency in &spec.depends_on {
            let state = self
                .node(dependency)
                .ok_or_else(|| invalid("missing dependency"))?;
            if state.status == NodeStatus::Complete {
                dependencies.insert(
                    dependency.clone(),
                    state
                        .output
                        .clone()
                        .ok_or_else(|| invalid("dependency has no output"))?,
                );
            } else if state.status != NodeStatus::Skipped || !spec.allow_skipped_dependencies {
                return Err(invalid(
                    "node was dispatched before its dependencies completed",
                ));
            }
        }
        if !spec.condition.evaluate(&dependencies) {
            return Err(invalid("node was dispatched with a false condition"));
        }
        if !spec.select.is_empty() {
            dependencies = spec
                .select
                .iter()
                .map(|(alias, selection)| {
                    let value = dependencies
                        .get(&selection.node)
                        .and_then(|value| value.pointer(&selection.pointer))
                        .cloned()
                        .ok_or_else(|| invalid("input selector has no dependency value"))?;
                    Ok((alias.clone(), value))
                })
                .collect::<Result<_, RuntimeError>>()?;
        }
        Ok(NodeContext {
            workflow_id: self.id.clone(),
            node_id: id.into(),
            run_id: child_run_id(&self.id, id)?,
            input: self.input.clone(),
            params: spec.params.clone(),
            dependencies,
        })
    }
}

pub(super) fn child_run_id(workflow: &str, node: &str) -> Result<String, RuntimeError> {
    Ok(format!("workflow-{}", fingerprint(&(workflow, node))?))
}
pub(super) fn invalid(detail: &str) -> RuntimeError {
    RuntimeError::Storage(format!("invalid workflow: {detail}"))
}

pub(super) fn optional_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}
