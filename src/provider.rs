//! One chat-completion attempt at one provider, and the [`GatewayTransport`]
//! that runs attempts through a [`Gateway`].
//!
//! A [`Provider`] sends exactly one request and classifies its failure; it
//! never retries. The gateway decides on admission, retries, breakers and
//! fallback models, so a provider stays a thin protocol adapter.
//! `openai::OpenAiCompatible` (feature `openai`)
//! is the built-in one.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};

use crate::gateway::{AttemptOutput, Gateway, ModelError, TokenEstimator, TokenUsage};
use crate::task::Answer;
use crate::transport::{EmbedRequest, Embedded, Format, Request, Response, Transport};
use crate::TaskError;

#[cfg(feature = "runtime")]
mod runtime;

/// One chat-completion request to one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub images: Vec<PathBuf>,
    pub temperature: f32,
    /// `None` lets the provider choose its own output allowance.
    pub max_output_tokens: Option<u32>,
    /// Ask for a JSON object (`response_format`).
    pub json: bool,
    /// The schema the JSON answer must follow, sent only to models whose
    /// profile `supports_json_schema`.
    pub schema: Option<serde_json::Value>,
    /// A name for the schema (the task label, sanitized).
    pub schema_name: String,
    pub seed: Option<i64>,
}

/// How a streaming attempt reports and watches its answer.
#[derive(Clone, Copy, Default)]
pub struct Streaming<'a> {
    /// Receives answer text as it arrives.
    pub on_delta: Option<&'a (dyn Fn(&str) + Send + Sync)>,
    /// The attempt fails as transient when no output arrives for this long.
    pub idle_timeout: Option<Duration>,
}

/// An embeddings request to one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRequest {
    pub model: String,
    pub inputs: Vec<String>,
}

/// The vectors of an embeddings request, in input order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmbeddingResponse {
    pub vectors: Vec<Vec<f32>>,
    pub usage: Option<TokenUsage>,
}

/// What the provider answered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatResponse {
    pub content: String,
    pub usage: Option<TokenUsage>,
    /// `stop`, `length`, ...; providers normalize content refusals to
    /// `refusal`, so the gateway can keep usage before rejecting the answer.
    pub finish_reason: Option<String>,
}

/// A chat-completion endpoint.
pub trait Provider: Send + Sync {
    /// The provider's identity for concurrency and breakers (its base URL).
    fn id(&self) -> &str;

    /// Endpoint and answer-affecting protocol settings. Do not include secrets.
    fn cache_namespace(&self) -> String {
        self.id().to_string()
    }

    /// Sends one request; no retries. With `streaming`, a provider that
    /// can stream reports text as it arrives and enforces the idle timeout;
    /// one that cannot ignores it.
    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        streaming: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>>;

    /// One native tool-calling turn (feature `runtime`), without retries.
    /// Keeps reported usage on protocol failures in [`crate::runtime::ModelReply`].
    #[cfg(feature = "runtime")]
    fn converse<'a>(
        &'a self,
        _request: &'a crate::runtime::ModelRequest,
    ) -> BoxFuture<'a, Result<crate::runtime::ModelReply, ModelError>> {
        Box::pin(async {
            Err(ModelError::misconfigured(
                "provider does not support native agent turns",
            ))
        })
    }

    /// Embeds the inputs; no retries. Unsupported unless implemented.
    fn embed<'a>(
        &'a self,
        _request: &'a EmbeddingRequest,
    ) -> BoxFuture<'a, Result<EmbeddingResponse, ModelError>> {
        Box::pin(async {
            Err(ModelError::misconfigured(
                "this provider does not serve embeddings",
            ))
        })
    }
}

