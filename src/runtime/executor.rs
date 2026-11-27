use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::Instant;

use super::{
    types::validate_history, BudgetSnapshot, ContextCompressor, ContextPolicy, ContextRecord,
    ExtractiveCompressor, Message, Model, ModelRequest, RunBudget, RuntimeError, ToolContext,
    ToolEffect, ToolPolicy, ToolRegistry, ToolResult,
};
use crate::{ledger::Usage, CancelToken};

/// Limits apply to the entire run. A step is one model decision (gateway
/// retries are charged individually but belong to that decision).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLimits {
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_duration: Duration,
    pub tool_timeout: Duration,
    pub max_tool_result_bytes: usize,
    /// Maximum serialized native model reply (turn and serving model).
    /// Durable runs reserve capacity before dispatch and reject larger replies
    /// with a persisted terminal error instead of replaying an unrecorded call.
    #[serde(default = "default_model_result_bytes")]
    pub max_model_result_bytes: usize,
    /// Model costs only, in the gateway price table's micro-units. The built-in
    /// model adapter requires known valid prices and reserves the model's
    /// configured maximum input/output cost before every attempt.
    pub max_cost_micros: Option<u64>,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_steps: 16,
            max_tool_calls: 32,
            max_duration: Duration::from_secs(300),
            tool_timeout: Duration::from_secs(30),
            max_tool_result_bytes: 64 * 1024,
            max_model_result_bytes: default_model_result_bytes(),
            max_cost_micros: None,
        }
    }
}

fn default_model_result_bytes() -> usize {
    64 * 1024
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    StepLimit,
    ToolLimit,
    Deadline,
    Cancelled,
    Budget,
    ModelError,
    ProtocolError,
    ToolError,
    PermissionDenied,
    ConfigurationError,
    Paused,
    ApprovalRequired,
    Uncertain,
    StorageError,
    ContextLimit,
    StorageLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEventKind {
    Started,
    ModelStarted {
        step: usize,
        model: String,
    },
    ModelFinished {
        step: usize,
        model: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
    },
    ToolFinished {
        call_id: String,
        is_error: bool,
    },
    Stopped {
        reason: StopReason,
    },
    Resumed,
    ContextPrepared {
        step: usize,
        omitted_messages: usize,
        input_bytes: usize,
    },
    OutputRejected {
        step: usize,
        validator: String,
        attempt: usize,
        detail: String,
        repair: bool,
    },
    OutputValidated {
        step: usize,
        validator: String,
    },
    ApprovalRequested {
        request_id: String,
        call_id: String,
    },
    ApprovalDecided {
        request_id: String,
        approved: bool,
        actor: String,
    },
    RecoveryRequired {
        call_id: String,
    },
    RecoveredModel,
    ToolReconciled {
        call_id: String,
        actor: String,
        action: String,
    },
}

/// Events contain correlation/ordering data; content is kept in the report's
/// messages rather than emitted to logging subscribers by default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    pub sequence: usize,
    pub elapsed_ms: u64,
    pub kind: RunEventKind,
}

/// Called synchronously in event order. Sinks should enqueue work and return
/// promptly; blocking callbacks cannot be interrupted by async cancellation.
pub type EventSink<'a> = dyn Fn(&RunEvent) + Send + Sync + 'a;

/// Available even on failure/limits/cancellation. The transcript may end with
/// unanswered tool calls after interruption; it must not be blindly replayed
/// against tools with side effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub run_id: String,
    pub messages: Vec<Message>,
    pub stop_reason: StopReason,
    pub detail: Option<String>,
    pub steps: usize,
    pub tool_calls: usize,
    pub usage: Usage,
    pub budget: BudgetSnapshot,
    pub elapsed_ms: u64,
    pub events: Vec<RunEvent>,
    #[serde(default)]
    pub contexts: Vec<ContextRecord>,
}

impl RunReport {
    pub fn final_text(&self) -> Option<&str> {
        if self.stop_reason != StopReason::Completed {
            return None;
        }
        match self.messages.last() {
            Some(Message::Assistant { turn }) if turn.tool_calls.is_empty() => Some(&turn.content),
            _ => None,
        }
    }
}

/// Sequential, bounded model/tool execution. A whole tool batch is preflighted
/// before any tool is dispatched; requested tools are executed in order.
#[derive(Clone)]
pub struct AgentLoop<'a> {
    pub(super) model: &'a dyn Model,
    pub(super) tools: &'a ToolRegistry,
    pub(super) policy: ToolPolicy,
    pub(super) limits: RunLimits,
    pub(super) model_id: String,
    pub(super) label: String,
    pub(super) temperature: f32,
    pub(super) max_output_tokens: u32,
    pub(super) cancel: Option<&'a CancelToken>,
    events: Option<&'a EventSink<'a>>,
    pub(super) context_policy: Option<ContextPolicy>,
    pub(super) compressor: &'a dyn ContextCompressor,
    pub(super) output_policy: Option<super::FinalOutputPolicy>,
}

