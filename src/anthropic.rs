//! The Anthropic Messages API provider (feature `anthropic`).
//!
//! `POST {base_url}/v1/messages` with an API key. Images go inline as
//! base64; a task's JSON Schema goes as `output_config.format` (structured
//! outputs). Sampling parameters are omitted by default for models that do
//! not accept them; [`Anthropic::with_sampling`] enables them when supported.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use crate::gateway::{ModelError, TokenUsage};
use crate::provider::{ChatRequest, ChatResponse, Provider, Streaming};

#[cfg(feature = "runtime")]
mod runtime;

/// The API version sent by default.
pub const DEFAULT_API_VERSION: &str = "2023-06-01";
/// `max_tokens` when the task leaves the output allowance to the provider
/// (the Messages API requires one).
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;

/// An Anthropic Messages API endpoint.
#[derive(Clone)]
pub struct Anthropic {
    base_url: String,
    api_key: String,
    version: String,
    betas: Vec<String>,
    client: reqwest::Client,
    sampling: bool,
    default_max_tokens: u32,
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Anthropic")
            .field("base_url", &self.base_url)
            .field("version", &self.version)
            .field("betas", &self.betas)
            .finish_non_exhaustive()
    }
}

impl Anthropic {
    /// The first-party API (`https://api.anthropic.com`).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url("https://api.anthropic.com", api_key)
    }

    /// Another endpoint speaking the Messages API (a proxy, a gateway).
    pub fn with_base_url(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim().trim_end_matches('/').to_string(),
            api_key: api_key.into().trim().to_string(),
            version: DEFAULT_API_VERSION.to_string(),
            betas: Vec::new(),
            client: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            sampling: false,
            default_max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// The `anthropic-version` header.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// `anthropic-beta` feature ids.
    pub fn with_betas(mut self, betas: Vec<String>) -> Self {
        self.betas = betas;
        self
    }

    /// Sends the task's `temperature` (for API versions that accept it).
    pub fn with_sampling(mut self, enabled: bool) -> Self {
        self.sampling = enabled;
        self
    }

    /// `max_tokens` of tasks that set no output limit.
    pub fn with_default_max_tokens(mut self, tokens: u32) -> Self {
        self.default_max_tokens = tokens.max(1);
        self
    }

    fn body(&self, request: &ChatRequest) -> Result<Value, ModelError> {
        let mut content = Vec::with_capacity(request.images.len() + 1);
        for path in &request.images {
            let (media_type, data) = image(path)?;
            content.push(json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            }));
        }
        content.push(json!({ "type": "text", "text": request.user_prompt }));
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_output_tokens.unwrap_or(self.default_max_tokens),
            "messages": [{ "role": "user", "content": content }],
        });
        if !request.system_prompt.trim().is_empty() {
            body["system"] = json!(request.system_prompt);
        }
        if self.sampling {
            body["temperature"] = json!(request.temperature);
        }
        if let (true, Some(schema)) = (request.json, &request.schema) {
            body["output_config"] =
                json!({ "format": { "type": "json_schema", "schema": schema } });
        }
        Ok(body)
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, ModelError> {
        if self.api_key.is_empty() {
            return Err(ModelError::misconfigured("configure the Anthropic API key"));
        }
        let mut builder = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .json(body);
        if !self.betas.is_empty() {
            builder = builder.header("anthropic-beta", self.betas.join(","));
        }
        let response = builder.send().await.map_err(|error| {
            ModelError::from_transport_error(&error, format!("messages request failed: {error}"))
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        Err(ModelError::from_http_status(
            status.as_u16(),
            &text,
            format!("messages request failed with status {status}: {text}"),
        ))
    }

    async fn send(
        &self,
        request: &ChatRequest,
        streaming: Option<Streaming<'_>>,
    ) -> Result<ChatResponse, ModelError> {
        if request.model.trim().is_empty() {
            return Err(ModelError::misconfigured("configure the model"));
        }
        let mut body = self.body(request)?;
        let Some(streaming) = streaming else {
            let response = self.post(&body).await?;
            let value: Value = response.json().await.map_err(|error| {
                ModelError::schema_mismatch(format!("messages response is not JSON: {error}"))
            })?;
            return Ok(parse_message(&value));
        };
        body["stream"] = json!(true);
        let response = self.post(&body).await?;
        let mut answer = ChatResponse::default();
        let mut usage = TokenUsage::default();
        let stopped = crate::sse::read_events(response, streaming.idle_timeout, |event| {
            let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
                return Ok(false);
            };
            match data.get("type").and_then(Value::as_str).unwrap_or_default() {
                "message_start" => {
                    usage.prompt_tokens = data
                        .pointer("/message/usage/input_tokens")
                        .and_then(Value::as_u64);
                }
                "content_block_delta"
                    if data.pointer("/delta/type").and_then(Value::as_str)
                        == Some("text_delta") =>
                {
                    if let Some(text) = data.pointer("/delta/text").and_then(Value::as_str) {
                        answer.content.push_str(text);
                        if let Some(on_delta) = streaming.on_delta {
                            on_delta(text);
                        }
                    }
                }
                "message_delta" => {
                    if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str)
                    {
                        answer.finish_reason = Some(finish_reason(reason));
                    }
                    if let Some(output) =
                        data.pointer("/usage/output_tokens").and_then(Value::as_u64)
                    {
                        usage.completion_tokens = Some(output);
                    }
                }
                "message_stop" => return Ok(true),
                "error" => return Err(stream_error(&data)),
                _ => {}
            }
            Ok(false)
        })
        .await?;
        if !stopped {
            return Err(ModelError::transient(
                None,
                "Messages stream ended before message_stop",
            ));
        }
        usage.total_tokens = match (usage.prompt_tokens, usage.completion_tokens) {
            (Some(input), Some(output)) => Some(input + output),
            _ => None,
        };
        answer.usage = Some(usage);
        Ok(answer)
    }
}

