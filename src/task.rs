//! What a task is: its label, prompts, models, protocol and decoding.

use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::{Rejection, TaskError};

/// One model task. Implementors carry their own input as fields, so a task
/// value is the whole request; the framework never sees domain types.
pub trait AgentTask: Send + Sync {
    /// What the task produces once its answer is decoded and validated.
    type Output: Send;

    /// Stable task label: the call ledger, the gateway's task budget and
    /// recording keys use it.
    fn label(&self) -> &str;

    /// Models, protocol, sampling and deadline.
    fn spec(&self) -> TaskSpec;

    fn system_prompt(&self) -> Cow<'_, str>;

    /// The request itself. `Err` refuses the task before anything is sent
    /// (an input the prompt cannot be built from).
    fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError>;

    /// Images sent with the request.
    fn images(&self) -> &[PathBuf] {
        &[]
    }

    /// Turns the answer into the task's result. A [`Rejection`] makes the
    /// runner try the next model of [`TaskSpec::models`], if any.
    fn decode(&self, answer: Answer) -> Result<Self::Output, Rejection>;
}

/// The answer as the transport returned it.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Json(serde_json::Value),
    Text(String),
}

impl Answer {
    pub fn into_json(self) -> Result<serde_json::Value, Rejection> {
        match self {
            Self::Json(value) => Ok(value),
            Self::Text(_) => Err(Rejection::new("expected a JSON answer, got text")),
        }
    }

    pub fn into_text(self) -> Result<String, Rejection> {
        match self {
            Self::Text(text) => Ok(text),
            Self::Json(value) => Ok(value.to_string()),
        }
    }

    pub(crate) fn to_record(&self) -> serde_json::Value {
        match self {
            Self::Json(value) => serde_json::json!({ "json": value }),
            Self::Text(text) => serde_json::json!({ "text": text }),
        }
    }

    pub(crate) fn from_record(record: &serde_json::Value) -> Option<Self> {
        if let Some(value) = record.get("json") {
            return Some(Self::Json(value.clone()));
        }
        record
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(|text| Self::Text(text.to_string()))
    }
}

/// Which model a task asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    /// A model the caller already chose.
    Named(String),
    /// The model configured for a role ("writer", "vision", ...), resolved
    /// by the runtime's [`ModelResolver`](crate::ModelResolver); skipped when
    /// the role has no model.
    Role(Cow<'static, str>),
    /// The model of a role when it has one, else the named model.
    RoleOr(Cow<'static, str>, String),
}

impl ModelChoice {
    pub fn named(model: impl Into<String>) -> Self {
        Self::Named(model.into())
    }

    pub fn role(role: &'static str) -> Self {
        Self::Role(Cow::Borrowed(role))
    }

    pub fn role_or(role: &'static str, fallback: impl Into<String>) -> Self {
        Self::RoleOr(Cow::Borrowed(role), fallback.into())
    }
}

impl From<&str> for ModelChoice {
    fn from(model: &str) -> Self {
        Self::Named(model.to_string())
    }
}

impl From<String> for ModelChoice {
    fn from(model: String) -> Self {
        Self::Named(model)
    }
}

/// How the answer is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// One JSON request; the answer is [`Answer::Json`].
    Json,
    /// One free-text request; the answer is [`Answer::Text`].
    Text,
    /// A structured JSON request; an unusable answer is asked once more with
    /// another seed ([`crate::retry`]), and a provider that keeps failing the
    /// structured protocol gets one plain-text turn whose JSON object is
    /// recovered locally. Context overflow, auth, configuration and content
    /// rejection fail at once: the text protocol would fail the same way.
    ResilientJson,
}

/// A file sent with the request (an audio or video clip, a document).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub path: PathBuf,
    pub mime_type: String,
}

/// One transport option value: any type the caller's transport reads (a
/// timeout policy, a provider flag). Its `Debug` form is part of the
/// request's identity for recordings and the step cache.
pub trait TransportOption: Any + fmt::Debug + Send + Sync {}

impl<T: Any + fmt::Debug + Send + Sync> TransportOption for T {}

/// Transport options by type, for transports with settings of their own.
#[derive(Clone, Default)]
pub struct TransportOptions {
    entries: Vec<(TypeId, Arc<dyn TransportOption>)>,
}

impl TransportOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the option of type `T` (replacing an earlier one).
    pub fn with<T: TransportOption>(mut self, value: T) -> Self {
        self.insert(value);
        self
    }

    pub fn insert<T: TransportOption>(&mut self, value: T) {
        let id = TypeId::of::<T>();
        self.entries.retain(|(entry, _)| *entry != id);
        self.entries.push((id, Arc::new(value)));
    }

    /// The option of type `T`, if set.
    pub fn get<T: TransportOption>(&self) -> Option<&T> {
        let id = TypeId::of::<T>();
        self.entries
            .iter()
            .find(|(entry, _)| *entry == id)
            .and_then(|(_, value)| (value.as_ref() as &dyn Any).downcast_ref::<T>())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for TransportOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entries(self.entries.iter().map(|(_, value)| value))
            .finish()
    }
}

