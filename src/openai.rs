//! The OpenAI-compatible chat-completions provider (feature `openai`).
//!
//! Works with OpenAI and the many gateways that speak the same protocol
//! (`POST {base_url}/chat/completions`). Images are sent inline as data URLs.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use crate::gateway::{ModelError, TokenUsage};
use crate::provider::{
    ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse, Provider, Streaming,
};

#[cfg(feature = "runtime")]
mod runtime;

/// An OpenAI-compatible endpoint.
#[derive(Clone)]
pub struct OpenAiCompatible {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    json_mode: bool,
    image_detail: Option<String>,
}

impl std::fmt::Debug for OpenAiCompatible {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCompatible")
            .field("base_url", &self.base_url)
            .field("json_mode", &self.json_mode)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatible {
    /// `base_url` up to the API version (`https://api.openai.com/v1`).
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim().trim_end_matches('/').to_string(),
            api_key: api_key.into().trim().to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            json_mode: true,
            image_detail: None,
        }
    }

    /// Uses `client` (proxies, TLS, timeouts of your own).
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Whether JSON requests send `response_format: json_object` (default
    /// on). Turn it off for gateways that reject it: the answer's JSON is
    /// still recovered from the text.
    pub fn with_json_mode(mut self, enabled: bool) -> Self {
        self.json_mode = enabled;
        self
    }

    /// The `detail` of image inputs (`low`, `high`, `auto`).
    pub fn with_image_detail(mut self, detail: impl Into<String>) -> Self {
        self.image_detail = Some(detail.into());
        self
    }

    fn body(&self, request: &ChatRequest) -> Result<Value, ModelError> {
        let user = if request.images.is_empty() {
            Value::String(request.user_prompt.clone())
        } else {
            let mut parts = vec![json!({ "type": "text", "text": request.user_prompt })];
            for path in &request.images {
                let mut image = json!({ "url": data_url(path)? });
                if let Some(detail) = &self.image_detail {
                    image["detail"] = Value::String(detail.clone());
                }
                parts.push(json!({ "type": "image_url", "image_url": image }));
            }
            Value::Array(parts)
        };
        let mut body = json!({
            "model": request.model,
            "messages": [
                { "role": "system", "content": request.system_prompt },
                { "role": "user", "content": user },
            ],
            "temperature": request.temperature,
        });
        if let Some(max) = request.max_output_tokens {
            body["max_tokens"] = json!(max);
        }
        match &request.schema {
            Some(schema) if request.json => {
                body["response_format"] = json!({
                    "type": "json_schema",
                    "json_schema": { "name": request.schema_name, "schema": schema, "strict": false },
                });
            }
            _ if request.json && self.json_mode => {
                body["response_format"] = json!({ "type": "json_object" });
            }
            _ => {}
        }
        if let Some(seed) = request.seed {
            body["seed"] = json!(seed);
        }
        Ok(body)
    }

    fn check(&self, model: &str) -> Result<(), ModelError> {
        if self.base_url.is_empty() || self.api_key.is_empty() || model.trim().is_empty() {
            return Err(ModelError::misconfigured(
                "configure the API base URL, key and model",
            ));
        }
        Ok(())
    }

    async fn post(&self, path: &str, body: &Value) -> Result<reqwest::Response, ModelError> {
        let response = self
            .client
            .post(format!("{}/{path}", self.base_url))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                ModelError::from_transport_error(&error, format!("{path} request failed: {error}"))
            })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        Err(ModelError::from_http_status(
            status.as_u16(),
            &text,
            format!("{path} failed with status {status}: {text}"),
        ))
    }

    async fn send(
        &self,
        request: &ChatRequest,
        streaming: Option<Streaming<'_>>,
    ) -> Result<ChatResponse, ModelError> {
        self.check(&request.model)?;
        let mut body = self.body(request)?;
        let Some(streaming) = streaming else {
            let response = self.post("chat/completions", &body).await?;
            let text = response.text().await.map_err(|error| {
                ModelError::from_transport_error(
                    &error,
                    format!("read chat completion failed: {error}"),
                )
            })?;
            return parse_response(&text);
        };
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });
        let response = self.post("chat/completions", &body).await?;
        let mut answer = ChatResponse::default();
        let mut refused = false;
        let mut saw_finish_reason = false;
        let done = crate::sse::read_events(response, streaming.idle_timeout, |event| {
            if event.data.trim() == "[DONE]" {
                return Ok(true);
            }
            let Ok(chunk) = serde_json::from_str::<Value>(&event.data) else {
                return Ok(false);
            };
            if let Some(choice) = chunk.get("choices").and_then(|choices| choices.get(0)) {
                refused |= choice.get("delta").is_some_and(has_refusal);
                if let Some(delta) = choice.pointer("/delta/content").and_then(Value::as_str) {
                    answer.content.push_str(delta);
                    if let Some(on_delta) = streaming.on_delta {
                        on_delta(delta);
                    }
                }
                let reason = choice
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .filter(|reason| !reason.trim().is_empty());
                saw_finish_reason |= reason.is_some();
                refused |= matches!(reason, Some("refusal" | "content_filter"));
                if reason.is_some() || refused {
                    answer.finish_reason = normalized_finish_reason(reason, refused);
                }
            }
            if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
                answer.usage = serde_json::from_value::<TokenUsage>(usage.clone()).ok();
            }
            Ok(false)
        })
        .await?;
        // Some compatible endpoints omit [DONE] but provide finish_reason.
        // Keep reading after finish_reason so a trailing usage chunk is kept.
        if !done && !saw_finish_reason {
            return Err(ModelError::transient(
                None,
                "chat completion stream ended before [DONE] or a finish_reason",
            ));
        }
        Ok(answer)
    }

    async fn embeddings(
        &self,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse, ModelError> {
        self.check(&request.model)?;
        let body = json!({ "model": request.model, "input": request.inputs });
        let response = self.post("embeddings", &body).await?;
        let value: Value = response.json().await.map_err(|error| {
            ModelError::schema_mismatch(format!("embeddings response is not JSON: {error}"))
        })?;
        parse_embeddings(&value)
    }
}