impl Provider for Anthropic {
    #[cfg(feature = "runtime")]
    fn converse<'a>(
        &'a self,
        request: &'a crate::runtime::ModelRequest,
    ) -> BoxFuture<'a, Result<crate::runtime::ModelReply, ModelError>> {
        Box::pin(runtime::converse(self, request))
    }
    fn cache_namespace(&self) -> String {
        format!(
            "anthropic-v2|{}|{}|{:?}|{}|{}",
            self.base_url, self.version, self.betas, self.sampling, self.default_max_tokens
        )
    }

    fn id(&self) -> &str {
        &self.base_url
    }

    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        streaming: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        Box::pin(self.send(request, streaming))
    }
}

/// `max_tokens` reads as `length`, `refusal` stays `refusal`.
fn finish_reason(stop_reason: &str) -> String {
    match stop_reason {
        "max_tokens" => "length".to_string(),
        other => other.to_string(),
    }
}

fn parse_message(value: &Value) -> ChatResponse {
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let mut usage = value
        .get("usage")
        .and_then(|usage| serde_json::from_value::<TokenUsage>(usage.clone()).ok());
    if let Some(usage) = usage.as_mut() {
        if usage.total_tokens.is_none() {
            usage.total_tokens = match (usage.prompt_tokens, usage.completion_tokens) {
                (Some(input), Some(output)) => Some(input + output),
                _ => None,
            };
        }
    }
    ChatResponse {
        content,
        usage,
        finish_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(finish_reason),
    }
}