impl<P: Provider + ?Sized> Provider for Arc<P> {
    #[cfg(feature = "runtime")]
    fn converse<'a>(
        &'a self,
        request: &'a crate::runtime::ModelRequest,
    ) -> BoxFuture<'a, Result<crate::runtime::ModelReply, ModelError>> {
        (**self).converse(request)
    }
    fn cache_namespace(&self) -> String {
        (**self).cache_namespace()
    }

    fn id(&self) -> &str {
        (**self).id()
    }

    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        streaming: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        (**self).complete(request, streaming)
    }

    fn embed<'a>(
        &'a self,
        request: &'a EmbeddingRequest,
    ) -> BoxFuture<'a, Result<EmbeddingResponse, ModelError>> {
        (**self).embed(request)
    }
}

/// Transport option: the wall-clock cap of one attempt, including admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptTimeout(pub Duration);

/// Transport option: attempts per model for transient failures (≥ 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransientAttempts(pub usize);

/// Transport option: stream the answer and fail an attempt as transient
/// when no output arrives for this long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleTimeout(pub Duration);

/// Runs every request through a [`Gateway`] on one [`Provider`]: budget
/// pre-check, admission, transient retry, the fallback chain of the model
/// registry, statistics.
pub struct GatewayTransport<P> {
    gateway: Arc<Gateway>,
    provider: P,
    estimator: TokenEstimator,
    transient_attempts: usize,
    attempt_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    seeds: bool,
    cache_namespace: String,
}

impl<P: Provider> GatewayTransport<P> {
    pub fn new(gateway: Arc<Gateway>, provider: P) -> Self {
        Self {
            gateway,
            provider,
            estimator: TokenEstimator::default(),
            transient_attempts: 2,
            attempt_timeout: Some(Duration::from_secs(120)),
            idle_timeout: None,
            seeds: false,
            cache_namespace: String::new(),
        }
    }

    pub fn with_estimator(mut self, estimator: TokenEstimator) -> Self {
        self.estimator = estimator;
        self
    }

    /// Attempts per model for transient failures (default 2); a request's
    /// [`TransientAttempts`] option overrides it.
    pub fn with_transient_attempts(mut self, attempts: usize) -> Self {
        self.transient_attempts = attempts.max(1);
        self
    }

