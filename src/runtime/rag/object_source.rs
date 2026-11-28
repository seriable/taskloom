//! Host-pinned presigned object GET snapshots. The URL is an opaque secret.
use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{invalid, Document, MAX_DOCUMENT_BYTES};
use crate::runtime::{state::fingerprint, RuntimeError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresignedObjectConfig {
    /// Host-chosen public logical identities, not bucket credentials or URLs.
    pub source_namespace: String,
    pub source_version: String,
    pub object_id: String,
    pub expected_sha256: String,
    pub expected_bytes: usize,
    pub max_bytes: usize,
    pub timeout_ms: u64,
    #[serde(default)]
    pub allow_http_loopback_for_tests: bool,
}
impl PresignedObjectConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let label = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        };
        if !label(&self.source_namespace)
            || !label(&self.source_version)
            || !label(&self.object_id)
            || self.expected_sha256.len() != 64
            || !self
                .expected_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !(1..=MAX_DOCUMENT_BYTES).contains(&self.max_bytes)
            || self.expected_bytes == 0
            || self.expected_bytes > self.max_bytes
            || !(1..=60_000).contains(&self.timeout_ms)
        {
            return Err(rejected());
        }
        Ok(())
    }
}
/// No Debug, Serialize, URL getter, redirects, proxies, retries or URL overrides.
pub struct PresignedObjectSource {
    config: PresignedObjectConfig,
    url: reqwest::Url,
    client: reqwest::Client,
}
/// Inert exact bytes plus public provenance; contains no signed URL or headers.
pub struct ObjectSourceSnapshot {
    config: PresignedObjectConfig,
    policy_sha256: String,
    bytes: Vec<u8>,
}
impl ObjectSourceSnapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn source_namespace(&self) -> &str {
        &self.config.source_namespace
    }
    pub fn source_version(&self) -> &str {
        &self.config.source_version
    }
    pub fn sha256(&self) -> &str {
        &self.config.expected_sha256
    }
    /// Plain UTF-8 bytes only. Binary objects require a separately reviewed parser.
    pub fn document(&self, id: &str, title: &str) -> Result<Document, RuntimeError> {
        let text = std::str::from_utf8(&self.bytes)
            .map_err(|_| rejected())?
            .to_owned();
        let document = Document {
            id: id.into(),
            title: title.into(),
            text,
            source: format!(
                "taskloom-object://{}/{}/{}#sha256={}",
                self.config.source_namespace,
                self.config.source_version,
                self.config.object_id,
                self.config.expected_sha256
            ),
            metadata: BTreeMap::from([
                ("format".into(), "presigned-object-utf8-snapshot-v1".into()),
                (
                    "source_namespace".into(),
                    self.config.source_namespace.clone(),
                ),
                ("source_version".into(), self.config.source_version.clone()),
                ("source_object_id".into(), self.config.object_id.clone()),
                (
                    "source_body_sha256".into(),
                    self.config.expected_sha256.clone(),
                ),
                ("source_body_bytes".into(), self.bytes.len().to_string()),
                ("source_policy_sha256".into(), self.policy_sha256.clone()),
                (
                    "citation_offsets".into(),
                    "UTF-8 bytes in exact object content snapshot".into(),
                ),
            ]),
        };
        document.validate().map_err(|_| rejected())?;
        Ok(document)
    }
}
impl PresignedObjectSource {
    /// The host supplies the URL once; it must never come from model/tool/HTTP input.
    pub fn new(config: PresignedObjectConfig, secret_url: String) -> Result<Self, RuntimeError> {
        config.validate()?;
        let url = reqwest::Url::parse(&secret_url).map_err(|_| rejected())?;
        if secret_url.trim() != secret_url
            || secret_url.len() > 16384
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !(url.scheme() == "https"
                || config.allow_http_loopback_for_tests
                    && url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "[::1]")))
        {
            return Err(rejected());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|_| rejected())?;
        Ok(Self {
            config,
            url,
            client,
        })
    }
    pub async fn load(&self) -> Result<ObjectSourceSnapshot, RuntimeError> {
        tokio::time::timeout(
            Duration::from_millis(self.config.timeout_ms),
            self.load_inner(),
        )
        .await
        .map_err(|_| rejected())?
    }
    async fn load_inner(&self) -> Result<ObjectSourceSnapshot, RuntimeError> {
        let response = self
            .client
            .get(self.url.clone())
            .send()
            .await
            .map_err(|_| rejected())?;
        if response.status() != reqwest::StatusCode::OK
            || response
                .headers()
                .contains_key(reqwest::header::CONTENT_RANGE)
            || response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|v| v.to_str().ok() != Some("identity"))
            || response
                .content_length()
                .is_some_and(|n| n != self.config.expected_bytes as u64)
        {
            return Err(rejected());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| rejected())?;
            if bytes.len().saturating_add(chunk.len()) > self.config.expected_bytes
                || bytes.len().saturating_add(chunk.len()) > self.config.max_bytes
            {
                return Err(rejected());
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() != self.config.expected_bytes
            || hex::encode(Sha256::digest(&bytes)) != self.config.expected_sha256
        {
            return Err(rejected());
        }
        Ok(ObjectSourceSnapshot {
            config: self.config.clone(),
            policy_sha256: fingerprint(&("presigned-object-source-v1", &self.config))?,
            bytes,
        })
    }
}
fn rejected() -> RuntimeError {
    invalid(
        "object snapshot policy/fetch/size/digest rejected; no partial object or automatic retry",
    )
}
