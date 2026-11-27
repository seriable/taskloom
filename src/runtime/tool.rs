use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RuntimeError, ToolCall, ToolDefinition};

/// Recovery metadata, not a claim that this library isolates the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    /// The implementer guarantees repeated calls with the same context key
    /// have the same externally visible effect.
    Idempotent,
    NonIdempotent,
}

/// Stable identifiers to use in tool-side deduplication/audit logs.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub run_id: String,
    pub call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ToolError {}

/// Tools must be asynchronous and cancellation-safe where possible. Dropping
/// the future cannot roll back a remote operation. No tool is auto-retried.
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    fn execute<'a>(
        &'a self,
        arguments: Value,
        context: ToolContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>>;
}

/// An asynchronous closure as a tool.
pub struct FnTool<F> {
    definition: ToolDefinition,
    execute: F,
}

impl<F> FnTool<F> {
    pub fn new(definition: ToolDefinition, execute: F) -> Self {
        Self {
            definition,
            execute,
        }
    }
}

impl<F, Fut> Tool for FnTool<F>
where
    F: Fn(Value, ToolContext) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Value, ToolError>> + Send + 'static,
{
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn execute<'a>(
        &'a self,
        arguments: Value,
        context: ToolContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin((self.execute)(arguments, context))
    }
}

/// Denies everything by default. A tool needs both its name allowed and
/// every declared permission granted. Only permitted tools reach the model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPolicy {
    pub allowed_tools: BTreeSet<String>,
    pub grants: BTreeSet<String>,
}

impl ToolPolicy {
    pub fn allow(mut self, name: impl Into<String>) -> Self {
        self.allowed_tools.insert(name.into());
        self
    }
    pub fn grant(mut self, permission: impl Into<String>) -> Self {
        self.grants.insert(permission.into());
        self
    }
    pub fn permits(&self, definition: &ToolDefinition) -> bool {
        self.allowed_tools.contains(&definition.name)
            && definition.permissions.is_subset(&self.grants)
    }
}

struct RegisteredTool {
    definition: ToolDefinition,
    validator: jsonschema::Validator,
    tool: Arc<dyn Tool>,
}

pub(crate) struct PreparedTool {
    pub executor: Arc<dyn Tool>,
    pub effect: ToolEffect,
}

/// Definitions and schemas are frozen at registration, so a tool cannot
/// change its advertised permissions between preflight and dispatch.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: impl Tool + 'static) -> Result<(), RuntimeError> {
        let definition = tool.definition();
        if definition.name.is_empty()
            || definition.name.len() > 64
            || !definition
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(RuntimeError::Configuration(
                "tool name must have 1..=64 ASCII letters, digits, '_' or '-'".into(),
            ));
        }
        if self.tools.contains_key(&definition.name) {
            return Err(RuntimeError::Configuration(format!(
                "duplicate tool: {}",
                definition.name
            )));
        }
        if definition.input_schema.get("type").and_then(Value::as_str) != Some("object") {
            return Err(RuntimeError::Configuration(
                "tool input schema must declare type: object".into(),
            ));
        }
        let validator = jsonschema::validator_for(&definition.input_schema).map_err(|error| {
            RuntimeError::Configuration(format!("invalid tool schema: {error}"))
        })?;
        self.tools.insert(
            definition.name.clone(),
            RegisteredTool {
                definition,
                validator,
                tool: Arc::new(tool),
            },
        );
        Ok(())
    }

    pub fn definitions(&self, policy: &ToolPolicy) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .filter(|tool| policy.permits(&tool.definition))
            .map(|tool| tool.definition.clone())
            .collect()
    }

    pub(crate) fn prepare(
        &self,
        call: &ToolCall,
        policy: &ToolPolicy,
    ) -> Result<PreparedTool, RuntimeError> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| RuntimeError::Tool(format!("unknown tool: {}", call.name)))?;
        if !policy.permits(&tool.definition) {
            return Err(RuntimeError::Permission(format!(
                "tool is not permitted: {}",
                call.name
            )));
        }
        if let Err(error) = tool.validator.validate(&call.arguments) {
            return Err(RuntimeError::Tool(format!(
                "{}: invalid tool arguments: {error}",
                call.name
            )));
        }
        Ok(PreparedTool {
            executor: Arc::clone(&tool.tool),
            effect: tool.definition.effect,
        })
    }
}