impl PartialEq for TransportOptions {
    fn eq(&self, other: &Self) -> bool {
        format!("{self:?}") == format!("{other:?}")
    }
}

/// The protocol of a task with what travels beside the prompt: an
/// attachment and transport options. A [`Protocol`] converts into it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolSpec {
    pub protocol: Protocol,
    pub attachment: Option<Attachment>,
    pub options: TransportOptions,
}

impl ProtocolSpec {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            attachment: None,
            options: TransportOptions::default(),
        }
    }

    pub fn with_attachment(mut self, attachment: Attachment) -> Self {
        self.attachment = Some(attachment);
        self
    }

    pub fn with_option<T: TransportOption>(mut self, value: T) -> Self {
        self.options.insert(value);
        self
    }
}

impl From<Protocol> for ProtocolSpec {
    fn from(protocol: Protocol) -> Self {
        Self::new(protocol)
    }
}

/// A copy of a slow request raced against it ([`crate::hedge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HedgePolicy {
    pub after: Duration,
    pub copies: usize,
}

/// Everything about a task that is not its prompt or its decoding.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSpec {
    /// Tried in order: a model whose request fails or whose answer is
    /// rejected hands the task to the next (duplicates are skipped).
    pub models: Vec<ModelChoice>,
    pub protocol: Protocol,
    pub attachment: Option<Attachment>,
    pub options: TransportOptions,
    pub temperature: f32,
    pub max_output_tokens: u32,
    /// Limit for the whole task, every model included. The transport's own
    /// per-request timeouts apply inside it.
    pub deadline: Option<Duration>,
    pub hedge: Option<HedgePolicy>,
    /// Whether the run is recorded in the call ledger under the label.
    pub ledger: bool,
    /// Whether a validated answer may be served again from the step cache
    /// for the same request. Off unless the task's result is never asked
    /// again for variety.
    pub step_cache: bool,
    /// The JSON Schema the answer must follow: sent to providers that
    /// enforce one (a model profile's `supports_json_schema`), and part of
    /// the request's identity.
    pub schema: Option<serde_json::Value>,
    /// How many times a rejected answer is sent back to the same model with
    /// the rejection reason before the next model is tried (0: never).
    pub repair_rounds: u32,
}

impl TaskSpec {
    /// A task on `model` under `protocol`.
    pub fn new(
        model: impl Into<ModelChoice>,
        protocol: impl Into<ProtocolSpec>,
        temperature: f32,
        max_output_tokens: u32,
    ) -> Self {
        let protocol = protocol.into();
        Self {
            models: vec![model.into()],
            protocol: protocol.protocol,
            attachment: protocol.attachment,
            options: protocol.options,
            temperature,
            max_output_tokens,
            deadline: None,
            hedge: None,
            ledger: true,
            step_cache: false,
            schema: None,
            repair_rounds: 0,
        }
    }

    /// Adds `model` to try when the earlier ones fail.
    pub fn then(mut self, model: impl Into<ModelChoice>) -> Self {
        self.models.push(model.into());
        self
    }

    pub fn deadline(mut self, limit: Duration) -> Self {
        self.deadline = Some(limit);
        self
    }

    pub fn hedge(mut self, hedge: Option<HedgePolicy>) -> Self {
        self.hedge = hedge.filter(|policy| !policy.after.is_zero() && policy.copies > 0);
        self
    }

    pub fn without_ledger(mut self) -> Self {
        self.ledger = false;
        self
    }

    pub fn step_cache(mut self, enabled: bool) -> Self {
        self.step_cache = enabled;
        self
    }

    /// The JSON Schema of the answer.
    pub fn schema(mut self, schema: serde_json::Value) -> Self {
        self.schema = Some(schema);
        self
    }

    /// The JSON Schema of `T` (feature `schemars`).
    #[cfg(feature = "schemars")]
    pub fn schema_of<T: schemars::JsonSchema>(self) -> Self {
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
        self.schema(schema)
    }

    /// Sends a rejected answer back with its rejection reason up to
    /// `rounds` times before trying the next model.
    pub fn repair(mut self, rounds: u32) -> Self {
        self.repair_rounds = rounds;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Timeout(u64);

    #[test]
    fn options_are_found_by_type_and_replaced() {
        let options = TransportOptions::new().with(Timeout(5)).with(Timeout(9));
        assert_eq!(options.get::<Timeout>(), Some(&Timeout(9)));
        assert_eq!(options.get::<String>(), None);
        assert_eq!(format!("{options:?}"), "[Timeout(9)]");
    }
}
