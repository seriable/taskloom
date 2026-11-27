use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    types::pending_calls, Message, RunLimits, RunReport, RuntimeError, StopReason, ToolCall,
    ToolDefinition, ToolEffect, ToolPolicy, ToolResult,
};

pub const RUN_STATE_VERSION: u32 = 1;

/// Persistent runs require approval for tools with effects by default.
/// An application can explicitly choose a different policy at run creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    pub read_only: bool,
    pub idempotent: bool,
    pub non_idempotent: bool,
    pub tools: BTreeSet<String>,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            read_only: false,
            idempotent: true,
            non_idempotent: true,
            tools: BTreeSet::new(),
        }
    }
}

impl ApprovalPolicy {
    pub fn requires(&self, tool: &ToolDefinition) -> bool {
        self.tools.contains(&tool.name)
            || match tool.effect {
                ToolEffect::ReadOnly => self.read_only,
                ToolEffect::Idempotent => self.idempotent,
                ToolEffect::NonIdempotent => self.non_idempotent,
            }
    }
}

/// Frozen at creation. Resuming with changed model, limits, permissions or
/// tool definitions is rejected; create a new run for a different contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunConfig {
    pub model: String,
    pub label: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub limits: RunLimits,
    pub policy: ToolPolicy,
    pub approvals: ApprovalPolicy,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ready,
    Running,
    Paused,
    WaitingApproval,
    NeedsRecovery,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunCursor {
    ModelReady,
    ModelInFlight {
        started_unix_ms: u64,
        elapsed_before_ms: u64,
    },
    /// Indices into `tool_steps` and `report.messages`. Earlier batches stay
    /// in the state so their approvals/results remain auditable.
    Tools {
        first: usize,
        count: usize,
        next: usize,
        assistant_message: usize,
    },
    Finished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub run_id: String,
    pub call: ToolCall,
    pub definition: ToolDefinition,
    pub attempt: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub approved: bool,
    pub actor: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub request: ApprovalRequest,
    pub decision: ApprovalDecision,
    pub decided_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolProgress {
    Pending,
    AwaitingApproval {
        request: ApprovalRequest,
    },
    InFlight {
        started_unix_ms: u64,
        elapsed_before_ms: u64,
    },
    Complete {
        result: ToolResult,
    },
    Uncertain {
        detail: String,
        reported_result: Option<ToolResult>,
    },
}

/// Operator decisions are explicit and auditable. RetrySafe is accepted only
/// for read-only/idempotent tools and preserves the original run/call ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecoveryDecision {
    RecordResult {
        output: Value,
        is_error: bool,
        actor: String,
        note: String,
    },
    RetrySafe {
        actor: String,
        note: String,
    },
    Abort {
        actor: String,
        note: String,
    },
}

impl RecoveryDecision {
    pub fn actor(&self) -> &str {
        match self {
            Self::RecordResult { actor, .. }
            | Self::RetrySafe { actor, .. }
            | Self::Abort { actor, .. } => actor,
        }
    }
    pub fn note(&self) -> &str {
        match self {
            Self::RecordResult { note, .. }
            | Self::RetrySafe { note, .. }
            | Self::Abort { note, .. } => note,
        }
    }
    pub fn action(&self) -> &'static str {
        match self {
            Self::RecordResult { .. } => "record_result",
            Self::RetrySafe { .. } => "retry_safe",
            Self::Abort { .. } => "abort",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub decision: RecoveryDecision,
    pub decided_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolStep {
    pub call: ToolCall,
    pub definition: ToolDefinition,
    pub attempts: usize,
    pub progress: ToolProgress,
    pub approvals: Vec<ApprovalRecord>,
    pub recoveries: Vec<RecoveryRecord>,
}

impl ToolStep {
    pub(crate) fn request(
        &self,
        run_id: &str,
        attempt: usize,
    ) -> Result<ApprovalRequest, RuntimeError> {
        let request_id = fingerprint(&(run_id, &self.call, &self.definition, attempt))?;
        Ok(ApprovalRequest {
            request_id,
            run_id: run_id.into(),
            call: self.call.clone(),
            definition: self.definition.clone(),
            attempt,
        })
    }
}

/// The durable format. Files are application data and must be stored in a
/// trusted directory. Hashes bind approvals to input, not to user identity;
/// the application must authenticate actors before accepting decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub version: u32,
    pub revision: u64,
    pub config: RunConfig,
    pub status: RunStatus,
    pub cursor: RunCursor,
    pub report: RunReport,
    pub tool_steps: Vec<ToolStep>,
}

impl RunState {
    pub fn pending_approval(&self) -> Option<&ApprovalRequest> {
        match &self.tool_steps.get(self.current_tool_index()?)?.progress {
            ToolProgress::AwaitingApproval { request } => Some(request),
            _ => None,
        }
    }