    /// Cap of one attempt (default 120 s; `None`: the provider's own
    /// timeouts only); a request's [`AttemptTimeout`] option overrides it.
    pub fn with_attempt_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.attempt_timeout = timeout;
        self
    }

    /// Streams every answer and fails an attempt as transient when no
    /// output arrives for `timeout` (default off); a request's
    /// [`IdleTimeout`] option overrides it. A stalled provider is noticed in
    /// seconds instead of at the attempt timeout.
    pub fn with_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Sends a seed derived from the request and its retry salt
    /// ([`crate::retry`]): the same request asks for the same sample, a
    /// re-asking for another. Off by default (not every provider takes one).
    pub fn with_seeds(mut self, seeds: bool) -> Self {
        self.seeds = seeds;
        self
    }

    /// Isolates a tenant or custom HTTP-client configuration when the
    /// endpoint alone does not identify its answers. Never put secrets here.
    pub fn with_cache_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.cache_namespace = namespace.into();
        self
    }

    pub fn gateway(&self) -> &Gateway {
        &self.gateway
    }

    async fn send_request(&self, request: Request<'_>) -> Result<Response, ModelError> {
        if request.attachment.is_some() {
            return Err(ModelError::misconfigured(format!(
                "{}: this transport sends no file attachments",
                request.label
            )));
        }
        let mut context = self.gateway.call_context(
            request.label,
            self.provider.id(),
            request.model,
            request.system_prompt,
            request.user_prompt,
            request.images.len(),
            request.max_output_tokens as usize,
            self.estimator,
        );
        context.transient_attempts = request
            .options
            .get::<TransientAttempts>()
            .map_or(self.transient_attempts, |attempts| attempts.0)
            .max(1);
        context.attempt_timeout = request
            .options
            .get::<AttemptTimeout>()
            .map(|timeout| timeout.0)
            .or(self.attempt_timeout);
        self.gateway.precheck(&context)?;
        let json = request.format == Format::Json;
        let seed = self.seeds.then(|| seed_of(&request));
        let idle_timeout = request
            .options
            .get::<IdleTimeout>()
            .map(|timeout| timeout.0)
            .or(self.idle_timeout);
        let streaming =
            (request.on_delta.is_some() || idle_timeout.is_some()).then_some(Streaming {
                on_delta: request.on_delta,
                idle_timeout,
            });
        let base = ChatRequest {
            model: request.model.to_string(),
            system_prompt: request.system_prompt.to_string(),
            user_prompt: request.user_prompt.to_string(),
            images: request.images.to_vec(),
            temperature: request.temperature,
            max_output_tokens: (request.max_output_tokens > 0).then_some(request.max_output_tokens),
            json,
            schema: None,
            schema_name: schema_name(request.label),
            seed,
        };
        let label = request.label;
        let schema = request.schema;
        let provider = &self.provider;
        let gateway = &self.gateway;
        let base = &base;
        self.gateway
            .execute_validated(
                &context,
                |model: String| async move {
                    let schema = schema
                        .filter(|_| json && gateway.model_profile(&model).supports_json_schema)
                        .cloned();
                    let attempt = ChatRequest {
                        model,
                        schema,
                        ..base.clone()
                    };
                    let response = provider.complete(&attempt, streaming).await?;
                    let usage = response.usage;
                    Ok(AttemptOutput::new((attempt.model, response), usage))
                },
                |(model, response)| {
                    if matches!(
                        response.finish_reason.as_deref(),
                        Some("refusal" | "content_filter")
                    ) {
                        return Err(ModelError::ContentRejected {
                            detail: format!("{label}: the model refused to answer"),
                        });
                    }
                    let answer = if json {
                        Answer::Json(decode_json(label, &response)?)
                    } else {
                        Answer::Text(response.content)
                    };
                    Ok(Response { answer, model })
                },
            )
            .await
    }

    async fn embed_request(&self, request: EmbedRequest<'_>) -> Result<Embedded, ModelError> {
        let joined = request.inputs.join("\n");
        let mut context = self.gateway.call_context(
            request.label,
            self.provider.id(),
            request.model,
            "",
            &joined,
            0,
            0,
            self.estimator,
        );
        // Different models have different vector spaces. Partial cache hits
        // must never be combined with vectors from a fallback model.
        context.allow_fallback = false;
        context.transient_attempts = request
            .options
            .get::<TransientAttempts>()
            .map_or(self.transient_attempts, |attempts| attempts.0)
            .max(1);
        context.attempt_timeout = request
            .options
            .get::<AttemptTimeout>()
            .map(|timeout| timeout.0)
            .or(self.attempt_timeout);
        self.gateway.precheck(&context)?;
        let inputs = request.inputs.to_vec();
        let provider = &self.provider;
        let inputs = &inputs;
        let expected = inputs.len();
        self.gateway
            .execute_validated(
                &context,
                |model: String| async move {
                    let response = provider
                        .embed(&EmbeddingRequest {
                            model,
                            inputs: inputs.clone(),
                        })
                        .await?;
                    let usage = response.usage;
                    Ok(AttemptOutput::new(response, usage))
                },
                |response| {
                    if response.vectors.len() != expected {
                        return Err(ModelError::schema_mismatch(format!(
                            "expected {expected} embeddings, got {}",
                            response.vectors.len()
                        )));
                    }
                    Ok(Embedded {
                        vectors: response.vectors,
                    })
                },
            )
            .await
    }
}

impl<P: Provider> Transport for GatewayTransport<P> {
    fn cache_namespace(&self) -> Option<String> {
        // Only what changes the answer: the endpoint and its protocol
        // settings, the caller's own namespace and sampling seeds. Timeouts,
        // retry counts, concurrency and budgets do not, so tuning them keeps
        // recordings and cached steps valid.
        let settings = format!(
            "taskloom-store-v4|{}|{}|{}|{}",
            self.provider.cache_namespace(),
            self.cache_namespace,
            self.seeds,
            self.gateway.config().models.answer_settings(),
        );
        Some(hex::encode(Sha256::digest(settings.as_bytes())))
    }

