//! Fixed-host REST connector. Sources always come from the exact local corpus;
//! remote payloads cannot replace text, choose a scope or grant a write.
use super::{
    invalid, CorpusSnapshot, EmbeddingIndex, EmbeddingModel, RetrievalQuery, Retriever, ScoredChunk,
};
use crate::runtime::{
    state::fingerprint,
    store::{atomic_write, blocking, read_json, storage},
    RuntimeError,
};
use fs2::FileExt;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QdrantConfig {
    pub endpoint: String,
    pub collection: String,
    /// Trusted namespace, not a request/model-selected tenant filter.
    pub namespace: String,
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
    pub max_request_bytes: usize,
    pub batch_points: usize,
}
impl QdrantConfig {
    /// Static, credential-free configuration check.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let url =
            reqwest::Url::parse(&self.endpoint).map_err(|_| invalid("invalid Qdrant endpoint"))?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || !(url.scheme() == "https"
                || url.scheme() == "http"
                    && url.host_str().is_some_and(|host| {
                        matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
                    }))
            || self.collection.is_empty()
            || self.collection.len() > 96
            || !self
                .collection
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            || self.namespace.trim().is_empty()
            || self.namespace.len() > 1024
            || self.namespace.chars().any(char::is_control)
            || !(100..=120_000).contains(&self.timeout_ms)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_response_bytes)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_request_bytes)
            || !(1..=128).contains(&self.batch_points)
            || self.api_key_env.as_ref().is_some_and(|name| {
                name.is_empty()
                    || name.len() > 128
                    || (!name.as_bytes()[0].is_ascii_alphabetic() && name.as_bytes()[0] != b'_')
                    || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            })
        {
            return Err(invalid(
                "invalid fixed Qdrant target, credentials name or request bounds",
            ));
        }
        Ok(())
    }
    pub fn binding(&self) -> Result<String, RuntimeError> {
        fingerprint(&("qdrant-target-v1", self))
    }
}
pub struct QdrantClient {
    config: QdrantConfig,
    http: reqwest::Client,
    key: Option<reqwest::header::HeaderValue>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BatchState {
    Planned,
    Pending,
    Acknowledged,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QdrantPublishBatch {
    pub ordinal: usize,
    pub request_sha256: String,
    pub points: usize,
    state: BatchState,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QdrantPublishSnapshot {
    pub version: u32,
    pub target_binding: String,
    pub index_binding: String,
    pub batches: Vec<QdrantPublishBatch>,
    pub complete: bool,
}
/// Returned only after all exact batches acknowledge `completed`. No public
/// constructor/Deserialize: an incomplete write cannot authorize query use.
pub struct QdrantPublishReceipt {
    target: String,
    index: String,
}
/// Host-private local journal. Pending includes dispatched/lost-ack and must be
/// treated as unknown. This version refuses restart/retry of any existing file.
#[derive(Clone)]
pub struct FileQdrantPublishJournal {
    root: PathBuf,
}
impl FileQdrantPublishJournal {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, RuntimeError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(invalid("Qdrant publish journal path must be absolute"));
        }
        Ok(Self { root })
    }
    async fn lock(&self) -> Result<File, RuntimeError> {
        let root = self.root.clone();
        blocking(move || {
            std::fs::create_dir_all(&root).map_err(storage)?;
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(root.join("publish.lock"))
                .map_err(storage)?;
            file.try_lock_exclusive().map_err(|_| {
                RuntimeError::Conflict("Qdrant publish owned by another process".into())
            })?;
            Ok(file)
        })
        .await
    }
    async fn save(&self, snapshot: &QdrantPublishSnapshot) -> Result<(), RuntimeError> {
        let path = self.root.join("publish.json");
        let value = snapshot.clone();
        blocking(move || atomic_write(&path, &value, 8 * 1024 * 1024)).await
    }
    pub async fn snapshot(&self) -> Result<Option<QdrantPublishSnapshot>, RuntimeError> {
        let _lock = self.lock().await?;
        let path = self.root.join("publish.json");
        blocking(move || read_json(&path, 8 * 1024 * 1024)).await
    }
}
struct Batch {
    request: Value,
    record: QdrantPublishBatch,
}
impl QdrantClient {
    /// Explicit host operation: resolves a named credential but performs no
    /// network call. A client is never constructed from an agent query body.
    pub fn new(config: QdrantConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        let key = config
            .api_key_env
            .as_ref()
            .map(|name| {
                let raw = std::env::var(name)
                    .map_err(|_| invalid("Qdrant named credential is missing"))?;
                if raw.trim().is_empty() || raw.len() > 8192 {
                    return Err(invalid("invalid Qdrant named credential"));
                }
                let mut header = reqwest::header::HeaderValue::from_str(&raw)
                    .map_err(|_| invalid("invalid Qdrant credential header"))?;
                header.set_sensitive(true);
                Ok(header)
            })
            .transpose()?;
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(storage)?;
        Ok(Self { config, http, key })
    }
}
impl QdrantConfig {
    fn index_binding(
        &self,
        snapshot: &CorpusSnapshot,
        index: &EmbeddingIndex,
    ) -> Result<String, RuntimeError> {
        index.validate(snapshot, &index.model_identity)?;
        self.validate()?;
        fingerprint(&("qdrant-index-v1", self.binding()?, index))
    }
    fn point_id(&self, binding: &str, chunk: &str) -> Result<String, RuntimeError> {
        let hash = fingerprint(&("qdrant-point-v1", binding, chunk))?;
        let bytes = hex::decode(&hash[..32]).map_err(storage)?;
        let mut value = [0u8; 16];
        value.copy_from_slice(&bytes);
        // Deterministic content-bound UUID, never an upstream chunk's ordinal.
        value[6] = (value[6] & 15) | 0x50;
        value[8] = (value[8] & 63) | 0x80;
        Ok(uuid::Uuid::from_bytes(value).to_string())
    }
    fn payload(&self, binding: &str, index: &EmbeddingIndex, chunk: &str) -> Value {
        json!({"taskloom_version":1,"taskloom_scope":self.namespace,"taskloom_index":binding,"taskloom_corpus":index.corpus_fingerprint,"taskloom_model":index.model_identity,"taskloom_chunk":chunk})
    }
    fn batches(
        &self,
        snapshot: &CorpusSnapshot,
        index: &EmbeddingIndex,
        binding: &str,
    ) -> Result<Vec<Batch>, RuntimeError> {
        let mut batches = Vec::new();
        let mut identifiers = BTreeSet::new();
        for (ordinal, chunks) in snapshot.chunks.chunks(self.batch_points).enumerate() {
            let mut points = Vec::new();
            for chunk in chunks {
                let id = self.point_id(binding, &chunk.id)?;
                if !identifiers.insert(id.clone()) {
                    return Err(invalid("Qdrant point UUID collision"));
                }
                points.push(json!({"id":id,"vector":index.vectors[&chunk.id],"payload":self.payload(binding,index,&chunk.id)}));
            }
            let request = json!({"points":points});
            let bytes = serde_json::to_vec(&request).map_err(storage)?;
            if bytes.len() > self.max_request_bytes {
                return Err(invalid("Qdrant publish batch exceeds byte cap; no write"));
            }
            batches.push(Batch {
                record: QdrantPublishBatch {
                    ordinal,
                    request_sha256: fingerprint(&request)?,
                    points: chunks.len(),
                    state: BatchState::Planned,
                },
                request,
            });
        }
        Ok(batches)
    }
    /// Pure, credential-free validation of the complete exact publish plan.
    /// Local host journals are trusted storage, never request-owned receipts.
    pub fn validate_publish_snapshot(
        &self,
        snapshot: &CorpusSnapshot,
        index: &EmbeddingIndex,
        state: &QdrantPublishSnapshot,
    ) -> Result<QdrantPublishReceipt, RuntimeError> {
        let binding = self.index_binding(snapshot, index)?;
        let expected = self.batches(snapshot, index, &binding)?;
        if state.version != 1
            || !state.complete
            || state.target_binding != self.binding()?
            || state.index_binding != binding
            || state.batches.len() != expected.len()
            || state.batches.iter().zip(expected).any(|(actual, planned)| {
                actual.ordinal != planned.record.ordinal
                    || actual.points != planned.record.points
                    || actual.request_sha256 != planned.record.request_sha256
                    || actual.state != BatchState::Acknowledged
            })
        {
            return Err(invalid("Qdrant publish incomplete, unknown or bound to another target/index; query refused"));
        }
        Ok(QdrantPublishReceipt {
            target: state.target_binding.clone(),
            index: binding,
        })
    }
}
/// Host admission/fencing checks before every batch and final completion.
pub trait QdrantPublishGuard: Send + Sync {
    fn verify(&self) -> BoxFuture<'_, Result<(), RuntimeError>>;
}
struct HostExplicit;
impl QdrantPublishGuard for HostExplicit {
    fn verify(&self) -> BoxFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async { Ok(()) })
    }
}
impl QdrantClient {
    async fn request(
        &self,
        method: reqwest::Method,
        suffix: &str,
        body: Option<&Value>,
    ) -> Result<Value, RuntimeError> {
        let url = format!(
            "{}/collections/{}{}",
            self.config.endpoint.trim_end_matches('/'),
            self.config.collection,
            suffix
        );
        let mut request = self.http.request(method, url);
        if let Some(key) = &self.key {
            request = request.header("api-key", key);
        }
        if let Some(body) = body {
            let raw = serde_json::to_vec(body).map_err(storage)?;
            if raw.len() > self.config.max_request_bytes {
                return Err(invalid("Qdrant request exceeds byte cap"));
            }
            request = request.header("content-type", "application/json").body(raw);
        }
        let mut response = request.send().await.map_err(|_| {
            invalid("Qdrant request failed; pending publish outcome remains unknown")
        })?;
        if !response.status().is_success() {
            return Err(invalid(&format!(
                "Qdrant HTTP status {}; pending publish outcome remains unknown",
                response.status().as_u16()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| invalid("Qdrant response interrupted; retain pending publish"))?
        {
            if bytes.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                return Err(invalid(
                    "Qdrant response exceeds byte cap; retain pending publish",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("invalid Qdrant response JSON; retain pending publish"))?;
        if value["status"] != "ok" || value.get("result").is_none() {
            return Err(invalid(
                "Qdrant response did not report success; retain pending publish",
            ));
        }
        Ok(value["result"].clone())
    }
    async fn collection(&self, dimensions: usize) -> Result<(), RuntimeError> {
        let value = self.request(reqwest::Method::GET, "", None).await?;
        let vectors = &value["config"]["params"]["vectors"];
        if vectors["size"].as_u64() != Some(dimensions as u64) || vectors["distance"] != "Cosine" {
            return Err(invalid(
                "Qdrant requires existing unnamed cosine collection with exact index dimensions",
            ));
        }
        Ok(())
    }
    /// Host-only explicit upsert of already-built vectors. Creates no collection,
    /// performs no embedding and never retries, deletes or silently resumes.
    /// The caller must retain the journal after failure/cancellation.
    pub async fn publish(
        &self,
        snapshot: &CorpusSnapshot,
        index: &EmbeddingIndex,
        journal: &FileQdrantPublishJournal,
    ) -> Result<QdrantPublishReceipt, RuntimeError> {
        self.publish_guarded(snapshot, index, journal, &HostExplicit)
            .await
    }
    pub async fn publish_guarded(
        &self,
        snapshot: &CorpusSnapshot,
        index: &EmbeddingIndex,
        journal: &FileQdrantPublishJournal,
        guard: &dyn QdrantPublishGuard,
    ) -> Result<QdrantPublishReceipt, RuntimeError> {
        let binding = self.config.index_binding(snapshot, index)?;
        let batches = self.config.batches(snapshot, index, &binding)?;
        let _lock = journal.lock().await?;
        let path = journal.root.join("publish.json");
        if blocking(move || Ok(path.exists())).await? {
            return Err(RuntimeError::Conflict("Qdrant publish journal already exists; inspect pending/unknown batches, no automatic retry".into()));
        }
        guard.verify().await?;
        self.collection(index.dimensions).await?;
        let mut state = QdrantPublishSnapshot {
            version: 1,
            target_binding: self.config.binding()?,
            index_binding: binding.clone(),
            batches: batches.iter().map(|batch| batch.record.clone()).collect(),
            complete: false,
        };
        journal.save(&state).await?;
        for (ordinal, batch) in batches.iter().enumerate() {
            state.batches[ordinal].state = BatchState::Pending;
            journal.save(&state).await?;
            guard.verify().await?;
            let value = self
                .request(
                    reqwest::Method::PUT,
                    "/points?wait=true",
                    Some(&batch.request),
                )
                .await?;
            if value["status"] != "completed" || value["operation_id"].as_u64().is_none() {
                return Err(invalid(
                    "Qdrant upsert lacks completed acknowledgement; batch remains pending/unknown",
                ));
            }
            state.batches[ordinal].state = BatchState::Acknowledged;
            journal.save(&state).await?;
        }
        guard.verify().await?;
        state.complete = true;
        journal.save(&state).await?;
        Ok(QdrantPublishReceipt {
            target: state.target_binding,
            index: binding,
        })
    }
    /// Reopen only a fully acknowledged local host journal. It is not a
    /// client-supplied import format or proof of remote completeness/availability.
    pub async fn completed_receipt(
        &self,
        snapshot: &CorpusSnapshot,
        index: &EmbeddingIndex,
        journal: &FileQdrantPublishJournal,
    ) -> Result<QdrantPublishReceipt, RuntimeError> {
        let state = journal
            .snapshot()
            .await?
            .ok_or_else(|| invalid("Qdrant publish journal missing"))?;
        self.config
            .validate_publish_snapshot(snapshot, index, &state)
    }
}

/// Host injects a durable-metered EmbeddingModel for any paid service. The
/// connector adds no unmetered provider and never charges generation RunBudget.
pub struct QdrantRetriever {
    client: Arc<QdrantClient>,
    index: Arc<EmbeddingIndex>,
    receipt: QdrantPublishReceipt,
    model: Arc<dyn EmbeddingModel>,
    min_score: f64,
}
impl QdrantRetriever {
    pub fn new(
        client: Arc<QdrantClient>,
        snapshot: &CorpusSnapshot,
        index: Arc<EmbeddingIndex>,
        receipt: QdrantPublishReceipt,
        model: Arc<dyn EmbeddingModel>,
        min_score: f64,
    ) -> Result<Self, RuntimeError> {
        index.validate(snapshot, &model.identity())?;
        if !min_score.is_finite()
            || !(0.0..=1.0).contains(&min_score)
            || receipt.target != client.config.binding()?
            || receipt.index != client.config.index_binding(snapshot, &index)?
        {
            return Err(invalid(
                "Qdrant query target/index/receipt/score binding mismatch",
            ));
        }
        Ok(Self {
            client,
            index,
            receipt,
            model,
            min_score,
        })
    }
}
impl Retriever for QdrantRetriever {
    fn identity(&self) -> Option<String> {
        Some(format!(
            "qdrant-retriever-v1:{}",
            fingerprint(&(
                &self.receipt.target,
                &self.receipt.index,
                self.model.identity(),
                self.min_score
            ))
            .expect("serializable Qdrant binding")
        ))
    }
    fn retrieve<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            query.validate()?;
            self.index.validate(snapshot, &self.model.identity())?;
            if self.receipt.target != self.client.config.binding()?
                || self.receipt.index != self.client.config.index_binding(snapshot, &self.index)?
            {
                return Err(invalid(
                    "Qdrant snapshot/index binding changed; no query embedding",
                ));
            }
            let payload = self
                .client
                .config
                .payload(&self.receipt.index, &self.index, "");
            let filter = payload
                .as_object()
                .expect("payload object")
                .iter()
                .filter(|(key, _)| key.as_str() != "taskloom_chunk")
                .map(|(key, value)| json!({"key":key,"match":{"value":value}}))
                .collect::<Vec<_>>();
            let mut body = json!({"query":[],"filter":{"must":filter},"limit":query.top_k,"with_payload":true,"with_vector":false});
            // A finite f32's shortest JSON form fits in 32 bytes, plus a comma.
            // Reserve that entire vector envelope before any paid embedding,
            // rather than discovering an impossible request cap afterwards.
            let envelope = serde_json::to_vec(&body).map_err(storage)?.len();
            if envelope.saturating_add(self.index.dimensions.saturating_mul(33))
                > self.client.config.max_request_bytes
            {
                return Err(invalid(
                    "Qdrant conservative query envelope exceeds request cap; no embedding dispatch",
                ));
            }
            // Validate the remote collection before the paid query. The local
            // frozen source remains authoritative even if remote data changes.
            self.client.collection(self.index.dimensions).await?;
            let batch = self.model.embed(std::slice::from_ref(&query.text)).await?;
            if batch.identity != self.index.model_identity
                || self.model.identity() != self.index.model_identity
                || batch.vectors.len() != 1
            {
                return Err(invalid("Qdrant query embedding identity/count changed"));
            }
            let vector = &batch.vectors[0];
            if vector.len() != self.index.dimensions
                || vector.iter().any(|n| !n.is_finite())
                || vector.iter().all(|n| *n == 0.0)
            {
                return Err(invalid("invalid Qdrant query vector"));
            }
            body["query"] = json!(vector);
            let value = self
                .client
                .request(reqwest::Method::POST, "/points/query", Some(&body))
                .await?;
            let points = value["points"]
                .as_array()
                .ok_or_else(|| invalid("Qdrant query points array missing"))?;
            if points.len() > query.top_k {
                return Err(invalid("Qdrant returned too many points"));
            }
            let known = snapshot
                .chunks
                .iter()
                .map(|chunk| {
                    Ok((
                        self.client
                            .config
                            .point_id(&self.receipt.index, &chunk.id)?,
                        chunk.id.as_str(),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, RuntimeError>>()?;
            let mut seen = BTreeSet::new();
            let mut ranked = Vec::new();
            for point in points {
                let id = point["id"]
                    .as_str()
                    .ok_or_else(|| invalid("Qdrant point UUID missing"))?;
                let chunk = known
                    .get(id)
                    .ok_or_else(|| invalid("Qdrant returned an unknown point UUID"))?;
                let score = point["score"]
                    .as_f64()
                    .filter(|n| n.is_finite() && (-1.0..=1.0).contains(n))
                    .ok_or_else(|| invalid("invalid Qdrant cosine score"))?;
                if !seen.insert(id)
                    || point["payload"]
                        != self
                            .client
                            .config
                            .payload(&self.receipt.index, &self.index, chunk)
                {
                    return Err(invalid(
                        "duplicate Qdrant point or foreign/corrupt scope/index payload",
                    ));
                }
                // Validate even excluded low-score points before filtering.
                if score > 0.0 && score >= self.min_score {
                    ranked.push(ScoredChunk {
                        chunk_id: (*chunk).into(),
                        score,
                    });
                }
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
