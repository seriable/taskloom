use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::invalid;
use crate::ledger::Usage;
use crate::runtime::{
    rag::SearchResult, state::fingerprint, workflow::*, Message, RunEventKind, RunState, RunStatus,
    RuntimeError, Session, StopReason,
};

const MAX_SPANS: usize = 4096;
const MAX_EVENTS: usize = 32768;
const MAX_CHILDREN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Session,
    Turn,
    Workflow,
    Node,
    NodeAttempt,
    Run,
    Model,
    Tool,
    Retrieval,
    Approval,
    Recovery,
    Validation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanOutcome {
    Open,
    Ok,
    Error,
    Waiting,
    Uncertain,
    Cancelled,
    Skipped,
}

/// Names and attributes are an exporter allowlist. Raw IDs, prompts, tool
/// names/arguments/results, source quotes, errors and endpoints are excluded.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticSpan {
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub kind: SpanKind,
    pub outcome: SpanOutcome,
    pub start_unix_ms: Option<u64>,
    pub end_unix_ms: Option<u64>,
    /// Distinguishes an actual end event with an unavailable clock from a
    /// genuinely open operation. Timestamp absence never means completion.
    pub ended: bool,
    pub active_elapsed_ms: Option<u64>,
    pub attributes: BTreeMap<String, Value>,
}
impl DiagnosticSpan {
    fn new(
        id: String,
        parent: Option<String>,
        kind: SpanKind,
        start: Option<u64>,
        end: Option<u64>,
        outcome: SpanOutcome,
    ) -> Self {
        Self {
            span_id: id,
            parent_span_id: parent,
            kind,
            outcome,
            start_unix_ms: start,
            end_unix_ms: end,
            ended: end.is_some(),
            active_elapsed_ms: None,
            attributes: BTreeMap::new(),
        }
    }
    fn attr(&mut self, key: &str, value: impl Into<Value>) {
        self.attributes.insert(key.into(), value.into());
    }
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiagnosticMetrics {
    pub saved_run_observations_complete: bool,
    pub generation_usage_reported: bool,
    pub model_decisions_started: u64,
    pub model_decisions_finished: u64,
    pub tool_attempts: u64,
    pub tool_errors: u64,
    pub model_recoveries: u64,
    pub output_rejections: u64,
    pub output_validations: u64,
    pub omitted_context_messages: u64,
    pub retrievals: u64,
    pub empty_retrievals: u64,
    pub skipped_oversize_chunks: u64,
    pub generation_usage: Usage,
    pub generation_committed_micros: u64,
    pub generation_uncertain_micros: u64,
    pub generation_in_flight_micros: u64,
    pub generation_uncertain_attempts: u64,
    /// Never assume that a generation ledger pays these services. There is
    /// currently no per-run allocation from the independent embedding ledger.
    pub embedding_attributed_cost_micros: Option<u64>,
    pub tool_services_attributed_cost_micros: Option<u64>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RetrievalDiagnostic {
    pub span_id: String,
    pub corpus_revision: u64,
    pub returned_chunks: usize,
    pub skipped_oversize: usize,
    /// Hashed chunk identities and finite scores, never text or source URIs.
    pub scores: Vec<(String, f64)>,
    /// Local inspection only. The OTLP encoder never serializes this field.
    pub hits: Vec<RetrievalHit>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RetrievalHit {
    pub document_id: String,
    pub chunk_id: String,
    pub title: String,
    pub start: usize,
    pub end: usize,
    pub score: f64,
}
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticIssue {
    pub span_id: String,
    pub stage: String,
    pub code: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct TraceSnapshot {
    pub version: u32,
    pub trace_id: String,
    pub root_span_id: String,
    pub source_revision_key: String,
    pub captured_unix_ms: Option<u64>,
    pub spans: Vec<DiagnosticSpan>,
    pub metrics: DiagnosticMetrics,
    pub retrievals: Vec<RetrievalDiagnostic>,
    pub issues: Vec<DiagnosticIssue>,
    /// Missing children are visible; snapshots may race a live worker.
    pub missing_children: usize,
}

pub(super) fn key(parts: &[&str], bytes: usize) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    let mut id = hex::encode(&h.finalize()[..bytes]);
    if id.bytes().all(|b| b == b'0') {
        id.replace_range(0..1, "1");
    }
    id
}
fn stop_outcome(reason: StopReason) -> SpanOutcome {
    match reason {
        StopReason::Completed => SpanOutcome::Ok,
        StopReason::Cancelled => SpanOutcome::Cancelled,
        StopReason::Paused | StopReason::ApprovalRequired => SpanOutcome::Waiting,
        StopReason::Uncertain => SpanOutcome::Uncertain,
        _ => SpanOutcome::Error,
    }
}
fn workflow_outcome(status: WorkflowStatus) -> SpanOutcome {
    match status {
        WorkflowStatus::Completed => SpanOutcome::Ok,
        WorkflowStatus::Cancelled => SpanOutcome::Cancelled,
        WorkflowStatus::Ready | WorkflowStatus::Running => SpanOutcome::Open,
        WorkflowStatus::Paused | WorkflowStatus::Waiting => SpanOutcome::Waiting,
        WorkflowStatus::NeedsRecovery => SpanOutcome::Uncertain,
        _ => SpanOutcome::Error,
    }
}
impl TraceSnapshot {
    fn new(namespace: &str, kind: &str, id: &str, revision: u64) -> Result<Self, RuntimeError> {
        if namespace.trim().is_empty() || namespace.len() > 512 {
            return Err(invalid("namespace must contain 1..=512 bytes"));
        }
        let trace_id = key(&["taskloom-trace-v1", namespace, kind, id], 16);
        Ok(Self {
            version: 1,
            root_span_id: key(&[&trace_id, "root"], 8),
            source_revision_key: key(&[&trace_id, &revision.to_string()], 16),
            trace_id,
            captured_unix_ms: super::unix_ms(),
            spans: Vec::new(),
            metrics: DiagnosticMetrics {
                saved_run_observations_complete: true,
                generation_usage_reported: true,
                ..Default::default()
            },
            retrievals: Vec::new(),
            issues: Vec::new(),
            missing_children: 0,
        })
    }
    fn span_key(&self, parts: &[&str]) -> String {
        let mut keys = vec![self.trace_id.as_str()];
        keys.extend_from_slice(parts);
        key(&keys, 8)
    }
    fn push(&mut self, span: DiagnosticSpan) -> Result<usize, RuntimeError> {
        if self.spans.len() >= MAX_SPANS {
            return Err(invalid(
                "snapshot exceeds 4096 spans; inspect a narrower run",
            ));
        }
        let index = self.spans.len();
        self.spans.push(span);
        Ok(index)
    }
    pub fn run(namespace: &str, state: &RunState) -> Result<Self, RuntimeError> {
        let mut snapshot = Self::new(namespace, "run", &state.report.run_id, state.revision)?;
        snapshot.add_run(state, snapshot.root_span_id.clone(), None)?;
        Ok(snapshot)
    }
    fn add_run(
        &mut self,
        state: &RunState,
        id: String,
        parent: Option<String>,
    ) -> Result<(), RuntimeError> {
        state.validate()?;
        let report = &state.report;
        if report.events.len() > MAX_EVENTS {
            return Err(invalid("run exceeds diagnostic event bound"));
        }
        let mut span = DiagnosticSpan::new(
            id.clone(),
            parent,
            SpanKind::Run,
            report.events.first().and_then(|e| e.unix_ms),
            if state.status == RunStatus::Finished {
                report.events.last().and_then(|e| e.unix_ms)
            } else {
                None
            },
            stop_outcome(report.stop_reason),
        );
        span.active_elapsed_ms = Some(report.elapsed_ms);
        span.ended = state.status == RunStatus::Finished;
        span.attr("taskloom.run.revision", state.revision);
        span.attr(
            "taskloom.stop_reason",
            serde_json::to_value(report.stop_reason).map_err(crate::runtime::store::storage)?,
        );
        self.push(span)?;
        if report.stop_reason != StopReason::Completed {
            let stage = match report.stop_reason {
                StopReason::ModelError => "model",
                StopReason::ToolError => "tool",
                StopReason::ProtocolError
                    if report
                        .events
                        .iter()
                        .any(|e| matches!(e.kind, RunEventKind::OutputRejected { .. })) =>
                {
                    "output_validation"
                }
                StopReason::ProtocolError => "protocol",
                StopReason::Budget => "generation_budget",
                StopReason::StorageError | StopReason::StorageLimit => "storage",
                StopReason::ContextLimit => "context",
                StopReason::PermissionDenied | StopReason::ApprovalRequired => "authorization",
                StopReason::Uncertain => "recovery",
                _ => "control",
            };
            self.issues.push(DiagnosticIssue {
                span_id: id.clone(),
                stage: stage.into(),
                code: serde_json::to_value(report.stop_reason)
                    .map_err(crate::runtime::store::storage)?
                    .as_str()
                    .unwrap()
                    .into(),
            });
        }
        self.metrics.generation_usage.add(report.usage);
        if (report.usage.requests == 0
            && report
                .events
                .iter()
                .any(|e| matches!(e.kind, RunEventKind::ModelStarted { .. })))
            || report.budget.uncertain_attempts > 0
            || report.budget.in_flight_attempts > 0
            || matches!(
                state.cursor,
                crate::runtime::RunCursor::ModelInFlight { .. }
            )
        {
            self.metrics.generation_usage_reported = false;
        }
        self.metrics.generation_committed_micros = self
            .metrics
            .generation_committed_micros
            .saturating_add(report.budget.committed_micros);
        self.metrics.generation_uncertain_micros = self
            .metrics
            .generation_uncertain_micros
            .saturating_add(report.budget.uncertain_micros);
        self.metrics.generation_in_flight_micros = self
            .metrics
            .generation_in_flight_micros
            .saturating_add(report.budget.in_flight_micros);
        self.metrics.generation_uncertain_attempts = self
            .metrics
            .generation_uncertain_attempts
            .saturating_add(report.budget.uncertain_attempts);
        let mut model: Option<(usize, u64)> = None;
        let mut tools = BTreeMap::<String, (usize, u64)>::new();
        let mut tool_history = BTreeMap::<String, String>::new();
        let mut approvals = BTreeMap::<String, (usize, u64)>::new();
        for event in &report.events {
            match &event.kind {
                RunEventKind::ModelStarted {
                    step,
                    model: requested,
                } => {
                    self.metrics.model_decisions_started += 1;
                    let mut span = DiagnosticSpan::new(
                        self.span_key(&[&id, "model", &event.sequence.to_string()]),
                        Some(id.clone()),
                        SpanKind::Model,
                        event.unix_ms,
                        None,
                        SpanOutcome::Open,
                    );
                    span.attr("taskloom.model.step", *step as u64);
                    span.attr("taskloom.model.key", key(&[&self.trace_id, requested], 16));
                    model = Some((self.push(span)?, event.elapsed_ms));
                }
                RunEventKind::ModelFinished { .. } => {
                    self.metrics.model_decisions_finished += 1;
                    if let Some((index, start)) = model.take() {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            SpanOutcome::Ok,
                        );
                    }
                }
                RunEventKind::RecoveredModel => {
                    self.metrics.model_recoveries += 1;
                    if let Some((index, start)) = model.take() {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            SpanOutcome::Uncertain,
                        );
                    }
                }
                RunEventKind::ToolStarted { call_id, name } => {
                    self.metrics.tool_attempts += 1;
                    let mut span = DiagnosticSpan::new(
                        self.span_key(&[&id, "tool", &event.sequence.to_string()]),
                        Some(id.clone()),
                        SpanKind::Tool,
                        event.unix_ms,
                        None,
                        SpanOutcome::Open,
                    );
                    span.attr("taskloom.tool.key", key(&[&self.trace_id, name], 16));
                    tool_history.insert(call_id.clone(), span.span_id.clone());
                    tools.insert(call_id.clone(), (self.push(span)?, event.elapsed_ms));
                }
                RunEventKind::ToolFinished { call_id, is_error } => {
                    self.metrics.tool_errors += u64::from(*is_error);
                    if let Some((index, start)) = tools.remove(call_id) {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            if *is_error {
                                SpanOutcome::Error
                            } else {
                                SpanOutcome::Ok
                            },
                        );
                        if !is_error {
                            let found = report.messages.iter().find_map(|m| match m {
                                Message::Tool { result }
                                    if result.call_id == *call_id && !result.is_error =>
                                {
                                    serde_json::from_value::<SearchResult>(result.output.clone())
                                        .ok()
                                }
                                _ => None,
                            });
                            if let Some(found) = found {
                                self.retrieval(index, found)?;
                            }
                        }
                    }
                }
                RunEventKind::RecoveryRequired { call_id } => {
                    if let Some((index, start)) = tools.remove(call_id) {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            SpanOutcome::Uncertain,
                        );
                    }
                }
                RunEventKind::Stopped { reason } => {
                    if let Some((index, start)) = model.take() {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            stop_outcome(*reason),
                        );
                    }
                    for (_, (index, start)) in std::mem::take(&mut tools) {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            SpanOutcome::Uncertain,
                        );
                    }
                }
                RunEventKind::ApprovalRequested { request_id, .. } => {
                    let span = DiagnosticSpan::new(
                        self.span_key(&[&id, "approval", &event.sequence.to_string()]),
                        Some(id.clone()),
                        SpanKind::Approval,
                        event.unix_ms,
                        None,
                        SpanOutcome::Waiting,
                    );
                    approvals.insert(request_id.clone(), (self.push(span)?, event.elapsed_ms));
                }
                RunEventKind::ApprovalDecided {
                    request_id,
                    approved,
                    ..
                } => {
                    if let Some((index, start)) = approvals.remove(request_id) {
                        self.finish(
                            index,
                            event.unix_ms,
                            event.elapsed_ms.saturating_sub(start),
                            if *approved {
                                SpanOutcome::Ok
                            } else {
                                SpanOutcome::Cancelled
                            },
                        );
                        self.spans[index].attr("taskloom.approval.approved", *approved);
                    }
                }
                RunEventKind::ToolReconciled {
                    call_id, action, ..
                } => {
                    let parent = tool_history
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| id.clone());
                    let mut span = DiagnosticSpan::new(
                        self.span_key(&[&id, "recovery", &event.sequence.to_string()]),
                        Some(parent),
                        SpanKind::Recovery,
                        event.unix_ms,
                        event.unix_ms,
                        SpanOutcome::Ok,
                    );
                    span.ended = true;
                    span.active_elapsed_ms = Some(0);
                    span.attr("taskloom.recovery.action", action.clone());
                    self.push(span)?;
                }
                RunEventKind::OutputRejected { step, .. }
                | RunEventKind::OutputValidated { step, .. } => {
                    let rejected = matches!(event.kind, RunEventKind::OutputRejected { .. });
                    if rejected {
                        self.metrics.output_rejections += 1;
                    } else {
                        self.metrics.output_validations += 1;
                    }
                    let mut span = DiagnosticSpan::new(
                        self.span_key(&[&id, "validation", &event.sequence.to_string()]),
                        Some(id.clone()),
                        SpanKind::Validation,
                        event.unix_ms,
                        event.unix_ms,
                        if rejected {
                            SpanOutcome::Error
                        } else {
                            SpanOutcome::Ok
                        },
                    );
                    span.ended = true;
                    span.active_elapsed_ms = Some(0);
                    span.attr("taskloom.model.step", *step as u64);
                    self.push(span)?;
                }
                RunEventKind::ContextPrepared {
                    omitted_messages, ..
                } => self.metrics.omitted_context_messages += *omitted_messages as u64,
                _ => {}
            }
        }
        Ok(())
    }
    fn finish(&mut self, index: usize, end: Option<u64>, elapsed: u64, outcome: SpanOutcome) {
        let span = &mut self.spans[index];
        span.end_unix_ms = end;
        span.ended = true;
        span.active_elapsed_ms = Some(elapsed);
        span.outcome = outcome;
    }
    fn retrieval(&mut self, index: usize, found: SearchResult) -> Result<(), RuntimeError> {
        if found.hits.len() > 20 || found.hits.iter().any(|h| !h.score.is_finite()) {
            return Err(invalid(
                "retrieval observation exceeds bounds or contains invalid scores",
            ));
        }
        let tool = &self.spans[index];
        let mut span = DiagnosticSpan::new(
            self.span_key(&[&tool.span_id, "retrieval"]),
            Some(tool.span_id.clone()),
            SpanKind::Retrieval,
            tool.start_unix_ms,
            tool.end_unix_ms,
            SpanOutcome::Ok,
        );
        span.active_elapsed_ms = tool.active_elapsed_ms;
        span.ended = tool.ended;
        span.attr(
            "taskloom.retrieval.returned_chunks",
            found.hits.len() as u64,
        );
        span.attr(
            "taskloom.retrieval.skipped_oversize",
            found.skipped_oversize as u64,
        );
        self.metrics.retrievals += 1;
        self.metrics.empty_retrievals += u64::from(found.hits.is_empty());
        self.metrics.skipped_oversize_chunks = self
            .metrics
            .skipped_oversize_chunks
            .saturating_add(found.skipped_oversize as u64);
        self.retrievals.push(RetrievalDiagnostic {
            span_id: span.span_id.clone(),
            corpus_revision: found.corpus_revision,
            returned_chunks: found.hits.len(),
            skipped_oversize: found.skipped_oversize,
            scores: found
                .hits
                .iter()
                .map(|h| (key(&[&self.trace_id, &h.chunk.id], 16), h.score))
                .collect(),
            hits: found
                .hits
                .iter()
                .map(|h| RetrievalHit {
                    document_id: h.chunk.document_id.clone(),
                    chunk_id: h.chunk.id.clone(),
                    title: h.chunk.title.clone(),
                    start: h.chunk.start,
                    end: h.chunk.end,
                    score: h.score,
                })
                .collect(),
        });
        self.push(span)?;
        Ok(())
    }
    pub fn workflow(
        namespace: &str,
        state: &WorkflowState,
        children: &[RunState],
    ) -> Result<Self, RuntimeError> {
        state.validate()?;
        if state.events.len() > MAX_EVENTS || children.len() > MAX_CHILDREN {
            return Err(invalid("workflow diagnostic input exceeds bounds"));
        }
        let mut snapshot = Self::new(namespace, "workflow", &state.id, state.revision)?;
        let mut root = DiagnosticSpan::new(
            snapshot.root_span_id.clone(),
            None,
            SpanKind::Workflow,
            state.events.first().and_then(|e| e.unix_ms),
            if state.status.is_terminal() {
                state.events.last().and_then(|e| e.unix_ms)
            } else {
                None
            },
            workflow_outcome(state.status),
        );
        root.active_elapsed_ms = Some(state.elapsed_ms);
        root.ended = state.status.is_terminal();
        root.attr("taskloom.workflow.dispatches", state.dispatches as u64);
        snapshot.push(root)?;
        let mut used = BTreeSet::new();
        for node in &state.nodes {
            let id = snapshot.span_key(&["node", &node.id]);
            let events = state
                .events
                .iter()
                .filter(|e| node_id(&e.kind) == Some(node.id.as_str()))
                .collect::<Vec<_>>();
            let outcome = match node.status {
                NodeStatus::Complete => SpanOutcome::Ok,
                NodeStatus::Failed => SpanOutcome::Error,
                NodeStatus::Skipped => SpanOutcome::Skipped,
                NodeStatus::Uncertain => SpanOutcome::Uncertain,
                NodeStatus::Waiting => SpanOutcome::Waiting,
                _ => SpanOutcome::Open,
            };
            let terminal = matches!(
                node.status,
                NodeStatus::Complete | NodeStatus::Failed | NodeStatus::Skipped
            );
            let mut node_span = DiagnosticSpan::new(
                id.clone(),
                Some(snapshot.root_span_id.clone()),
                SpanKind::Node,
                events.first().and_then(|e| e.unix_ms),
                if terminal {
                    events.last().and_then(|e| e.unix_ms)
                } else {
                    None
                },
                outcome,
            );
            node_span.ended = terminal;
            snapshot.push(node_span)?;
            let mut attempt = None;
            for e in events {
                if let WorkflowEventKind::NodeStarted { dispatch, .. } = &e.kind {
                    let mut span = DiagnosticSpan::new(
                        snapshot.span_key(&[&id, "dispatch", &dispatch.to_string()]),
                        Some(id.clone()),
                        SpanKind::NodeAttempt,
                        e.unix_ms,
                        None,
                        SpanOutcome::Open,
                    );
                    span.attr("taskloom.node.dispatch", *dispatch as u64);
                    attempt = Some((snapshot.push(span)?, e.elapsed_ms));
                } else if let Some((index, start)) = attempt.take() {
                    snapshot.finish(
                        index,
                        e.unix_ms,
                        e.elapsed_ms.saturating_sub(start),
                        match e.kind {
                            WorkflowEventKind::NodeCompleted { .. } => SpanOutcome::Ok,
                            WorkflowEventKind::NodeWaiting { .. } => SpanOutcome::Waiting,
                            WorkflowEventKind::NodeUncertain { .. }
                            | WorkflowEventKind::Recovered { .. } => SpanOutcome::Uncertain,
                            _ => SpanOutcome::Error,
                        },
                    );
                }
            }
            if let Some(reference) = workflow_run_id(state, node) {
                if let Some(child) = children.iter().find(|r| r.report.run_id == reference) {
                    if !used.insert(reference) {
                        return Err(invalid("duplicate workflow child"));
                    }
                    let context = node
                        .context
                        .as_ref()
                        .ok_or_else(|| invalid("child has no persisted node context"))?;
                    let expected =
                        serde_json::to_string(context).map_err(crate::runtime::store::storage)?;
                    if !matches!(child.report.messages.get(child.initial_message_count.saturating_sub(1)),Some(Message::User{content}) if *content==expected)
                    {
                        return Err(invalid(
                            "child input differs from persisted workflow context",
                        ));
                    }
                    snapshot.add_run(child, snapshot.span_key(&[&id, "run"]), Some(id))?;
                } else {
                    snapshot.missing_children += 1;
                }
            }
        }
        if used.len() != children.len() {
            return Err(invalid("unreferenced or duplicate child state"));
        }
        snapshot.metrics.saved_run_observations_complete = snapshot.missing_children == 0;
        if snapshot.missing_children > 0 {
            snapshot.metrics.generation_usage_reported = false;
        }
        snapshot.source_revision_key = key(
            &[&snapshot.source_revision_key, &children_key(children)?],
            16,
        );
        Ok(snapshot)
    }
    pub fn session(
        namespace: &str,
        session: &Session,
        children: &[RunState],
    ) -> Result<Self, RuntimeError> {
        session.validate()?;
        if children.len() > MAX_CHILDREN || session.turns.len() > MAX_CHILDREN {
            return Err(invalid("session diagnostic input exceeds bounds"));
        }
        let mut snapshot = Self::new(namespace, "session", &session.id, session.revision)?;
        snapshot.push(DiagnosticSpan::new(
            snapshot.root_span_id.clone(),
            None,
            SpanKind::Session,
            None,
            None,
            SpanOutcome::Open,
        ))?;
        let mut used = BTreeSet::new();
        for (binding, closed) in session
            .turns
            .iter()
            .map(|t| (&t.binding, Some(t)))
            .chain(session.active.iter().map(|t| (t, None)))
        {
            let Some(child) = children.iter().find(|r| r.report.run_id == binding.run_id) else {
                snapshot.missing_children += 1;
                continue;
            };
            if !used.insert(binding.run_id.as_str()) {
                return Err(invalid("duplicate session child"));
            }
            child.validate()?;
            if child.initial_message_count != binding.base_message_count + 1
                || child.report.messages.get(..binding.base_message_count)
                    != session.messages.get(..binding.base_message_count)
                || !matches!(child.report.messages.get(binding.base_message_count),Some(Message::User{content}) if *content==binding.input)
                || closed.is_some_and(|t| {
                    fingerprint(&child.report).ok().as_ref() != Some(&t.report_fingerprint)
                })
            {
                return Err(invalid("child journal differs from session binding"));
            }
            let id = snapshot.span_key(&["turn", &binding.turn_id]);
            let start = child.report.events.first().and_then(|e| e.unix_ms);
            let end = closed.and_then(|_| child.report.events.last().and_then(|e| e.unix_ms));
            let mut turn_span = DiagnosticSpan::new(
                id.clone(),
                Some(snapshot.root_span_id.clone()),
                SpanKind::Turn,
                start,
                end,
                stop_outcome(child.report.stop_reason),
            );
            turn_span.ended = closed.is_some();
            snapshot.push(turn_span)?;
            snapshot.add_run(child, snapshot.span_key(&[&id, "run"]), Some(id))?;
        }
        if used.len() != children.len() {
            return Err(invalid("unreferenced session child state"));
        }
        snapshot.metrics.saved_run_observations_complete = snapshot.missing_children == 0;
        if snapshot.missing_children > 0 {
            snapshot.metrics.generation_usage_reported = false;
        }
        snapshot.spans[0].start_unix_ms =
            snapshot.spans.iter().filter_map(|s| s.start_unix_ms).min();
        snapshot.source_revision_key = key(
            &[&snapshot.source_revision_key, &children_key(children)?],
            16,
        );
        Ok(snapshot)
    }
    pub fn billing_boundaries(&self) -> Value {
        json!({"generation":"saved run checkpoint budget/usage; a live reservation journal may be newer; micro-units use the configured price-table currency","embedding":"independent host embedding ledger; per-run attribution unavailable","tool_services":"not reported by generic tool protocol; unknown, not zero"})
    }
}
/// Includes a running durable child's frozen context before its first outcome
/// has returned to the parent. Pure/manual nodes never acquire guessed run IDs.
pub fn workflow_run_id<'a>(state: &'a WorkflowState, node: &'a NodeState) -> Option<&'a str> {
    node.child.as_ref().map(|c| c.run_id.as_str()).or_else(|| {
        let spec = state.definition.nodes.iter().find(|s| s.id == node.id)?;
        let task = state.tasks.iter().find(|t| t.name == spec.task)?;
        if task.recovery == TaskRecovery::DurableChild {
            node.context.as_ref().map(|c| c.run_id.as_str())
        } else {
            None
        }
    })
}
fn children_key(children: &[RunState]) -> Result<String, RuntimeError> {
    let mut revisions = children
        .iter()
        .map(|r| (&r.report.run_id, r.revision))
        .collect::<Vec<_>>();
    revisions.sort();
    fingerprint(&revisions)
}
fn node_id(kind: &WorkflowEventKind) -> Option<&str> {
    match kind {
        WorkflowEventKind::NodeStarted { node, .. }
        | WorkflowEventKind::NodeCompleted { node }
        | WorkflowEventKind::NodeSkipped { node }
        | WorkflowEventKind::NodeWaiting { node, .. }
        | WorkflowEventKind::NodeFailed { node }
        | WorkflowEventKind::NodeUncertain { node }
        | WorkflowEventKind::NodeReconciled { node, .. }
        | WorkflowEventKind::Recovered { node } => Some(node),
        _ => None,
    }
}