static EXTRACTIVE: ExtractiveCompressor = ExtractiveCompressor;

impl<'a> AgentLoop<'a> {
    pub fn new(model: &'a dyn Model, tools: &'a ToolRegistry, model_id: impl Into<String>) -> Self {
        Self {
            model,
            tools,
            model_id: model_id.into(),
            policy: ToolPolicy::default(),
            limits: RunLimits::default(),
            label: "agent_loop".into(),
            temperature: 0.0,
            max_output_tokens: 4096,
            cancel: None,
            events: None,
            context_policy: None,
            compressor: &EXTRACTIVE,
            output_policy: None,
        }
    }

    pub fn with_policy(mut self, policy: ToolPolicy) -> Self {
        self.policy = policy;
        self
    }
    pub fn with_limits(mut self, limits: RunLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn with_cancel(mut self, cancel: &'a CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
    pub fn with_events(mut self, events: &'a EventSink<'a>) -> Self {
        self.events = Some(events);
        self
    }
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
    pub fn with_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = tokens;
        self
    }
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn with_context(mut self, policy: ContextPolicy) -> Self {
        self.context_policy = Some(policy);
        self
    }
    pub fn with_context_compressor(mut self, compressor: &'a dyn ContextCompressor) -> Self {
        self.compressor = compressor;
        self
    }
    /// An explicit final-output contract. Repairs use the original run's
    /// bounded model budget and a narrowed, read-only tool policy.
    pub fn with_output_policy(mut self, policy: super::FinalOutputPolicy) -> Self {
        self.output_policy = Some(policy);
        self
    }
    pub fn without_output_policy(mut self) -> Self {
        self.output_policy = None;
        self
    }
    pub(super) fn active_policy(&self, report: &RunReport) -> ToolPolicy {
        match &self.output_policy {
            Some(policy) if super::output::rejected_count(report) > 0 => {
                policy.repair_policy(&self.policy)
            }
            _ => self.policy.clone(),
        }
    }

    pub async fn run(&self, run_id: impl Into<String>, messages: Vec<Message>) -> RunReport {
        let started = Instant::now();
        let budget = RunBudget::new(self.limits.max_cost_micros);
        let mut report = RunReport {
            run_id: run_id.into(),
            messages,
            stop_reason: StopReason::ConfigurationError,
            detail: None,
            steps: 0,
            tool_calls: 0,
            usage: Usage::default(),
            budget: budget.snapshot(),
            elapsed_ms: 0,
            events: Vec::new(),
            contexts: Vec::new(),
        };
        self.emit(&mut report, started, RunEventKind::Started);
        let validation = self
            .validate(&report)
            .and_then(|()| validate_history(&report.messages));
        let result = match validation {
            Err(error) => Err(error),
            Ok(()) => {
                let cancel = self
                    .cancel
                    .cloned()
                    .or_else(crate::cancel::current)
                    .unwrap_or_default();
                let work = async {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => Ok(StopReason::Cancelled),
                        () = tokio::time::sleep_until(started + self.limits.max_duration) => Ok(StopReason::Deadline),
                        result = Box::pin(self.drive(&mut report, &budget, started)) => result,
                    }
                };
                let (result, usage) = crate::ledger::metered(work).await;
                report.usage = usage;
                result
            }
        };
        match result {
            Ok(reason) => report.stop_reason = reason,
            Err(error) => {
                report.stop_reason = match error {
                    RuntimeError::Model(crate::TaskError::Cancelled) => StopReason::Cancelled,
                    RuntimeError::Model(crate::TaskError::Deadline(_)) => StopReason::Deadline,
                    RuntimeError::Model(_) => StopReason::ModelError,
                    RuntimeError::Budget(_) => StopReason::Budget,
                    RuntimeError::Protocol(_) => StopReason::ProtocolError,
                    RuntimeError::Tool(_) => StopReason::ToolError,
                    RuntimeError::Permission(_) => StopReason::PermissionDenied,
                    RuntimeError::Configuration(_) => StopReason::ConfigurationError,
                    RuntimeError::Storage(_) | RuntimeError::Conflict(_) => {
                        StopReason::StorageError
                    }
                    RuntimeError::Context(_) => StopReason::ContextLimit,
                    RuntimeError::StorageLimit(_) => StopReason::StorageLimit,
                };
                report.detail = Some(error.to_string());
            }
        }
        report.budget = budget.snapshot();
        report.elapsed_ms = millis(started.elapsed());
        let reason = report.stop_reason;
        self.emit(&mut report, started, RunEventKind::Stopped { reason });
        report
    }

