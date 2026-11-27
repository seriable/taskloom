//! Bounded, observable agent execution (feature `runtime`).
//!
//! This layer is independent of [`crate::AgentTask`]. Models implement
//! [`Model`]; the built-in [`crate::GatewayTransport`] implements it using
//! native provider tool calls. Only explicitly allowed tools are exposed.
//! Tools execute in process; this library does not create a security sandbox.

mod budget;
mod executor;
mod tool;
pub(crate) mod types;

pub use budget::{BudgetSnapshot, Reservation, RunBudget};
pub use executor::{
    AgentLoop, EventSink, RunEvent, RunEventKind, RunLimits, RunReport, StopReason,
};
pub use tool::{FnTool, Tool, ToolContext, ToolEffect, ToolError, ToolPolicy, ToolRegistry};
pub use types::{
    AssistantTurn, Message, Model, ModelOutput, ModelReply, ModelRequest, RuntimeError, ToolCall,
    ToolDefinition, ToolResult,
};
