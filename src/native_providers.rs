//! Explicit nonstreaming native protocols. Each call sends exactly one HTTP
//! request; admission, retries and uncertainty belong to GatewayTransport.
//! No server-side conversation identifiers, built-in tools or implicit grants.
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::gateway::{ModelError, TokenUsage};
use crate::provider::{ChatRequest, ChatResponse, Provider, Streaming};

/// Azure uses an explicitly configured full deployment completion endpoint,
/// including its pinned api-version. Vertex base ends in publishers/google.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeProtocol {
    Responses,
    Gemini,
    Vertex,
    Bedrock,
    Azure,
}

#[derive(Clone)]
pub struct NativeProvider {
    protocol: NativeProtocol,
    endpoint: String,
    credential: String,
    client: reqwest::Client,
}
impl std::fmt::Debug for NativeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeProvider")
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}
impl NativeProvider {
    pub fn new(
        protocol: NativeProtocol,
        endpoint: impl Into<String>,
        credential: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let endpoint = endpoint.into();
        Self::validate_endpoint(protocol, &endpoint)?;
        let credential = credential.into();
        if credential.trim().is_empty() || credential.contains(['\r', '\n']) {
            return Err(ModelError::misconfigured(
                "native provider credential is empty or invalid",
            ));
        }
        Ok(Self {
            protocol,
            endpoint: endpoint.trim_end_matches('/').into(),
            credential,
            client: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|_| {
                    ModelError::misconfigured("native provider HTTP client initialization failed")
                })?,
        })
    }
    /// Pure validation, without resolving credentials or contacting a service.
    pub fn validate_endpoint(protocol: NativeProtocol, endpoint: &str) -> Result<(), ModelError> {
        let url = reqwest::Url::parse(endpoint)
            .map_err(|_| ModelError::misconfigured("invalid native provider endpoint"))?;
        let loopback = url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || (url.scheme() == "http" && !loopback)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || (protocol != NativeProtocol::Azure && url.query().is_some())
            || (protocol == NativeProtocol::Azure
                && (url.path().ends_with('/')
                    || !url.path().ends_with("/chat/completions")
                    || url.query_pairs().count() != 1
                    || !url
                        .query_pairs()
                        .any(|(k, v)| k == "api-version" && !v.is_empty())))
        {
            return Err(ModelError::misconfigured("native endpoint requires HTTPS (or loopback HTTP), no embedded credentials, and an explicit Azure api-version only"));
        }
        Ok(())
    }
    /// Pure request-model binding check. Azure's configured model is its exact
    /// deployment identifier; no credential or network work is performed.
    pub fn validate_model_endpoint(
        protocol: NativeProtocol,
        endpoint: &str,
        model: &str,
    ) -> Result<(), ModelError> {
        Self::validate_endpoint(protocol, endpoint)?;
        Self::model_url(protocol, endpoint, model).map(|_| ())
    }
    fn url(&self, model: &str) -> Result<reqwest::Url, ModelError> {
        Self::model_url(self.protocol, &self.endpoint, model)
    }
    fn model_url(
        protocol: NativeProtocol,
        endpoint: &str,
        model: &str,
    ) -> Result<reqwest::Url, ModelError> {
        if model.trim().is_empty() || model.len() > 512 || model.chars().any(char::is_control) {
            return Err(ModelError::misconfigured("invalid native provider model"));
        }
        let mut url = reqwest::Url::parse(endpoint)
            .map_err(|_| ModelError::misconfigured("invalid endpoint"))?;
        match protocol {
            NativeProtocol::Azure => {
                let segments = url
                    .path_segments()
                    .map(|parts| parts.collect::<Vec<_>>())
                    .unwrap_or_default();
                if segments.len() != 5
                    || segments[..2] != ["openai", "deployments"]
                    || segments[2] != model
                    || segments[3..] != ["chat", "completions"]
                {
                    return Err(ModelError::misconfigured("Azure model must equal the exact deployment identifier in /openai/deployments/DEPLOYMENT/chat/completions"));
                }
                return Ok(url);
            }
            NativeProtocol::Responses => {
                url.path_segments_mut()
                    .map_err(|_| ModelError::misconfigured("invalid endpoint path"))?
                    .pop_if_empty()
                    .push("responses");
            }
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                if model.contains('/') {
                    return Err(ModelError::misconfigured(
                        "Gemini/Vertex model must be a model identifier, not a resource path",
                    ));
                }
                url.path_segments_mut()
                    .map_err(|_| ModelError::misconfigured("invalid endpoint path"))?
                    .pop_if_empty()
                    .push("models")
                    .push(&format!("{model}:generateContent"));
            }
            NativeProtocol::Bedrock => {
                url.path_segments_mut()
                    .map_err(|_| ModelError::misconfigured("invalid endpoint path"))?
                    .pop_if_empty()
                    .push("model")
                    .push(model)
                    .push("converse");
            }
        }
        Ok(url)
    }
    async fn post(&self, model: &str, body: &Value) -> Result<Value, ModelError> {
        let encoded = serde_json::to_vec(body)
            .map_err(|_| ModelError::misconfigured("native request serialization failed"))?;
        if encoded.len() > 2 * 1024 * 1024 {
            return Err(ModelError::misconfigured("native request exceeds 2 MiB"));
        }
        let mut request = self
            .client
            .post(self.url(model)?)
            .header("Content-Type", "application/json");
        request = match self.protocol {
            NativeProtocol::Gemini => request.header("x-goog-api-key", &self.credential),
            NativeProtocol::Azure => request.header("api-key", &self.credential),
            _ => request.bearer_auth(&self.credential),
        };
        // Error strings deliberately omit request URLs, headers and raw bodies.
        let mut response = request.body(encoded).send().await.map_err(|e| {
            ModelError::from_transport_error(&e, "native provider transport failed")
        })?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ModelError::from_transport_error(&e, "native response read failed"))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(ModelError::schema_mismatch(
                    "native response exceeds 4 MiB; usage unknown",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            return Err(ModelError::from_http_status(
                status.as_u16(),
                &text,
                format!("native provider HTTP {}", status.as_u16()),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            ModelError::schema_mismatch("native provider returned invalid JSON; usage unknown")
        })
    }
    fn chat_body(&self, request: &ChatRequest) -> Result<Value, ModelError> {
        if !request.images.is_empty() || request.seed.is_some() {
            return Err(ModelError::misconfigured("this native adapter supports text inputs without seeds; unsupported options are not silently ignored"));
        }
        if !request.temperature.is_finite()
            || request.max_output_tokens == Some(0)
            || request.schema.is_some() && !request.json
        {
            return Err(ModelError::misconfigured("native generation needs finite temperature, positive output bound and explicit JSON schema mode"));
        }
        let max = request.max_output_tokens.ok_or_else(|| {
            ModelError::misconfigured("native provider requires explicit output token bound")
        })?;
        let mut body = match self.protocol {
            NativeProtocol::Responses => {
                json!({"model":request.model,"store":false,"stream":false,"input":[{"role":"system","content":request.system_prompt},{"role":"user","content":request.user_prompt}],"temperature":request.temperature,"max_output_tokens":max})
            }
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                json!({"systemInstruction":{"parts":[{"text":request.system_prompt}]},"contents":[{"role":"user","parts":[{"text":request.user_prompt}]}],"generationConfig":{"temperature":request.temperature,"maxOutputTokens":max}})
            }
            NativeProtocol::Bedrock => {
                json!({"system":[{"text":request.system_prompt}],"messages":[{"role":"user","content":[{"text":request.user_prompt}]}],"inferenceConfig":{"temperature":request.temperature,"maxTokens":max}})
            }
            NativeProtocol::Azure => {
                json!({"messages":[{"role":"system","content":request.system_prompt},{"role":"user","content":request.user_prompt}],"temperature":request.temperature,"max_tokens":max,"stream":false})
            }
        };
        if request.json {
            match self.protocol {
                NativeProtocol::Responses => {
                    body["text"] = json!({"format":match &request.schema {Some(schema)=>json!({"type":"json_schema","name":request.schema_name,"schema":schema,"strict":false}),None=>json!({"type":"json_object"})}});
                }
                NativeProtocol::Gemini | NativeProtocol::Vertex => {
                    body["generationConfig"]["responseMimeType"] = json!("application/json");
                    if let Some(schema) = &request.schema {
                        body["generationConfig"]["responseJsonSchema"] = schema.clone();
                    }
                }
                NativeProtocol::Azure => {
                    body["response_format"] = match &request.schema {
                        Some(schema) => {
                            json!({"type":"json_schema","json_schema":{"name":request.schema_name,"schema":schema,"strict":false}})
                        }
                        None => json!({"type":"json_object"}),
                    };
                }
                NativeProtocol::Bedrock => {
                    return Err(ModelError::misconfigured("Bedrock adapter does not silently emulate structured-output schema; use text protocol or a model-specific adapter"));
                }
            }
        }
        Ok(body)
    }
    fn usage(&self, value: &Value) -> Option<TokenUsage> {
        let (root, input, output) = match self.protocol {
            NativeProtocol::Responses => ("usage", "input_tokens", "output_tokens"),
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                ("usageMetadata", "promptTokenCount", "candidatesTokenCount")
            }
            NativeProtocol::Bedrock => ("usage", "inputTokens", "outputTokens"),
            NativeProtocol::Azure => ("usage", "prompt_tokens", "completion_tokens"),
        };
        let usage = value.get(root)?;
        let mut completion = usage.get(output).and_then(Value::as_u64);
        if matches!(
            self.protocol,
            NativeProtocol::Gemini | NativeProtocol::Vertex
        ) {
            // Thinking is also billed output; partial counters remain unknown.
            if let Some(thoughts) = usage.get("thoughtsTokenCount").and_then(Value::as_u64) {
                completion = completion.and_then(|n| n.checked_add(thoughts));
            }
        }
        Some(TokenUsage {
            prompt_tokens: usage.get(input).and_then(Value::as_u64),
            completion_tokens: completion,
            total_tokens: usage
                .get("total_tokens")
                .or_else(|| usage.get("totalTokenCount"))
                .or_else(|| usage.get("totalTokens"))
                .and_then(Value::as_u64),
        })
    }
    fn chat_response(&self, value: &Value) -> Result<ChatResponse, ModelError> {
        // ChatResponse has no error-with-usage field. Native markers are
        // rejected by GatewayTransport only AFTER usage accounting; they never
        // convert a valid JSON prefix or partial text into a successful answer.
        let (content, finish_reason) = match self.response_text(value, false) {
            Ok(parts) => parts,
            Err(_) => (String::new(), "native_invalid"),
        };
        Ok(ChatResponse {
            content,
            usage: self.usage(value),
            finish_reason: Some(finish_reason.into()),
        })
    }

    fn response_text(
        &self,
        value: &Value,
        tools: bool,
    ) -> Result<(String, &'static str), ModelError> {
        let invalid = || {
            ModelError::schema_mismatch(
                "native response contains missing, malformed or unsupported content",
            )
        };
        let mut text = String::new();
        let finish = match self.protocol {
            NativeProtocol::Responses => {
                let output = value
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?;
                let mut refused = false;
                for item in output {
                    if item
                        .get("status")
                        .is_some_and(|status| status.as_str() != Some("completed"))
                    {
                        return Err(invalid());
                    }
                    match item.get("type").and_then(Value::as_str) {
                        Some("message") => {
                            if item.get("role").and_then(Value::as_str) != Some("assistant") {
                                return Err(invalid());
                            }
                            for part in item
                                .get("content")
                                .and_then(Value::as_array)
                                .ok_or_else(invalid)?
                            {
                                match part.get("type").and_then(Value::as_str) {
                                    Some("output_text") => text.push_str(
                                        part.get("text")
                                            .and_then(Value::as_str)
                                            .ok_or_else(invalid)?,
                                    ),
                                    Some("refusal")
                                        if part
                                            .get("refusal")
                                            .and_then(Value::as_str)
                                            .is_some() =>
                                    {
                                        refused = true
                                    }
                                    _ => return Err(invalid()),
                                }
                            }
                        }
                        Some("function_call") if tools => (),
                        // Reasoning/encrypted state cannot be represented by
                        // the legacy Message contract; never drop it silently.
                        _ => return Err(invalid()),
                    }
                }
                if refused {
                    "refusal"
                } else {
                    match value.get("status").and_then(Value::as_str) {
                        Some("completed") => "stop",
                        Some("incomplete") => "native_incomplete",
                        _ => return Err(invalid()),
                    }
                }
            }
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                if value
                    .pointer("/promptFeedback/blockReason")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    return Ok((text, "refusal"));
                }
                let candidates = value
                    .get("candidates")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?;
                if candidates.len() != 1 {
                    return Err(invalid());
                }
                let candidate = &candidates[0];
                let reason = match candidate.get("finishReason").and_then(Value::as_str) {
                    Some("STOP") => "stop",
                    Some("MAX_TOKENS") => "native_incomplete",
                    Some("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII") => {
                        return Ok((text, "refusal"))
                    }
                    _ => return Err(invalid()),
                };
                if candidate.pointer("/content/role").and_then(Value::as_str) != Some("model") {
                    return Err(invalid());
                }
                for part in candidate
                    .pointer("/content/parts")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?
                {
                    let object = part.as_object().ok_or_else(invalid)?;
                    if object
                        .keys()
                        .any(|k| !matches!(k.as_str(), "text" | "functionCall" | "thought"))
                        || object.get("thought").is_some_and(|v| !v.is_boolean())
                    {
                        return Err(invalid());
                    }
                    if let Some(content) = object.get("text") {
                        if object.contains_key("functionCall") {
                            return Err(invalid());
                        }
                        let content = content.as_str().ok_or_else(invalid)?;
                        if object.get("thought").and_then(Value::as_bool) != Some(true) {
                            text.push_str(content);
                        }
                    } else if !(tools
                        && object.get("functionCall").is_some_and(Value::is_object)
                        && object.get("thought").and_then(Value::as_bool) != Some(true))
                    {
                        return Err(invalid());
                    }
                }
                reason
            }
            NativeProtocol::Bedrock => {
                let reason = match value.get("stopReason").and_then(Value::as_str) {
                    Some("end_turn" | "stop_sequence") => "stop",
                    Some("tool_use") if tools => "stop",
                    Some("guardrail_intervened" | "content_filtered") => {
                        return Ok((text, "refusal"))
                    }
                    Some("max_tokens") => "native_incomplete",
                    _ => return Err(invalid()),
                };
                if value
                    .pointer("/output/message/role")
                    .and_then(Value::as_str)
                    != Some("assistant")
                {
                    return Err(invalid());
                }
                for part in value
                    .pointer("/output/message/content")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?
                {
                    let object = part.as_object().ok_or_else(invalid)?;
                    if object.len() != 1 {
                        return Err(invalid());
                    }
                    if let Some(content) = object.get("text") {
                        text.push_str(content.as_str().ok_or_else(invalid)?);
                    } else if !(tools && object.get("toolUse").is_some_and(Value::is_object)) {
                        return Err(invalid());
                    }
                }
                reason
            }
            NativeProtocol::Azure => {
                let choices = value
                    .get("choices")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?;
                if choices.len() != 1 {
                    return Err(invalid());
                }
                let choice = &choices[0];
                let message = choice
                    .get("message")
                    .and_then(Value::as_object)
                    .ok_or_else(invalid)?;
                if choice.get("finish_reason").and_then(Value::as_str) == Some("content_filter") {
                    return Ok((text, "refusal"));
                }
                if message.get("role").and_then(Value::as_str) != Some("assistant")
                    || message.get("function_call").is_some_and(|v| !v.is_null())
                    || message.get("audio").is_some_and(|v| !v.is_null())
                {
                    return Err(invalid());
                }
                if message
                    .get("refusal")
                    .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
                {
                    return Ok((text, "refusal"));
                }
                match message.get("content") {
                    Some(Value::String(content)) => text.push_str(content),
                    Some(Value::Null) | None if tools => (),
                    _ => return Err(invalid()),
                }
                if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
                    let calls = calls.as_array().ok_or_else(invalid)?;
                    if !tools && !calls.is_empty() {
                        return Err(invalid());
                    }
                }
                match choice.get("finish_reason").and_then(Value::as_str) {
                    Some("stop") => "stop",
                    Some("tool_calls") if tools => "stop",
                    Some("content_filter") => "refusal",
                    Some("length") => "native_incomplete",
                    _ => return Err(invalid()),
                }
            }
        };
        if finish == "stop" && text.trim().is_empty() && !tools {
            return Err(invalid());
        }
        Ok((text, finish))
    }
}
impl Provider for NativeProvider {
    fn embed<'a>(
        &'a self,
        request: &'a crate::EmbeddingRequest,
    ) -> BoxFuture<'a, Result<crate::EmbeddingResponse, ModelError>> {
        Box::pin(async move { embedding::embed(self, request).await })
    }
    fn id(&self) -> &str {
        &self.endpoint
    }
    fn cache_namespace(&self) -> String {
        format!("native-v1:{:?}:{}", self.protocol, self.endpoint)
    }
    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        _streaming: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        Box::pin(async move {
            let body = self.chat_body(request)?;
            let value = self.post(&request.model, &body).await?;
            self.chat_response(&value)
        })
    }
    #[cfg(feature = "runtime")]
    fn converse<'a>(
        &'a self,
        request: &'a crate::runtime::ModelRequest,
    ) -> BoxFuture<'a, Result<crate::runtime::ModelReply, ModelError>> {
        Box::pin(async move { self.native_turn(request).await })
    }
}

mod embedding;
#[cfg(feature = "runtime")]
mod runtime;