    pub fn current_tool_index(&self) -> Option<usize> {
        match self.cursor {
            RunCursor::Tools {
                first, next, count, ..
            } if next < count => first.checked_add(next),
            _ => None,
        }
    }

    /// Checks structural invariants before a loaded checkpoint can cause work.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let invalid =
            |detail: &str| RuntimeError::Storage(format!("invalid run checkpoint: {detail}"));
        if self.version != RUN_STATE_VERSION || self.revision >= u64::MAX - 1024 {
            return Err(invalid("unsupported version or revision"));
        }
        if self.report.run_id.trim().is_empty()
            || !self.config.temperature.is_finite()
            || self.config.max_output_tokens == 0
        {
            return Err(invalid("invalid run configuration"));
        }
        if self.config.limits.max_cost_micros != self.report.budget.limit_micros
            || self.report.budget.uncertain_micros > self.report.budget.committed_micros
            || self.report.steps > self.config.limits.max_steps
            || self.report.tool_calls > self.config.limits.max_tool_calls
        {
            return Err(invalid("invalid counters or budget"));
        }
        let mut previous_ms = 0;
        let mut model_steps = 0;
        let mut tool_attempts = 0;
        for (index, event) in self.report.events.iter().enumerate() {
            if event.sequence != index
                || event.elapsed_ms < previous_ms
                || event.elapsed_ms > self.report.elapsed_ms
            {
                return Err(invalid("invalid event order"));
            }
            previous_ms = event.elapsed_ms;
            if matches!(event.kind, super::RunEventKind::ModelStarted { .. }) {
                model_steps += 1;
            }
            if matches!(event.kind, super::RunEventKind::ToolStarted { .. }) {
                tool_attempts += 1;
            }
        }
        let attempts = self
            .tool_steps
            .iter()
            .try_fold(0usize, |sum, step| sum.checked_add(step.attempts));
        if model_steps != self.report.steps
            || tool_attempts != self.report.tool_calls
            || attempts != Some(tool_attempts)
        {
            return Err(invalid("attempt counters do not match the trajectory"));
        }
        let pending =
            pending_calls(&self.report.messages).map_err(|error| invalid(&error.to_string()))?;
        let mut ids = BTreeSet::new();
        for (step_index, step) in self.tool_steps.iter().enumerate() {
            let in_current_batch = matches!(self.cursor, RunCursor::Tools {first,count,..} if step_index >= first && step_index < first.saturating_add(count));
            if !in_current_batch && !matches!(step.progress, ToolProgress::Complete { .. }) {
                return Err(invalid("unfinished tool is outside the current batch"));
            }
            if matches!(step.progress, ToolProgress::InFlight { .. })
                && self.status != RunStatus::Running
                && self.status != RunStatus::Finished
            {
                return Err(invalid("in-flight tool is not owned by a running worker"));
            }
            if matches!(step.progress, ToolProgress::AwaitingApproval { .. })
                && self.status != RunStatus::WaitingApproval
            {
                return Err(invalid("approval cursor is not waiting"));
            }
            if matches!(step.progress, ToolProgress::Uncertain { .. })
                && self.status != RunStatus::NeedsRecovery
                && self.status != RunStatus::Finished
            {
                return Err(invalid("uncertain tool lacks a recovery boundary"));
            }
            if !ids.insert(&step.call.id)
                || step.call.name != step.definition.name
                || !self.config.tools.contains(&step.definition)
                || !self.config.policy.permits(&step.definition)
            {
                return Err(invalid("invalid or changed tool definition"));
            }
            if !self.report.messages.iter().any(|m| matches!(m, Message::Assistant { turn } if turn.tool_calls.contains(&step.call))) {
                return Err(invalid("tool step has no matching assistant request"));
            }
            if let ToolProgress::Complete { result } = &step.progress {
                if step.attempts == 0
                    || result.call_id != step.call.id
                    || !self
                        .report
                        .messages
                        .iter()
                        .any(|m| matches!(m, Message::Tool { result: saved } if saved == result))
                {
                    return Err(invalid("completed tool has no matching result"));
                }
            }
            if let ToolProgress::AwaitingApproval { request } = &step.progress {
                if *request != step.request(&self.report.run_id, step.attempts + 1)? {
                    return Err(invalid("approval request does not match its input"));
                }
            }
            for record in &step.approvals {
                if record.decision.actor.trim().is_empty()
                    || record.decision.actor.len() > 256
                    || record.decision.note.len() > 4096
                {
                    return Err(invalid("invalid approval actor or note"));
                }
                let attempt = record.request.attempt;
                if attempt == 0
                    || attempt > step.attempts + 1
                    || record.request != step.request(&self.report.run_id, attempt)?
                {
                    return Err(invalid("approval decision does not match its input"));
                }
            }
            if matches!(
                step.progress,
                ToolProgress::InFlight { .. }
                    | ToolProgress::Complete { .. }
                    | ToolProgress::Uncertain { .. }
            ) && self.config.approvals.requires(&step.definition)
                && !step.approvals.last().is_some_and(|record| {
                    record.decision.approved && record.request.attempt == step.attempts
                })
            {
                return Err(invalid(
                    "tool with effects ran without its required approval",
                ));
            }
        }
        match self.cursor {
            RunCursor::ModelReady | RunCursor::ModelInFlight { .. } if !pending.is_empty() => {
                return Err(invalid("model cursor has unresolved tools"))
            }
            RunCursor::Tools {
                first,
                count,
                next,
                assistant_message,
            } => {
                if count == 0
                    || next >= count
                    || first.checked_add(count) != Some(self.tool_steps.len())
                    || assistant_message
                        .checked_add(1)
                        .and_then(|index| index.checked_add(next))
                        != Some(self.report.messages.len())
                {
                    return Err(invalid("invalid tool cursor"));
                }
                let Some(Message::Assistant { turn }) = self.report.messages.get(assistant_message)
                else {
                    return Err(invalid("tool batch has no assistant message"));
                };
                let batch = &self.tool_steps[first..];
                if turn.tool_calls
                    != batch
                        .iter()
                        .map(|step| step.call.clone())
                        .collect::<Vec<_>>()
                    || batch[..next]
                        .iter()
                        .any(|step| !matches!(step.progress, ToolProgress::Complete { .. }))
                    || batch[next..]
                        .iter()
                        .any(|step| matches!(step.progress, ToolProgress::Complete { .. }))
                    || pending
                        != batch[next..]
                            .iter()
                            .map(|step| step.call.id.clone())
                            .collect()
                {
                    return Err(invalid("tool batch does not match the conversation"));
                }
                if batch[next + 1..].iter().any(|step| {
                    !matches!(step.progress, ToolProgress::Pending)
                        || step.attempts != 0
                        || !step.approvals.is_empty()
                        || !step.recoveries.is_empty()
                }) {
                    return Err(invalid(
                        "future tools have already been dispatched or approved",
                    ));
                }
                for (offset, step) in batch.iter().take(next).enumerate() {
                    let ToolProgress::Complete { result } = &step.progress else {
                        unreachable!()
                    };
                    if !matches!(&self.report.messages[assistant_message+1+offset], Message::Tool { result:saved } if saved == result)
                    {
                        return Err(invalid("tool results are out of order"));
                    }
                }
            }
            _ => {}
        }
        if matches!(self.cursor, RunCursor::ModelInFlight { .. })
            && self.status != RunStatus::Running
            && self.status != RunStatus::Finished
        {
            return Err(invalid("in-flight model is not owned by a running worker"));
        }
        if self.cursor == RunCursor::Finished && self.status != RunStatus::Finished {
            return Err(invalid("finished cursor has an active status"));
        }
        if self.status == RunStatus::WaitingApproval && self.pending_approval().is_none() {
            return Err(invalid("waiting run has no approval request"));
        }
        if self.status == RunStatus::NeedsRecovery
            && !self.current_tool_index().is_some_and(|i| {
                matches!(self.tool_steps[i].progress, ToolProgress::Uncertain { .. })
            })
        {
            return Err(invalid("recovery run has no uncertain tool"));
        }
        if self.status == RunStatus::Finished
            && self.report.stop_reason == StopReason::Completed
            && (self.report.final_text().is_none()
                || !pending.is_empty()
                || self.cursor != RunCursor::Finished)
        {
            return Err(invalid("completed run has no complete final answer"));
        }
        Ok(())
    }
}

pub(crate) fn fingerprint<T: Serialize>(value: &T) -> Result<String, RuntimeError> {
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.into_iter().map(canonical).collect()),
            other => other,
        }
    }
    let value = serde_json::to_value(value).map_err(|e| RuntimeError::Storage(e.to_string()))?;
    let bytes =
        serde_json::to_vec(&canonical(value)).map_err(|e| RuntimeError::Storage(e.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