    pub(super) fn validate_output_policy(&self) -> Result<(), RuntimeError> {
        if let Some(policy) = &self.output_policy {
            policy.validate()?;
            if policy.max_repairs() > 0 && self.limits.max_cost_micros.is_none() {
                return Err(RuntimeError::Configuration(
                    "output repairs require an explicit run model-spend cap".into(),
                ));
            }
            if let super::FinalOutputPolicy::CitedReport {
                retrieval_tools, ..
            } = policy
            {
                for name in retrieval_tools {
                    if !self
                        .tools
                        .definition(name)
                        .is_some_and(|d| self.policy.permits(d) && d.effect == ToolEffect::ReadOnly)
                    {
                        return Err(RuntimeError::Configuration(
                            "citation validation requires permitted read-only retrieval tools"
                                .into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn validate(&self, report: &RunReport) -> Result<(), RuntimeError> {
        self.validate_output_policy()?;
        if let Some(policy) = &self.context_policy {
            policy.validate()?;
            if self.compressor.id().trim().is_empty() {
                return Err(RuntimeError::Configuration(
                    "context compressor needs a stable id".into(),
                ));
            }
        }
        if report.run_id.trim().is_empty()
            || self.model_id.trim().is_empty()
            || self.label.trim().is_empty()
        {
            return Err(RuntimeError::Configuration(
                "run id, model and label must be nonempty".into(),
            ));
        }
        if !self.temperature.is_finite()
            || self.max_output_tokens == 0
            || self.limits.tool_timeout.is_zero()
            || self.limits.max_tool_result_bytes < 4
            || self.limits.max_model_result_bytes < 64
            || Instant::now()
                .checked_add(self.limits.max_duration)
                .is_none()
            || Instant::now()
                .checked_add(self.limits.tool_timeout)
                .is_none()
        {
            return Err(RuntimeError::Configuration(
                "invalid sampling, output, timeout or result limit".into(),
            ));
        }
        Ok(())
    }

    async fn drive(
        &self,
        report: &mut RunReport,
        budget: &RunBudget,
        started: Instant,
    ) -> Result<StopReason, RuntimeError> {
        let mut seen = report
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Assistant { turn } => {
                    Some(turn.tool_calls.iter().map(|call| call.id.clone()))
                }
                _ => None,
            })
            .flatten()
            .collect::<BTreeSet<_>>();
        loop {
            tokio::task::yield_now().await;
            if report.steps >= self.limits.max_steps {
                return Ok(StopReason::StepLimit);
            }
            report.steps += 1;
            self.emit(
                report,
                started,
                RunEventKind::ModelStarted {
                    step: report.steps,
                    model: self.model_id.clone(),
                },
            );
            // Give deadline/cancellation priority after a sink notification,
            // even when the following model future would complete immediately.
            tokio::task::yield_now().await;
            let active_policy = self.active_policy(report);
            let tools = self.tools.definitions(&active_policy);
            let messages = self.prepare_context(report, &tools)?;
            if let Some(record) = report.contexts.last() {
                self.emit(
                    report,
                    started,
                    RunEventKind::ContextPrepared {
                        step: report.steps,
                        omitted_messages: record.omitted_messages(),
                        input_bytes: record.input_bytes,
                    },
                );
            }
            tokio::task::yield_now().await;
            let request = ModelRequest {
                label: self.label.clone(),
                model: self.model_id.clone(),
                messages,
                tools,
                temperature: self.temperature,
                max_output_tokens: self.max_output_tokens,
            };
            let output = self.model.generate(&request, budget).await?;
            if serde_json::to_vec(&output)
                .map_err(|e| RuntimeError::Protocol(e.to_string()))?
                .len()
                > self.limits.max_model_result_bytes
            {
                return Err(RuntimeError::StorageLimit(
                    "model reply exceeded max_model_result_bytes; reply not retained, usage charged, no tools dispatched".into(),
                ));
            }
            if let Some(detail) = budget.snapshot().denial {
                return Err(RuntimeError::Budget(detail));
            }
            output.turn.validate()?;
            self.emit(
                report,
                started,
                RunEventKind::ModelFinished {
                    step: report.steps,
                    model: output.model,
                },
            );
            let turn = output.turn;
            let calls = turn.tool_calls.clone();
            report.messages.push(Message::Assistant { turn });
            if calls.is_empty() {
                if let Some(policy) = &self.output_policy {
                    let validator = policy.identity()?;
                    match policy.check(report) {
                        Ok(()) => self.emit(
                            report,
                            started,
                            RunEventKind::OutputValidated {
                                step: report.steps,
                                validator,
                            },
                        ),
                        Err(detail) => {
                            let attempt = super::output::rejected_count(report) + 1;
                            let repair = attempt <= policy.max_repairs();
                            let detail = super::output::bounded_detail(&detail);
                            self.emit(
                                report,
                                started,
                                RunEventKind::OutputRejected {
                                    step: report.steps,
                                    validator,
                                    attempt,
                                    detail: detail.clone(),
                                    repair,
                                },
                            );
                            if repair {
                                report.messages.push(super::output::feedback(&detail));
                                continue;
                            }
                            return Err(RuntimeError::Protocol(format!(
                                "final-output validation rejected the answer: {detail}"
                            )));
                        }
                    }
                }
                return Ok(StopReason::Completed);
            }
            if calls.len() > self.limits.max_tool_calls.saturating_sub(report.tool_calls) {
                return Ok(StopReason::ToolLimit);
            }
            // Validate the entire batch before executing even its first call.
            let mut prepared = Vec::with_capacity(calls.len());
            for call in &calls {
                if !seen.insert(call.id.clone()) {
                    return Err(RuntimeError::Protocol(format!(
                        "reused tool call id: {}",
                        call.id
                    )));
                }
                prepared.push(self.tools.prepare(call, &active_policy)?);
            }
            for (call, tool) in calls.into_iter().zip(prepared) {
                tokio::task::yield_now().await;
                report.tool_calls += 1;
                self.emit(
                    report,
                    started,
                    RunEventKind::ToolStarted {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                    },
                );
                tokio::task::yield_now().await;
                let effect = tool.effect;
                let result = self
                    .execute_tool(tool, call.clone(), report.run_id.clone())
                    .await;
                let is_error = result.is_error;
                report.messages.push(Message::Tool { result });
                self.emit(
                    report,
                    started,
                    RunEventKind::ToolFinished {
                        call_id: call.id,
                        is_error,
                    },
                );
                if is_error && effect != ToolEffect::ReadOnly {
                    return Err(RuntimeError::Tool("tool with external effects did not produce a usable result; continuation stopped to avoid repeating effects".into()));
                }
            }
        }
    }

    pub(super) fn prepare_context(
        &self,
        report: &mut RunReport,
        tools: &[super::ToolDefinition],
    ) -> Result<Vec<Message>, RuntimeError> {
        let Some(policy) = &self.context_policy else {
            return Ok(report.messages.clone());
        };
        if let Some(previous) = report.contexts.last() {
            if previous.source_message_count == report.messages.len()
                && previous.compressor_id == self.compressor.id()
            {
                let messages = previous.restore(&report.messages, tools, policy)?;
                let mut record = previous.clone();
                record.step = report.steps;
                report.contexts.push(record);
                return Ok(messages);
            }
        }
        let (messages, record) =
            policy.prepare(&report.messages, tools, self.compressor, report.steps)?;
        report.contexts.push(record);
        Ok(messages)
    }

    pub(super) async fn execute_tool(
        &self,
        tool: super::tool::PreparedTool,
        call: super::ToolCall,
        run_id: String,
    ) -> ToolResult {
        let context = ToolContext {
            run_id,
            call_id: call.id.clone(),
        };
        let execution = tokio::time::timeout(
            self.limits.tool_timeout,
            tool.executor.execute(call.arguments, context),
        )
        .await;
        let (mut value, is_error) = match execution {
            Ok(Ok(value))
                if serde_json::to_vec(&value)
                    .map_or(true, |v| v.len() > self.limits.max_tool_result_bytes) =>
            {
                (
                    json!({"error": "tool result exceeded the byte limit"}),
                    true,
                )
            }
            Ok(Ok(value)) => (value, false),
            Ok(Err(error)) => (
                json!({"error": error.to_string().chars().take(1024).collect::<String>()}),
                true,
            ),
            Err(_) => (
                json!({"error": "tool timed out; external effects may be uncertain"}),
                true,
            ),
        };
        if serde_json::to_vec(&value).map_or(true, |v| v.len() > self.limits.max_tool_result_bytes)
        {
            value = serde_json::Value::Null;
        }
        ToolResult {
            call_id: call.id,
            output: value,
            is_error,
        }
    }

    fn emit(&self, report: &mut RunReport, started: Instant, kind: RunEventKind) {
        self.emit_at(report, millis(started.elapsed()), kind);
    }

    pub(super) fn emit_at(&self, report: &mut RunReport, elapsed_ms: u64, kind: RunEventKind) {
        let event = RunEvent {
            sequence: report.events.len(),
            elapsed_ms,
            kind,
        };
        tracing::debug!(run_id = %report.run_id, sequence = event.sequence, kind = ?event.kind, "agent runtime event");
        if let Some(sink) = self.events {
            sink(&event);
        }
        report.events.push(event);
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
