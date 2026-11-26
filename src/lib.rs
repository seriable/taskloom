//! # taskloom
//!
//! Typed, replayable LLM task execution with token budgets and call accounting.
//!
//! A task ([`AgentTask`], usually a [`PromptTask`]) says only what is its
//! own: its label, its prompts, the models and protocol it uses
//! ([`TaskSpec`]) and how its answer becomes a typed result
//! ([`AgentTask::decode`]). An [`Agent`] does the rest the same way for every
//! task:
//!
//! - resolves the model chain (named models and roles, [`ModelResolver`]);
//! - sends each request through a [`Transport`] — the built-in
//!   [`GatewayTransport`] over a [`Provider`] (the OpenAI-compatible one
//!   with feature `openai`) adds budget pre-checks,
//!   per-provider concurrency, per-model circuit breakers, transient retry
//!   and fallback chains ([`gateway`]);
//! - the resilient JSON protocol: a re-seeded retry, then a plain-text turn
//!   whose JSON is recovered locally;
//! - falls back to the next model on failure or rejection;
//! - enforces the task deadline, hedges slow requests ([`hedge`]);
//! - records the call ledger ([`ledger`]);
//! - records or replays answers, and serves validated answers again from a
//!   step cache ([`Environment`]).
//!
//! This is not an autonomous agent loop: a task is one structured answer,
//! and the caller's deterministic code decides what happens next.

#[cfg(feature = "anthropic")]
pub mod anthropic;
pub mod cancel;
pub mod environment;
pub mod eval;
pub mod gateway;
pub mod hedge;
pub mod json_text;
pub mod ledger;
pub mod locale;
pub mod many;
#[cfg(feature = "openai")]
pub mod openai;
mod prompt_task;
pub mod prompts;
pub mod provider;
pub mod registry;
pub mod retry;
mod runner;
#[cfg(any(feature = "openai", feature = "anthropic"))]
mod sse;
mod task;
mod task_error;
#[cfg(all(test, any(feature = "openai", feature = "anthropic")))]
mod test_http;
pub mod transport;

pub use cancel::CancelToken;
pub use environment::{Environment, RecordedRequest, Recording, RequestKey};
pub use gateway::{Gateway, GatewayConfig, ModelError};
pub use locale::Locale;
pub use prompt_task::{json_value, PromptTask};
pub use prompts::PromptOverrides;
pub use provider::{
    AttemptTimeout, ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse,
    GatewayTransport, IdleTimeout, Provider, Streaming, TransientAttempts,
};
pub use runner::{Agent, Embeddings, Progress, TaskOutcome};
pub use task::{
    AgentTask, Answer, Attachment, HedgePolicy, ModelChoice, Protocol, ProtocolSpec, TaskSpec,
    TransportOption, TransportOptions,
};
pub use task_error::{Rejection, TaskError};
pub use transport::{
    EmbedRequest, Embedded, FnTransport, Format, ModelResolver, NoRoles, Phase, Request, Response,
    Transport,
};

/// Decodes a JSON answer into `T` (for [`AgentTask::decode`] of tasks whose
/// answer maps onto a type directly).
pub fn decode_json<T: serde::de::DeserializeOwned>(answer: Answer) -> Result<T, Rejection> {
    match answer {
        Answer::Json(value) => serde_json::from_value(value)
            .map_err(|error| Rejection::new(format!("answer does not match its type: {error}"))),
        Answer::Text(text) => Err(Rejection::new(format!(
            "expected a JSON answer, got text ({} chars)",
            text.chars().count()
        ))),
    }
}
