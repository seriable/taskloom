//! How a request reaches a model: the [`Transport`] the runner sends every
//! request through, and the [`ModelResolver`] that turns model roles into
//! model ids.

use std::path::PathBuf;

use futures_util::future::BoxFuture;

use crate::task::{Answer, Attachment, TransportOptions};
use crate::TaskError;

/// What is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A JSON value; the transport returns [`Answer::Json`].
    Json,
    /// Free text; the transport returns [`Answer::Text`].
    Text,
}

/// Where a request stands in its protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The only request of a [`Protocol::Json`](crate::Protocol::Json) or
    /// [`Protocol::Text`](crate::Protocol::Text) task.
    Single,
    /// A structured request of a
    /// [`Protocol::ResilientJson`](crate::Protocol::ResilientJson) task
    /// (`attempt` 1 or 2; the second runs under another retry salt).
    Structured { attempt: u32 },
    /// The plain-text turn of a resilient task after the structured protocol
    /// failed; the runner recovers the JSON object from the text.
    TextFallback,
    /// A repair turn: the user prompt carries the rejected answer and the
    /// reason ([`TaskSpec::repair`](crate::TaskSpec::repair)).
    Repair { round: u32 },
}

/// An answer together with the model that actually served it. Transports
/// that route requests to another model override [`Transport::send_with_metadata`].
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub answer: Answer,
    pub model: String,
}

/// One request to one model.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub label: &'a str,
    pub model: &'a str,
    pub system_prompt: &'a str,
    pub user_prompt: &'a str,
    pub images: &'a [PathBuf],
    pub attachment: Option<&'a Attachment>,
    pub format: Format,
    pub phase: Phase,
    pub temperature: f32,
    pub max_output_tokens: u32,
    /// The JSON Schema the answer must follow, if the task declares one.
    pub schema: Option<&'a serde_json::Value>,
    pub options: &'a TransportOptions,
    /// Receives answer text as it streams in, when the caller wants
    /// progress; a transport that does not stream ignores it.
    pub on_delta: Option<&'a (dyn Fn(&str) + Send + Sync)>,
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Request")
            .field("label", &self.label)
            .field("model", &self.model)
            .field("format", &self.format)
            .field("phase", &self.phase)
            .field("images", &self.images.len())
            .field("attachment", &self.attachment)
            .field("schema", &self.schema.is_some())
            .field("options", &self.options)
            .field("streaming", &self.on_delta.is_some())
            .finish_non_exhaustive()
    }
}

/// Sends one request and returns the answer in the requested [`Format`].
///
/// A transport classifies its failures: [`TaskError::Provider`] with a
/// [`ModelError`](crate::ModelError) kind (transient, schema mismatch,
/// context overflow, ...) lets the runner decide on retries and fallbacks.
/// [`GatewayTransport`](crate::GatewayTransport) is the built-in one.
pub trait Transport: Send + Sync {
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>>;

    /// A stable identity for the endpoint, routing and answer-affecting
    /// settings. Custom transports sharing a store should supply one.
    fn cache_namespace(&self) -> Option<String> {
        None
    }

    /// Whole-task time limit, including queueing, retries and model chains.
    /// The runner uses the shorter of this and `TaskSpec::deadline`.
    fn task_timeout(&self, _label: &str) -> Option<std::time::Duration> {
        None
    }

    /// Sends a request while preserving the serving model. The default
    /// assumes no routing, so existing transports only need `send`.
    fn send_with_metadata<'a>(
        &'a self,
        request: Request<'a>,
    ) -> BoxFuture<'a, Result<Response, TaskError>> {
        Box::pin(async move {
            self.send(request).await.map(|answer| Response {
                answer,
                model: request.model.to_string(),
            })
        })
    }

    /// Embeds texts ([`Agent::embed`](crate::Agent::embed)). Unsupported
    /// unless implemented.
    fn embed<'a>(
        &'a self,
        _request: EmbedRequest<'a>,
    ) -> BoxFuture<'a, Result<Embedded, TaskError>> {
        Box::pin(async {
            Err(TaskError::Misconfigured(
                "this transport does not serve embeddings".to_string(),
            ))
        })
    }
}

/// Texts to embed with one model.
#[derive(Debug, Clone, Copy)]
pub struct EmbedRequest<'a> {
    pub label: &'a str,
    pub model: &'a str,
    pub inputs: &'a [String],
    pub options: &'a TransportOptions,
}

/// Embedding vectors, in input order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Embedded {
    pub vectors: Vec<Vec<f32>>,
}

impl<T: Transport + ?Sized> Transport for &T {
    fn cache_namespace(&self) -> Option<String> {
        (**self).cache_namespace()
    }

    fn task_timeout(&self, label: &str) -> Option<std::time::Duration> {
        (**self).task_timeout(label)
    }

    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
        (**self).send(request)
    }

    fn send_with_metadata<'a>(
        &'a self,
        request: Request<'a>,
    ) -> BoxFuture<'a, Result<Response, TaskError>> {
        (**self).send_with_metadata(request)
    }

    fn embed<'a>(
        &'a self,
        request: EmbedRequest<'a>,
    ) -> BoxFuture<'a, Result<Embedded, TaskError>> {
        (**self).embed(request)
    }
}

impl<T: Transport + ?Sized> Transport for std::sync::Arc<T> {
    fn cache_namespace(&self) -> Option<String> {
        (**self).cache_namespace()
    }

    fn task_timeout(&self, label: &str) -> Option<std::time::Duration> {
        (**self).task_timeout(label)
    }

    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
        (**self).send(request)
    }

    fn send_with_metadata<'a>(
        &'a self,
        request: Request<'a>,
    ) -> BoxFuture<'a, Result<Response, TaskError>> {
        (**self).send_with_metadata(request)
    }

    fn embed<'a>(
        &'a self,
        request: EmbedRequest<'a>,
    ) -> BoxFuture<'a, Result<Embedded, TaskError>> {
        (**self).embed(request)
    }
}

/// The model ids of roles ([`ModelChoice::Role`](crate::ModelChoice::Role)).
pub trait ModelResolver: Send + Sync {
    /// The model of `role`, or `None` when the role has none.
    fn resolve_role(&self, role: &str) -> Option<String>;
}

/// No roles: every role is unresolved.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRoles;

impl ModelResolver for NoRoles {
    fn resolve_role(&self, _role: &str) -> Option<String> {
        None
    }
}

/// Roles from a fixed table.
impl ModelResolver for std::collections::HashMap<String, String> {
    fn resolve_role(&self, role: &str) -> Option<String> {
        self.get(role).cloned()
    }
}

impl ModelResolver for std::collections::BTreeMap<String, String> {
    fn resolve_role(&self, role: &str) -> Option<String> {
        self.get(role).cloned()
    }
}

/// A transport that answers from a function — for tests, examples and
/// offline demos.
pub struct FnTransport<F>(pub F);

impl<F> Transport for FnTransport<F>
where
    F: for<'r> Fn(Request<'r>) -> Result<Answer, TaskError> + Send + Sync,
{
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
        let answer = (self.0)(request);
        Box::pin(async move { answer })
    }
}
