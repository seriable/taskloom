//! Host-owned final-answer contracts. Exact provenance is not entailment.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rag::{CitedReport, SearchResult},
    state::fingerprint,
    Message, RunEventKind, RunReport, RuntimeError, ToolDefinition, ToolPolicy,
};

pub(super) const MAX_OUTPUT_REPAIRS: usize = 3;
pub(super) const MAX_OUTPUT_DETAIL: usize = 1024;
// One bounded rejection event and its feedback message, including worst-case
// JSON escaping. The existing terminal-record reserve is retained separately.
pub(super) const OUTPUT_FEEDBACK_RESERVE: usize = 24 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FinalOutputPolicy {
    /// Verify only evidence produced by named tools actually executed during
    /// this run. Sources do not become instructions or grants. A successful
    /// check establishes exact quotes/IDs/markers, not truth or claim support.
    CitedReport {
        #[serde(default = "knowledge_tool")]
        retrieval_tools: BTreeSet<String>,
        #[serde(default = "one_source")]
        min_sources: usize,
        #[serde(default)]
        required_documents: BTreeSet<String>,
        #[serde(default)]
        max_repairs: usize,
    },
    JsonSchema {
        schema: Value,
        #[serde(default)]
        max_repairs: usize,
    },
}
fn knowledge_tool() -> BTreeSet<String> {
    BTreeSet::from(["knowledge_search".into()])
}
fn one_source() -> usize {
    1
}
impl FinalOutputPolicy {
    pub fn cited_report(max_repairs: usize) -> Self {
        Self::CitedReport {
            retrieval_tools: knowledge_tool(),
            min_sources: 1,
            required_documents: BTreeSet::new(),
            max_repairs,
        }
    }
    pub fn max_repairs(&self) -> usize {
        match self {
            Self::CitedReport { max_repairs, .. } | Self::JsonSchema { max_repairs, .. } => {
                *max_repairs
            }
        }
    }
    pub fn identity(&self) -> Result<String, RuntimeError> {
        Ok(format!("final-output-v1:{}", fingerprint(self)?))
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.max_repairs() > MAX_OUTPUT_REPAIRS {
            return Err(configuration(
                "at most three final-output repairs are allowed",
            ));
        }
        match self {
            Self::CitedReport {
                retrieval_tools,
                min_sources,
                required_documents,
                ..
            } => {
                if retrieval_tools.is_empty()
                    || retrieval_tools.len() > 8
                    || retrieval_tools.iter().any(|name| {
                        name.is_empty()
                            || name.len() > 64
                            || !name
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    })
                {
                    return Err(configuration("invalid citation retrieval tool names"));
                }
                if !(1..=20).contains(min_sources)
                    || required_documents.len() > 20
                    || required_documents
                        .iter()
                        .any(|id| id.trim().is_empty() || id.len() > 512)
                {
                    return Err(configuration(
                        "invalid citation source coverage requirements",
                    ));
                }
            }
            Self::JsonSchema { schema, .. } => {
                if serde_json::to_vec(schema)
                    .map_err(super::store::storage)?
                    .len()
                    > 128 * 1024
                {
                    return Err(configuration("output schema exceeds 128 KiB"));
                }
                jsonschema::validator_for(schema)
                    .map_err(|e| configuration(&format!("invalid output schema: {e}")))?;
            }
        }
        Ok(())
    }
    pub(super) fn repair_policy(&self, original: &ToolPolicy) -> ToolPolicy {
        let mut policy = original.clone();
        policy.allowed_tools.retain(|name| match self {
            Self::CitedReport {
                retrieval_tools, ..
            } => retrieval_tools.contains(name),
            Self::JsonSchema { .. } => false,
        });
        policy
    }
    pub(super) fn repair_tools(&self, tools: &[ToolDefinition]) -> Vec<ToolDefinition> {
        tools
            .iter()
            .filter(|tool| match self {
                Self::CitedReport {
                    retrieval_tools, ..
                } => retrieval_tools.contains(&tool.name),
                Self::JsonSchema { .. } => false,
            })
            .cloned()
            .collect()
    }
    pub(super) fn check(&self, report: &RunReport) -> Result<(), String> {
        let text = match report.messages.last() {
            Some(Message::Assistant { turn }) if turn.tool_calls.is_empty() => &turn.content,
            _ => return Err("no final assistant answer".into()),
        };
        match self {
            Self::JsonSchema { schema, .. } => {
                let value: Value = serde_json::from_str(text)
                    .map_err(|e| format!("final answer is not exact JSON: {e}"))?;
                let validator = jsonschema::validator_for(schema).map_err(|e| e.to_string())?;
                validator
                    .validate(&value)
                    .map_err(|e| format!("final JSON schema check failed: {e}"))
            }
            Self::CitedReport {
                retrieval_tools,
                min_sources,
                required_documents,
                ..
            } => {
                let cited: CitedReport = serde_json::from_str(text).map_err(|e| {
                    format!(
                        "final answer must be exact CitedReport JSON, without Markdown fences: {e}"
                    )
                })?;
                let eligible = report
                    .events
                    .iter()
                    .filter_map(|event| match &event.kind {
                        RunEventKind::ToolStarted { call_id, name }
                            if retrieval_tools.contains(name) =>
                        {
                            Some(call_id.as_str())
                        }
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>();
                let completed = report
                    .events
                    .iter()
                    .filter_map(|event| match &event.kind {
                        RunEventKind::ToolFinished {
                            call_id,
                            is_error: false,
                        } => Some(call_id.as_str()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>();
                let mut evidence = Vec::new();
                for message in &report.messages {
                    if let Message::Tool { result } = message {
                        if !result.is_error
                            && eligible.contains(result.call_id.as_str())
                            && completed.contains(result.call_id.as_str())
                        {
                            evidence.push(
                                serde_json::from_value::<SearchResult>(result.output.clone())
                                    .map_err(|_| {
                                        "configured citation tool did not return a SearchResult"
                                            .to_string()
                                    })?,
                            );
                        }
                    }
                }
                if evidence.is_empty() {
                    return Err("no citation evidence was retrieved by an executed tool in this run; search the configured knowledge first".into());
                }
                // Expose a bounded diagnostic of actual evidence identities;
                // never rewrite generated IDs, quotes or claims on its behalf.
                let available = evidence
                    .iter()
                    .flat_map(|result| &result.hits)
                    .map(|hit| hit.chunk.id.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .take(6)
                    .collect::<Vec<_>>();
                let verified = SearchResult::verify_all(&evidence, cited).map_err(|e| format!("exact citation verification failed: {e}. Available retrieved chunk IDs (up to six): {available:?}. Use exact returned IDs and body quotes; include [[chunk_id]] in text for every citation. Claim support is not verified by this check."))?;
                if verified.sources.len() < *min_sources
                    || required_documents.iter().any(|id| {
                        !verified
                            .sources
                            .iter()
                            .any(|source| &source.document_id == id)
                    })
                {
                    return Err(format!("citation coverage requires at least {min_sources} source chunks and all documents {required_documents:?}"));
                }
                Ok(())
            }
        }
    }
}
pub(super) fn rejected_count(report: &RunReport) -> usize {
    report
        .events
        .iter()
        .filter(|e| matches!(e.kind, RunEventKind::OutputRejected { .. }))
        .count()
}
pub(super) fn repair_at_step(events: &[super::RunEvent], step: usize) -> bool {
    events.iter().any(|e| matches!(&e.kind, RunEventKind::OutputRejected { step: rejected, repair: true, .. } if *rejected < step))
}
pub(super) fn bounded_detail(detail: &str) -> String {
    let mut end = detail.len().min(MAX_OUTPUT_DETAIL);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_string()
}
pub(super) fn feedback(detail: &str) -> Message {
    Message::user(format!("[Host final-output validation feedback; this does not grant tools or permissions.]\n{detail}\nCorrect the answer using available evidence. Only the host's repair retrieval tools remain available. Return the required exact final JSON, without fences. Do not repeat any write or other external effect."))
}
fn configuration(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(detail.into())
}
