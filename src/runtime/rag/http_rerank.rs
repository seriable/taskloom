//! Explicit input-token-only reranking endpoint and independent durable ledger.
use futures_util::{future::BoxFuture, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{invalid, CorpusSnapshot, Reranker, RetrievalQuery, ScoredChunk};
use crate::runtime::{
    state::fingerprint,
    store::{atomic_write, blocking, read_json, storage},
    RuntimeError,
};

/// Explicit host configuration; environment names only, never credential values.
/// Billing is this declared input-only tariff, not a verified provider invoice.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRerankConfig {
    pub endpoint: String,
    pub key_env: String,
    pub namespace: String,
    pub model: String,
    pub currency: String,
    pub input_micros_per_million_tokens: u64,
    pub max_input_bytes: usize,
    pub max_response_bytes: usize,
    pub max_request_cost_micros: u64,
    pub max_total_cost_micros: u64,
    pub max_requests: u64,
    pub timeout_ms: u64,
    pub ledger_dir: PathBuf,
}
impl std::fmt::Debug for HttpRerankConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRerankConfig")
            .field("model", &self.model)
            .field("namespace", &self.namespace)
            .field("currency", &self.currency)
            .finish_non_exhaustive()
    }
}
impl HttpRerankConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let endpoint = reqwest::Url::parse(&self.endpoint)
            .map_err(|_| invalid("invalid reranking endpoint"))?;
        let loopback = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !matches!(endpoint.scheme(), "http" | "https")
            || (endpoint.scheme() == "http" && !loopback)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || self.key_env.is_empty()
            || self.key_env.len() > 128
            || self.key_env.as_bytes()[0].is_ascii_digit()
            || !self
                .key_env
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || self.namespace.trim().is_empty()
            || self.namespace.len() > 256
            || self.model.trim().is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
            || !(3..=8).contains(&self.currency.len())
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
            || !(128..=16 * 1024 * 1024).contains(&self.max_input_bytes)
            || !(128..=16 * 1024 * 1024).contains(&self.max_response_bytes)
            || !(1..=10000).contains(&self.max_requests)
            || !(1..=3_600_000).contains(&self.timeout_ms)
            || self.max_request_cost_micros > self.max_total_cost_micros
            || !self.ledger_dir.is_absolute()
        {
            return Err(invalid(
                "invalid explicit reranking endpoint/identity/price/limits/ledger",
            ));
        }
        Ok(())
    }
    pub fn binding(&self) -> Result<String, RuntimeError> {
        self.validate()?;
        fingerprint(&("taskloom-http-rerank-v1", self))
    }
    fn cost(&self, tokens: u64) -> Result<u64, RuntimeError> {
        u64::try_from(
            (u128::from(tokens) * u128::from(self.input_micros_per_million_tokens))
                .div_ceil(1_000_000),
        )
        .map_err(|_| {
            invalid("reranking token cost exceeds representable range; reservation retained")
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankAttempt {
    pub ordinal: u64,
    pub request_sha256: String,
    pub input_bytes: usize,
    pub reserved_micros: u64,
    /// None means reserved/active/unknown. It is never silently refunded.
    pub reported_input_tokens: Option<u64>,
    pub settled_micros: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankLedgerSnapshot {
    pub format: u32,
    pub binding: String,
    pub model: String,
    pub namespace: String,
    pub currency: String,
    pub attempts: Vec<RerankAttempt>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RerankTotals {
    pub requests: u64,
    pub usage_reported_requests: u64,
    #[serde(serialize_with = "decimal_string")]
    pub settled_micros: u128,
    #[serde(serialize_with = "decimal_string")]
    pub reserved_or_unknown_micros: u128,
    pub overrun: bool,
}
fn decimal_string<S: serde::Serializer>(value: &u128, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}
impl RerankLedgerSnapshot {
    pub fn totals(&self) -> RerankTotals {
        let mut total = RerankTotals {
            requests: self.attempts.len() as u64,
            usage_reported_requests: 0,
            settled_micros: 0,
            reserved_or_unknown_micros: 0,
            overrun: false,
        };
        for attempt in &self.attempts {
            match attempt.settled_micros {
                Some(cost) => {
                    total.usage_reported_requests += 1;
                    total.settled_micros += u128::from(cost);
                    total.overrun |= cost > attempt.reserved_micros;
                }
                None => total.reserved_or_unknown_micros += u128::from(attempt.reserved_micros),
            }
        }
        total
    }
    fn validate(&self, config: &HttpRerankConfig) -> Result<(), RuntimeError> {
        if self.format != 1
            || self.binding != config.binding()?
            || self.model != config.model
            || self.namespace != config.namespace
            || self.currency != config.currency
            || self.attempts.len() > 10000
        {
            return Err(invalid(
                "reranking ledger version/configuration mismatch; explicit migration required",
            ));
        }
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.ordinal != index as u64 + 1
                || attempt.request_sha256.len() != 64
                || !attempt
                    .request_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || attempt.input_bytes > config.max_input_bytes
                || attempt.reserved_micros != config.max_request_cost_micros
                || attempt.reported_input_tokens.is_some() != attempt.settled_micros.is_some()
                || attempt
                    .reported_input_tokens
                    .map(|tokens| config.cost(tokens))
                    .transpose()?
                    != attempt.settled_micros
            {
                return Err(invalid("corrupt reranking reservation/settlement journal"));
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct FileRerankLedger {
    root: PathBuf,
}
/// Only returned by durable admission. No public constructor or Deserialize.
pub struct RerankReservation {
    ordinal: u64,
    binding: String,
}
impl FileRerankLedger {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, RuntimeError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(invalid("reranking ledger path must be absolute"));
        }
        Ok(Self { root })
    }
    fn check(&self, config: &HttpRerankConfig) -> Result<(), RuntimeError> {
        config.validate()?;
        if self.root != config.ledger_dir {
            return Err(invalid(
                "reranking ledger path differs from host configuration",
            ));
        }
        Ok(())
    }
    pub async fn snapshot(
        &self,
        config: &HttpRerankConfig,
    ) -> Result<RerankLedgerSnapshot, RuntimeError> {
        self.check(config)?;
        let root = self.root.clone();
        let config = config.clone();
        blocking(move || {
            let _lock = lock(&root)?;
            read_ledger(&root, &config)
        })
        .await
    }
    /// Reserve a complete serialized request digest and its conservative fixed
    /// cost ceiling before dispatch. Public for explicit host transport adapters.
    pub async fn reserve(
        &self,
        config: &HttpRerankConfig,
        request_sha256: &str,
        input_bytes: usize,
    ) -> Result<RerankReservation, RuntimeError> {
        self.check(config)?;
        if input_bytes == 0
            || input_bytes > config.max_input_bytes
            || request_sha256.len() != 64
            || !request_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("invalid reranking request preflight"));
        }
        // Full serialized UTF-8 byte count is an explicit admission estimate,
        // not a provider-token guarantee. The fixed ceiling remains reserved.
        if config.cost(input_bytes as u64)? > config.max_request_cost_micros {
            return Err(RuntimeError::Budget(
                "reranking byte/token estimate exceeds per-request ceiling".into(),
            ));
        }
        let root = self.root.clone();
        let config = config.clone();
        let request_sha256 = request_sha256.to_owned();
        blocking(move || {
            let _lock = lock(&root)?;
            let mut state = read_ledger(&root, &config)?;
            let totals = state.totals();
            if totals.overrun
                || totals.requests >= config.max_requests
                || totals.settled_micros
                    + totals.reserved_or_unknown_micros
                    + u128::from(config.max_request_cost_micros)
                    > u128::from(config.max_total_cost_micros)
            {
                return Err(RuntimeError::Budget(
                    "reranking allocation exhausted or observed overrun; no dispatch admitted"
                        .into(),
                ));
            }
            let ordinal = state.attempts.len() as u64 + 1;
            state.attempts.push(RerankAttempt {
                ordinal,
                request_sha256,
                input_bytes,
                reserved_micros: config.max_request_cost_micros,
                reported_input_tokens: None,
                settled_micros: None,
            });
            state.validate(&config)?;
            atomic_write(&root.join("rerank-ledger.json"), &state, 8 * 1024 * 1024)?;
            Ok(RerankReservation {
                ordinal,
                binding: state.binding,
            })
        })
        .await
    }
    async fn settle(
        &self,
        config: &HttpRerankConfig,
        reservation: RerankReservation,
        tokens: u64,
    ) -> Result<bool, RuntimeError> {
        self.check(config)?;
        let cost = config.cost(tokens)?;
        let root = self.root.clone();
        let config = config.clone();
        blocking(move || {
            let _lock = lock(&root)?;
            let mut state = read_ledger(&root, &config)?;
            if state.binding != reservation.binding {
                return Err(invalid("reranking reservation binding changed"));
            }
            let attempt = state
                .attempts
                .get_mut((reservation.ordinal - 1) as usize)
                .ok_or_else(|| invalid("reranking reservation disappeared"))?;
            if attempt.settled_micros.is_some() {
                return Err(invalid("reranking attempt already settled"));
            }
            attempt.reported_input_tokens = Some(tokens);
            attempt.settled_micros = Some(cost);
            state.validate(&config)?;
            let totals = state.totals();
            atomic_write(&root.join("rerank-ledger.json"), &state, 8 * 1024 * 1024)?;
            Ok(totals.overrun
                || totals.settled_micros + totals.reserved_or_unknown_micros
                    > u128::from(config.max_total_cost_micros))
        })
        .await
    }
}
fn read_ledger(
    root: &Path,
    config: &HttpRerankConfig,
) -> Result<RerankLedgerSnapshot, RuntimeError> {
    reject_links(&root.join("rerank-ledger.json"), true)?;
    let value =
        read_json::<RerankLedgerSnapshot>(&root.join("rerank-ledger.json"), 8 * 1024 * 1024)?
            .unwrap_or(RerankLedgerSnapshot {
                format: 1,
                binding: config.binding()?,
                model: config.model.clone(),
                namespace: config.namespace.clone(),
                currency: config.currency.clone(),
                attempts: Vec::new(),
            });
    value.validate(config)?;
    Ok(value)
}
struct Guard(File);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn lock(root: &Path) -> Result<Guard, RuntimeError> {
    reject_links(root, true)?;
    std::fs::create_dir_all(root).map_err(storage)?;
    reject_links(&root.join("rerank-ledger.lock"), true)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("rerank-ledger.lock"))
        .map_err(storage)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
        RuntimeError::Conflict(
            "reranking ledger is owned by another process; explicit retry required".into(),
        )
    })?;
    Ok(Guard(file))
}

fn reject_links(path: &Path, allow_missing: bool) -> Result<(), RuntimeError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(invalid("reranking ledger traversal refused"))
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            _ => current.push(component.as_os_str()),
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || reparse(&metadata) {
                    return Err(invalid("reranking ledger symlink/reparse point refused"));
                }
            }
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(storage("reranking ledger path unavailable")),
        }
    }
    Ok(())
}
#[cfg(windows)]
fn reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn reparse(_: &std::fs::Metadata) -> bool {
    false
}

