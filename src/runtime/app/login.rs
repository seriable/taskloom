//! Explicit browser authorization-code / PKCE login. Endpoints and issuer keys
//! are trusted host policy; cookies contain only an opaque in-memory session.
use super::oidc::OidcVerifier;
use crate::runtime::{
    database::{ApiToken, SqlIdentityStore},
    RuntimeError,
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserLoginConfig {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub client_secret_env: Option<String>,
    pub redirect_uri: String,
    pub success_path: String,
    pub pending_ttl_seconds: u64,
    pub session_ttl_seconds: u64,
    pub max_pending: usize,
    pub max_sessions: usize,
    /// Explicit development fixtures only. Every HTTP URL must be loopback.
    #[serde(default)]
    pub allow_http_loopback_for_tests: bool,
}
impl std::fmt::Debug for BrowserLoginConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserLoginConfig").finish_non_exhaustive()
    }
}
impl BrowserLoginConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        for endpoint in [
            &self.authorization_endpoint,
            &self.token_endpoint,
            &self.redirect_uri,
        ] {
            checked_url(endpoint, self.allow_http_loopback_for_tests)?;
        }
        let redirect = checked_url(&self.redirect_uri, self.allow_http_loopback_for_tests)?;
        if !redirect.path().ends_with("/auth/callback")
            || self.client_id.trim().is_empty()
            || self.client_id.len() > 512
            || !(1..=600).contains(&self.pending_ttl_seconds)
            || !(1..=86400).contains(&self.session_ttl_seconds)
            || !(1..=1024).contains(&self.max_pending)
            || !(1..=4096).contains(&self.max_sessions)
            || !self.success_path.starts_with('/')
            || self.success_path.starts_with("//")
            || self.success_path.contains(['\\', '?', '#'])
            || self.success_path.chars().any(char::is_control)
            || self.client_secret_env.as_ref().is_some_and(|name| {
                name.is_empty()
                    || name.len() > 128
                    || !name.bytes().enumerate().all(|(i, b)| {
                        b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit()
                    })
            })
        {
            return Err(invalid_config());
        }
        Ok(())
    }
}
pub struct BrowserLogin {
    config: BrowserLoginConfig,
    verifier: Arc<OidcVerifier>,
    identities: SqlIdentityStore,
    secret: Option<String>,
    client: reqwest::Client,
    origin: String,
    secure: bool,
    state: Mutex<LoginState>,
    closed: AtomicBool,
}
impl std::fmt::Debug for BrowserLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserLogin").finish_non_exhaustive()
    }
}
#[derive(Default)]
struct LoginState {
    pending: BTreeMap<String, Pending>,
    consumed: BTreeMap<String, Instant>,
    sessions: BTreeMap<String, Arc<Session>>,
    inflight: usize,
}
struct Pending {
    verifier: String,
    nonce: String,
    binding: String,
    expires: Instant,
}
struct Session {
    bearer: ApiToken,
    token_id: String,
    user: String,
    csrf: String,
    expires: Instant,
}
struct CallbackLease(Arc<BrowserLogin>);
impl Drop for CallbackLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.inflight = state.inflight.saturating_sub(1);
        }
    }
}
impl BrowserLogin {
    pub fn new(
        config: BrowserLoginConfig,
        verifier: Arc<OidcVerifier>,
        identities: SqlIdentityStore,
    ) -> Result<Arc<Self>, RuntimeError> {
        config.validate()?;
        if verifier.audience() != config.client_id {
            return Err(RuntimeError::Configuration(
                "browser ID-token verifier audience must equal the registered client ID".into(),
            ));
        }
        let redirect = checked_url(&config.redirect_uri, config.allow_http_loopback_for_tests)?;
        let secure = redirect.scheme() == "https";
        let secret = config
            .client_secret_env
            .as_ref()
            .map(|name| {
                std::env::var(name).map_err(|_| {
                    config_error("configured browser client secret environment is unavailable")
                })
            })
            .transpose()?;
        if secret.as_ref().is_some_and(|secret| {
            secret.trim().is_empty() || secret.len() > 8192 || secret.contains(['\r', '\n'])
        }) {
            return Err(invalid_config());
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| invalid_config())?;
        Ok(Arc::new(Self {
            origin: redirect.origin().ascii_serialization(),
            config,
            verifier,
            identities,
            secret,
            client,
            secure,
            state: Mutex::new(LoginState::default()),
            closed: AtomicBool::new(false),
        }))
    }
    fn session_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-taskloom-session"
        } else {
            "taskloom-dev-session"
        }
    }
    fn binding_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-taskloom-login"
        } else {
            "taskloom-dev-login"
        }
    }
    fn cookie(&self, name: &str, value: &str, age: u64) -> String {
        format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={age}{}",
            if self.secure { "; Secure" } else { "" }
        )
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, LoginState>, RuntimeError> {
        self.state
            .lock()
            .map_err(|_| RuntimeError::Storage("browser login state unavailable".into()))
    }
    fn reap(&self, state: &mut LoginState) {
        let now = Instant::now();
        state.pending.retain(|_, entry| entry.expires > now);
        state.consumed.retain(|_, expiry| *expiry > now);
        state.sessions.retain(|_, entry| entry.expires > now);
    }
    async fn session(&self, headers: &HeaderMap) -> Result<Option<Arc<Session>>, RuntimeError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(denied());
        }
        let Some(cookie) = cookie_value(headers, self.session_cookie())? else {
            return Ok(None);
        };
        if !opaque(&cookie) {
            return Err(denied());
        }
        let key = hash(&cookie);
        let session = {
            let mut state = self.lock()?;
            self.reap(&mut state);
            state.sessions.get(&key).cloned()
        };
        if let Some(session) = &session {
            if self
                .identities
                .authenticate(session.bearer.as_str())
                .await
                .is_err()
            {
                self.lock()?.sessions.remove(&key);
                return Err(denied());
            }
        }
        Ok(session)
    }
    fn csrf(&self, headers: &HeaderMap, session: &Session) -> Result<(), RuntimeError> {
        let mut origins = headers.get_all(header::ORIGIN).iter();
        if origins.next().and_then(|value| value.to_str().ok()) != Some(self.origin.as_str())
            || origins.next().is_some()
        {
            return Err(denied());
        }
        let mut tokens = headers.get_all("x-taskloom-csrf").iter();
        let token = tokens
            .next()
            .and_then(|value| value.to_str().ok())
            .ok_or_else(denied)?;
        if tokens.next().is_some() || token.len() > 128 || !same(token, &session.csrf) {
            return Err(denied());
        }
        Ok(())
    }
    /// Explicit shutdown revokes all remaining backend-issued local sessions.
    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        self.closed.store(true, Ordering::Release);
        let ids = {
            let mut state = self.lock()?;
            state.pending.clear();
            state.consumed.clear();
            std::mem::take(&mut state.sessions)
                .values()
                .map(|entry| entry.token_id.clone())
                .collect::<Vec<_>>()
        };
        for id in ids {
            self.identities
                .revoke_token(&id, "browser-login-shutdown")
                .await?;
        }
        Ok(())
    }
}
/// Mount at the registered callback path's host prefix. API routes separately
/// use `cookie_bridge`; the login endpoints never expose OAuth/local tokens.
pub fn router(login: Arc<BrowserLogin>) -> Router {
    Router::new()
        .route("/auth/login", get(begin))
        .route("/auth/callback", get(callback))
        .route("/auth/session", get(inspect))
        .route("/auth/logout", post(logout))
        .with_state(login)
}
async fn begin(State(login): State<Arc<BrowserLogin>>, request: Request) -> Response {
    match begin_inner(&login, request).await {
        Ok(response) => response,
        Err(error) => failure(error),
    }
}
async fn begin_inner(
    login: &Arc<BrowserLogin>,
    request: Request,
) -> Result<Response, RuntimeError> {
    if request.uri().query().is_some() {
        return Err(config_error(
            "browser login accepts no redirect or endpoint query input",
        ));
    }
    if login.session(request.headers()).await?.is_some() {
        return Err(RuntimeError::Conflict(
            "log out the existing browser session before signing in again".into(),
        ));
    }
    let binding = cookie_value(request.headers(), login.binding_cookie())?
        .filter(|value| opaque(value))
        .unwrap_or_else(random);
    let state_token = random();
    let nonce = random();
    let verifier = random_verifier();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut endpoint = checked_url(
        &login.config.authorization_endpoint,
        login.config.allow_http_loopback_for_tests,
    )?;
    endpoint.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", login.config.client_id.as_str()),
        ("redirect_uri", login.config.redirect_uri.as_str()),
        ("scope", "openid"),
        ("state", state_token.as_str()),
        ("nonce", nonce.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ]);
    {
        let mut state = login.lock()?;
        login.reap(&mut state);
        if state.pending.len() + state.inflight >= login.config.max_pending
            || state.consumed.len() >= 8192
        {
            return Err(RuntimeError::Conflict(
                "browser login capacity reached".into(),
            ));
        }
        state.pending.insert(
            hash(&state_token),
            Pending {
                verifier,
                nonce,
                binding: hash(&binding),
                expires: Instant::now() + Duration::from_secs(login.config.pending_ttl_seconds),
            },
        );
    }
    let mut response = redirect(endpoint.as_str())?;
    set_cookie(
        &mut response,
        login.cookie(
            login.binding_cookie(),
            &binding,
            login.config.pending_ttl_seconds,
        ),
    )?;
    Ok(response)
}
async fn callback(State(login): State<Arc<BrowserLogin>>, request: Request) -> Response {
    match callback_inner(login.clone(), request).await {
        Ok(response) => response,
        Err(error) => failure(error),
    }
}
async fn callback_inner(
    login: Arc<BrowserLogin>,
    request: Request,
) -> Result<Response, RuntimeError> {
    let query = callback_query(request.uri().query().ok_or_else(denied)?)?;
    let state_token = query
        .get("state")
        .filter(|value| opaque(value))
        .ok_or_else(denied)?;
    let binding = cookie_value(request.headers(), login.binding_cookie())?
        .filter(|value| opaque(value))
        .ok_or_else(denied)?;
    // Never overwrite or clear another existing authenticated browser session.
    if login.session(request.headers()).await?.is_some() {
        return Err(RuntimeError::Conflict(
            "an authenticated browser session already exists".into(),
        ));
    }
    let key = hash(state_token);
    let pending = {
        let mut state = login.lock()?;
        login.reap(&mut state);
        if state.consumed.contains_key(&key)
            || state.sessions.len() + state.inflight >= login.config.max_sessions
        {
            return Err(denied());
        }
        let pending = state.pending.get(&key).ok_or_else(denied)?;
        if !same(&pending.binding, &hash(&binding)) {
            return Err(denied());
        }
        let pending = state.pending.remove(&key).unwrap();
        state.consumed.insert(key, pending.expires);
        state.inflight += 1;
        pending
    };
    let _lease = CallbackLease(login.clone());
    if query.contains_key("error")
        || query
            .get("iss")
            .is_some_and(|issuer| issuer != login.verifier.issuer())
    {
        return Err(denied());
    }
    let code = query
        .get("code")
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 4096
                && code.is_ascii()
                && !code.chars().any(char::is_control)
        })
        .ok_or_else(denied)?;
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("client_id", login.config.client_id.clone()),
        ("redirect_uri", login.config.redirect_uri.clone()),
        ("code", code.clone()),
        ("code_verifier", pending.verifier),
    ];
    if let Some(secret) = &login.secret {
        form.push(("client_secret", secret.clone()));
    }
    // The state is consumed before this one external attempt. No retry or
    // redirect can resend an uncertain authorization-code exchange.
    let mut response = login
        .client
        .post(&login.config.token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|_| denied())?;
    if !response.status().is_success() {
        return Err(denied());
    }
    let mut raw = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| denied())? {
        if raw.len() + chunk.len() > 1024 * 1024 {
            return Err(denied());
        }
        raw.extend_from_slice(&chunk);
    }
    let token: Value = serde_json::from_slice(&raw).map_err(|_| denied())?;
    let id_token = token
        .get("id_token")
        .and_then(Value::as_str)
        .ok_or_else(denied)?;
    let (actor, id_expiry) = login
        .verifier
        .authenticate_nonce(&login.identities, id_token, &pending.nonce)
        .await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| denied())?
        .as_secs();
    let remaining = u64::try_from(id_expiry / 1000)
        .map_err(|_| denied())?
        .checked_sub(now)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(denied)?;
    let lifetime = remaining.min(login.config.session_ttl_seconds);
    let issued = login
        .identities
        .issue_token(
            actor.user_id(),
            Duration::from_secs(lifetime),
            "oidc-browser-login",
        )
        .await?;
    let session_token = random();
    let csrf = random();
    let issued_id = issued.id.clone();
    let published = {
        let mut state = login.lock()?;
        if login.closed.load(Ordering::Acquire) {
            false
        } else {
            state.sessions.insert(
                hash(&session_token),
                Arc::new(Session {
                    bearer: issued.bearer,
                    token_id: issued.id,
                    user: actor.user_id().to_owned(),
                    csrf,
                    expires: Instant::now() + Duration::from_secs(lifetime),
                }),
            );
            true
        }
    };
    if !published {
        login
            .identities
            .revoke_token(&issued_id, "browser-login-shutdown")
            .await?;
        return Err(denied());
    }
    let mut response = redirect(&login.config.success_path)?;
    set_cookie(
        &mut response,
        login.cookie(login.session_cookie(), &session_token, lifetime),
    )?;
    // The binding remains bounded and contains no authentication credential;
    // clearing it here could invalidate another tab's pending authorization.
    Ok(response)
}
async fn inspect(State(login): State<Arc<BrowserLogin>>, request: Request) -> Response {
    let response = async move {
        let session=login.session(request.headers()).await?.ok_or_else(denied)?;
        let mut response=Json(json!({"authenticated":true,"user":session.user,"csrf":session.csrf,"expires_in_seconds":session.expires.saturating_duration_since(Instant::now()).as_secs()})).into_response();
        secure_headers(&mut response);Ok::<_,RuntimeError>(response)
    }.await;
    response.unwrap_or_else(failure)
}
async fn logout(State(login): State<Arc<BrowserLogin>>, request: Request) -> Response {
    let result = async move {
        let session = login.session(request.headers()).await?.ok_or_else(denied)?;
        login.csrf(request.headers(), &session)?;
        let cookie = cookie_value(request.headers(), login.session_cookie())?.ok_or_else(denied)?;
        login.lock()?.sessions.remove(&hash(&cookie));
        login
            .identities
            .revoke_token(&session.token_id, "oidc-browser-logout")
            .await?;
        let mut response = StatusCode::NO_CONTENT.into_response();
        secure_headers(&mut response);
        set_cookie(&mut response, login.cookie(login.session_cookie(), "", 0))?;
        Ok::<_, RuntimeError>(response)
    }
    .await;
    result.unwrap_or_else(failure)
}
/// Layer only on API routes. Header-only authentication stays unchanged. A
/// request combining a session cookie and Authorization header is refused;
/// cookie mutation additionally requires exact Origin plus an anti-CSRF header.
pub async fn cookie_bridge(
    State(login): State<Arc<BrowserLogin>>,
    mut request: Request,
    next: Next,
) -> Response {
    let headers = request.headers().clone();
    let method = request.method().clone();
    let result = async {
        let cookie = cookie_value(&headers, login.session_cookie())?;
        if cookie.is_none() {
            return Ok(None);
        }
        if headers.contains_key(header::AUTHORIZATION) {
            return Err(config_error(
                "do not combine Authorization and browser-session cookies",
            ));
        }
        let session = login.session(&headers).await?.ok_or_else(denied)?;
        if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
            login.csrf(&headers, &session)?;
        } else if headers
            .get(header::ORIGIN)
            .is_some_and(|origin| origin.to_str().ok() != Some(login.origin.as_str()))
        {
            return Err(denied());
        }
        Ok(Some(session))
    }
    .await;
    match result {
        Ok(Some(session)) => {
            let auth = match HeaderValue::from_str(&format!("Bearer {}", session.bearer.as_str())) {
                Ok(value) => value,
                Err(_) => return failure(denied()),
            };
            request.headers_mut().insert(header::AUTHORIZATION, auth);
            next.run(request).await
        }
        Ok(None) => next.run(request).await,
        Err(error) => failure(error),
    }
}
fn checked_url(value: &str, allow_local: bool) -> Result<reqwest::Url, RuntimeError> {
    let url = reqwest::Url::parse(value).map_err(|_| invalid_config())?;
    let local = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || allow_local && local && url.scheme() == "http")
    {
        return Err(invalid_config());
    }
    Ok(url)
}
fn callback_query(raw: &str) -> Result<BTreeMap<String, String>, RuntimeError> {
    if raw.len() > 8192 {
        return Err(denied());
    }
    let url =
        reqwest::Url::parse(&format!("https://callback.invalid/?{raw}")).map_err(|_| denied())?;
    let mut output = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if !matches!(
            key.as_ref(),
            "state" | "code" | "error" | "error_description" | "iss" | "session_state"
        ) || output
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(denied());
        }
    }
    if output.contains_key("error") && output.contains_key("code") {
        return Err(denied());
    }
    Ok(output)
}
fn cookie_value(headers: &HeaderMap, name: &str) -> Result<Option<String>, RuntimeError> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        let header = header.to_str().map_err(|_| denied())?;
        if header.len() > 16384 {
            return Err(denied());
        }
        for pair in header.split(';') {
            if let Some((key, value)) = pair.trim().split_once('=') {
                if key == name {
                    if found.is_some() {
                        return Err(denied());
                    }
                    found = Some(value.to_owned());
                }
            }
        }
    }
    Ok(found)
}
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn random_verifier() -> String {
    let mut bytes = Vec::new();
    for _ in 0..3 {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
fn opaque(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn same(left: &str, right: &str) -> bool {
    let a = Sha256::digest(left.as_bytes());
    let b = Sha256::digest(right.as_bytes());
    a.iter().zip(b).fold(0u8, |n, (x, y)| n | (*x ^ y)) == 0
}
fn set_cookie(response: &mut Response, value: String) -> Result<(), RuntimeError> {
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).map_err(|_| invalid_config())?,
    );
    Ok(())
}
fn redirect(location: &str) -> Result<Response, RuntimeError> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SEE_OTHER;
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(location).map_err(|_| invalid_config())?,
    );
    secure_headers(&mut response);
    Ok(response)
}
fn secure_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; form-action 'none'"),
    );
}
fn failure(error: RuntimeError) -> Response {
    let status = match error {
        RuntimeError::Permission(_) => StatusCode::UNAUTHORIZED,
        RuntimeError::Configuration(_) => StatusCode::BAD_REQUEST,
        RuntimeError::Conflict(_) => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    let mut response = (
        status,
        Json(json!({"error":"browser authentication request refused"})),
    )
        .into_response();
    secure_headers(&mut response);
    response
}
fn denied() -> RuntimeError {
    RuntimeError::Permission("browser login refused".into())
}
fn invalid_config() -> RuntimeError {
    config_error("invalid explicit browser login endpoints, callback, client or bounds")
}
fn config_error(message: &str) -> RuntimeError {
    RuntimeError::Configuration(message.into())
}
