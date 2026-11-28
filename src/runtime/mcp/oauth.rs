//! Host-pinned public OAuth client. Independent of App/SSO login identities.
use super::{error, RuntimeError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// Public host policy, never populated from a server's WWW-Authenticate URL.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpOAuthConfig {
    pub resource: String,
    pub resource_metadata_url: String,
    pub issuer: String,
    pub authorization_metadata_url: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: BTreeSet<String>,
    pub timeout: Duration,
    pub authorization_ttl: Duration,
    pub max_response_bytes: usize,
    #[serde(default)]
    pub allow_http_loopback_for_tests: bool,
}
impl McpOAuthConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        for url in [
            &self.resource,
            &self.resource_metadata_url,
            &self.issuer,
            &self.authorization_metadata_url,
            &self.authorization_endpoint,
            &self.token_endpoint,
            &self.redirect_uri,
        ] {
            checked_url(url, self.allow_http_loopback_for_tests)?;
        }
        if self.client_id.is_empty()
            || self.client_id.len() > 512
            || self.client_id.chars().any(char::is_control)
            || self.scopes.is_empty()
            || self.scopes.len() > 16
            || self.scopes.iter().any(|scope| {
                scope.is_empty()
                    || scope.len() > 128
                    || !scope
                        .bytes()
                        .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
            })
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(60)
            || self.authorization_ttl.is_zero()
            || self.authorization_ttl > Duration::from_secs(600)
            || !(512..=1024 * 1024).contains(&self.max_response_bytes)
        {
            return Err(denied());
        }
        Ok(())
    }
}
/// Verified discovery documents plus fixed host policy. No dynamic registration.
pub struct McpOAuthClient {
    config: McpOAuthConfig,
    client: reqwest::Client,
    identity: String,
}
impl McpOAuthClient {
    pub async fn discover(config: McpOAuthConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(config.timeout)
            .build()
            .map_err(|_| denied())?;
        let resource = bounded_json(
            client.get(&config.resource_metadata_url),
            config.max_response_bytes,
        )
        .await?;
        if resource.get("resource").and_then(Value::as_str) != Some(&config.resource)
            || resource.get("authorization_servers") != Some(&serde_json::json!([config.issuer]))
        {
            return Err(denied());
        }
        let metadata = bounded_json(
            client.get(&config.authorization_metadata_url),
            config.max_response_bytes,
        )
        .await?;
        if metadata.get("issuer").and_then(Value::as_str) != Some(&config.issuer)
            || metadata
                .get("authorization_endpoint")
                .and_then(Value::as_str)
                != Some(&config.authorization_endpoint)
            || metadata.get("token_endpoint").and_then(Value::as_str)
                != Some(&config.token_endpoint)
            || !has(&metadata, "response_types_supported", "code")
            || !has(&metadata, "grant_types_supported", "authorization_code")
            || !has(&metadata, "code_challenge_methods_supported", "S256")
            || !has(&metadata, "token_endpoint_auth_methods_supported", "none")
            || metadata
                .get("authorization_response_iss_parameter_supported")
                .and_then(Value::as_bool)
                != Some(true)
            || config.scopes.iter().any(|scope| {
                !has(&metadata, "scopes_supported", scope)
                    || resource
                        .get("scopes_supported")
                        .is_some_and(|_| !has(&resource, "scopes_supported", scope))
            })
        {
            return Err(denied());
        }
        let identity = crate::runtime::state::fingerprint(&("mcp-oauth-public-v1", &config))?;
        Ok(Self {
            config,
            client,
            identity,
        })
    }
    /// Generates a fresh state and PKCE verifier. The host opens this URL and
    /// routes only this flow's callback to exchange. It is not model tool data.
    pub fn begin(&self) -> Result<McpOAuthAuthorization, RuntimeError> {
        let verifier = random();
        let state = random();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = checked_url(
            &self.config.authorization_endpoint,
            self.config.allow_http_loopback_for_tests,
        )?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.config.redirect_uri)
            .append_pair("scope", &scope_text(&self.config.scopes))
            .append_pair("resource", &self.config.resource)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(McpOAuthAuthorization {
            config: self.config.clone(),
            client: self.client.clone(),
            identity: self.identity.clone(),
            url: url.into(),
            state,
            verifier,
            expires: Instant::now() + self.config.authorization_ttl,
        })
    }
}
/// Owned, non-cloneable and non-serializable pending flow. Consumed on every
/// exchange attempt, including invalid callbacks/timeouts/cancellation.
pub struct McpOAuthAuthorization {
    config: McpOAuthConfig,
    client: reqwest::Client,
    identity: String,
    url: String,
    state: String,
    verifier: String,
    expires: Instant,
}
impl McpOAuthAuthorization {
    pub fn authorization_url(&self) -> &str {
        &self.url
    }
    pub async fn exchange(self, callback_url: &str) -> Result<McpOAuthCredential, RuntimeError> {
        if Instant::now() >= self.expires {
            return Err(denied());
        }
        let callback = reqwest::Url::parse(callback_url).map_err(|_| denied())?;
        let mut without_query = callback.clone();
        without_query.set_query(None);
        if without_query.as_str() != self.config.redirect_uri || callback.fragment().is_some() {
            return Err(denied());
        }
        let mut query = BTreeMap::new();
        for (name, value) in callback.query_pairs() {
            if !matches!(name.as_ref(), "code" | "state" | "iss")
                || query
                    .insert(name.into_owned(), value.into_owned())
                    .is_some()
            {
                return Err(denied());
            }
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
        if query.get("state") != Some(&self.state) || query.get("iss") != Some(&self.config.issuer)
        {
            return Err(denied());
        }
        // No client secret, ambient bearer or refresh token is sent. This one
        // exchange consumes the owned flow before network dispatch; no retry.
        let exchange_started = Instant::now();
        let token = bounded_json(
            self.client.post(&self.config.token_endpoint).form(&[
                ("grant_type", "authorization_code"),
                ("client_id", self.config.client_id.as_str()),
                ("redirect_uri", self.config.redirect_uri.as_str()),
                ("resource", self.config.resource.as_str()),
                ("code", code.as_str()),
                ("code_verifier", self.verifier.as_str()),
            ]),
            self.config.max_response_bytes,
        )
        .await?;
        let access_token = token
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| {
                !token.is_empty()
                    && token.len() <= 16384
                    && token.bytes().all(|b| b.is_ascii_graphic())
            })
            .ok_or_else(denied)?
            .to_owned();
        if token
            .get("token_type")
            .and_then(Value::as_str)
            .is_none_or(|value| !value.eq_ignore_ascii_case("bearer"))
            || token.get("scope").and_then(Value::as_str).map(|value| {
                value
                    .split_ascii_whitespace()
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            }) != Some(self.config.scopes.clone())
        {
            return Err(denied());
        }
        let seconds = token
            .get("expires_in")
            .and_then(Value::as_u64)
            .filter(|n| (1..=86400).contains(n))
            .ok_or_else(denied)?;
        let expires = exchange_started + Duration::from_secs(seconds);
        if Instant::now() >= expires {
            return Err(denied());
        }
        Ok(McpOAuthCredential {
            resource: self.config.resource,
            identity: self.identity,
            access_token,
            expires,
        })
    }
}
/// Opaque access token is never Debug/Serialize or exposed by a getter. RFC8707
/// resource/issuer pinning does not cryptographically verify an opaque aud.
pub struct McpOAuthCredential {
    resource: String,
    identity: String,
    access_token: String,
    expires: Instant,
}
impl McpOAuthCredential {
    pub(super) fn apply(
        self,
        config: &mut super::McpHttpConfig,
    ) -> Result<(String, Instant), RuntimeError> {
        if config.bearer_token.is_some()
            || config.endpoint != self.resource
            || Instant::now() >= self.expires
        {
            return Err(denied());
        }
        config.bearer_token = Some(self.access_token);
        Ok((self.identity, self.expires))
    }
}
fn checked_url(value: &str, local: bool) -> Result<reqwest::Url, RuntimeError> {
    let url = reqwest::Url::parse(value).map_err(|_| denied())?;
    if url.as_str() != value
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || local
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")))
    {
        return Err(denied());
    }
    Ok(url)
}
fn random() -> String {
    // Two independent UUIDv4 values yield 244 random bits. Hex is a valid
    // 64-character PKCE verifier, and no system pseudo-random fallback is used.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn scope_text(scopes: &BTreeSet<String>) -> String {
    scopes.iter().cloned().collect::<Vec<_>>().join(" ")
}
fn has(value: &Value, field: &str, needle: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(needle)))
}
async fn bounded_json(
    request: reqwest::RequestBuilder,
    maximum: usize,
) -> Result<Value, RuntimeError> {
    let mut response = request.send().await.map_err(|_| denied())?;
    if !response.status().is_success()
        || response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            != Some("application/json")
        || response
            .content_length()
            .is_some_and(|length| length > maximum as u64)
    {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| denied())? {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(denied());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| denied())
}
fn denied() -> RuntimeError {
    error("OAuth pinned policy/discovery/callback/exchange rejected; no automatic retry")
}
