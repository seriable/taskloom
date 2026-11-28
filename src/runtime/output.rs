//! Host-owned final-answer contracts. Exact provenance is not entailment.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rag::{
        CitedReport, LocatedReport, ResolvedLocatedReport, SearchResult, SourceSelectionReport,
        VerifiedReport,
    },
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
        /// Optional additional schema for the original model JSON. It can
        /// require task sections/fields, but does not establish semantic truth.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_schema: Option<Value>,
    },
    /// A separate model contract with `[[1]]` markers and exact retrieved
    /// document/revision/start locators. Canonical rendering is host-derived;
    /// the original assistant message is never rewritten.
    LocatedCitedReport {
        #[serde(default = "knowledge_tool")]
        retrieval_tools: BTreeSet<String>,
        #[serde(default = "one_source")]
        min_sources: usize,
        #[serde(default)]
        required_documents: BTreeSet<String>,
        #[serde(default)]
        max_repairs: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_schema: Option<Value>,
    },
    /// Model selects exact retrieved chunk IDs; canonical quotes are full
    /// source bodies generated explicitly by the host. No model quote repair.
    SelectedSources {
        #[serde(default = "knowledge_tool")]
        retrieval_tools: BTreeSet<String>,
        #[serde(default = "one_source")]
        min_sources: usize,
        #[serde(default)]
        required_documents: BTreeSet<String>,
        #[serde(default)]
        max_repairs: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_schema: Option<Value>,
        /// Host renders an answer-level source list; inline markers are not
        /// required. Existing supplied markers still require exact sources.
        #[serde(default, skip_serializing_if = "is_false")]
        bibliography: bool,
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
fn is_false(value: &bool) -> bool {
    !value
}
impl FinalOutputPolicy {
    pub fn cited_report(max_repairs: usize) -> Self {
        Self::CitedReport {
            retrieval_tools: knowledge_tool(),
            min_sources: 1,
            required_documents: BTreeSet::new(),
            max_repairs,
            answer_schema: None,
        }
    }
    pub fn located_cited_report(max_repairs: usize) -> Self {
        Self::LocatedCitedReport {
            retrieval_tools: knowledge_tool(),
            min_sources: 1,
            required_documents: BTreeSet::new(),
            max_repairs,
            answer_schema: None,
        }
    }
    pub fn selected_sources(max_repairs: usize) -> Self {
        Self::SelectedSources {
            retrieval_tools: knowledge_tool(),
            min_sources: 1,
            required_documents: BTreeSet::new(),
            max_repairs,
            answer_schema: None,
            bibliography: false,
        }
    }
    pub fn with_host_bibliography(mut self) -> Result<Self, RuntimeError> {
        if let Self::SelectedSources { bibliography, .. } = &mut self {
            *bibliography = true;
            Ok(self)
        } else {
            Err(configuration(
                "host bibliography requires the explicit selected_sources contract",
            ))
        }
    }
    pub fn citation_tools(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::CitedReport {
                retrieval_tools, ..
            }
            | Self::LocatedCitedReport {
                retrieval_tools, ..
            }
            | Self::SelectedSources {
                retrieval_tools, ..
            } => Some(retrieval_tools),
            Self::JsonSchema { .. } => None,
        }
    }
    /// Add structural task requirements to a citation contract. Both checks
    /// must pass in the same durable run; repairs keep the original tool grants.
    pub fn with_answer_schema(mut self, schema: Value) -> Result<Self, RuntimeError> {
        match &mut self {
            Self::CitedReport { answer_schema, .. }
            | Self::LocatedCitedReport { answer_schema, .. }
            | Self::SelectedSources { answer_schema, .. } => *answer_schema = Some(schema),
            Self::JsonSchema { .. } => {
                return Err(configuration("answer_schema requires a citation contract"))
            }
        }
        self.validate()?;
        Ok(self)
    }
    fn answer_schema(&self) -> Option<&Value> {
        match self {
            Self::CitedReport { answer_schema, .. }
            | Self::LocatedCitedReport { answer_schema, .. }
            | Self::SelectedSources { answer_schema, .. } => answer_schema.as_ref(),
            Self::JsonSchema { .. } => None,
        }
    }
    fn check_answer_schema(&self, report: &RunReport) -> Result<(), String> {
        if let Some(schema) = self.answer_schema() {
            let value: Value = serde_json::from_str(final_text(report)?)
                .map_err(|e| format!("final answer is not exact JSON: {e}"))?;
            jsonschema::validator_for(schema)
                .map_err(|e| e.to_string())?
                .validate(&value)
                .map_err(|e| {
                    format!("cited answer schema rejected the original model JSON at {} (schema {}): {}",e.instance_path,e.schema_path,e.masked())
                })?;
        }
        Ok(())
    }
    pub fn max_repairs(&self) -> usize {
        match self {
            Self::CitedReport { max_repairs, .. }
            | Self::LocatedCitedReport { max_repairs, .. }
            | Self::SelectedSources { max_repairs, .. }
            | Self::JsonSchema { max_repairs, .. } => *max_repairs,
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
        if let Some(schema) = self.answer_schema() {
            validate_schema(schema)?;
        }
        match self {
            Self::CitedReport {
                retrieval_tools,
                min_sources,
                required_documents,
                ..
            }
            | Self::LocatedCitedReport {
                retrieval_tools,
                min_sources,
                required_documents,
                ..
            }
            | Self::SelectedSources {
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
                validate_schema(schema)?;
            }
        }
        Ok(())
    }
    pub(super) fn repair_policy(&self, original: &ToolPolicy) -> ToolPolicy {
        let mut policy = original.clone();
        policy.allowed_tools.retain(|name| match self {
            Self::CitedReport {
                retrieval_tools, ..
            }
            | Self::LocatedCitedReport {
                retrieval_tools, ..
            }
            | Self::SelectedSources {
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
                }
                | Self::LocatedCitedReport {
                    retrieval_tools, ..
                }
                | Self::SelectedSources {
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
                validator.validate(&value).map_err(|e| {
                    format!(
                        "final JSON schema check failed at {} (schema {}): {}",
                        e.instance_path,
                        e.schema_path,
                        e.masked()
                    )
                })
            }
            Self::CitedReport { .. }
            | Self::LocatedCitedReport { .. }
            | Self::SelectedSources { .. } => self.verified_citations(report).map(|_| ()),
        }
    }
    fn coverage(&self, verified: &VerifiedReport) -> Result<(), String> {
        let (min_sources, required_documents) = match self {
            Self::CitedReport {
                min_sources,
                required_documents,
                ..
            }
            | Self::LocatedCitedReport {
                min_sources,
                required_documents,
                ..
            }
            | Self::SelectedSources {
                min_sources,
                required_documents,
                ..
            } => (min_sources, required_documents),
            _ => return Err("output contract is not a citation contract".into()),
        };
        let missing = required_documents
            .iter()
            .filter(|id| {
                !verified
                    .sources
                    .iter()
                    .any(|source| &source.document_id == *id)
            })
            .collect::<Vec<_>>();
        if verified.sources.len() < *min_sources || !missing.is_empty() {
            let shown = missing.iter().take(6).collect::<Vec<_>>();
            return Err(format!("citation coverage has {} source chunks, requires at least {min_sources}; {} missing documents (up to six shown): {shown:?}",verified.sources.len(),missing.len()));
        }
        Ok(())
    }
    pub fn resolve_located(&self, report: &RunReport) -> Result<ResolvedLocatedReport, String> {
        if !matches!(self, Self::LocatedCitedReport { .. }) {
            return Err("output contract is not a located report".into());
        }
        let evidence = executed_evidence(report, self.citation_tools().unwrap())?;
        let located: LocatedReport = serde_json::from_str(final_text(report)?).map_err(|e| {
            format!("final answer must be exact LocatedReport JSON, without fences: {e}. Return ONE complete object containing BOTH text (string) and citations (array of document_id, corpus_revision, start, quote objects). Keep all correct fields while repairing the defective field; section headings belong inside text, never as new top-level keys.")
        })?;
        let available=serde_json::Value::Array(evidence.iter().flat_map(|r|r.hits.iter().map(move|h|serde_json::json!({"document_id":h.chunk.document_id,"corpus_revision":r.corpus_revision,"start":h.chunk.start}))).take(6).collect());
        let resolved=SearchResult::verify_located_all(&evidence,located).map_err(|e|format!("exact source-locator verification failed: {e}. Available locators (up to six): {available}. Put [[1]], [[2]], ... IN text for citation-array order. Copy quote only from chunk.text, without a source/title prefix. This does not verify claim support."))?;
        self.coverage(&resolved.verified)?;
        self.check_answer_schema(report)?;
        Ok(resolved)
    }
    pub fn verified_citations(&self, report: &RunReport) -> Result<VerifiedReport, String> {
        if matches!(self, Self::LocatedCitedReport { .. }) {
            return Ok(self.resolve_located(report)?.verified);
        }
        let tools = self
            .citation_tools()
            .ok_or("output contract is not a citation contract")?;
        let evidence = executed_evidence(report, tools)?;
        if matches!(self, Self::SelectedSources { .. }) {
            let selected:SourceSelectionReport=serde_json::from_str(final_text(report)?)
                .map_err(|e|format!("final answer must be SourceSelectionReport JSON with BOTH text and sources (array of exact retrieved chunk IDs): {e}. Do not add quote/citations fields; full source-body quotes are generated by the host."))?;
            let available = evidence
                .iter()
                .flat_map(|r| r.hits.iter().map(|h| h.chunk.id.as_str()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(6)
                .collect::<Vec<_>>();
            let bibliography = matches!(
                self,
                Self::SelectedSources {
                    bibliography: true,
                    ..
                }
            );
            let verified=if bibliography {SearchResult::render_selection_bibliography_all(&evidence,selected)}else{SearchResult::verify_selection_all(&evidence,selected)}
                .map_err(|e|format!("exact source selection failed: {e}. Available IDs (up to six): {available:?}. Select only actual results from your own executed retrieval. {} This does not verify semantic support.",if bibliography{"The host renders an answer-level source list; supplied markers must still match selected sources."}else{"Every selected ID needs its [[full_chunk_id]] text marker."}))?;
            self.coverage(&verified)?;
            self.check_answer_schema(report)?;
            return Ok(verified);
        }
        let cited: CitedReport = serde_json::from_str(final_text(report)?).map_err(|e| {
            format!("final answer must be exact CitedReport JSON, without Markdown fences: {e}. Return ONE complete object containing BOTH text (string) and citations (array of chunk_id and quote objects). Keep all correct fields while repairing the defective field; section headings belong inside text, never as new top-level keys.")
        })?;
        let available = evidence
            .iter()
            .flat_map(|r| r.hits.iter().map(|h| h.chunk.id.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(6)
            .collect::<Vec<_>>();
        let verified=SearchResult::verify_all(&evidence,cited).map_err(|e|format!("exact citation verification failed: {e}. Available retrieved chunk IDs (up to six): {available:?}. Use exact returned IDs and body quotes; include [[chunk_id]] in text for every citation. Claim support is not verified by this check."))?;
        self.coverage(&verified)?;
        self.check_answer_schema(report)?;
        Ok(verified)
    }
    /// The workflow value is a verified rendering for the located contract.
    /// The immutable child transcript still holds the original model JSON.
    pub fn validated_value(&self, report: &RunReport) -> Result<Value, String> {
        if matches!(self, Self::LocatedCitedReport { .. }) {
            serde_json::to_value(self.resolve_located(report)?.verified.report)
                .map_err(|e| e.to_string())
        } else if matches!(self, Self::SelectedSources { .. }) {
            serde_json::to_value(self.verified_citations(report)?.report).map_err(|e| e.to_string())
        } else {
            self.check(report)?;
            serde_json::from_str(final_text(report)?).map_err(|e| e.to_string())
        }
    }
}
fn validate_schema(schema: &Value) -> Result<(), RuntimeError> {
    if serde_json::to_vec(schema)
        .map_err(super::store::storage)?
        .len()
        > 128 * 1024
    {
        return Err(configuration("output schema exceeds 128 KiB"));
    }
    jsonschema::validator_for(schema)
        .map_err(|e| configuration(&format!("invalid output schema: {e}")))?;
    Ok(())
}
fn final_text(report: &RunReport) -> Result<&str, String> {
    match report.messages.last() {
        Some(Message::Assistant { turn }) if turn.tool_calls.is_empty() => Ok(&turn.content),
        _ => Err("no final assistant answer".into()),
    }
}
fn executed_evidence(
    report: &RunReport,
    tools: &BTreeSet<String>,
) -> Result<Vec<SearchResult>, String> {
    let eligible = report
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::ToolStarted { call_id, name } if tools.contains(name) => {
                Some(call_id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let completed = report
        .events
        .iter()
        .filter_map(|e| match &e.kind {
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
                        .map_err(|_| "configured citation tool did not return a SearchResult")?,
                );
            }
        }
    }
    if evidence.is_empty() {
        return Err(format!("no citation evidence was retrieved by an executed tool in this run; dependency citations are not your own tool results. Call an available host-granted retrieval tool before finalizing. Configured citation tool names: {tools:?}. Use its returned locators; do not invent document, revision or offset values. This feedback grants no tool permission."));
    }
    Ok(evidence)
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
