use std::collections::BTreeMap;
use std::future::Future;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::graph::{config, valid_name};
use crate::ledger::Usage;
use crate::runtime::{
    AgentLoop, ApprovalPolicy, CancelReason, DurableAgent, Message, RunStatus, RunStore,
    RuntimeError, StopReason, ToolProgress,
};
use crate::CancelToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRecovery {
    /// No effects or paid I/O. Safe to recompute after interruption.
    Pure,
    /// Must delegate to the context's stable durable child run id.
    DurableChild,
    /// Unknown completion after interruption requires operator reconciliation.
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDefinition {
    pub name: String,
    pub version: String,
    pub recovery: TaskRecovery,
    pub max_cost_micros: Option<u64>,
    /// Frozen, nonsecret execution settings. Version custom code contracts.
    pub config: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeContext {
    pub workflow_id: String,
    pub node_id: String,
    pub run_id: String,
    pub input: Value,
    pub params: Value,
    pub dependencies: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowChild {
    pub run_id: String,
    pub status: Option<RunStatus>,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeOutcomeStatus {
    Complete,
    Waiting,
    NeedsRecovery,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOutcome {
    pub status: NodeOutcomeStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::state::optional_value"
    )]
    pub output: Option<Value>,
    pub detail: Option<String>,
    pub child: Option<WorkflowChild>,
    /// Cumulative for a durable child; per invocation for other handlers.
    /// None means unknown, conservatively charged at the declared ceiling.
    pub cost_micros: Option<u64>,
    pub uncertain_micros: u64,
    pub usage: Usage,
}

impl NodeOutcome {
    pub fn pure(output: Value) -> Self {
        Self {
            status: NodeOutcomeStatus::Complete,
            output: Some(output),
            detail: None,
            child: None,
            cost_micros: Some(0),
            uncertain_micros: 0,
            usage: Usage::default(),
        }
    }
}

pub trait WorkflowTask: Send + Sync {
    fn definition(&self) -> Result<TaskDefinition, RuntimeError>;
    /// Effects belong inside the returned future, never its construction.
    /// Honor the recovery declaration and paid-attempt ceiling.
    fn run<'a>(
        &'a self,
        context: NodeContext,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<NodeOutcome, RuntimeError>>;
}

pub struct FnWorkflowTask<F> {
    definition: TaskDefinition,
    run: F,
}
impl<F> FnWorkflowTask<F> {
    pub fn new(definition: TaskDefinition, run: F) -> Self {
        Self { definition, run }
    }
}
impl<F, Fut> WorkflowTask for FnWorkflowTask<F>
where
    F: Fn(NodeContext, CancelToken) -> Fut + Send + Sync,
    Fut: Future<Output = Result<NodeOutcome, RuntimeError>> + Send + 'static,
{
    fn definition(&self) -> Result<TaskDefinition, RuntimeError> {
        Ok(self.definition.clone())
    }
    fn run<'a>(
        &'a self,
        context: NodeContext,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<NodeOutcome, RuntimeError>> {
        Box::pin((self.run)(context, cancel.clone()))
    }
}

pub(super) struct RegisteredTask<'a> {
    pub definition: TaskDefinition,
    pub task: Box<dyn WorkflowTask + 'a>,
}
#[derive(Default)]
pub struct WorkflowTasks<'a> {
    pub(super) tasks: BTreeMap<String, RegisteredTask<'a>>,
}
impl<'a> WorkflowTasks<'a> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, task: impl WorkflowTask + 'a) -> Result<(), RuntimeError> {
        let definition = task.definition()?;
        if !valid_name(&definition.name) || definition.version.trim().is_empty() {
            return Err(config("task needs a valid name and nonempty version"));
        }
        if definition.recovery == TaskRecovery::Pure && definition.max_cost_micros != Some(0) {
            return Err(config("pure tasks must declare zero paid cost"));
        }
        if self.tasks.contains_key(&definition.name) {
            return Err(config("duplicate workflow task"));
        }
        self.tasks.insert(
            definition.name.clone(),
            RegisteredTask {
                definition,
                task: Box::new(task),
            },
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowOutput {
    Text,
    Json {
        schema: Value,
    },
    /// Validate before the durable child can complete. Optional repairs are
    /// bounded within that same child and never repeat effectful tools.
    Validated {
        policy: crate::runtime::FinalOutputPolicy,
    },
}

/// A model/tool role backed by a durable child run. JSON output is decoded and
/// schema-checked before it can become another node's input.
pub struct AgentWorkflowTask<'a, 'model> {
    name: String,
    agent: &'a AgentLoop<'model>,
    store: &'a dyn RunStore,
    approvals: ApprovalPolicy,
    system: String,
    output: WorkflowOutput,
    validator: Option<jsonschema::Validator>,
    cancel_reason: Option<std::sync::Arc<CancelReason>>,
}

impl<'a, 'model> AgentWorkflowTask<'a, 'model> {
    pub fn new(
        name: impl Into<String>,
        agent: &'a AgentLoop<'model>,
        store: &'a dyn RunStore,
        system: impl Into<String>,
        output: WorkflowOutput,
    ) -> Result<Self, RuntimeError> {
        match &output {
            WorkflowOutput::Validated { policy } => agent
                .clone()
                .with_output_policy(policy.clone())
                .validate_output_policy()?,
            _ => agent.validate_output_policy()?,
        }
        let validator = match &output {
            WorkflowOutput::Text => None,
            WorkflowOutput::Json { schema } => Some(
                jsonschema::validator_for(schema)
                    .map_err(|error| config(&format!("invalid workflow output schema: {error}")))?,
            ),
            WorkflowOutput::Validated { policy } => {
                policy.validate()?;
                None
            }
        };
        Ok(Self {
            name: name.into(),
            agent,
            store,
            approvals: ApprovalPolicy::default(),
            system: system.into(),
            output,
            validator,
            cancel_reason: None,
        })
    }
    pub fn with_approvals(mut self, approvals: ApprovalPolicy) -> Self {
        self.approvals = approvals;
        self
    }
    fn configured_agent(&self) -> AgentLoop<'model> {
        let agent = self.agent.clone();
        match &self.output {
            WorkflowOutput::Validated { policy } => agent.with_output_policy(policy.clone()),
            _ => agent,
        }
    }
    /// Attribute a cancellation of the child run to an operator.
    pub fn with_cancel_reason(mut self, reason: std::sync::Arc<CancelReason>) -> Self {
        self.cancel_reason = Some(reason);
        self
    }
}

impl WorkflowTask for AgentWorkflowTask<'_, '_> {
    fn definition(&self) -> Result<TaskDefinition, RuntimeError> {
        let agent = self.configured_agent();
        let config = DurableAgent::new(&agent, self.store)
            .with_approvals(self.approvals.clone())
            .configuration();
        Ok(TaskDefinition {
            name: self.name.clone(),
            version: "agent-v1".into(),
            recovery: TaskRecovery::DurableChild,
            max_cost_micros: config.limits.max_cost_micros,
            config: json!({"kind":"durable_agent","run":config,"system":self.system,"output":self.output}),
        })
    }
    fn run<'a>(
        &'a self,
        context: NodeContext,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<NodeOutcome, RuntimeError>> {
        Box::pin(async move {
            let input =
                serde_json::to_string(&context).map_err(|error| config(&error.to_string()))?;
            let child_cancel = CancelToken::new();
            let original_cancel = self
                .agent
                .cancel
                .cloned()
                .or_else(crate::cancel::current)
                .unwrap_or_default();
            let agent = self.configured_agent().with_cancel(&child_cancel);
            let mut durable =
                DurableAgent::new(&agent, self.store).with_approvals(self.approvals.clone());
            if let Some(reason) = &self.cancel_reason {
                durable = durable.with_cancel_reason(reason);
            }
            let run = durable.start_or_resume(
                &context.run_id,
                vec![Message::system(&self.system), Message::user(input)],
            );
            tokio::pin!(run);
            let state = tokio::select! {
                result=&mut run=>result,
                ()=cancel.cancelled()=>{child_cancel.cancel();run.await},
                ()=original_cancel.cancelled()=>{child_cancel.cancel();run.await},
            }?;
            let mut outcome = NodeOutcome {
                status: NodeOutcomeStatus::Waiting,
                output: None,
                detail: state.report.detail.clone(),
                child: Some(WorkflowChild {
                    run_id: context.run_id.clone(),
                    status: Some(state.status),
                    stop_reason: Some(state.report.stop_reason),
                }),
                cost_micros: Some(state.report.budget.committed_micros),
                uncertain_micros: state.report.budget.uncertain_micros,
                usage: state.report.usage,
            };
            if state.status == RunStatus::NeedsRecovery
                || state.tool_steps.iter().any(|step| {
                    matches!(
                        step.progress,
                        ToolProgress::Uncertain { .. } | ToolProgress::InFlight { .. }
                    )
                })
            {
                outcome.status = NodeOutcomeStatus::NeedsRecovery;
            } else if state.status == RunStatus::Finished {
                if state.report.stop_reason != StopReason::Completed {
                    outcome.status = NodeOutcomeStatus::Failed;
                    return Ok(outcome);
                }
                // A finished child never changes again: re-entering it cannot
                // produce output, so this is a terminal node failure rather
                // than a recoverable wait that burns dispatches.
                let Some(text) = state.report.final_text() else {
                    outcome.status = NodeOutcomeStatus::Failed;
                    outcome.detail = Some(format!(
                        "child run {} finished without final text",
                        context.run_id
                    ));
                    return Ok(outcome);
                };
                let decoded = match &self.output {
                    WorkflowOutput::Text => Ok(Value::String(text.into())),
                    WorkflowOutput::Json { .. } | WorkflowOutput::Validated { .. } => {
                        serde_json::from_str::<Value>(text)
                            .map_err(|error| format!("invalid JSON role output: {error}"))
                    }
                };
                match decoded.and_then(|value| {
                    if let Some(validator) = &self.validator {
                        validator
                            .validate(&value)
                            .map_err(|error| format!("role output schema mismatch: {error}"))?;
                    }
                    Ok(value)
                }) {
                    Ok(value) => {
                        outcome.status = NodeOutcomeStatus::Complete;
                        outcome.output = Some(value);
                    }
                    Err(detail) => {
                        outcome.status = NodeOutcomeStatus::Failed;
                        outcome.detail = Some(detail);
                    }
                }
            }
            Ok(outcome)
        })
    }
}
