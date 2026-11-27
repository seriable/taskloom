//! Explicitly configured MCP Streamable HTTP tool access (feature `mcp`).
//! Pins protocol negotiation to 2025-06-18 / 2025-03-26, supports JSON and
//! finite response SSE, and never retries a tools/call or trusts effect hints.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{future::BoxFuture, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{RuntimeError, Tool, ToolContext, ToolDefinition, ToolEffect, ToolError};

const PROTOCOLS: [&str; 2] = ["2025-06-18", "2025-03-26"];

/// No Debug implementation: endpoint queries and bearer tokens can be secret.
pub struct McpHttpConfig {
    pub endpoint: String,
    pub bearer_token: Option<String>,
    /// Stable nonsecret tenant/configuration identity for durable reentry.
    pub namespace: String,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_tools: usize,
}
impl McpHttpConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            bearer_token: None,
            namespace: "default".into(),
            timeout: Duration::from_secs(30),
            max_response_bytes: 2 * 1024 * 1024,
            max_tools: 256,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}
/// Every remote tool binding requires a host-authored side-effect declaration.
/// MCP annotations are advisory and never grant tool execution permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolBinding {
    pub name: String,
    pub remote_name: String,
    pub effect: ToolEffect,
    pub permissions: BTreeSet<String>,
}

#[derive(Clone)]
pub struct McpClient {
    connection: Arc<Connection>,
    tools: Arc<BTreeMap<String, McpToolInfo>>,
    namespace: String,
}
struct Connection {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    session: Option<String>,
    protocol: Option<String>,
    max_bytes: usize,
    next_id: AtomicU64,
    closed: AtomicBool,
}
impl McpClient {
    /// Initializes a session and discovers a bounded tool catalog. This does
    /// not execute advertised tools or register any of them in the runtime.
    pub async fn connect(config: McpHttpConfig) -> Result<Self, RuntimeError> {
        let endpoint =
            reqwest::Url::parse(&config.endpoint).map_err(|_| error("invalid endpoint URL"))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || config.timeout.is_zero()
            || config.max_response_bytes < 128
            || config.max_response_bytes > 16 * 1024 * 1024
            || config.max_tools == 0
            || config.max_tools > 1024
            || config.namespace.trim().is_empty()
            || config.namespace.len() > 256
        {
            return Err(error(
                "invalid endpoint, timeout or response/catalog limits",
            ));
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        if let Some(token) = config.bearer_token {
            if token.is_empty() {
                return Err(error("empty bearer token"));
            }
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| error("invalid bearer header"))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(http_error)?;
        let mut connection = Connection {
            client,
            endpoint,
            session: None,
            protocol: None,
            max_bytes: config.max_response_bytes,
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        };
        let (initialize,session)=connection.request("initialize",json!({"protocolVersion":PROTOCOLS[0],"capabilities":{},"clientInfo":{"name":"taskloom","version":env!("CARGO_PKG_VERSION")}})).await?;
        let version = initialize
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|v| PROTOCOLS.contains(v))
            .ok_or_else(|| error("server negotiated an unsupported protocol version"))?;
        if !initialize
            .get("capabilities")
            .and_then(|v| v.get("tools"))
            .is_some_and(Value::is_object)
        {
            return Err(error("server does not advertise tools capability"));
        }
        connection.protocol = Some(version.into());
        connection.session = session;
        connection.notify("notifications/initialized").await?;
        let mut tools = BTreeMap::new();
        let mut cursor: Option<String> = None;
        let mut cursors = BTreeSet::new();
        let mut complete = false;
        for _ in 0..16 {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            let (page, _) = connection.request("tools/list", params).await?;
            let listed = page
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| error("invalid tools/list result"))?;
            if tools.len().saturating_add(listed.len()) > config.max_tools {
                return Err(error("tool catalog exceeds limit"));
            }
            for value in listed {
                let info: McpToolInfo = serde_json::from_value(value.clone())
                    .map_err(|_| error("malformed remote tool definition"))?;
                if info.name.trim().is_empty()
                    || info.name.len() > 256
                    || info.description.len() > 16 * 1024
                    || !info.input_schema.is_object()
                    || tools.insert(info.name.clone(), info).is_some()
                {
                    return Err(error("invalid or duplicate remote tool"));
                }
            }
            cursor = match page.get("nextCursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(next)) if !next.is_empty() && next.len() <= 4096 => {
                    Some(next.clone())
                }
                _ => return Err(error("invalid tool catalog cursor")),
            };
            match &cursor {
                None => {
                    complete = true;
                    break;
                }
                Some(next) if !cursors.insert(next.clone()) => {
                    return Err(error("repeated tool catalog cursor"))
                }
                _ => {}
            }
        }
        if !complete {
            return Err(error("tool catalog pagination limit exceeded"));
        }
        Ok(Self {
            connection: Arc::new(connection),
            tools: Arc::new(tools),
            namespace: config.namespace,
        })
    }
    pub fn tools(&self) -> impl Iterator<Item = &McpToolInfo> {
        self.tools.values()
    }
    pub fn protocol_version(&self) -> &str {
        self.connection.protocol.as_deref().unwrap()
    }
    pub fn bind(&self, binding: McpToolBinding) -> Result<McpTool, RuntimeError> {
        let remote = self
            .tools
            .get(&binding.remote_name)
            .ok_or_else(|| error("remote tool was not discovered"))?;
        let identity = serde_json::to_vec(&(
            self.connection.endpoint.as_str(),
            &self.namespace,
            &binding.remote_name,
            self.protocol_version(),
        ))
        .map_err(|_| error("cannot fingerprint tool binding"))?;
        let fingerprint = hex::encode(Sha256::digest(identity));
        let definition = ToolDefinition {
            name: binding.name,
            description: format!("{}\n[MCP binding {fingerprint}]", remote.description),
            effect: binding.effect,
            permissions: binding.permissions,
            input_schema: remote.input_schema.clone(),
        };
        Ok(McpTool {
            client: self.clone(),
            remote_name: binding.remote_name,
            definition,
        })
    }
    /// Optional explicit session teardown. It never reinitializes or replays
    /// an expired session automatically; rebuild a catalog/binding explicitly.
    pub async fn close(&self) -> Result<(), RuntimeError> {
        if self.connection.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if self.connection.session.is_none() {
            return Ok(());
        }
        let response = self
            .connection
            .headers(
                self.connection
                    .client
                    .delete(self.connection.endpoint.clone()),
            )
            .send()
            .await
            .map_err(http_error)?;
        if response.status().is_success()
            || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            Ok(())
        } else {
            Err(error(&format!(
                "session close returned HTTP {}",
                response.status().as_u16()
            )))
        }
    }
}