pub struct HttpReranker {
    config: HttpRerankConfig,
    ledger: FileRerankLedger,
    client: reqwest::Client,
    key: String,
}
impl HttpReranker {
    /// Loads only the configured credential environment name. Construction and
    /// ledger inspection never send a request or modify a spending journal.
    pub fn new(config: HttpRerankConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let key = std::env::var(&config.key_env)
            .map_err(|_| invalid("reranking credential environment value is missing"))?;
        if key.is_empty() || key.len() > 8192 {
            return Err(invalid("invalid reranking credential length"));
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| invalid("invalid reranking bearer header"))?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|_| invalid("cannot initialize reranking HTTP client"))?;
        Ok(Self {
            ledger: FileRerankLedger::new(config.ledger_dir.clone())?,
            config,
            client,
            key,
        })
    }
    pub fn ledger(&self) -> &FileRerankLedger {
        &self.ledger
    }
}
impl Reranker for HttpReranker {
    fn identity(&self) -> Option<String> {
        Some(format!(
            "http-rerank-v1:{}",
            self.config.binding().expect("validated HTTP reranker")
        ))
    }
    fn rerank<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
        candidates: &'a [ScoredChunk],
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            query.validate()?;
            snapshot.validate()?;
            if candidates.len() > 20 {
                return Err(invalid("HTTP reranking candidate count exceeds limit"));
            }
            let mut seen = BTreeSet::new();
            let mut documents = Vec::new();
            for candidate in candidates {
                if !candidate.score.is_finite()
                    || candidate.score < 0.0
                    || !seen.insert(&candidate.chunk_id)
                {
                    return Err(invalid("invalid HTTP reranking candidate IDs/scores"));
                }
                let chunk = snapshot
                    .chunks
                    .iter()
                    .find(|chunk| chunk.id == candidate.chunk_id)
                    .ok_or_else(|| invalid("HTTP reranking candidate outside corpus snapshot"))?;
                documents.push(&chunk.text);
            }
            if documents.is_empty() {
                return Ok(Vec::new());
            }
            let body=serde_json::to_vec(&json!({"model":self.config.model,"query":query.text,"documents":documents,"top_n":candidates.len()})).map_err(|_|invalid("cannot serialize reranking request"))?;
            if body.len() > self.config.max_input_bytes {
                return Err(invalid(
                    "complete reranking request exceeds input byte limit",
                ));
            }
            if body
                .windows(self.key.len())
                .any(|window| window == self.key.as_bytes())
            {
                return Err(invalid(
                    "reranking credential appeared in input body; dispatch refused",
                ));
            }
            let digest = hex::encode(Sha256::digest(&body));
            let reservation = self
                .ledger
                .reserve(&self.config, &digest, body.len())
                .await?;
            // Any cancellation/error after this point retains the durable slot
            // and ceiling. There is intentionally no Drop-based refund/retry.
            let response = self
                .client
                .post(&self.config.endpoint)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .map_err(|_| {
                    invalid("reranking HTTP request failed; charge unknown, reservation retained")
                })?;
            let status = response.status();
            if response
                .content_length()
                .is_some_and(|length| length > self.config.max_response_bytes as u64)
            {
                return Err(invalid(
                    "reranking response exceeds limit; reservation retained",
                ));
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(part) = stream.next().await {
                let part = part
                    .map_err(|_| invalid("reranking response interrupted; reservation retained"))?;
                if bytes.len().saturating_add(part.len()) > self.config.max_response_bytes {
                    return Err(invalid(
                        "reranking response exceeds limit; reservation retained",
                    ));
                }
                bytes.extend_from_slice(&part);
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                invalid("malformed reranking JSON; usage unknown, reservation retained")
            })?;
            let object = value.as_object().ok_or_else(|| {
                invalid("reranking response is not an object; usage unknown, reservation retained")
            })?;
            if object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "results" | "usage" | "model" | "id" | "object" | "request_id"
                )
            }) {
                return Err(invalid(
                    "unsupported reranking response/billing profile; reservation retained",
                ));
            }
            if value
                .get("model")
                .is_some_and(|v| v.as_str() != Some(self.config.model.as_str()))
            {
                return Err(invalid(
                    "reranking served model differs; unknown tariff, reservation retained",
                ));
            }
            let usage = value
                .get("usage")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    invalid("reranking input-only usage missing; reservation retained")
                })?;
            if usage.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "total_tokens" | "input_tokens" | "prompt_tokens"
                )
            }) {
                return Err(invalid("unsupported reranking billing fields; input-only tariff required, reservation retained"));
            }
            let tokens = usage
                .get("total_tokens")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("invalid reranking total_tokens; reservation retained"))?;
            if usage
                .iter()
                .any(|(key, value)| key != "total_tokens" && value.as_u64() != Some(tokens))
            {
                return Err(invalid(
                    "inconsistent reranking input usage; reservation retained",
                ));
            }
            if self
                .ledger
                .settle(&self.config, reservation, tokens)
                .await?
            {
                return Err(RuntimeError::Budget("reranking reported cost exceeded reservation/cap; charge retained, further dispatch blocked".into()));
            }
            if !status.is_success() {
                return Err(invalid("reranking returned unsuccessful HTTP status; reported usage recorded, no automatic retry"));
            }
            let results = value
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("reranking response has no result array; usage recorded"))?;
            if results.len() > candidates.len() {
                return Err(invalid(
                    "reranking result count exceeds candidate count; usage recorded",
                ));
            }
            let mut indices = BTreeSet::new();
            let mut ranked = Vec::new();
            for result in results {
                let index = result
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < candidates.len())
                    .ok_or_else(|| {
                        invalid("reranking result index outside actual candidates; usage recorded")
                    })?;
                let score = result
                    .get("relevance_score")
                    .and_then(Value::as_f64)
                    .filter(|score| score.is_finite() && *score >= 0.0)
                    .ok_or_else(|| invalid("invalid reranking result score; usage recorded"))?;
                if !indices.insert(index) {
                    return Err(invalid("duplicate reranking result index; usage recorded"));
                }
                ranked.push(ScoredChunk {
                    chunk_id: candidates[index].chunk_id.clone(),
                    score,
                });
            }
            ranked.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
            Ok(ranked)
        })
    }
}
