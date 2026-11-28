//! Explicit pinned-key JWT bearer verification. This is not browser login,
//! discovery, user provisioning, token refresh or authorization from claims.
use crate::runtime::{
    database::{AuthenticatedActor, SqlIdentityStore},
    RuntimeError,
};
use base64::Engine;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OidcAlgorithm {
    RS256,
    EdDSA,
}
impl OidcAlgorithm {
    fn algorithm(self) -> Algorithm {
        match self {
            Self::RS256 => Algorithm::RS256,
            Self::EdDSA => Algorithm::EdDSA,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::RS256 => "RS256",
            Self::EdDSA => "EdDSA",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcSubjectBinding {
    pub issuer: String,
    pub subject: String,
    pub user: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcPolicy {
    pub issuer: String,
    pub audience: String,
    pub algorithm: OidcAlgorithm,
    /// Host-supplied public JWKS snapshot. Never resolved from a token URL.
    pub jwks: Value,
    pub bindings: Vec<OidcSubjectBinding>,
    #[serde(default)]
    pub revoked_jti: BTreeSet<String>,
    pub max_token_lifetime_seconds: u64,
}
impl std::fmt::Debug for OidcPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcPolicy")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}
#[derive(Clone)]
pub struct OidcVerifier {
    policy: OidcPolicy,
    keys: BTreeMap<String, DecodingKey>,
}
impl std::fmt::Debug for OidcVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcVerifier")
            .field("algorithm", &self.policy.algorithm)
            .finish_non_exhaustive()
    }
}
/// No Deserialize or public constructor: only a verified JWT creates this.
#[derive(Clone)]
pub struct VerifiedOidcSubject {
    issuer: String,
    subject: String,
    expires_ms: i64,
    nonce: Option<String>,
    multiple_audiences: bool,
    authorized_party: Option<String>,
}
impl VerifiedOidcSubject {
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub(crate) fn expires_ms(&self) -> i64 {
        self.expires_ms
    }
}
#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Value,
    exp: u64,
    iat: u64,
    #[serde(default)]
    nbf: Option<u64>,
    jti: String,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    azp: Option<String>,
    // `aud` is checked by jsonwebtoken validation. Other claims, including
    // roles, permissions and user/tenant IDs, never create local authority.
}
impl OidcVerifier {
    pub fn audience(&self) -> &str {
        &self.policy.audience
    }
    pub fn issuer(&self) -> &str {
        &self.policy.issuer
    }
    pub fn new(policy: OidcPolicy) -> Result<Self, RuntimeError> {
        let issuer = reqwest::Url::parse(&policy.issuer).map_err(|_| invalid())?;
        if issuer.scheme() != "https"
            || issuer.host_str().is_none()
            || !issuer.username().is_empty()
            || issuer.password().is_some()
            || issuer.query().is_some()
            || issuer.fragment().is_some()
            || policy.audience.trim().is_empty()
            || policy.audience.len() > 512
            || !(1..=86400).contains(&policy.max_token_lifetime_seconds)
            || policy.bindings.is_empty()
            || policy.bindings.len() > 1024
            || policy.revoked_jti.len() > 4096
            || serde_json::to_vec(&policy.jwks)
                .map_err(|_| invalid())?
                .len()
                > 256 * 1024
        {
            return Err(invalid());
        }
        let mut subjects = BTreeSet::new();
        for binding in &policy.bindings {
            if binding.issuer != policy.issuer
                || !bounded(&binding.subject, 512)
                || !identity(&binding.user)
                || !subjects.insert(binding.subject.clone())
            {
                return Err(invalid());
            }
        }
        if policy.revoked_jti.iter().any(|jti| !bounded(jti, 256)) {
            return Err(invalid());
        }
        let object = policy.jwks.as_object().ok_or_else(invalid)?;
        if object.len() != 1 {
            return Err(invalid());
        }
        let source = object
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?;
        if source.is_empty() || source.len() > 32 {
            return Err(invalid());
        }
        let mut keys = BTreeMap::new();
        for value in source {
            let key = value.as_object().ok_or_else(invalid)?;
            let kid = key
                .get("kid")
                .and_then(Value::as_str)
                .filter(|kid| bounded(kid, 128))
                .ok_or_else(invalid)?;
            if key.get("alg").and_then(Value::as_str) != Some(policy.algorithm.name())
                || key.get("use").and_then(Value::as_str) != Some("sig")
                || key.get("key_ops").is_some_and(|ops| {
                    ops.as_array()
                        .is_none_or(|ops| ops.len() != 1 || ops[0].as_str() != Some("verify"))
                })
            {
                return Err(invalid());
            }
            let permitted = match policy.algorithm {
                OidcAlgorithm::RS256 => &["kid", "alg", "use", "key_ops", "kty", "n", "e"][..],
                OidcAlgorithm::EdDSA => &["kid", "alg", "use", "key_ops", "kty", "crv", "x"][..],
            };
            if key.keys().any(|field| !permitted.contains(&field.as_str())) {
                return Err(invalid());
            }
            if match policy.algorithm {
                OidcAlgorithm::RS256 => key.get("kty").and_then(Value::as_str) != Some("RSA"),
                OidcAlgorithm::EdDSA => {
                    key.get("kty").and_then(Value::as_str) != Some("OKP")
                        || key.get("crv").and_then(Value::as_str) != Some("Ed25519")
                }
            } {
                return Err(invalid());
            }
            let jwk: jsonwebtoken::jwk::Jwk =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            let decoding = DecodingKey::from_jwk(&jwk).map_err(|_| invalid())?;
            if keys.insert(kid.to_owned(), decoding).is_some() {
                return Err(invalid());
            }
        }
        Ok(Self { policy, keys })
    }
    /// Signature, issuer, audience, timing and key selection are checked on
    /// every call. The signed token can never choose a URL or a local role.
    pub fn verify(&self, bearer: &str) -> Result<VerifiedOidcSubject, RuntimeError> {
        if bearer.len() > 16384 || bearer.len() < 16 || !bearer.is_ascii() {
            return Err(denied());
        }
        let encoded_header = bearer.split('.').next().ok_or_else(denied)?;
        let raw_header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded_header)
            .map_err(|_| denied())?;
        let raw_header: Value = serde_json::from_slice(&raw_header).map_err(|_| denied())?;
        if raw_header.as_object().is_none_or(|header| {
            header
                .keys()
                .any(|field| !matches!(field.as_str(), "alg" | "kid" | "typ"))
        }) {
            return Err(denied());
        }
        let header = decode_header(bearer).map_err(|_| denied())?;
        if header.alg != self.policy.algorithm.algorithm()
            || header.jku.is_some()
            || header.jwk.is_some()
            || header.x5u.is_some()
            || header.x5c.is_some()
            || header
                .typ
                .as_deref()
                .is_some_and(|typ| typ != "JWT" && typ != "at+jwt")
        {
            return Err(denied());
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| bounded(kid, 128))
            .ok_or_else(denied)?;
        let key = self.keys.get(kid).ok_or_else(denied)?;
        let mut validation = Validation::new(self.policy.algorithm.algorithm());
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.set_issuer(&[&self.policy.issuer]);
        validation.set_audience(&[&self.policy.audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
        let claims = decode::<Claims>(bearer, key, &validation)
            .map_err(|_| denied())?
            .claims;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| denied())?
            .as_secs();
        if claims.iss != self.policy.issuer
            || !bounded(&claims.sub, 512)
            || !bounded(&claims.jti, 256)
            || self.policy.revoked_jti.contains(&claims.jti)
            || claims.iat > now
            || claims.exp <= now
            || claims.exp <= claims.iat
            || claims.exp - claims.iat > self.policy.max_token_lifetime_seconds
            || claims.nbf.is_some_and(|nbf| nbf > now || nbf >= claims.exp)
            || !self
                .policy
                .bindings
                .iter()
                .any(|binding| binding.subject == claims.sub)
        {
            return Err(denied());
        }
        let expires_ms = claims
            .exp
            .checked_mul(1000)
            .and_then(|ms| i64::try_from(ms).ok())
            .ok_or_else(denied)?;
        Ok(VerifiedOidcSubject {
            issuer: claims.iss,
            subject: claims.sub,
            expires_ms,
            nonce: claims.nonce,
            multiple_audiences: claims
                .aud
                .as_array()
                .is_some_and(|audiences| audiences.len() > 1),
            authorized_party: claims.azp,
        })
    }
    /// Authenticate a preprovisioned local SQL user. No remote claim role,
    /// email or group creates a user or membership. Call authorize afterwards.
    pub async fn authenticate(
        &self,
        store: &SqlIdentityStore,
        bearer: &str,
    ) -> Result<AuthenticatedActor, RuntimeError> {
        let subject = self.verify(bearer)?;
        let user = &self
            .policy
            .bindings
            .iter()
            .find(|binding| binding.subject == subject.subject && binding.issuer == subject.issuer)
            .ok_or_else(denied)?
            .user;
        store.authenticate_oidc_subject(&subject, user).await
    }
    /// Browser authorization-code login requires a signed nonce in addition to
    /// all ordinary issuer/audience/signature/timing checks. No JWT is exposed.
    pub async fn authenticate_nonce(
        &self,
        store: &SqlIdentityStore,
        bearer: &str,
        expected_nonce: &str,
    ) -> Result<(AuthenticatedActor, i64), RuntimeError> {
        use sha2::Digest;
        let subject = self.verify(bearer)?;
        if decode_header(bearer)
            .map_err(|_| denied())?
            .typ
            .as_deref()
            .is_some_and(|typ| typ != "JWT")
            || subject.multiple_audiences
                && subject.authorized_party.as_deref() != Some(self.policy.audience.as_str())
            || subject
                .authorized_party
                .as_deref()
                .is_some_and(|party| party != self.policy.audience)
        {
            return Err(denied());
        }
        let nonce = subject.nonce.as_deref().ok_or_else(denied)?;
        let actual = sha2::Sha256::digest(nonce.as_bytes());
        let expected = sha2::Sha256::digest(expected_nonce.as_bytes());
        if nonce.len() > 512
            || expected_nonce.is_empty()
            || actual
                .iter()
                .zip(expected)
                .fold(0u8, |n, (a, b)| n | (*a ^ b))
                != 0
        {
            return Err(denied());
        }
        let user = &self
            .policy
            .bindings
            .iter()
            .find(|binding| binding.subject == subject.subject && binding.issuer == subject.issuer)
            .ok_or_else(denied)?
            .user;
        let actor = store.authenticate_oidc_subject(&subject, user).await?;
        Ok((actor, subject.expires_ms))
    }
}
fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn identity(value: &str) -> bool {
    bounded(value, 128)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.@-".contains(&byte))
}
fn invalid() -> RuntimeError {
    RuntimeError::Configuration(
        "invalid explicitly pinned OIDC issuer/audience/key/subject policy".into(),
    )
}
fn denied() -> RuntimeError {
    RuntimeError::Permission("OIDC bearer authentication refused".into())
}