    fn task_timeout(&self, label: &str) -> Option<Duration> {
        self.gateway.task_profile(label).timeout
    }

    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
        Box::pin(async move {
            self.send_request(request)
                .await
                .map(|response| response.answer)
                .map_err(TaskError::from)
        })
    }

    fn send_with_metadata<'a>(
        &'a self,
        request: Request<'a>,
    ) -> BoxFuture<'a, Result<Response, TaskError>> {
        Box::pin(async move { self.send_request(request).await.map_err(TaskError::from) })
    }

    fn embed<'a>(
        &'a self,
        request: EmbedRequest<'a>,
    ) -> BoxFuture<'a, Result<Embedded, TaskError>> {
        Box::pin(async move { self.embed_request(request).await.map_err(TaskError::from) })
    }
}

/// A schema name providers accept: the label's letters, digits, `_`, `-`.
fn schema_name(label: &str) -> String {
    let name = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    if name.is_empty() {
        "answer".to_string()
    } else {
        name
    }
}

fn decode_json(label: &str, response: &ChatResponse) -> Result<serde_json::Value, ModelError> {
    let truncated = response.finish_reason.as_deref() == Some("length");
    crate::json_text::first_json_value_span(&response.content)
        .map_err(|error| error.to_string())
        .and_then(|span| serde_json::from_str(span).map_err(|error| error.to_string()))
        .map_err(|error| {
            ModelError::schema_mismatch(if truncated {
                format!("{label} answer was cut off at the output limit: {error}")
            } else {
                format!("{label} answer is not usable JSON: {error}")
            })
        })
}