fn parse_embeddings(value: &Value) -> Result<EmbeddingResponse, ModelError> {
    let mut items = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelError::schema_mismatch("embeddings response has no data"))?
        .iter()
        .enumerate()
        .map(|(position, item)| {
            let index = item
                .get("index")
                .and_then(Value::as_u64)
                .map_or(position, |index| index as usize);
            let vector = item
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| ModelError::schema_mismatch("an embedding has no vector"))?
                .iter()
                .map(|number| number.as_f64().map(|number| number as f32))
                .collect::<Option<Vec<f32>>>()
                .ok_or_else(|| ModelError::schema_mismatch("an embedding is not numeric"))?;
            Ok((index, vector))
        })
        .collect::<Result<Vec<_>, ModelError>>()?;
    items.sort_by_key(|(index, _)| *index);
    Ok(EmbeddingResponse {
        vectors: items.into_iter().map(|(_, vector)| vector).collect(),
        usage: value
            .get("usage")
            .and_then(|usage| serde_json::from_value::<TokenUsage>(usage.clone()).ok()),
    })
}

impl Provider for OpenAiCompatible {
    #[cfg(feature = "runtime")]
    fn converse<'a>(
        &'a self,
        request: &'a crate::runtime::ModelRequest,
    ) -> BoxFuture<'a, Result<crate::runtime::ModelReply, ModelError>> {
        Box::pin(runtime::converse(self, request))
    }
    fn cache_namespace(&self) -> String {
        format!(
            "openai-v2|{}|{}|{:?}",
            self.base_url, self.json_mode, self.image_detail
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

    fn embed<'a>(
        &'a self,
        request: &'a EmbeddingRequest,
    ) -> BoxFuture<'a, Result<EmbeddingResponse, ModelError>> {
        Box::pin(self.embeddings(request))
    }
}

fn has_refusal(message: &Value) -> bool {
    message
        .get("refusal")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
        || message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
            })
}

// Keep refusals in the response until gateway validation, so its reported
// usage is accounted for before ContentRejected is returned to the runner.
fn normalized_finish_reason(reason: Option<&str>, refused: bool) -> Option<String> {
    if refused || matches!(reason, Some("refusal" | "content_filter")) {
        Some("refusal".into())
    } else {
        reason.map(str::to_string)
    }
}

fn parse_response(text: &str) -> Result<ChatResponse, ModelError> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        ModelError::schema_mismatch(format!("chat completion response is not JSON: {error}"))
    })?;
    let choice = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or_else(|| ModelError::schema_mismatch("chat completion response has no choice"))?;
    let content = match choice.pointer("/message/content") {
        Some(Value::String(content)) => content.clone(),
        // Some gateways answer with content parts.
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    Ok(ChatResponse {
        content,
        usage: value
            .get("usage")
            .and_then(|usage| serde_json::from_value::<TokenUsage>(usage.clone()).ok()),
        finish_reason: normalized_finish_reason(
            choice.get("finish_reason").and_then(Value::as_str),
            choice.get("message").is_some_and(has_refusal),
        ),
    })
}

