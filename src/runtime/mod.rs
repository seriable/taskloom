//! Bounded, observable agent execution (feature `runtime`).
//!
//! This layer is independent of [`crate::AgentTask`]. Models implement
//! [`Model`]; the built-in [`crate::GatewayTransport`] implements it using
//! native provider tool calls. Only explicitly allowed tools are exposed.
//! Custom tools execute in process. The opt-in `sandbox` feature routes only
//! explicitly configured Python calls to an audited Linux-container worker;
//! registering ordinary tools does not isolate them.

#[cfg(feature = "app")]
pub mod app;
pub mod benchmark;
mod budget;
mod context;
#[cfg(feature = "database")]
pub mod database;
mod durable;
mod executor;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod memory;
pub mod observability;
mod output;
pub mod rag;
#[cfg(feature = "sandbox")]
pub mod sandbox;
mod session;
pub mod skills;
mod state;
mod store;
mod tool;
pub(crate) mod types;
pub mod workflow;

pub use budget::{BudgetJournal, BudgetSnapshot, Reservation, RunBudget};
pub use context::{ContextCompressor, ContextPolicy, ContextRecord, ExtractiveCompressor};
pub use durable::{CancelReason, DurableAgent};
pub use executor::{
    AgentLoop, EventSink, RunEvent, RunEventKind, RunLimits, RunReport, StopReason,
};
pub use output::FinalOutputPolicy;
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
