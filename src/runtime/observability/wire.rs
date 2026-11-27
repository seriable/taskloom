use serde::Serialize;
use serde_json::{json, Value};

use super::{invalid, DiagnosticSpan, SpanKind, SpanOutcome, TraceSnapshot};
use crate::runtime::RuntimeError;

#[derive(Debug, Clone, Serialize)]
pub struct OtlpTracePayload {
    pub body: Value,
    pub exported_spans: usize,
    pub omitted_open: usize,
    pub omitted_without_clock: usize,
    pub omitted_invalid_clock: usize,
}
fn service(name: &str) -> Result<Value, RuntimeError> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(invalid("service name needs 1..=128 printable bytes"));
    }
    Ok(
        json!({"attributes":[{"key":"service.name","value":{"stringValue":name}},{"key":"telemetry.sdk.name","value":{"stringValue":"taskloom-journal"}},{"key":"telemetry.sdk.version","value":{"stringValue":env!("CARGO_PKG_VERSION")}}]}),
    )
}
fn valid_id(id: &str, bytes: usize) -> bool {
    id.len() == 2 * bytes
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && !id.bytes().all(|b| b == b'0')
}
fn name(kind: SpanKind) -> &'static str {
    match kind {
        SpanKind::Session => "taskloom.session",
        SpanKind::Turn => "taskloom.session.turn",
        SpanKind::Workflow => "taskloom.workflow",
        SpanKind::Node => "taskloom.workflow.node",
        SpanKind::NodeAttempt => "taskloom.workflow.dispatch",
        SpanKind::Run => "taskloom.run",
        SpanKind::Model => "taskloom.model",
        SpanKind::Tool => "taskloom.tool",
        SpanKind::Retrieval => "taskloom.retrieval",
        SpanKind::Approval => "taskloom.approval.wait",
        SpanKind::Recovery => "taskloom.tool.reconciliation",
        SpanKind::Validation => "taskloom.output.validation",
    }
}
fn outcome(outcome: SpanOutcome) -> &'static str {
    match outcome {
        SpanOutcome::Open => "open",
        SpanOutcome::Ok => "ok",
        SpanOutcome::Error => "error",
        SpanOutcome::Waiting => "waiting",
        SpanOutcome::Uncertain => "uncertain",
        SpanOutcome::Cancelled => "cancelled",
        SpanOutcome::Skipped => "skipped",
    }
}
fn signed(value: u64) -> Result<String, RuntimeError> {
    i64::try_from(value)
        .map(|v| v.to_string())
        .map_err(|_| invalid("OTLP signed integer range exceeded"))
}
fn attributes(span: &DiagnosticSpan) -> Result<Vec<Value>, RuntimeError> {
    let mut result =
        vec![json!({"key":"taskloom.outcome","value":{"stringValue":outcome(span.outcome)}})];
    for (key, value) in &span.attributes {
        // Keep the wire boundary safe even if an SDK caller modifies a public
        // diagnostic snapshot before export. No free-form attribute forwarding.
        let value = match key.as_str() {
            "taskloom.model.key" | "taskloom.tool.key" => {
                let value = value
                    .as_str()
                    .filter(|v| valid_id(v, 16))
                    .ok_or_else(|| invalid("invalid pseudonymous attribute"))?;
                json!({"stringValue":value})
            }
            "taskloom.stop_reason" => {
                let reason: crate::runtime::StopReason = serde_json::from_value(value.clone())
                    .map_err(|_| invalid("invalid stop reason"))?;
                json!({"stringValue":serde_json::to_value(reason).map_err(crate::runtime::store::storage)?})
            }
            "taskloom.approval.approved" => {
                json!({"boolValue":value.as_bool().ok_or_else(||invalid("invalid approval attribute"))?})
            }
            "taskloom.recovery.action" => {
                let action = value
                    .as_str()
                    .filter(|v| matches!(*v, "record_result" | "retry_safe" | "abort"))
                    .ok_or_else(|| invalid("invalid recovery attribute"))?;
                json!({"stringValue":action})
            }
            "taskloom.run.revision"
            | "taskloom.workflow.dispatches"
            | "taskloom.node.dispatch"
            | "taskloom.model.step"
            | "taskloom.retrieval.returned_chunks"
            | "taskloom.retrieval.skipped_oversize" => {
                json!({"intValue":signed(value.as_u64().ok_or_else(||invalid("noninteger diagnostic attribute"))?)?})
            }
            _ => return Err(invalid("attribute is not on the telemetry allowlist")),
        };
        result.push(json!({"key":key,"value":value}));
    }
    if let Some(elapsed) = span.active_elapsed_ms {
        result.push(
            json!({"key":"taskloom.active_elapsed_ms","value":{"intValue":signed(elapsed)?}}),
        );
    }
    Ok(result)
}
impl TraceSnapshot {
    /// OTLP/HTTP JSON (hex IDs, decimal-string 64-bit integers). Missing or
    /// backwards wall clocks are counted, never synthesized from relative time.
    /// A live/open span stays local until an actual end event exists.
    pub fn otlp_traces(&self, service_name: &str) -> Result<OtlpTracePayload, RuntimeError> {
        if !valid_id(&self.trace_id, 16) || self.spans.len() > 4096 {
            return Err(invalid("invalid trace identity or size"));
        }
        let mut result = OtlpTracePayload {
            body: Value::Null,
            exported_spans: 0,
            omitted_open: 0,
            omitted_without_clock: 0,
            omitted_invalid_clock: 0,
        };
        let mut spans = Vec::new();
        let ids = self
            .spans
            .iter()
            .map(|s| s.span_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if ids.len() != self.spans.len() {
            return Err(invalid("duplicate span identity"));
        }
        for span in &self.spans {
            if !valid_id(&span.span_id, 8)
                || span.parent_span_id.as_ref().is_some_and(|id| {
                    !valid_id(id, 8) || !ids.contains(id.as_str()) || *id == span.span_id
                })
            {
                return Err(invalid("invalid span or parent identity"));
            }
            let attributes = attributes(span)?;
            if !span.ended {
                result.omitted_open += 1;
                continue;
            }
            let Some(start) = span.start_unix_ms else {
                result.omitted_without_clock += 1;
                continue;
            };
            let Some(end) = span.end_unix_ms else {
                result.omitted_without_clock += 1;
                continue;
            };
            let (Some(start_ns), Some(end_ns)) =
                (start.checked_mul(1_000_000), end.checked_mul(1_000_000))
            else {
                result.omitted_invalid_clock += 1;
                continue;
            };
            if start == 0 || end < start {
                result.omitted_invalid_clock += 1;
                continue;
            }
            let mut value = json!({"traceId":self.trace_id,"spanId":span.span_id,"name":name(span.kind),"kind":if span.kind==SpanKind::Model {3} else {1},"startTimeUnixNano":start_ns.to_string(),"endTimeUnixNano":end_ns.to_string(),"attributes":attributes,"status":{"code":if span.outcome==SpanOutcome::Error {2} else if span.outcome==SpanOutcome::Ok {1} else {0}}});
            if let Some(parent) = &span.parent_span_id {
                value["parentSpanId"] = json!(parent);
            }
            spans.push(value);
        }
        result.exported_spans = spans.len();
        result.body = json!({"resourceSpans":[{"resource":service(service_name)?,"scopeSpans":[{"scope":{"name":"taskloom.journal","version":"1"},"spans":spans}]}]});
        Ok(result)
    }
    /// Per-trace journal observations, exported as gauges. They are neither
    /// process-wide totals nor interval deltas, and parent/child usage is counted
    /// once. Unknown embedding/tool-service costs are omitted rather than zero.
    pub fn otlp_metrics(&self, service_name: &str) -> Result<Value, RuntimeError> {
        if !valid_id(&self.trace_id, 16) {
            return Err(invalid("invalid trace identity"));
        }
        let Some(time) = self.captured_unix_ms.and_then(|t| t.checked_mul(1_000_000)) else {
            return Err(invalid("metric observation has no valid wall clock"));
        };
        let m = &self.metrics;
        let fields = [
            ("model.decisions_started", m.model_decisions_started),
            ("model.decisions_finished", m.model_decisions_finished),
            ("tool.attempts", m.tool_attempts),
            ("tool.errors", m.tool_errors),
            ("model.recoveries", m.model_recoveries),
            ("output.rejections", m.output_rejections),
            ("output.validations", m.output_validations),
            ("context.omitted_messages", m.omitted_context_messages),
            ("retrieval.calls", m.retrievals),
            ("retrieval.empty", m.empty_retrievals),
            (
                "retrieval.skipped_oversize_chunks",
                m.skipped_oversize_chunks,
            ),
            ("generation.input_tokens", m.generation_usage.input_tokens),
            ("generation.output_tokens", m.generation_usage.output_tokens),
            ("generation.requests", m.generation_usage.requests),
            (
                "generation.committed_microunits",
                m.generation_committed_micros,
            ),
            (
                "generation.uncertain_microunits",
                m.generation_uncertain_micros,
            ),
            (
                "generation.in_flight_microunits",
                m.generation_in_flight_micros,
            ),
            (
                "generation.uncertain_attempts",
                m.generation_uncertain_attempts,
            ),
        ];
        let mut metrics = Vec::new();
        for (field, value) in fields {
            if field.starts_with("generation.") && !m.saved_run_observations_complete {
                continue;
            }
            if matches!(
                field,
                "generation.input_tokens" | "generation.output_tokens" | "generation.requests"
            ) && !m.generation_usage_reported
            {
                continue;
            }
            metrics.push(json!({"name":format!("taskloom.{field}"),"description":"Per-trace validated journal snapshot; generation excludes embedding and tool services","gauge":{"dataPoints":[{"attributes":[{"key":"taskloom.trace_id","value":{"stringValue":self.trace_id}},{"key":"taskloom.observation.complete","value":{"boolValue":m.saved_run_observations_complete}},{"key":"taskloom.generation.usage_reported","value":{"boolValue":m.generation_usage_reported}}],"timeUnixNano":time.to_string(),"asInt":signed(value)?}]}}));
        }
        Ok(
            json!({"resourceMetrics":[{"resource":service(service_name)?,"scopeMetrics":[{"scope":{"name":"taskloom.journal","version":"1"},"metrics":metrics}]}]}),
        )
    }
}
