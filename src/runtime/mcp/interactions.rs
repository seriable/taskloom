//! Opt-in host-reviewed sampling/form handoffs. No model/client/secret lookup.
use super::error;
use crate::runtime::{state::fingerprint, RuntimeError};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpInteractionPolicy {
    /// Host-pinned reply model label. None disables sampling. It never selects
    /// a provider, loads credentials or dispatches a generation request.
    pub sampling_model: Option<String>,
    pub max_sampling_tokens: u64,
    /// Exact safe form schema fingerprint -> host-reviewed field-name allowlist.
    pub elicitation_schemas: BTreeMap<String, BTreeSet<String>>,
    pub max_server_requests: usize,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout: Duration,
}
impl Default for McpInteractionPolicy {
    fn default() -> Self {
        Self {
            sampling_model: None,
            max_sampling_tokens: 128,
            elicitation_schemas: BTreeMap::new(),
            max_server_requests: 16,
            max_request_bytes: 16384,
            max_response_bytes: 16384,
            timeout: Duration::from_secs(5),
        }
    }
}
impl McpInteractionPolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.sampling_model.as_ref().is_some_and(|model| {
            model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control)
        }) || !(1..=32768).contains(&self.max_sampling_tokens)
            || self.elicitation_schemas.len() > 16
            || !(1..=256).contains(&self.max_server_requests)
            || !(512..=1024 * 1024).contains(&self.max_request_bytes)
            || !(512..=1024 * 1024).contains(&self.max_response_bytes)
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(60)
        {
            return Err(error("invalid host interaction policy/limits"));
        }
        for (schema, fields) in &self.elicitation_schemas {
            if schema.len() != 64
                || !schema
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || fields.len() > 16
                || fields.iter().any(|field| !safe_field(field))
            {
                return Err(error("invalid elicitation schema/field pin"));
            }
        }
        Ok(())
    }
}
/// Immutable untrusted server request plus a framework-created exact binding.
/// Deliberately not Deserialize: wire content cannot construct host authority.
#[derive(Clone)]
pub struct McpInteractionRequest {
    method: String,
    params: Value,
    binding: String,
    sampling_model: Option<String>,
}
impl McpInteractionRequest {
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn params(&self) -> &Value {
        &self.params
    }
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn sampling_model(&self) -> Option<&str> {
        self.sampling_model.as_deref()
    }
}
/// Host-only approval value, never deserializable from the server or model.
pub struct McpInteractionApproval {
    binding: String,
    actor: String,
    note: String,
    approved: bool,
}
impl McpInteractionApproval {
    pub fn allow(
        binding: impl Into<String>,
        actor: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let value = Self {
            binding: binding.into(),
            actor: actor.into(),
            note: note.into(),
            approved: true,
        };
        if value.binding.len() != 64
            || !value
                .binding
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || value.actor.trim().is_empty()
            || value.actor.len() > 256
            || value.actor.chars().any(char::is_control)
            || value.note.trim().is_empty()
            || value.note.len() > 1024
        {
            return Err(error(
                "invalid host interaction approval binding/actor/note",
            ));
        }
        Ok(value)
    }
    pub fn deny() -> Self {
        Self {
            binding: String::new(),
            actor: String::new(),
            note: String::new(),
            approved: false,
        }
    }
}
/// Trusted host code supplies review and an explicitly approved reply. Neither
/// callback is an automatic model backend. Any model/API work inside host code
/// still needs its own original durable budget/permission/uncertainty contract.
pub trait McpInteractionHandler: Send + Sync {
    fn identity(&self) -> String;
    fn approve<'a>(
        &'a self,
        request: &'a McpInteractionRequest,
    ) -> BoxFuture<'a, Result<McpInteractionApproval, RuntimeError>>;
    fn respond<'a>(
        &'a self,
        request: &'a McpInteractionRequest,
        approval: &'a McpInteractionApproval,
    ) -> BoxFuture<'a, Result<Value, RuntimeError>>;
}
#[derive(Debug, Clone, Serialize)]
pub struct McpInteractionEvent {
    pub request_binding: String,
    pub method: String,
    pub approved: bool,
    /// Prepared locally; never an assertion that HTTP/stdio delivered it.
    pub response_prepared: bool,
    pub state: String,
    pub approval_fingerprint: Option<String>,
}
struct State {
    seen: BTreeSet<String>,
    events: Vec<McpInteractionEvent>,
}
#[derive(Clone)]
pub struct McpInteractions {
    policy: McpInteractionPolicy,
    handler: Arc<dyn McpInteractionHandler>,
    handler_identity: String,
    identity: String,
    state: Arc<Mutex<State>>,
}
impl McpInteractions {
    pub fn new(
        policy: McpInteractionPolicy,
        handler: Arc<dyn McpInteractionHandler>,
    ) -> Result<Self, RuntimeError> {
        policy.validate()?;
        let handler_identity = handler.identity();
        if handler_identity.trim().is_empty()
            || handler_identity.len() > 256
            || handler_identity.chars().any(char::is_control)
        {
            return Err(error(
                "host interaction handler needs a stable nonsecret identity",
            ));
        }
        let identity = fingerprint(&("mcp-host-interactions-v1", &policy, &handler_identity))?;
        Ok(Self {
            policy,
            handler,
            handler_identity,
            identity,
            state: Arc::new(Mutex::new(State {
                seen: BTreeSet::new(),
                events: Vec::new(),
            })),
        })
    }
    /// New explicit transport connection: isolated request-ID history, same
    /// frozen host policy/handler identity. No reconnect/replay is initiated.
    pub(super) fn session(&self) -> Self {
        Self {
            policy: self.policy.clone(),
            handler: self.handler.clone(),
            handler_identity: self.handler_identity.clone(),
            identity: self.identity.clone(),
            state: Arc::new(Mutex::new(State {
                seen: BTreeSet::new(),
                events: Vec::new(),
            })),
        }
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn audit(&self) -> Result<Vec<McpInteractionEvent>, RuntimeError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| error("host interaction audit lock poisoned"))?
            .events
            .clone())
    }
    pub fn schema_fingerprint(schema: &Value) -> Result<String, RuntimeError> {
        safe_schema(schema)?;
        fingerprint(schema)
    }
    pub(super) fn capabilities(&self) -> Value {
        let mut capabilities = json!({});
        if self.policy.sampling_model.is_some() {
            capabilities["sampling"] = json!({});
        }
        if !self.policy.elicitation_schemas.is_empty() {
            capabilities["elicitation"] = json!({});
        }
        capabilities
    }
    pub(super) async fn handle(
        &self,
        scope: &str,
        protocol: &str,
        message: &Value,
    ) -> Result<Value, RuntimeError> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let valid_id = id.is_i64()
            || id.is_u64()
            || id.as_str().is_some_and(|v| {
                !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)
            });
        if !valid_id
            || message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || message.get("result").is_some()
            || message.get("error").is_some()
        {
            return Ok(rpc_error(id, -32600, "Invalid host interaction request"));
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(rpc_error(id, -32600, "Host interaction method missing"));
        };
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        if serde_json::to_vec(message)
            .map_err(|_| error("cannot encode server request"))?
            .len()
            > self.policy.max_request_bytes
        {
            return Ok(rpc_error(
                id,
                -32602,
                "Host interaction request exceeds limit",
            ));
        }
        let id_key =
            serde_json::to_string(&id).map_err(|_| error("cannot encode server request ID"))?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| error("host interaction state lock poisoned"))?;
            if state.seen.len() >= self.policy.max_server_requests {
                return Err(error("host interaction lifetime request count exceeded"));
            }
            if !state.seen.insert(id_key) {
                return Ok(rpc_error(
                    id,
                    -32600,
                    "Duplicate host interaction ID; no repeated callback",
                ));
            }
        }
        if self.handler.identity() != self.handler_identity {
            return Err(error("host interaction handler identity changed"));
        }
        let supported = match method {
            "sampling/createMessage" => self.validate_sampling(&params),
            "elicitation/create" if protocol == super::PROTOCOLS[0] => self.validate_form(&params),
            _ => Err(error("unavailable host interaction method/version")),
        };
        if supported.is_err() {
            return Ok(rpc_error(
                id,
                -32602,
                "Host interaction is unsupported or outside pinned policy",
            ));
        }
        let request = McpInteractionRequest {
            method: method.into(),
            params,
            binding: fingerprint(&(scope, protocol, &self.identity, message))?,
            sampling_model: self.policy.sampling_model.clone(),
        };
        let event = McpInteractionEvent {
            request_binding: request.binding.clone(),
            method: method.into(),
            approved: false,
            response_prepared: false,
            state: "pending_host_review".into(),
            approval_fingerprint: None,
        };
        let event_index = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| error("host interaction audit lock poisoned"))?;
            let index = state.events.len();
            state.events.push(event);
            index
        };
        let reply = tokio::time::timeout(self.policy.timeout, async {
            let approval = self.handler.approve(&request).await.map_err(|_| ())?;
            if !approval.approved || approval.binding != request.binding {
                return Err(());
            }
            self.update_event(event_index, |event| {
                event.approved = true;
                event.state = "approved_pending_response".into();
                event.approval_fingerprint =
                    fingerprint(&(&approval.binding, &approval.actor, &approval.note)).ok();
            })
            .map_err(|_| ())?;
            if self.handler.identity() != self.handler_identity {
                return Err(());
            }
            let result = self
                .handler
                .respond(&request, &approval)
                .await
                .map_err(|_| ())?;
            if self.handler.identity() != self.handler_identity {
                return Err(());
            }
            self.validate_reply(&request, &result).map_err(|_| ())?;
            let reply = json!({"jsonrpc":"2.0","id":id,"result":result});
            if serde_json::to_vec(&reply).map_err(|_| ())?.len() > self.policy.max_response_bytes {
                return Err(());
            }
            self.update_event(event_index, |event| {
                event.response_prepared = true;
                event.state = "response_prepared_not_delivery".into();
            })
            .map_err(|_| ())?;
            Ok(reply)
        })
        .await;
        Ok(match reply {
            Ok(Ok(value)) => value,
            Ok(Err(())) => {
                self.update_event(event_index, |event| {
                    event.state = "declined_or_invalid_response".into()
                })?;
                rpc_error(
                    id,
                    -32001,
                    "Host interaction declined or invalid approved response",
                )
            }
            Err(_) => {
                self.update_event(event_index, |event| {
                    event.state = "timed_out_callback_outcome_unknown".into()
                })?;
                rpc_error(id, -32001, "Host interaction timed out; no automatic retry")
            }
        })
    }
    fn update_event(
        &self,
        index: usize,
        update: impl FnOnce(&mut McpInteractionEvent),
    ) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("host interaction audit lock poisoned"))?;
        let event = state
            .events
            .get_mut(index)
            .ok_or_else(|| error("host interaction audit entry missing"))?;
        update(event);
        Ok(())
    }
    fn validate_sampling(&self, params: &Value) -> Result<(), RuntimeError> {
        if self.policy.sampling_model.is_none() {
            return Err(error("sampling is not host enabled"));
        }
        let object = params
            .as_object()
            .ok_or_else(|| error("invalid sampling params"))?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "messages"
                    | "maxTokens"
                    | "systemPrompt"
                    | "includeContext"
                    | "modelPreferences"
                    | "temperature"
                    | "stopSequences"
                    | "metadata"
                    | "_meta"
            )
        }) || params
            .get("includeContext")
            .is_some_and(|v| v.as_str() != Some("none"))
            || params
                .get("maxTokens")
                .and_then(Value::as_u64)
                .is_none_or(|n| n == 0 || n > self.policy.max_sampling_tokens)
            || params.get("systemPrompt").is_some_and(|v| !v.is_string())
            || params
                .get("modelPreferences")
                .is_some_and(|v| !v.is_object())
        {
            return Err(error("sampling constraints exceed host policy"));
        }
        let messages = params
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| error("sampling messages missing"))?;
        if messages.is_empty() || messages.len() > 32 {
            return Err(error("invalid sampling message count"));
        }
        for message in messages {
            if !matches!(
                message.get("role").and_then(Value::as_str),
                Some("user" | "assistant")
            ) {
                return Err(error("unsupported sampling message role"));
            }
            let content = message
                .get("content")
                .and_then(Value::as_object)
                .ok_or_else(|| error("sampling content must be text"))?;
            if content.len() != 2
                || content.get("type").and_then(Value::as_str) != Some("text")
                || !content.get("text").is_some_and(Value::is_string)
            {
                return Err(error("only explicit sampling text supported"));
            }
        }
        Ok(())
    }
    fn validate_form(&self, params: &Value) -> Result<(), RuntimeError> {
        let object = params
            .as_object()
            .ok_or_else(|| error("invalid elicitation params"))?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "message" | "requestedSchema" | "mode" | "_meta"
            )
        }) || !params.get("message").is_some_and(Value::is_string)
            || params
                .get("mode")
                .is_some_and(|v| v.as_str() != Some("form"))
        {
            return Err(error(
                "only explicitly pinned form elicitation is supported",
            ));
        }
        let schema = params
            .get("requestedSchema")
            .ok_or_else(|| error("elicitation schema missing"))?;
        safe_schema(schema)?;
        let fields = self
            .policy
            .elicitation_schemas
            .get(&fingerprint(schema)?)
            .ok_or_else(|| error("elicitation schema has no host pin"))?;
        if schema["properties"]
            .as_object()
            .expect("validated schema")
            .keys()
            .any(|field| !fields.contains(field))
        {
            return Err(error("elicitation field not host allowlisted"));
        }
        Ok(())
    }
    fn validate_reply(
        &self,
        request: &McpInteractionRequest,
        result: &Value,
    ) -> Result<(), RuntimeError> {
        let object = result
            .as_object()
            .ok_or_else(|| error("host reply must be an object"))?;
        if request.method == "sampling/createMessage" {
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "role" | "content" | "model" | "stopReason"))
                || result.get("role").and_then(Value::as_str) != Some("assistant")
                || result.get("model").and_then(Value::as_str)
                    != self.policy.sampling_model.as_deref()
                || result.get("stopReason").is_some_and(|v| {
                    !matches!(v.as_str(), Some("endTurn" | "maxTokens" | "stopSequence"))
                })
            {
                return Err(error("sampling reply differs from host model/policy"));
            }
            let content = result
                .get("content")
                .and_then(Value::as_object)
                .ok_or_else(|| error("sampling reply must be text"))?;
            if content.len() != 2
                || content.get("type").and_then(Value::as_str) != Some("text")
                || !content.get("text").is_some_and(Value::is_string)
            {
                return Err(error("only host-supplied sampling text supported"));
            }
        } else {
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "action" | "content"))
            {
                return Err(error("elicitation response has unsupported fields"));
            }
            match result.get("action").and_then(Value::as_str) {
                Some("accept") => {
                    let content = result
                        .get("content")
                        .ok_or_else(|| error("accepted form has no content"))?;
                    jsonschema::validator_for(&request.params["requestedSchema"])
                        .map_err(|_| error("invalid host-pinned form schema"))?
                        .validate(content)
                        .map_err(|_| error("host form response violates exact pinned schema"))?;
                }
                Some("decline" | "cancel") if result.get("content").is_none_or(Value::is_null) => {}
                _ => return Err(error("invalid form action/content")),
            }
        }
        Ok(())
    }
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn safe_field(field: &str) -> bool {
    let lower = field.to_ascii_lowercase();
    !field.is_empty()
        && field.len() <= 64
        && field
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        && !field.as_bytes()[0].is_ascii_digit()
        && !([
            "password",
            "secret",
            "token",
            "private-key",
            "private_key",
            "api-key",
            "api_key",
        ]
        .iter()
        .any(|part| lower.contains(part)))
}
fn safe_schema(schema: &Value) -> Result<(), RuntimeError> {
    let object = schema
        .as_object()
        .ok_or_else(|| error("form schema must be an object"))?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "type" | "properties" | "required" | "additionalProperties" | "title" | "description"
        )
    }) || schema.get("type").and_then(Value::as_str) != Some("object")
        || schema.get("additionalProperties").and_then(Value::as_bool) != Some(false)
    {
        return Err(error(
            "form schema must disallow additional properties/references",
        ));
    }
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| error("form properties missing"))?;
    if properties.len() > 16 {
        return Err(error("form has too many fields"));
    }
    for (name, property) in properties {
        if !safe_field(name) {
            return Err(error("secret-like/unbounded form field refused"));
        }
        let fields = property
            .as_object()
            .ok_or_else(|| error("form field schema must be an object"))?;
        if fields.keys().any(|key| {
            !matches!(
                key.as_str(),
                "type"
                    | "title"
                    | "description"
                    | "enum"
                    | "minLength"
                    | "maxLength"
                    | "minimum"
                    | "maximum"
                    | "default"
            )
        }) || !matches!(
            property.get("type").and_then(Value::as_str),
            Some("string" | "integer" | "number" | "boolean")
        ) {
            return Err(error(
                "only primitive bounded host-reviewed form fields supported",
            ));
        }
    }
    if serde_json::to_vec(schema)
        .map_err(|_| error("cannot encode form schema"))?
        .len()
        > 16384
    {
        return Err(error("form schema exceeds limit"));
    }
    jsonschema::validator_for(schema).map_err(|_| error("invalid form schema"))?;
    Ok(())
}
