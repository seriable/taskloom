use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RunBudget, ToolEffect};
use crate::{gateway::ModelError, gateway::TokenUsage, TaskError};

/// A provider-independent conversation entry. Tool output is never promoted
/// to a system instruction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System { content: String },
    User { content: String },
    Assistant { turn: AssistantTurn },
    Tool { result: ToolResult },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }
}

/// One completed assistant turn: final text or one or more tool requests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantTurn {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

impl AssistantTurn {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.content.trim().is_empty() && self.tool_calls.is_empty() {
            return Err(RuntimeError::Protocol("empty assistant turn".into()));
        }
        let mut ids = std::collections::BTreeSet::new();
        for call in &self.tool_calls {
            if call.id.trim().is_empty() || !ids.insert(&call.id) {
                return Err(RuntimeError::Protocol(
                    "empty or duplicate tool call id".into(),
                ));
            }
            if call.name.trim().is_empty() || !call.arguments.is_object() {
                return Err(RuntimeError::Protocol(
                    "tool calls need a name and object arguments".into(),
                ));
            }
        }
        Ok(())
    }
}

/// An invocation id must be unique within a run, not just within a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub output: Value,
    pub is_error: bool,
}

/// Description sent to the model. Permission/effect fields are enforced by
/// the host and are not instructions granting the model extra authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub effect: ToolEffect,
    #[serde(default)]
    pub permissions: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub label: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub temperature: f32,
    /// Must be positive; every agent turn has a bounded output allowance.
    pub max_output_tokens: u32,
}

/// A provider reply keeps usage even when the assistant turn is unusable.
/// Adapters must put refusals, truncation and invalid arguments in `turn`,
/// rather than dropping the reported usage with an early error.
#[derive(Debug)]
pub struct ModelReply {
    pub turn: Result<AssistantTurn, ModelError>,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelOutput {
    pub turn: AssistantTurn,
    pub model: String,
}

/// A model step. Implementations must reserve each paid attempt against
/// `budget` before dispatch. The gateway implementation does this for every
/// retry/fallback and records usage before validating the turn.
pub trait Model: Send + Sync {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>>;
}

#[derive(Debug)]
pub enum RuntimeError {
    Model(TaskError),
    Budget(String),
    Protocol(String),
    Configuration(String),
    Tool(String),
    Permission(String),
    Storage(String),
    Conflict(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(error) => error.fmt(f),
            Self::Budget(detail)
            | Self::Protocol(detail)
            | Self::Configuration(detail)
            | Self::Tool(detail)
            | Self::Permission(detail)
            | Self::Storage(detail)
            | Self::Conflict(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Model(error) => Some(error),
            _ => None,
        }
    }
}

/// Validates complete history, including globally unique call ids and one
/// result per outstanding call, before it can be sent to a provider.
pub(crate) fn validate_history(messages: &[Message]) -> Result<(), RuntimeError> {
    if !pending_calls(messages)?.is_empty() {
        return Err(RuntimeError::Protocol(
            "missing tool results in history".into(),
        ));
    }
    Ok(())
}

pub(crate) fn pending_calls(
    messages: &[Message],
) -> Result<std::collections::BTreeSet<String>, RuntimeError> {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    let mut pending = BTreeSet::new();
    let mut non_system = false;
    let mut has_user = false;
    for message in messages {
        if !pending.is_empty() && !matches!(message, Message::Tool { .. }) {
            return Err(RuntimeError::Protocol(
                "missing tool results in history".into(),
            ));
        }
        match message {
            Message::System { .. } if non_system => {
                return Err(RuntimeError::Protocol(
                    "system messages must precede conversation".into(),
                ));
            }
            Message::System { .. } => {}
            Message::User { .. } => {
                non_system = true;
                has_user = true;
            }
            Message::Assistant { turn } => {
                non_system = true;
                turn.validate()?;
                for call in &turn.tool_calls {
                    if !seen.insert(call.id.clone()) {
                        return Err(RuntimeError::Protocol(
                            "reused tool call id in history".into(),
                        ));
                    }
                    pending.insert(call.id.clone());
                }
            }
            Message::Tool { result } => {
                non_system = true;
                if !pending.remove(&result.call_id) {
                    return Err(RuntimeError::Protocol(
                        "unexpected or duplicate tool result".into(),
                    ));
                }
            }
        }
    }
    if !has_user {
        return Err(RuntimeError::Protocol(
            "history needs a user message and complete tool results".into(),
        ));
    }
    Ok(pending)
}