fn data_url(path: &Path) -> Result<String, ModelError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ModelError::misconfigured(format!("read image {} failed: {error}", path.display()))
    })?;
    let mime = match path
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
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_gives_its_content_usage_and_finish_reason() {
        let response = parse_response(
            r#"{"choices":[{"message":{"content":"{\"a\":1}"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}"#,
        )
        .unwrap();
        assert_eq!(response.content, "{\"a\":1}");
        assert_eq!(response.finish_reason.as_deref(), Some("stop"));
        assert_eq!(response.usage.unwrap().total_tokens, Some(8));
    }

    #[test]
    fn refusal_signals_are_normalized_without_losing_usage() {
        for (message, reason) in [
            (
                json!({"content": null, "refusal": "I cannot answer"}),
                "stop",
            ),
            (
                json!({"content": [{"type": "refusal", "refusal": "I cannot answer"}]}),
                "stop",
            ),
            (json!({"content": "filtered"}), "content_filter"),
        ] {
            let response = parse_response(
                &json!({
                    "choices": [{"message": message, "finish_reason": reason}],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(response.finish_reason.as_deref(), Some("refusal"));
            assert_eq!(response.usage.unwrap().total_tokens, Some(8));
        }
        for refusal in [Value::Null, json!(""), json!("   ")] {
            let response = parse_response(&json!({
                "choices": [{"message": {"content": "ok", "refusal": refusal}, "finish_reason": "stop"}]
            }).to_string()).unwrap();
            assert_eq!(response.finish_reason.as_deref(), Some("stop"));
            assert_eq!(response.content, "ok");
        }
    }

    #[test]
    fn a_json_request_asks_for_a_json_object_unless_turned_off() {
        let request = ChatRequest {
            model: "m".into(),
            system_prompt: "s".into(),
            user_prompt: "u".into(),
            images: Vec::new(),
            temperature: 0.1,
            max_output_tokens: Some(50),
            json: true,
            schema: None,
            schema_name: "t".into(),
            seed: Some(7),
        };
        let provider = OpenAiCompatible::new("https://x.test/v1/", "key");
        let body = provider.body(&request).unwrap();
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["max_tokens"], 50);
        assert_eq!(body["seed"], 7);
        assert_eq!(provider.id(), "https://x.test/v1");
        let with_schema = ChatRequest {
            schema: Some(json!({ "type": "object" })),
            ..request.clone()
        };
        let body = provider.body(&with_schema).unwrap();
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "t");
        let body = provider.with_json_mode(false).body(&request).unwrap();
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn embeddings_come_back_in_input_order() {
        let response = parse_embeddings(&json!({
            "data": [
                { "index": 1, "embedding": [0.5, 0.25] },
                { "index": 0, "embedding": [1.0, 0.0] },
            ],
            "usage": { "prompt_tokens": 4, "total_tokens": 4 },
        }))
        .unwrap();
        assert_eq!(response.vectors, vec![vec![1.0, 0.0], vec![0.5, 0.25]]);
        assert_eq!(response.usage.unwrap().prompt_tokens, Some(4));
    }

    fn chat(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            system_prompt: "s".into(),
            user_prompt: "u".into(),
            images: Vec::new(),
            temperature: 0.0,
            max_output_tokens: None,
            json: false,
            schema: None,
            schema_name: "t".into(),
            seed: None,
        }
    }

    fn event(value: Value) -> String {
        format!("data: {value}\n\n")
    }

    #[tokio::test]
    async fn eof_without_a_terminal_signal_is_transient_even_for_valid_json() {
        for (content, reason) in [
            ("partial text", Value::Null),
            ("{\"ok\":true}", Value::Null),
            ("{\"ok\":true}", json!("")),
            ("{\"ok\":true}", json!("   ")),
        ] {
            let (base, server) = crate::test_http::serve_once(
                200,
                true,
                vec![event(
                    json!({"choices": [{"delta": {"content": content}, "finish_reason": reason}]}),
                )],
                Duration::ZERO,
            )
            .await;
            let result = OpenAiCompatible::new(base, "key")
                .complete(&chat("m"), Some(Streaming::default()))
                .await;
            assert!(
                matches!(result, Err(ModelError::Transient { .. })),
                "{result:?}"
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn either_terminal_signal_is_enough_and_trailing_usage_is_retained() {
        for done_marker in [false, true] {
            let mut choice = json!({"delta": {"content": "complete"}});
            if !done_marker {
                choice["finish_reason"] = json!("stop");
            }
            let mut chunks = vec![
                event(json!({"choices": [choice]})),
                event(
                    json!({"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3}}),
                ),
            ];
            if done_marker {
                chunks.push("data: [DONE]\n\n".into());
            }
            let (base, server) =
                crate::test_http::serve_once(200, true, chunks, Duration::ZERO).await;
            let response = OpenAiCompatible::new(base, "key")
                .complete(&chat("m"), Some(Streaming::default()))
                .await
                .unwrap();
            assert_eq!(response.content, "complete");
            assert_eq!(response.usage.unwrap().completion_tokens, Some(3));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn streamed_refusal_survives_a_later_stop_reason_and_keeps_usage() {
        let chunks = vec![
            event(json!({"choices": [{"delta": {"refusal": "I cannot answer"}}]})),
            event(json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
            event(json!({"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3}})),
            "data: [DONE]\n\n".into(),
        ];
        let (base, server) = crate::test_http::serve_once(200, true, chunks, Duration::ZERO).await;
        let response = OpenAiCompatible::new(base, "key")
            .complete(&chat("m"), Some(Streaming::default()))
            .await
            .unwrap();
        assert_eq!(response.finish_reason.as_deref(), Some("refusal"));
        assert_eq!(response.usage.unwrap().prompt_tokens, Some(5));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn real_refusals_stop_the_resilient_protocol_and_keep_accounting() {
        use crate::gateway::{GatewayConfig, ModelPrice, ModelRegistry, PriceTable};
        use crate::{
            Agent, Environment, Gateway, GatewayTransport, PromptTask, Protocol, Recording,
            TaskError, TaskSpec,
        };
        use std::sync::Arc;

        for (streaming, filtered) in [(false, false), (true, false), (false, true), (true, true)] {
            let usage = json!({"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8});
            let reason = if filtered { "content_filter" } else { "stop" };
            let chunks = if streaming {
                let delta = if filtered {
                    json!({"content": "{\"ok\":true}"})
                } else {
                    json!({"refusal": "I cannot answer"})
                };
                vec![
                    event(json!({"choices": [{"delta": delta}]})),
                    event(json!({"choices": [{"delta": {}, "finish_reason": reason}]})),
                    event(json!({"choices": [], "usage": usage})),
                    "data: [DONE]\n\n".into(),
                ]
            } else {
                let message = if filtered {
                    json!({"content": "{\"ok\":true}"})
                } else {
                    json!({"content": null, "refusal": "I cannot answer"})
                };
                vec![json!({"choices": [{"message": message, "finish_reason": reason}], "usage": usage}).to_string()]
            };
            let (base, server) =
                crate::test_http::serve_once(200, streaming, chunks, Duration::ZERO).await;
            let gateway = Arc::new(Gateway::new(GatewayConfig {
                models: ModelRegistry::new().with_default_fallback_chain(vec!["backup".into()]),
                prices: PriceTable::new([ModelPrice::new("m", 1.0, 2.0)]),
                ..Default::default()
            }));
            let transport =
                GatewayTransport::new(gateway.clone(), OpenAiCompatible::new(base, "key"))
                    .with_transient_attempts(1);
            let store = tempfile::tempdir().unwrap();
            let cache = store.path().join("cache");
            let record = store.path().join("record");
            let env = Environment::new()
                .with_step_cache_dir(cache.clone())
                .with_recording(Recording::Record(record.clone()));
            let task = PromptTask::new(
                "refusal",
                TaskSpec::new("m", Protocol::ResilientJson, 0.0, 100)
                    .step_cache(true)
                    .repair(2)
                    .deadline(Duration::from_secs(2)),
                "system",
                "user",
                crate::json_value,
            );
            let progress = |_: &str, _: &str, _: &str| {};
            let agent = Agent::new(&transport, &env);
            let agent = if streaming {
                agent.with_progress(&progress)
            } else {
                agent
            };
            let ledger = crate::ledger::CallLedger::new();
            let result = ledger.scope(agent.run(&task)).await;
            assert!(
                matches!(
                    result,
                    Err(TaskError::Provider(ModelError::ContentRejected { .. }))
                ),
                "{result:?}"
            );
            let stats = gateway.stats();
            assert_eq!(
                stats.sent_attempts, 1,
                "no protocol retry, repair or gateway fallback"
            );
            assert_eq!(stats.outcomes.get("content_rejected"), Some(&1));
            assert_eq!(stats.usage.cost_micros, 11);
            assert_eq!(ledger.snapshot().total_usage().cost_micros, 11);
            assert!(!cache.exists());
            assert!(!record.exists());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn only_completed_streams_are_recorded_and_cached() {
        use crate::{
            Agent, Environment, Gateway, GatewayTransport, PromptTask, Protocol, Recording,
            TaskError, TaskSpec,
        };
        use std::sync::Arc;

        for completed in [false, true] {
            let mut chunks = vec![event(
                json!({"choices": [{"delta": {"content": "{\"ok\":true}"}}]}),
            )];
            if completed {
                chunks.push("data: [DONE]\n\n".into());
            }
            let (base, server) =
                crate::test_http::serve_once(200, true, chunks, Duration::ZERO).await;
            let transport = GatewayTransport::new(
                Arc::new(Gateway::default()),
                OpenAiCompatible::new(base, "key"),
            )
            .with_transient_attempts(1);
            let store = tempfile::tempdir().unwrap();
            let cache = store.path().join("cache");
            let record = store.path().join("record");
            let env = Environment::new()
                .with_step_cache_dir(cache.clone())
                .with_recording(Recording::Record(record.clone()));
            let task = PromptTask::new(
                "stream",
                TaskSpec::new("m", Protocol::Json, 0.0, 100)
                    .step_cache(true)
                    .deadline(Duration::from_secs(2)),
                "system",
                "user",
                crate::json_value,
            );
            let progress = |_: &str, _: &str, _: &str| {};
            let agent = Agent::new(&transport, &env).with_progress(&progress);
            let result = agent.run_traced(&task).await;
            if completed {
                assert_eq!(result.unwrap().output, json!({"ok": true}));
                assert!(agent.run_traced(&task).await.unwrap().reused);
                let replay = Environment::new().with_recording(Recording::Replay(record));
                assert!(
                    Agent::new(&transport, &replay)
                        .run_traced(&task)
                        .await
                        .unwrap()
                        .reused
                );
            } else {
                assert!(
                    matches!(
                        result,
                        Err(TaskError::Provider(ModelError::Transient { .. }))
                    ),
                    "{result:?}"
                );
                assert!(
                    !cache.exists(),
                    "incomplete output must not populate the step cache"
                );
                assert!(
                    !record.exists(),
                    "incomplete output must not become a replayable answer"
                );
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_streamed_answer_reports_deltas_and_usage() {
        let chunks = vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n".to_string(),
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n".to_string(),
        ];
        let (base, server) =
            crate::test_http::serve_once(200, true, chunks, std::time::Duration::from_millis(5))
                .await;
        let provider = OpenAiCompatible::new(format!("{base}/v1"), "key");
        let deltas = std::sync::Mutex::new(Vec::new());
        let on_delta = |delta: &str| deltas.lock().unwrap().push(delta.to_string());
        let response = provider
            .complete(
                &chat("m"),
                Some(Streaming {
                    on_delta: Some(&on_delta),
                    idle_timeout: Some(std::time::Duration::from_secs(5)),
                }),
            )
            .await
            .unwrap();
        assert_eq!(response.content, "Hello");
        assert_eq!(*deltas.lock().unwrap(), vec!["Hel", "lo"]);
        assert_eq!(response.usage.unwrap().completion_tokens, Some(2));
        let request = server.await.unwrap();
        assert!(
            request.starts_with("POST /v1/chat/completions"),
            "{request}"
        );
        assert!(request.contains("\"stream\":true"), "{request}");
    }

    #[tokio::test]
    async fn a_stalled_stream_fails_as_transient_at_the_idle_timeout() {
        let chunks = vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ];
        let (base, _server) =
            crate::test_http::serve_once(200, true, chunks, std::time::Duration::from_secs(3))
                .await;
        let provider = OpenAiCompatible::new(base, "key");
        let started = std::time::Instant::now();
        let error = provider
            .complete(
                &chat("m"),
                Some(Streaming {
                    on_delta: None,
                    idle_timeout: Some(std::time::Duration::from_millis(200)),
                }),
            )
            .await
            .unwrap_err();
        assert!(error.is_transient(), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn an_http_error_is_classified() {
        let (base, _server) = crate::test_http::serve_once(
            429,
            false,
            vec!["{\"error\":{\"message\":\"rate limited\"}}".to_string()],
            std::time::Duration::ZERO,
        )
        .await;
        let error = OpenAiCompatible::new(base, "key")
            .complete(&chat("m"), None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ModelError::Transient {
                    status: Some(429),
                    ..
                }
            ),
            "{error}"
        );
    }
}
