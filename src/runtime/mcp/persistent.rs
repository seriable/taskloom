//! SQL-owned durable HTTP recovery. No automatic POST replay or host callbacks.
use super::{
    error, event_end, validate_response, McpClient, McpHttpConfig, McpHttpExtensions, McpToolInfo,
    PROTOCOLS,
};
use crate::runtime::{
    database::{DatabaseNamespace, SqlDatabase, SqlObjectLease, SqlObjectStore},
    state::fingerprint,
    RuntimeError, ToolContext,
};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPersistentPolicy {
    pub max_calls: usize,
    pub max_get_attempts: usize,
    pub max_events: usize,
    pub max_event_id_bytes: usize,
    pub max_request_bytes: usize,
    pub max_journal_bytes: usize,
    #[serde(default)]
    pub allow_http_loopback_for_tests: bool,
}
impl Default for McpPersistentPolicy {
    fn default() -> Self {
        Self {
            max_calls: 64,
            max_get_attempts: 1,
            max_events: 256,
            max_event_id_bytes: 256,
            max_request_bytes: 1024 * 1024,
            max_journal_bytes: 8 * 1024 * 1024,
            allow_http_loopback_for_tests: false,
        }
    }
}
/// Host-only transport/principal policy. Never accepts a model-selected store,
/// bearer, namespace or extension. All extensions are statically refused in v1.
pub struct McpPersistentConfig {
    pub http: McpHttpConfig,
    pub principal_namespace: String,
    pub policy: McpPersistentPolicy,
    pub extensions: McpHttpExtensions,
}
impl McpPersistentConfig {
    pub fn new(http: McpHttpConfig, principal_namespace: impl Into<String>) -> Self {
        Self {
            http,
            principal_namespace: principal_namespace.into(),
            policy: Default::default(),
            extensions: Default::default(),
        }
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.extensions.interactions.is_some()
            || self.extensions.oauth.is_some()
            || self.extensions.resumption.is_some()
        {
            return Err(error("persistent v1 refuses sampling/elicitation/OAuth/ephemeral extensions before dispatch"));
        }
        let url = reqwest::Url::parse(&self.http.endpoint).map_err(|_| bad())?;
        let policy = &self.policy;
        if self.http.endpoint.len() > 4096
            || self.http.endpoint.trim() != self.http.endpoint
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || !(url.scheme() == "https"
                || policy.allow_http_loopback_for_tests
                    && url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
            || self.http.timeout.is_zero()
            || self.http.timeout > Duration::from_secs(60)
            || !(128..=2 * 1024 * 1024).contains(&self.http.max_response_bytes)
            || !(1..=256).contains(&self.http.max_tools)
            || !label(&self.http.namespace)
            || !label(&self.principal_namespace)
            || !(1..=256).contains(&policy.max_calls)
            || !(1..=8).contains(&policy.max_get_attempts)
            || !(1..=1024).contains(&policy.max_events)
            || !(1..=1024).contains(&policy.max_event_id_bytes)
            || !(512..=2 * 1024 * 1024).contains(&policy.max_request_bytes)
            || !(16384..=16 * 1024 * 1024).contains(&policy.max_journal_bytes)
            || policy.max_journal_bytes < self.reservation_bytes()
        {
            return Err(bad());
        }
        if let Some(token) = &self.http.bearer_token {
            if token.is_empty()
                || token.len() > 16384
                || HeaderValue::from_str(&format!("Bearer {token}")).is_err()
            {
                return Err(bad());
            }
        }
        Ok(())
    }
    fn reservation_bytes(&self) -> usize {
        self.http.max_response_bytes
            + self.policy.max_events * (self.policy.max_event_id_bytes + 32)
            + 8192
    }
    fn binding(&self, scope: &str) -> Result<String, RuntimeError> {
        fingerprint(&(
            "mcp-persistent-http-v1",
            scope,
            &self.http.endpoint,
            &self.http.namespace,
            &self.principal_namespace,
            self.http.timeout,
            self.http.max_response_bytes,
            self.http.max_tools,
            &self.policy,
        ))
    }
    fn client(&self) -> Result<reqwest::Client, RuntimeError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        if let Some(token) = &self.http.bearer_token {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| bad())?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(self.http.timeout)
            .build()
            .map_err(|_| bad())
    }
}
#[derive(Clone)]
pub struct SqlMcpSessionStore {
    objects: SqlObjectStore,
    scope: String,
    max_sessions: usize,
}
impl SqlMcpSessionStore {
    pub fn new(database: SqlDatabase, tenant: &str, project: &str) -> Result<Self, RuntimeError> {
        Ok(Self {
            objects: database.objects(DatabaseNamespace::new(tenant, project)?),
            scope: fingerprint(&("mcp-protected-store-v1", tenant, project))?,
            max_sessions: 64,
        })
    }
    pub fn with_capacity(mut self, max_sessions: usize) -> Result<Self, RuntimeError> {
        if !(1..=256).contains(&max_sessions) {
            return Err(bad());
        }
        self.max_sessions = max_sessions;
        Ok(self)
    }
    fn key(&self, name: &str) -> Result<String, RuntimeError> {
        if !label(name) {
            return Err(bad());
        }
        Ok(format!("mcp-http-v1-{}", fingerprint(&name)?))
    }
    async fn reserve(&self, key: &str, instance: &str) -> Result<(), RuntimeError> {
        let key = key.to_owned();
        let instance = instance.to_owned();
        let cap = self.max_sessions;
        self.objects
            .modify(
                "mcp-persistent-registry-v1",
                Registry {
                    version: 1,
                    capacity: cap,
                    slots: BTreeMap::new(),
                },
                move |state| {
                    if state.version != 1
                        || state.capacity != cap
                        || state.slots.len() >= cap
                        || state.slots.contains_key(&key)
                    {
                        return Err(error(
                            "persistent session capacity/name already reserved; no silent reset",
                        ));
                    }
                    state.slots.insert(key, instance);
                    Ok(())
                },
            )
            .await
            .map_err(|_| error("persistent session slot unavailable; no network dispatch"))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    capacity: usize,
    slots: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTool {
    name: String,
    description: String,
    input_schema: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    version: u32,
    instance: String,
    binding: String,
    ready: bool,
    session_header: Option<String>,
    protocol: Option<String>,
    next_id: u64,
    catalog: Vec<CatalogTool>,
    transport_sha256: Option<String>,
    calls: BTreeMap<String, Call>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    request_id: u64,
    binding: String,
    remote_name: String,
    args_sha256: String,
    cursor: Option<String>,
    seen_events: BTreeSet<String>,
    blocked_callback_ids: BTreeSet<String>,
    get_attempts: usize,
    events_seen: usize,
    state: CallState,
    response: Option<Value>,
    response_sha256: Option<String>,
    adoption_sha256: Option<String>,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CallState {
    PostOutcomeUnknown,
    ObservedResult,
    HostAcknowledged,
    AbandonedUnknown,
    BlockedCallbackUnknown,
}
impl CallState {
    fn name(self) -> &'static str {
        match self {
            Self::PostOutcomeUnknown => "post_outcome_unknown",
            Self::ObservedResult => "observed_result",
            Self::HostAcknowledged => "host_acknowledged",
            Self::AbandonedUnknown => "abandoned_unknown",
            Self::BlockedCallbackUnknown => "blocked_callback_unknown",
        }
    }
}
/// No Deserialize/Debug: wire content cannot construct a recovery authority.
#[derive(Clone)]
pub struct PersistentMcpCall {
    instance: String,
    key: String,
    binding: String,
    request_id: u64,
}
impl PersistentMcpCall {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }
    pub fn binding(&self) -> &str {
        &self.binding
    }
}
/// Intentionally contains no session header, endpoint, params, event ID or text.
#[derive(Clone, Debug, Serialize)]
pub struct PersistentMcpSummary {
    pub request_id: u64,
    pub binding: String,
    pub state: String,
    pub has_cursor: bool,
    pub get_attempts: usize,
    pub response_sha256: Option<String>,
}
/// Exact protected response, retained in SQL until the host explicitly ACKs it.
/// The host must durably adopt it to its RunStore before acknowledging.
pub struct PersistentMcpObserved {
    call: PersistentMcpCall,
    response: Value,
    sha256: String,
}
impl PersistentMcpObserved {
    pub fn call(&self) -> &PersistentMcpCall {
        &self.call
    }
    pub fn response(&self) -> &Value {
        &self.response
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn result(&self) -> Result<&Value, RuntimeError> {
        if self.response.get("error").is_some() {
            return Err(error("original MCP call returned a durable JSON-RPC error"));
        }
        self.response.get("result").ok_or_else(bad)
    }
}
#[derive(Clone)]
pub struct PersistentMcpClient {
    inner: Arc<Inner>,
}
struct Inner {
    store: SqlMcpSessionStore,
    key: String,
    config: McpPersistentConfig,
    http: reqwest::Client,
    lease: Mutex<Option<SqlObjectLease>>,
    gate: AsyncMutex<()>,
    closed: AtomicBool,
    save_unknown: AtomicBool,
    instance: String,
    binding: String,
}
impl PersistentMcpClient {
    /// One-time bootstrap. Any existing/reserved logical name is refused, even
    /// if initialization died before its session header could be persisted.
    pub async fn create(
        config: McpPersistentConfig,
        store: SqlMcpSessionStore,
        name: &str,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let key = store.key(name)?;
        let binding = config.binding(&store.scope)?;
        let http = config.client()?;
        let lease = store.objects.acquire(&key).await.map_err(|_| owner())?;
        if store
            .objects
            .load::<Session>(&key)
            .await
            .map_err(|_| bad())?
            .is_some()
        {
            return Err(error(
                "durable session already exists; use open, never reinitialize",
            ));
        }
        let instance = uuid::Uuid::new_v4().simple().to_string();
        store.reserve(&key, &instance).await?;
        let mut state = Session {
            version: 1,
            instance: instance.clone(),
            binding: binding.clone(),
            ready: false,
            session_header: None,
            protocol: None,
            next_id: 1,
            catalog: Vec::new(),
            transport_sha256: None,
            calls: BTreeMap::new(),
        };
        lease.save(&state).await.map_err(|_| owner())?;
        let bootstrap = McpHttpConfig {
            endpoint: config.http.endpoint.clone(),
            bearer_token: config.http.bearer_token.clone(),
            namespace: config.http.namespace.clone(),
            timeout: config.http.timeout,
            max_response_bytes: config.http.max_response_bytes,
            max_tools: config.http.max_tools,
        };
        let client = McpClient::connect(bootstrap).await?;
        let super::Connection::Http(connection) = client.connection.as_ref() else {
            return Err(bad());
        };
        state.session_header=Some(connection.session.clone().filter(|s|safe_header(s)).ok_or_else(||error("durable recovery requires an original server-assigned session; bootstrap retained unknown"))?);
        state.protocol = connection.protocol.clone();
        state.next_id = connection.next_id.load(Ordering::SeqCst);
        state.catalog = client
            .tools()
            .map(|tool| CatalogTool {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            })
            .collect();
        state.ready = true;
        state.transport_sha256 = Some(fingerprint(&(
            &state.session_header,
            &state.protocol,
            &state.catalog,
        ))?);
        validate(&state, &config, &binding)?;
        lease.save(&state).await.map_err(|_| owner())?;
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                key,
                config,
                http,
                lease: Mutex::new(Some(lease)),
                gate: AsyncMutex::new(()),
                closed: AtomicBool::new(false),
                save_unknown: AtomicBool::new(false),
                instance,
                binding,
            }),
        })
    }
    /// No HTTP initialization/catalog request is made. Host supplies a fresh
    /// bearer for the same explicit principal domain; opaque tokens do not prove it.
    pub async fn open(
        config: McpPersistentConfig,
        store: SqlMcpSessionStore,
        name: &str,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let key = store.key(name)?;
        let binding = config.binding(&store.scope)?;
        let http = config.client()?;
        let lease = store.objects.acquire(&key).await.map_err(|_| owner())?;
        let state = store
            .objects
            .load::<Session>(&key)
            .await
            .map_err(|_| bad())?
            .ok_or_else(bad)?;
        validate(&state, &config, &binding)?;
        if !state.ready {
            return Err(error(
                "bootstrap outcome unknown; takeover cannot initialize a replacement session",
            ));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                key,
                config,
                http,
                lease: Mutex::new(Some(lease)),
                gate: AsyncMutex::new(()),
                closed: AtomicBool::new(false),
                save_unknown: AtomicBool::new(false),
                instance: state.instance,
                binding,
            }),
        })
    }
    /// Releases local ownership only. Never DELETEs/closes the remote session.
    /// Already-dispatched external work cannot be recalled by a SQL lease.
    pub fn release_owner(&self) -> Result<(), RuntimeError> {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.lease.lock().map_err(|_| owner())?.take();
        Ok(())
    }
    fn lease(&self) -> Result<SqlObjectLease, RuntimeError> {
        if self.inner.closed.load(Ordering::SeqCst)
            || self.inner.save_unknown.load(Ordering::SeqCst)
        {
            return Err(error(
                "persistent owner closed or commit confirmation unknown; release and reopen",
            ));
        }
        self.inner
            .lease
            .lock()
            .map_err(|_| owner())?
            .clone()
            .ok_or_else(owner)
    }
    async fn load(&self, lease: &SqlObjectLease) -> Result<Session, RuntimeError> {
        self.lease()?;
        lease.verify_ownership().await.map_err(|_| owner())?;
        let state = self
            .inner
            .store
            .objects
            .load::<Session>(&self.inner.key)
            .await
            .map_err(|_| bad())?
            .ok_or_else(bad)?;
        validate(&state, &self.inner.config, &self.inner.binding)?;
        if state.instance != self.inner.instance || !state.ready {
            return Err(bad());
        }
        Ok(state)
    }
    async fn save(&self, lease: &SqlObjectLease, state: &Session) -> Result<(), RuntimeError> {
        self.lease()?;
        validate(state, &self.inner.config, &self.inner.binding)?;
        // Cancellation/error during a commit poisons this owner. Reacquiring a
        // new epoch after release orders behind any in-flight commit/rollback.
        self.inner.save_unknown.store(true, Ordering::SeqCst);
        lease.save(state).await.map_err(|_| {
            error("SQL commit confirmation unknown; release owner and reopen; no dispatch")
        })?;
        self.inner.save_unknown.store(false, Ordering::SeqCst);
        Ok(())
    }
    pub async fn pending(
        &self,
    ) -> Result<Vec<(PersistentMcpCall, PersistentMcpSummary)>, RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let state = self.load(&lease).await?;
        Ok(state
            .calls
            .iter()
            .map(|(key, call)| (handle(&state, key, call), summary(call)))
            .collect())
    }
    pub async fn tools(&self) -> Result<Vec<McpToolInfo>, RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let state = self.load(&lease).await?;
        Ok(state
            .catalog
            .into_iter()
            .map(|tool| McpToolInfo {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
            })
            .collect())
    }
    /// Public frozen host/catalog identity, excluding credentials/session header.
    pub async fn identity(&self) -> Result<String, RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let state = self.load(&lease).await?;
        Ok(format!(
            "mcp-persistent-v1:{}",
            fingerprint(&(
                &state.instance,
                &state.binding,
                &state.protocol,
                &state.catalog
            ))?
        ))
    }
    pub async fn call(
        &self,
        remote_name: &str,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<PersistentMcpObserved, RuntimeError> {
        tokio::time::timeout(
            self.inner.config.http.timeout,
            self.call_inner(remote_name, arguments, context),
        )
        .await
        .map_err(|_| {
            error(
                "original call deadline; durable outcome retained unknown, no automatic POST retry",
            )
        })?
    }
    async fn call_inner(
        &self,
        remote_name: &str,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<PersistentMcpObserved, RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let mut state = self.load(&lease).await?;
        if !arguments.is_object()
            || context.run_id.is_empty()
            || context.run_id.len() > 1024
            || context.call_id.is_empty()
            || context.call_id.len() > 1024
            || !state.catalog.iter().any(|tool| tool.name == remote_name)
        {
            return Err(bad());
        }
        let key = fingerprint(&("host-call-v1", &context.run_id, &context.call_id))?;
        let args_sha256 = fingerprint(&arguments)?;
        if let Some(call) = state.calls.get(&key) {
            if call.remote_name != remote_name || call.args_sha256 != args_sha256 {
                return Err(error("original call context/input binding changed"));
            }
            return observed(&state, &key, call);
        }
        if state.calls.len() >= self.inner.config.policy.max_calls
            || state.calls.values().any(|call| {
                matches!(
                    call.state,
                    CallState::PostOutcomeUnknown | CallState::BlockedCallbackUnknown
                )
            })
        {
            return Err(error("pending unknown call/capacity blocks a new POST; explicit recovery or abandonment required"));
        }
        if serde_json::to_vec(&state)
            .map_err(|_| bad())?
            .len()
            .saturating_add(self.inner.config.reservation_bytes())
            > self.inner.config.policy.max_journal_bytes
        {
            return Err(error("durable result/event capacity refused before POST"));
        }
        let id = state.next_id;
        state.next_id = id.checked_add(1).ok_or_else(bad)?;
        let wire = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":remote_name,"arguments":arguments}});
        let body = serde_json::to_vec(&wire).map_err(|_| bad())?;
        if body.len() > self.inner.config.policy.max_request_bytes {
            return Err(error("original request exceeds byte limit; no POST"));
        }
        let binding = fingerprint(&(
            "durable-call-v1",
            &state.instance,
            &state.binding,
            id,
            &wire,
        ))?;
        state.calls.insert(
            key.clone(),
            Call {
                request_id: id,
                binding,
                remote_name: remote_name.into(),
                args_sha256,
                cursor: None,
                seen_events: BTreeSet::new(),
                blocked_callback_ids: BTreeSet::new(),
                get_attempts: 0,
                events_seen: 0,
                state: CallState::PostOutcomeUnknown,
                response: None,
                response_sha256: None,
                adoption_sha256: None,
            },
        );
        self.save(&lease, &state).await?;
        self.lease()?;
        lease.verify_ownership().await.map_err(|_| owner())?;
        let response = self
            .headers(
                self.inner.http.post(&self.inner.config.http.endpoint),
                &state,
            )?
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| error("original POST transport unknown; never automatically replayed"))?;
        self.consume(response, &lease, &mut state, &key, false)
            .await
    }
    pub async fn resume(
        &self,
        call: &PersistentMcpCall,
    ) -> Result<PersistentMcpObserved, RuntimeError> {
        tokio::time::timeout(self.inner.config.http.timeout, self.resume_inner(call))
            .await
            .map_err(|_| error("explicit GET deadline; original outcome still unknown"))?
    }
    async fn resume_inner(
        &self,
        requested: &PersistentMcpCall,
    ) -> Result<PersistentMcpObserved, RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let mut state = self.load(&lease).await?;
        check_handle(&state, requested)?;
        let call = state.calls.get_mut(&requested.key).ok_or_else(bad)?;
        if call.state == CallState::ObservedResult {
            return observed(&state, &requested.key, &state.calls[&requested.key]);
        }
        if call.state != CallState::PostOutcomeUnknown
            || call.get_attempts >= self.inner.config.policy.max_get_attempts
        {
            return Err(error(
                "callback/terminal state or GET allowance refuses recovery; no POST",
            ));
        }
        let cursor = call.cursor.clone().ok_or_else(|| {
            error("original call has no durable event ID; no GET or replacement session")
        })?;
        call.get_attempts += 1;
        self.save(&lease, &state).await?;
        self.lease()?;
        lease.verify_ownership().await.map_err(|_| owner())?;
        let response = self
            .headers(
                self.inner.http.get(&self.inner.config.http.endpoint),
                &state,
            )?
            .header("Last-Event-ID", cursor)
            .send()
            .await
            .map_err(|_| error("explicit original-session GET unknown; no POST retry"))?;
        self.consume(response, &lease, &mut state, &requested.key, true)
            .await
    }
    pub async fn acknowledge(
        &self,
        requested: &PersistentMcpCall,
        expected_sha256: &str,
        adoption_reference: &str,
    ) -> Result<(), RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let mut state = self.load(&lease).await?;
        check_handle(&state, requested)?;
        if !label(adoption_reference) {
            return Err(bad());
        }
        let call = state.calls.get_mut(&requested.key).ok_or_else(bad)?;
        if call.state != CallState::ObservedResult
            || call.response_sha256.as_deref() != Some(expected_sha256)
        {
            return Err(error(
                "ACK requires exact observed response hash; host must save RunStore first",
            ));
        }
        call.response = None;
        call.cursor = None;
        call.seen_events.clear();
        call.state = CallState::HostAcknowledged;
        call.adoption_sha256 = Some(fingerprint(&adoption_reference)?);
        self.save(&lease, &state).await
    }
    pub async fn abandon_unknown(
        &self,
        requested: &PersistentMcpCall,
        host_note: &str,
    ) -> Result<(), RuntimeError> {
        let _gate = self.inner.gate.lock().await;
        let lease = self.lease()?;
        let mut state = self.load(&lease).await?;
        check_handle(&state, requested)?;
        if !label(host_note) {
            return Err(bad());
        }
        let call = state.calls.get_mut(&requested.key).ok_or_else(bad)?;
        if !matches!(
            call.state,
            CallState::PostOutcomeUnknown | CallState::BlockedCallbackUnknown
        ) {
            return Err(error("only unknown calls may be explicitly abandoned"));
        }
        call.state = CallState::AbandonedUnknown;
        call.cursor = None;
        call.seen_events.clear();
        call.adoption_sha256 = Some(fingerprint(&host_note)?);
        self.save(&lease, &state).await
    }
    fn headers(
        &self,
        builder: reqwest::RequestBuilder,
        state: &Session,
    ) -> Result<reqwest::RequestBuilder, RuntimeError> {
        let mut session = HeaderValue::from_str(state.session_header.as_deref().ok_or_else(bad)?)
            .map_err(|_| bad())?;
        session.set_sensitive(true);
        Ok(builder.header("Mcp-Session-Id", session).header(
            "MCP-Protocol-Version",
            state.protocol.as_deref().ok_or_else(bad)?,
        ))
    }
    async fn consume(
        &self,
        response: reqwest::Response,
        lease: &SqlObjectLease,
        state: &mut Session,
        key: &str,
        resuming: bool,
    ) -> Result<PersistentMcpObserved, RuntimeError> {
        if response.status() != reqwest::StatusCode::OK
            || response
                .headers()
                .get("Mcp-Session-Id")
                .is_some_and(|header| header.to_str().ok() != state.session_header.as_deref())
        {
            return Err(error(
                "HTTP status/session identity rejected; retained unknown; no replacement POST",
            ));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::trim);
        let sse = match content_type {
            Some("text/event-stream") => true,
            Some("application/json") if !resuming => false,
            _ => return Err(error("unsupported recovery response type; outcome unknown")),
        };
        let max = self.inner.config.http.max_response_bytes;
        if response
            .content_length()
            .is_some_and(|size| size > max as u64)
        {
            return Err(error("response exceeds durable bound"));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        let mut total = 0usize;
        let mut scanned = 0usize;
        while let Some(part) = stream.next().await {
            let part =
                part.map_err(|_| error("response interrupted; original call retained unknown"))?;
            total = total.checked_add(part.len()).ok_or_else(bad)?;
            if total > max {
                return Err(error("response exceeds durable bound"));
            }
            bytes.extend_from_slice(&part);
            if sse {
                let mut start = 0usize;
                loop {
                    let (end, delimiter) = match event_end(&bytes, scanned) {
                        Ok(found) => found,
                        Err(resume) => {
                            scanned = resume;
                            break;
                        }
                    };
                    let event = std::str::from_utf8(&bytes[start..end]).map_err(|_| bad())?;
                    let ids = event
                        .split(['\r', '\n'])
                        .filter_map(|line| {
                            line.strip_prefix("id:")
                                .map(|v| v.strip_prefix(' ').unwrap_or(v))
                        })
                        .collect::<Vec<_>>();
                    if ids.len() > 1 {
                        return Err(bad());
                    }
                    let event_id = ids.first().copied();
                    let data = event
                        .split(['\r', '\n'])
                        .filter_map(|line| {
                            line.strip_prefix("data:")
                                .map(|v| v.strip_prefix(' ').unwrap_or(v))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !data.is_empty() {
                        let message: Value = serde_json::from_str(&data).map_err(|_| bad())?;
                        if let Some(result) =
                            self.event(lease, state, key, event_id, &message).await?
                        {
                            return Ok(result);
                        }
                    }
                    start = end + delimiter;
                    scanned = start;
                }
                bytes.drain(..start);
                scanned -= start;
            }
        }
        if sse {
            return Err(error("SSE ended without original result; retained unknown"));
        }
        let message: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
        self.event(lease, state, key, None, &message)
            .await?
            .ok_or_else(|| error("JSON response did not contain original result"))
    }
    async fn event(
        &self,
        lease: &SqlObjectLease,
        state: &mut Session,
        key: &str,
        event_id: Option<&str>,
        message: &Value,
    ) -> Result<Option<PersistentMcpObserved>, RuntimeError> {
        self.lease()?;
        lease.verify_ownership().await.map_err(|_| owner())?;
        let mut next = state.clone();
        let call = next.calls.get_mut(key).ok_or_else(bad)?;
        if call.events_seen >= self.inner.config.policy.max_events {
            return Err(error("durable event count limit exceeded"));
        }
        call.events_seen += 1;
        if let Some(id) = event_id {
            if !event_id_valid(id, &self.inner.config.policy)
                || call.seen_events.len() >= self.inner.config.policy.max_events
                || call.seen_events.contains(id)
            {
                return Err(error("invalid/replayed/overlimit durable SSE event ID"));
            }
        }
        if message.get("method").is_some() && message.get("id").is_some() {
            let id = fingerprint(&message["id"])?;
            call.blocked_callback_ids.insert(id);
            call.state = CallState::BlockedCallbackUnknown;
            self.save(lease, &next).await?;
            *state = next;
            return Err(error("persistent v1 refuses server sampling/elicitation/callback requests; no host callback or response POST"));
        }
        let final_response = message.get("id").is_some();
        if final_response {
            validate_response(message, call.request_id, true)?;
            if serde_json::to_vec(message).map_err(|_| bad())?.len()
                > self.inner.config.http.max_response_bytes
            {
                return Err(bad());
            }
            call.response = Some(message.clone());
            call.response_sha256 = Some(fingerprint(message)?);
            call.state = CallState::ObservedResult;
        } else if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !message.get("method").is_some_and(Value::is_string)
            || message.get("result").is_some()
            || message.get("error").is_some()
        {
            return Err(bad());
        }
        if let Some(id) = event_id {
            call.cursor = Some(id.into());
            call.seen_events.insert(id.into());
        }
        self.save(lease, &next).await?;
        *state = next;
        if final_response {
            Ok(Some(observed(state, key, &state.calls[key])?))
        } else {
            Ok(None)
        }
    }
}
fn handle(state: &Session, key: &str, call: &Call) -> PersistentMcpCall {
    PersistentMcpCall {
        instance: state.instance.clone(),
        key: key.into(),
        binding: call.binding.clone(),
        request_id: call.request_id,
    }
}
fn summary(call: &Call) -> PersistentMcpSummary {
    PersistentMcpSummary {
        request_id: call.request_id,
        binding: call.binding.clone(),
        state: call.state.name().into(),
        has_cursor: call.cursor.is_some(),
        get_attempts: call.get_attempts,
        response_sha256: call.response_sha256.clone(),
    }
}
fn check_handle(state: &Session, handle: &PersistentMcpCall) -> Result<(), RuntimeError> {
    let call = state.calls.get(&handle.key).ok_or_else(bad)?;
    if handle.instance != state.instance
        || handle.binding != call.binding
        || handle.request_id != call.request_id
    {
        return Err(error("durable recovery handle source mismatch"));
    }
    Ok(())
}
fn observed(
    state: &Session,
    key: &str,
    call: &Call,
) -> Result<PersistentMcpObserved, RuntimeError> {
    if call.state != CallState::ObservedResult {
        return Err(error(
            "original call is unknown/acknowledged/abandoned; no POST replay",
        ));
    }
    Ok(PersistentMcpObserved {
        call: handle(state, key, call),
        response: call.response.clone().ok_or_else(bad)?,
        sha256: call.response_sha256.clone().ok_or_else(bad)?,
    })
}
fn validate(
    state: &Session,
    config: &McpPersistentConfig,
    binding: &str,
) -> Result<(), RuntimeError> {
    if state.version != 1
        || state.binding != binding
        || !hex_string(&state.instance, 32)
        || state.next_id == 0
        || state.calls.len() > config.policy.max_calls
        || state.catalog.len() > config.http.max_tools
        || serde_json::to_vec(state).map_err(|_| bad())?.len() > config.policy.max_journal_bytes
    {
        return Err(error(
            "durable stored policy/principal/version/capacity mismatch",
        ));
    }
    if state.ready
        && (state
            .session_header
            .as_deref()
            .is_none_or(|v| !safe_header(v))
            || state
                .protocol
                .as_deref()
                .is_none_or(|v| !PROTOCOLS.contains(&v))
            || state.catalog.is_empty())
    {
        return Err(bad());
    }
    if state.ready
        && state.transport_sha256.as_deref()
            != Some(
                fingerprint(&(&state.session_header, &state.protocol, &state.catalog))?.as_str(),
            )
    {
        return Err(error(
            "protected session/protocol/catalog checksum mismatch",
        ));
    }
    let mut tool_names = BTreeSet::new();
    if state.catalog.iter().any(|tool| {
        tool.name.trim().is_empty()
            || tool.name.len() > 256
            || tool.description.len() > 16 * 1024
            || !tool.input_schema.is_object()
            || !tool_names.insert(&tool.name)
    }) {
        return Err(bad());
    }
    let mut ids = BTreeSet::new();
    for (key, call) in &state.calls {
        if call.request_id >= state.next_id
            || !hex_string(key, 64)
            || !ids.insert(call.request_id)
            || !hex_string(&call.binding, 64)
            || !hex_string(&call.args_sha256, 64)
            || call.get_attempts > config.policy.max_get_attempts
            || call.events_seen > config.policy.max_events
            || call.seen_events.len() > call.events_seen
            || call.seen_events.len() > config.policy.max_events
            || call
                .seen_events
                .iter()
                .any(|id| !event_id_valid(id, &config.policy))
            || call
                .cursor
                .as_ref()
                .is_some_and(|id| !call.seen_events.contains(id))
            || call.blocked_callback_ids.len() > 1
            || call
                .blocked_callback_ids
                .iter()
                .any(|id| !hex_string(id, 64))
            || call
                .response_sha256
                .as_deref()
                .is_some_and(|id| !hex_string(id, 64))
            || call
                .adoption_sha256
                .as_deref()
                .is_some_and(|id| !hex_string(id, 64))
        {
            return Err(bad());
        }
        if call.state == CallState::ObservedResult {
            let response = call.response.as_ref().ok_or_else(bad)?;
            validate_response(response, call.request_id, true)?;
            if call.response_sha256.as_deref() != Some(fingerprint(response)?.as_str()) {
                return Err(bad());
            }
        } else if call.response.is_some() {
            return Err(bad());
        }
        let valid_state = match call.state {
            CallState::PostOutcomeUnknown => {
                call.response_sha256.is_none()
                    && call.adoption_sha256.is_none()
                    && call.blocked_callback_ids.is_empty()
            }
            CallState::BlockedCallbackUnknown => {
                call.response_sha256.is_none()
                    && call.adoption_sha256.is_none()
                    && call.blocked_callback_ids.len() == 1
            }
            CallState::ObservedResult => {
                call.response_sha256.is_some()
                    && call.adoption_sha256.is_none()
                    && call.blocked_callback_ids.is_empty()
            }
            CallState::HostAcknowledged => {
                call.response_sha256.is_some()
                    && call.adoption_sha256.is_some()
                    && call.blocked_callback_ids.is_empty()
            }
            CallState::AbandonedUnknown => {
                call.response_sha256.is_none() && call.adoption_sha256.is_some()
            }
        };
        if !valid_state {
            return Err(bad());
        }
    }
    Ok(())
}
fn event_id_valid(value: &str, policy: &McpPersistentPolicy) -> bool {
    !value.is_empty()
        && value.len() <= policy.max_event_id_bytes
        && value.bytes().all(|b| b.is_ascii_graphic())
}
fn hex_string(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn safe_header(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && value.bytes().all(|b| b.is_ascii_graphic())
}
fn label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn bad() -> RuntimeError {
    error("durable policy/state/response rejected; no automatic POST retry")
}
fn owner() -> RuntimeError {
    error("durable SQL owner unavailable/stale; no network dispatch")
}
