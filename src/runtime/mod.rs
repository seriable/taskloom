//! Bounded, observable agent execution (feature `runtime`).
//!
//! This layer is independent of [`crate::AgentTask`]. Models implement
//! [`Model`]; the built-in [`crate::GatewayTransport`] implements it using
//! native provider tool calls. Only explicitly allowed tools are exposed.
//! Tools execute in process; this library does not create a security sandbox.

mod budget;
mod context;
mod durable;
mod executor;
mod session;
mod state;
mod store;
mod tool;
pub(crate) mod types;

pub use budget::{BudgetJournal, BudgetSnapshot, Reservation, RunBudget};
pub use context::{ContextCompressor, ContextPolicy, ContextRecord, ExtractiveCompressor};
pub use durable::DurableAgent;
pub use executor::{
    AgentLoop, EventSink, RunEvent, RunEventKind, RunLimits, RunReport, StopReason,
};
pub use session::{
    FileSessionStore, Session, SessionBinding, SessionInput, SessionManager, SessionRun,
    SessionStore, SessionTurn, SESSION_VERSION,
};
pub use state::{
    ApprovalDecision, ApprovalPolicy, ApprovalRecord, ApprovalRequest, RecoveryDecision,
    RecoveryRecord, RunConfig, RunCursor, RunState, RunStatus, ToolProgress, ToolStep,
    RUN_STATE_VERSION,
};
pub use store::{FileRunStore, RunLease, RunStore};
pub use tool::{FnTool, Tool, ToolContext, ToolEffect, ToolError, ToolPolicy, ToolRegistry};
pub use types::{
    AssistantTurn, Message, Model, ModelOutput, ModelReply, ModelRequest, RuntimeError, ToolCall,
    ToolDefinition, ToolResult,
};
