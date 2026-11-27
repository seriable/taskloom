//! Scoped, reviewed long-term memory. Stored values are untrusted data, never
//! tool grants. Conversation history and durable model spending stay separate.
mod manager;
mod store;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{state::fingerprint, RuntimeError};
pub use manager::{MemoryExtraction, MemoryManager};
pub use store::{FileMemoryStore, MemoryStore};

pub const MEMORY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryScope {
    pub tenant: String,
    pub project: String,
    pub owner: String,
}
impl MemoryScope {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        for name in [&self.tenant, &self.project, &self.owner] {
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.@-".contains(&b))
            {
                return Err(invalid(
                    "scope identities must be 1..=128 ASCII identity bytes",
                ));
            }
        }
        Ok(())
    }
    pub fn identity(&self) -> Result<String, RuntimeError> {
        self.validate()?;
        fingerprint(&("memory-scope-v1", self))
    }
}

/// Constructed only by the trusted host after authentication/authorization.
/// Deliberately has no Deserialize implementation. This is not authentication
/// by itself; do not construct it from caller/model-owned scope or actor fields.
#[derive(Debug, Clone)]
pub struct MemoryAccess {
    scope: MemoryScope,
    actor: String,
    curate: bool,
}
impl MemoryAccess {
    pub fn reader(scope: MemoryScope, actor: impl Into<String>) -> Result<Self, RuntimeError> {
        Self::new(scope, actor.into(), false)
    }
    pub fn curator(scope: MemoryScope, actor: impl Into<String>) -> Result<Self, RuntimeError> {
        Self::new(scope, actor.into(), true)
    }
    fn new(scope: MemoryScope, actor: String, curate: bool) -> Result<Self, RuntimeError> {
        scope.validate()?;
        bounded_text(&actor, 256)?;
        Ok(Self {
            scope,
            actor,
            curate,
        })
    }
    pub fn scope(&self) -> &MemoryScope {
        &self.scope
    }
    pub(crate) fn require_curator(&self) -> Result<(), RuntimeError> {
        if self.curate {
            Ok(())
        } else {
            Err(RuntimeError::Permission(
                "memory curator permission is required".into(),
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Fact,
    Preference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryProposal {
    pub kind: MemoryKind,
    pub key: String,
    pub value: String,
    pub quote: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReviewDecision {
    pub accept: bool,
    #[serde(default)]
    pub replace: bool,
    pub note: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryExtractionRequest {
    pub expected_revision: u64,
    pub turn_id: String,
    pub request_id: String,
}
impl MemoryProposal {
    fn validate(&self) -> Result<(), RuntimeError> {
        validate_key(&self.key)?;
        bounded_text(&self.value, 2048)?;
        bounded_text(&self.quote, 4096)?;
        if self.quote.chars().filter(|c| !c.is_whitespace()).count() < 4 {
            return Err(invalid("memory needs a nontrivial exact source quote"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySource {
    pub session_id: String,
    pub turn_id: String,
    pub run_id: String,
    pub message_index: usize,
    pub message_fingerprint: String,
    pub turn_fingerprint: String,
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryOrigin {
    Extraction {
        run_id: String,
    },
    OperatorEdit {
        previous: String,
        actor: String,
        note: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    Candidate,
    Active,
    Superseded,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryDecision {
    pub actor: String,
    pub note: String,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: String,
    pub kind: MemoryKind,
    pub key: String,
    pub value: String,
    pub source: MemorySource,
    pub origin: MemoryOrigin,
    pub status: MemoryStatus,
    pub created_revision: u64,
    pub updated_revision: u64,
    pub decision: Option<MemoryDecision>,
}
impl MemoryRecord {
    fn identity(&self, scope: &MemoryScope) -> Result<String, RuntimeError> {
        fingerprint(&(
            "memory-record-v1",
            scope,
            self.kind,
            &self.key,
            &self.value,
            &self.source,
            &self.origin,
        ))
    }
    fn source_binding(&self, scope: &MemoryScope) -> Result<String, RuntimeError> {
        // Erasure blocks resurrection from the same historical source, even
        // if a later model invents a different value/quote from that message.
        fingerprint(&(
            "forgotten-memory-source-v1",
            scope,
            self.kind,
            &self.key,
            &self.source.session_id,
            &self.source.turn_id,
            self.source.message_index,
            &self.source.message_fingerprint,
        ))
    }
    fn validate(&self, scope: &MemoryScope, revision: u64) -> Result<(), RuntimeError> {
        MemoryProposal {
            kind: self.kind,
            key: self.key.clone(),
            value: self.value.clone(),
            quote: self.source.quote.clone(),
        }
        .validate()?;
        for id in [
            &self.source.session_id,
            &self.source.turn_id,
            &self.source.run_id,
        ] {
            bounded_text(id, 256)?;
        }
        for id in [
            &self.source.message_fingerprint,
            &self.source.turn_fingerprint,
        ] {
            validate_hash(id)?;
        }
        if self.id != self.identity(scope)?
            || self.created_revision == 0
            || self.updated_revision < self.created_revision
            || self.updated_revision > revision
            || (self.status == MemoryStatus::Candidate) != self.decision.is_none()
        {
            return Err(invalid(
                "memory record identity, revision or review state mismatch",
            ));
        }
        match &self.origin {
            MemoryOrigin::Extraction { run_id } => bounded_text(run_id, 256)?,
            MemoryOrigin::OperatorEdit {
                previous,
                actor,
                note,
            } => {
                validate_hash(previous)?;
                bounded_text(actor, 256)?;
                bounded_text(note, 2048)?;
            }
        }
        if let Some(d) = &self.decision {
            bounded_text(&d.actor, 256)?;
            bounded_text(&d.note, 2048)?;
            if d.revision < self.created_revision || d.revision > self.updated_revision {
                return Err(invalid("invalid memory review revision"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    Propose,
    Accept,
    Reject,
    Edit,
    Forget,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEvent {
    pub revision: u64,
    pub actor: String,
    pub action: MemoryAction,
    /// Hashes only: forgetting a key removes its values/quotes from this store.
    pub record_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySnapshot {
    pub version: u32,
    pub scope: MemoryScope,
    pub revision: u64,
    pub records: BTreeMap<String, MemoryRecord>,
    pub forgotten_sources: BTreeSet<String>,
    pub events: Vec<MemoryEvent>,
}
impl MemorySnapshot {
    pub fn empty(scope: MemoryScope) -> Self {
        Self {
            version: MEMORY_VERSION,
            scope,
            revision: 0,
            records: BTreeMap::new(),
            forgotten_sources: BTreeSet::new(),
            events: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.scope.validate()?;
        if self.version != MEMORY_VERSION
            || self.revision >= u64::MAX - 1024
            || self.revision != self.events.len() as u64
            || self.events.len() > 20_000
            || self.records.len() > 4096
            || self.forgotten_sources.len() > 16_384
            // Every retained record keeps room for one administrative erasure
            // event and one source tombstone, even at admission capacity.
            || self.events.len().saturating_add(self.records.len()) > 20_000
            || self.forgotten_sources.len().saturating_add(self.records.len()) > 16_384
        {
            return Err(invalid("unsupported or oversized memory snapshot"));
        }
        let mut active = BTreeSet::new();
        for (id, record) in &self.records {
            record.validate(&self.scope, self.revision)?;
            if id != &record.id
                || self
                    .forgotten_sources
                    .contains(&record.source_binding(&self.scope)?)
                || (record.status == MemoryStatus::Active
                    && !active.insert((record.kind, &record.key)))
            {
                return Err(invalid("duplicate, forgotten or conflicting memory record"));
            }
        }
        for id in &self.forgotten_sources {
            validate_hash(id)?;
        }
        for (i, event) in self.events.iter().enumerate() {
            bounded_text(&event.actor, 256)?;
            if event.revision != i as u64 + 1
                || event.record_ids.is_empty()
                || event.record_ids.len() > 4096
            {
                return Err(invalid("invalid memory audit sequence"));
            }
            for id in &event.record_ids {
                validate_hash(id)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryRecallPolicy {
    pub max_items: usize,
    pub max_bytes: usize,
    /// Explicit reviewed preferences may be useful without lexical overlap.
    pub include_preferences: bool,
}
impl Default for MemoryRecallPolicy {
    fn default() -> Self {
        Self {
            max_items: 8,
            max_bytes: 16 * 1024,
            include_preferences: true,
        }
    }
}
impl MemoryRecallPolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if !(1..=20).contains(&self.max_items) || !(512..=64 * 1024).contains(&self.max_bytes) {
            return Err(invalid("invalid memory recall bounds"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryHit {
    pub record: MemoryRecord,
    pub matched_terms: usize,
    pub preference: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecall {
    pub scope: MemoryScope,
    pub revision: u64,
    pub query: String,
    pub hits: Vec<MemoryHit>,
    pub skipped_oversize: usize,
    pub omitted: usize,
}

fn validate_key(key: &str) -> Result<(), RuntimeError> {
    if key.is_empty()
        || key.len() > 96
        || !key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(invalid(
            "memory key must be a bounded lowercase semantic slot",
        ));
    }
    Ok(())
}
fn validate_hash(value: &str) -> Result<(), RuntimeError> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("invalid memory fingerprint"));
    }
    Ok(())
}
fn bounded_text(value: &str, max: usize) -> Result<(), RuntimeError> {
    if value.trim().is_empty()
        || value.len() > max
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(invalid("empty, oversized or control-bearing memory text"));
    }
    Ok(())
}
fn invalid(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(format!("memory: {detail}"))
}