fn seed_of(request: &Request<'_>) -> i64 {
    let mut hasher = Sha256::new();
    for part in [
        request.label,
        request.system_prompt,
        request.user_prompt,
        &crate::retry::current_retry_salt().to_string(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    // Positive and within JavaScript's safe integer range.
    (i64::from_le_bytes(bytes) & 0x001F_FFFF_FFFF_FFFF).max(1)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::gateway::{GatewayConfig, ModelProfileOverride, ModelRegistry};
    use crate::{Agent, Environment, PromptTask, Protocol, TaskSpec};

    /// Fails `failing` with a 503, answers the others with `content`.
    struct Fake {
        failing: &'static str,
        content: &'static str,
        asked: Mutex<Vec<String>>,
    }

    /// Keeps the schema and streaming settings each attempt was sent with.
    #[derive(Default)]
    struct Watching {
        seen: Mutex<Vec<(Option<serde_json::Value>, Option<Duration>)>>,
    }

    impl Provider for Watching {
        fn id(&self) -> &str {
            "https://watching.test/v1"
        }

        fn complete<'a>(
            &'a self,
            request: &'a ChatRequest,
            streaming: Option<Streaming<'a>>,
        ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
            self.seen.lock().unwrap().push((
                request.schema.clone(),
                streaming.and_then(|streaming| streaming.idle_timeout),
            ));
            Box::pin(async {
                Ok(ChatResponse {
                    content: "{\"ok\": true}".into(),
                    ..ChatResponse::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn the_schema_goes_to_capable_models_and_idle_timeouts_stream() {
        let gateway = Arc::new(Gateway::new(GatewayConfig {
            models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
                id: "plain".into(),
                supports_json_schema: Some(false),
                ..ModelProfileOverride::default()
            }]),
            ..GatewayConfig::default()
        }));
        let transport = GatewayTransport::new(gateway, Watching::default())
            .with_idle_timeout(Some(Duration::from_secs(20)));
        let environment = Environment::new();
        let schema = serde_json::json!({ "type": "object" });
        for model in ["capable", "plain"] {
            let task = PromptTask::new(
                "schema_task",
                TaskSpec::new(model, Protocol::Json, 0.0, 50).schema(schema.clone()),
                "system",
                "user",
                crate::json_value,
            );
            Agent::new(&transport, &environment)
                .run(&task)
                .await
                .unwrap();
        }
        let seen = transport.provider.seen.lock().unwrap();
        assert_eq!(seen[0], (Some(schema), Some(Duration::from_secs(20))));
        assert_eq!(seen[1], (None, Some(Duration::from_secs(20))));
    }

    impl Provider for Fake {
        fn id(&self) -> &str {
            "https://fake.test/v1"
        }

        fn complete<'a>(
            &'a self,
            request: &'a ChatRequest,
            _streaming: Option<Streaming<'a>>,
        ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
            self.asked.lock().unwrap().push(request.model.clone());
            Box::pin(async move {
                if request.model == self.failing {
                    Err(ModelError::transient(Some(503), "status 503"))
                } else {
                    Ok(ChatResponse {
                        content: self.content.to_string(),
                        usage: Some(TokenUsage {
                            prompt_tokens: Some(10),
                            completion_tokens: Some(2),
                            total_tokens: Some(12),
                        }),
                        finish_reason: Some("stop".into()),
                    })
                }
            })
        }
    }

    #[tokio::test]
    async fn a_failing_model_falls_back_through_the_gateway_chain() {
        let gateway = Arc::new(Gateway::new(GatewayConfig {
            models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
                id: "primary".into(),
                fallback_chain: Some(vec!["backup".into()]),
                ..ModelProfileOverride::default()
            }]),
            prices: crate::gateway::PriceTable::new([crate::gateway::ModelPrice::new(
                "backup", 1.0, 2.0,
            )]),
            time_divisor: 1000,
            ..GatewayConfig::default()
        }));
        let provider = Fake {
            failing: "primary",
            content: "```json\n{\"title\": \"ok\"}\n```",
            asked: Mutex::new(Vec::new()),
        };
        let transport = GatewayTransport::new(gateway.clone(), provider).with_transient_attempts(1);
        let environment = Environment::new();
        let task = PromptTask::new(
            "title",
            TaskSpec::new("primary", Protocol::Json, 0.2, 100),
            "system",
            "user",
            crate::json_value,
        );
        let answer = Agent::new(&transport, &environment)
            .run(&task)
            .await
            .unwrap();
        assert_eq!(answer, serde_json::json!({ "title": "ok" }));
        assert_eq!(
            *transport.provider.asked.lock().unwrap(),
            vec!["primary".to_string(), "backup".to_string()]
        );
        let stats = gateway.stats();
        assert_eq!(stats.sent_attempts, 2);
        assert_eq!(stats.degraded_successes, 1);
        assert_eq!(stats.usage.cost_micros, 14);
    }

    #[tokio::test]
    async fn an_unusable_json_answer_is_a_schema_mismatch() {
        let gateway = Arc::new(Gateway::default());
        let provider = Fake {
            failing: "",
            content: "no json here",
            asked: Mutex::new(Vec::new()),
        };
        let transport = GatewayTransport::new(gateway, provider);
        let environment = Environment::new();
        let task = PromptTask::new(
            "title",
            TaskSpec::new("m", Protocol::Json, 0.2, 100),
            "system",
            "user",
            crate::json_value,
        );
        let error = Agent::new(&transport, &environment)
            .run(&task)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                TaskError::Provider(ModelError::SchemaMismatch { .. })
            ),
            "{error}"
        );
    }

    #[test]
    fn seeds_follow_the_request_and_its_retry_salt() {
        let options = crate::TransportOptions::default();
        let request = Request {
            label: "t",
            model: "m",
            system_prompt: "s",
            user_prompt: "u",
            images: &[],
            attachment: None,
            format: Format::Json,
            phase: crate::Phase::Single,
            temperature: 0.0,
            max_output_tokens: 0,
            schema: None,
            options: &options,
            on_delta: None,
        };
        let first = seed_of(&request);
        assert_eq!(first, seed_of(&request));
        assert!(first > 0);
    }
}