#[derive(Clone)]
pub struct McpTool {
    client: McpClient,
    remote_name: String,
    definition: ToolDefinition,
}
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn execute<'a>(
        &'a self,
        arguments: Value,
        context: ToolContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            // Stable runtime context is deliberately not presented as an MCP
            // idempotency guarantee: JSON-RPC request IDs do not deduplicate effects.
            let _ = context;
            let (result, _) = self
                .client
                .connection
                .request(
                    "tools/call",
                    json!({"name":self.remote_name,"arguments":arguments}),
                )
                .await
                .map_err(|e| ToolError(e.to_string()))?;
            if !result.get("content").is_some_and(Value::is_array)
                || result.get("isError").is_some_and(|v| !v.is_boolean())
            {
                return Err(ToolError(
                    "MCP tools/call returned malformed content/isError".into(),
                ));
            }
            if result.get("isError").and_then(Value::as_bool) == Some(true) {
                return Err(ToolError(format!("MCP tool reported error: {result}")));
            }
            // Preserve content, structuredContent and resource links as data;
            // no remote resources, image URLs or instructions are executed.
            Ok(result)
        })
    }
}
impl Connection {
    fn headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let builder = if let Some(session) = &self.session {
            builder.header("Mcp-Session-Id", session)
        } else {
            builder
        };
        if let Some(protocol) = &self.protocol {
            builder.header("MCP-Protocol-Version", protocol)
        } else {
            builder
        }
    }
    async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(Value, Option<String>), RuntimeError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(error("session is closed"));
        }
        let mut id = self.next_id.load(Ordering::Relaxed);
        loop {
            let next = id
                .checked_add(1)
                .ok_or_else(|| error("request ID exhausted"))?;
            match self
                .next_id
                .compare_exchange_weak(id, next, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(current) => id = current,
            }
        }
        let response = self
            .headers(self.client.post(self.endpoint.clone()))
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .map_err(http_error)?;
        if !response.status().is_success() {
            return Err(error(&format!(
                "{method} returned HTTP {}; no automatic retry",
                response.status().as_u16()
            )));
        }
        let session = response
            .headers()
            .get("Mcp-Session-Id")
            .map(|header| {
                header
                    .to_str()
                    .map(str::to_string)
                    .map_err(|_| error("invalid session header"))
            })
            .transpose()?;
        if session.as_ref().is_some_and(|s| {
            s.is_empty() || s.len() > 1024 || !s.bytes().all(|b| b.is_ascii_graphic())
        }) {
            return Err(error("invalid session header"));
        }
        if self.session.is_some() && session.is_some() && self.session != session {
            return Err(error("server changed session identity"));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let sse = match content_type {
            "text/event-stream" => true,
            "application/json" => false,
            _ => return Err(error("unsupported MCP response content type")),
        };
        let value = read_response(response, sse, self.max_bytes, id).await?;
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || value.get("id").and_then(Value::as_u64) != Some(id)
        {
            return Err(error("JSON-RPC response identity mismatch"));
        }
        if value.get("error").is_some() {
            return Err(error(
                "server returned a JSON-RPC error; no automatic retry",
            ));
        }
        let result = value
            .get("result")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| error("JSON-RPC response has no object result"))?;
        Ok((result, session))
    }
    async fn notify(&self, method: &str) -> Result<(), RuntimeError> {
        let response = self
            .headers(self.client.post(self.endpoint.clone()))
            .json(&json!({"jsonrpc":"2.0","method":method}))
            .send()
            .await
            .map_err(http_error)?;
        if response.status() != reqwest::StatusCode::ACCEPTED {
            return Err(error(
                "initialized notification was not accepted with HTTP 202",
            ));
        }
        Ok(())
    }
}
async fn read_response(
    response: reqwest::Response,
    sse: bool,
    max: usize,
    id: u64,
) -> Result<Value, RuntimeError> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(error("MCP response exceeds byte limit"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut total = 0usize;
    while let Some(part) = stream.next().await {
        let part = part.map_err(http_error)?;
        total = total
            .checked_add(part.len())
            .ok_or_else(|| error("response byte overflow"))?;
        if total > max {
            return Err(error("MCP response exceeds byte limit"));
        }
        bytes.extend_from_slice(&part);
        if sse {
            while let Some((end, delimiter)) = event_end(&bytes) {
                let event = std::str::from_utf8(&bytes[..end])
                    .map_err(|_| error("non-UTF-8 SSE response"))?;
                let data = event
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|data| data.strip_prefix(' ').unwrap_or(data))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !data.is_empty() {
                    let value: Value = serde_json::from_str(&data)
                        .map_err(|_| error("malformed SSE JSON-RPC data"))?;
                    if value.get("id").is_some() {
                        if value.get("id").and_then(Value::as_u64) != Some(id) {
                            return Err(error("SSE JSON-RPC response ID mismatch"));
                        }
                        return Ok(value);
                    }
                    // Only JSON-RPC notifications may precede this response.
                    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                        || !value.get("method").is_some_and(Value::is_string)
                    {
                        return Err(error("invalid SSE notification"));
                    }
                }
                bytes.drain(..end + delimiter);
            }
        }
    }
    if sse {
        return Err(error("SSE ended without a complete matching response"));
    }
    serde_json::from_slice(&bytes).map_err(|_| error("malformed JSON-RPC response"))
}
fn event_end(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes.windows(2).position(|w| w == b"\n\n").map(|n| (n, 2));
    let crlf = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|n| (n, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}
fn error(detail: &str) -> RuntimeError {
    RuntimeError::Protocol(format!("MCP: {detail}"))
}
fn http_error(error: reqwest::Error) -> RuntimeError {
    RuntimeError::Protocol(format!("MCP HTTP: {}", error.without_url()))
}