/// An `error` event inside a stream: overload and server errors are
/// transient, the rest are refused requests.
fn stream_error(data: &Value) -> ModelError {
    let kind = data
        .pointer("/error/type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let detail = format!("the answer stream failed: {data}");
    match kind {
        "overloaded_error" | "api_error" => ModelError::transient(Some(529), detail),
        "rate_limit_error" => ModelError::transient(Some(429), detail),
        "authentication_error" | "permission_error" => ModelError::Auth { detail },
        _ => ModelError::misconfigured(detail),
    }
}

fn image(path: &Path) -> Result<(&'static str, String), ModelError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ModelError::misconfigured(format!("read image {} failed: {error}", path.display()))
    })?;
    let media_type = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => "image/jpeg",
    };
    Ok((
        media_type,
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ChatRequest {
        ChatRequest {
            model: "claude-opus-5-5".into(),
            system_prompt: "Answer in JSON.".into(),
            user_prompt: "Title this.".into(),
            images: Vec::new(),
            temperature: 0.2,
            max_output_tokens: None,
            json: true,
            schema: Some(json!({ "type": "object", "additionalProperties": false })),
            schema_name: "title".into(),
            seed: None,
        }
    }

    #[test]
    fn a_request_body_has_no_sampling_and_carries_the_schema() {
        let provider = Anthropic::new("key");
        let body = provider.body(&request()).unwrap();
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["system"], "Answer in JSON.");
        assert!(body.get("temperature").is_none());
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        let body = provider.with_sampling(true).body(&request()).unwrap();
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_message_gives_its_text_usage_and_stop_reason() {
        let response = parse_message(&json!({
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "{\"title\":" },
                { "type": "text", "text": "\"ok\"}" },
            ],
            "stop_reason": "max_tokens",
            "usage": { "input_tokens": 12, "output_tokens": 3 },
        }));
        assert_eq!(response.content, "{\"title\":\"ok\"}");
        assert_eq!(response.finish_reason.as_deref(), Some("length"));
        let usage = response.usage.unwrap();
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (Some(12), Some(3), Some(15))
        );
    }

    #[test]
    fn stream_errors_are_classified() {
        let overloaded =
            stream_error(&json!({ "type": "error", "error": { "type": "overloaded_error" } }));
        assert!(overloaded.is_transient());
        let invalid =
            stream_error(&json!({ "type": "error", "error": { "type": "invalid_request_error" } }));
        assert!(!invalid.is_transient());
    }

    #[tokio::test]
    async fn a_stream_without_message_stop_is_transient() {
        let chunks = vec![
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"{\\\"ok\\\":true}\"}}\n\n".into(),
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n".into(),
        ];
        let (base, server) = crate::test_http::serve_once(200, true, chunks, Duration::ZERO).await;
        let result = Anthropic::with_base_url(base, "key")
            .complete(&request(), Some(Streaming::default()))
            .await;
        assert!(
            matches!(result, Err(ModelError::Transient { .. })),
            "{result:?}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_streamed_message_reports_deltas_usage_and_headers() {
        let events = [
            ("message_start", "{\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}"),
            ("content_block_start", "{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}"),
            ("content_block_delta", "{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"{\\\"a\\\":\"}}"),
            ("content_block_delta", "{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"1}\"}}"),
            ("message_delta", "{\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}"),
            ("message_stop", "{\"type\":\"message_stop\"}"),
        ];
        let chunks = events
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect();
        let (base, server) =
            crate::test_http::serve_once(200, true, chunks, std::time::Duration::from_millis(2))
                .await;
        let provider = Anthropic::with_base_url(base, "key");
        let deltas = std::sync::Mutex::new(String::new());
        let on_delta = |delta: &str| deltas.lock().unwrap().push_str(delta);
        let response = provider
            .complete(
                &request(),
                Some(Streaming {
                    on_delta: Some(&on_delta),
                    idle_timeout: Some(std::time::Duration::from_secs(5)),
                }),
            )
            .await
            .unwrap();
        assert_eq!(response.content, "{\"a\":1}");
        assert_eq!(*deltas.lock().unwrap(), "{\"a\":1}");
        assert_eq!(response.finish_reason.as_deref(), Some("end_turn"));
        let usage = response.usage.unwrap();
        assert_eq!(
            (usage.prompt_tokens, usage.completion_tokens),
            (Some(9), Some(4))
        );
        let raw = server.await.unwrap().to_ascii_lowercase();
        assert!(raw.starts_with("post /v1/messages"), "{raw}");
        assert!(raw.contains("x-api-key: key"), "{raw}");
        assert!(raw.contains("anthropic-version: 2023-06-01"), "{raw}");
    }
}
