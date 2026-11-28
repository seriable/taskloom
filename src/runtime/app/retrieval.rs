//! Explicit local index lifecycle and separately journaled embedding spending.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    config::{config_error, secret},
    AppConfig, AppProvider, AppStores,
};
use crate::runtime::{
    rag::{
        self, ConfiguredLexicalRetriever, CorpusStore, EmbeddingBatch, EmbeddingIndex,
        EmbeddingModel, HybridRetriever, IdentifierConstrainedRetriever, LexicalConfig, Retriever,
        VectorRetriever,
    },
    state::fingerprint,
    store::{atomic_write, blocking, read_json, storage},
    RuntimeError,
};

const INDEX_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    #[default]
    Lexical,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppRetrieval {
    pub mode: RetrievalMode,
    pub lexical: LexicalConfig,
    pub embedding: Option<AppEmbedding>,
    /// Host-fixed external vector target. None preserves legacy identity.
    #[cfg(feature = "qdrant")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qdrant: Option<rag::QdrantConfig>,
    pub lexical_weight: f64,
    pub semantic_weight: f64,
    /// Explicitly calibrated cosine cutoff; zero preserves positive-only behavior.
    pub semantic_min_score: f64,
    /// Explicit hard lexical constraints for long mixed letter/digit identifiers.
    /// Omitted when false to preserve legacy configuration fingerprints.
    #[serde(skip_serializing_if = "is_false")]
    pub exact_identifiers: bool,
    /// Only semantic-admitted candidates can contribute to hybrid fusion.
    /// False preserves the legacy union and its exact configuration binding.
    #[serde(skip_serializing_if = "is_false")]
    pub require_semantic_match: bool,
    /// Explicit query-only publisher instruction, never added to document vectors.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub query_prefix: String,
    /// Explicit index version2: embed visible chunk title along with its body.
    #[serde(skip_serializing_if = "is_false")]
    pub embedding_document_context: bool,
    /// Require distinct lexical query terms before semantic candidate ranking.
    /// Zero preserves legacy admission and serialized configuration identity.
    #[serde(skip_serializing_if = "is_zero")]
    pub semantic_min_lexical_terms: usize,
    pub rank_constant: f64,
    pub candidate_k: usize,
    /// Zero preserves legacy ranking/identity. Otherwise explicitly widen to
    /// candidate_k and cap final chunks from one document after reranking.
    #[serde(skip_serializing_if = "is_zero")]
    pub max_chunks_per_document: usize,
    /// Explicit local reranking baseline, without paid or hidden HTTP calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<AppLexicalRerank>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_rerank: Option<AppHttpRerank>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppHttpRerank {
    pub candidates: usize,
    pub max_candidate_bytes: usize,
    pub provider: rag::HttpRerankConfig,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppLexicalRerank {
    pub candidates: usize,
    pub max_candidate_bytes: usize,
    pub lexical: LexicalConfig,
}
impl Default for AppRetrieval {
    fn default() -> Self {
        Self {
            mode: RetrievalMode::Lexical,
            lexical: LexicalConfig::default(),
            embedding: None,
            #[cfg(feature = "qdrant")]
            qdrant: None,
            lexical_weight: 1.0,
            semantic_weight: 1.0,
            semantic_min_score: 0.0,
            exact_identifiers: false,
            require_semantic_match: false,
            query_prefix: String::new(),
            embedding_document_context: false,
            semantic_min_lexical_terms: 0,
            rank_constant: 60.0,
            candidate_k: 20,
            max_chunks_per_document: 0,
            rerank: None,
            http_rerank: None,
        }
    }
}
fn is_false(value: &bool) -> bool {
    !value
}
fn is_zero(value: &usize) -> bool {
    *value == 0
}
/// All billing fields are explicit. The host's per-request reservation must
/// cover any accepted input at the configured service's price. An unexpected
/// reported overrun blocks subsequent dispatch; missing usage keeps the full
/// reservation. This is a local admission limit, not a provider billing limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppEmbedding {
    pub provider: AppProvider,
    pub model: String,
    pub currency: String,
    pub input_micros_per_million_tokens: u64,
    pub max_request_cost_micros: u64,
    pub max_total_cost_micros: u64,
    pub max_input_bytes: usize,
    pub timeout_ms: u64,
}
impl AppRetrieval {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.max_chunks_per_document != 0 {
            rag::PerDocumentRetriever::new(
                Arc::new(rag::LexicalRetriever),
                self.max_chunks_per_document,
                self.candidate_k,
            )?;
            if self
                .rerank
                .as_ref()
                .is_some_and(|r| r.candidates < self.candidate_k)
                || self
                    .http_rerank
                    .as_ref()
                    .is_some_and(|r| r.candidates < self.candidate_k)
            {
                return Err(config_error("source diversity candidate_k exceeds explicitly authorized rerank candidate count"));
            }
        }
        self.lexical.validate()?;
        if self.rerank.is_some() && self.http_rerank.is_some() {
            return Err(config_error(
                "select one explicit reranker, not multiple implicit passes",
            ));
        }
        if let Some(rerank) = &self.http_rerank {
            rerank.provider.validate()?;
            rag::RerankingRetriever::new(
                Arc::new(rag::LexicalRetriever),
                Arc::new(rag::LexicalReranker::default()),
                rerank.candidates,
                rerank.max_candidate_bytes,
            )?;
        }
        if let Some(rerank) = &self.rerank {
            rag::RerankingRetriever::new(
                Arc::new(rag::LexicalRetriever),
                Arc::new(rag::LexicalReranker::new(rerank.lexical.clone())?),
                rerank.candidates,
                rerank.max_candidate_bytes,
            )?;
        }
        if self.semantic_min_lexical_terms > 256 {
            return Err(config_error("semantic_min_lexical_terms exceeds256"));
        }
        if self.query_prefix.len() > 4096 || self.query_prefix.contains('\0') {
            return Err(config_error(
                "query_prefix exceeds4096 bytes or contains NUL",
            ));
        }
        if !self.semantic_min_score.is_finite() || !(0.0..=1.0).contains(&self.semantic_min_score) {
            return Err(config_error(
                "semantic_min_score must be finite and in 0..=1",
            ));
        }
        HybridRetriever::new(
            Arc::new(rag::LexicalRetriever),
            Arc::new(rag::LexicalRetriever),
        )
        .with_fusion(
            self.lexical_weight,
            self.semantic_weight,
            self.rank_constant,
            self.candidate_k,
        )?
        .with_semantic_gate(self.require_semantic_match)?;
        if self.mode == RetrievalMode::Hybrid && self.embedding.is_none() {
            return Err(config_error(
                "hybrid retrieval requires embedding configuration",
            ));
        }
        if let Some(embedding) = &self.embedding {
            embedding.validate()?;
        }
        #[cfg(feature = "qdrant")]
        if let Some(target) = &self.qdrant {
            target.validate()?;
            if self.embedding.is_none() {
                return Err(config_error(
                    "Qdrant requires explicit metered embedding configuration",
                ));
            }
            if self.mode == RetrievalMode::Hybrid
                && (!self.query_prefix.is_empty() || self.semantic_min_lexical_terms != 0)
            {
                return Err(config_error("Qdrant hybrid does not support query_prefix or semantic_min_lexical_terms; these controls cannot be silently ignored"));
            }
        }
        Ok(())
    }
}
impl AppEmbedding {
    fn validate(&self) -> Result<(), RuntimeError> {
        if !matches!(
            self.provider,
            AppProvider::Openai { .. }
                | AppProvider::Native {
                    protocol: crate::native_providers::NativeProtocol::Gemini
                        | crate::native_providers::NativeProtocol::Vertex,
                    ..
                }
        ) {
            return Err(config_error(
                "embeddings require an explicit OpenAI-compatible, Gemini or Vertex provider",
            ));
        }
        // Reuse endpoint/key-name validation without initializing any service.
        let cfg = AppConfig {
            provider: self.provider.clone(),
            model: self.model.clone(),
            ..Default::default()
        };
        cfg.validate_provider()?;
        if let AppProvider::Openai {
            base_url,
            key_env,
            namespace,
        }
        | AppProvider::Native {
            base_url,
            key_env,
            namespace,
            ..
        } = &self.provider
        {
            if reqwest::Url::parse(base_url)
                .map_err(storage)?
                .query()
                .is_some()
                || base_url.len() > 4096
                || key_env.len() > 256
                || !key_env
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || namespace.len() > 1024
            {
                return Err(config_error("embedding endpoint must omit query credentials; use a named credential environment and bounded namespace"));
            }
        }
        if self.model.trim().is_empty()
            || self.model.len() > 256
            || self.currency.is_empty()
            || self.currency.len() > 8
            || !self.currency.bytes().all(|c| c.is_ascii_uppercase())
            || !(1..=512 * 1024).contains(&self.max_input_bytes)
            || !(1..=120_000).contains(&self.timeout_ms)
            || self.max_request_cost_micros > self.max_total_cost_micros
            || (self.input_micros_per_million_tokens > 0 && self.max_request_cost_micros == 0)
        {
            return Err(config_error(
                "invalid embedding model, price, reservation, cap or request limits",
            ));
        }
        Ok(())
    }
    fn binding(&self) -> Result<String, RuntimeError> {
        fingerprint(&(
            &self.provider,
            &self.model,
            &self.currency,
            self.input_micros_per_million_tokens,
        ))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spend {
    version: u32,
    binding: String,
    settled_micros: u64,
    reserved_or_unknown_micros: u64,
    requests: u64,
    usage_reported_requests: u64,
    overrun: bool,
}
struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn lock(root: &Path, name: &str) -> Result<Lock, RuntimeError> {
    std::fs::create_dir_all(root).map_err(storage)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(name))
        .map_err(storage)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| {
        RuntimeError::Conflict(
            "embedding operation owned by another process; retry explicitly".into(),
        )
    })?;
    Ok(Lock(file))
}
fn spend(root: &Path, cfg: &AppEmbedding) -> Result<Spend, RuntimeError> {
    let record = read_json::<Spend>(&root.join("spend.json"), 8192)?.unwrap_or(Spend::empty(cfg)?);
    record.validate(cfg)?;
    Ok(record)
}
impl Spend {
    fn empty(cfg: &AppEmbedding) -> Result<Self, RuntimeError> {
        Ok(Self {
            version: 1,
            binding: cfg.binding()?,
            ..Default::default()
        })
    }
    fn validate(&self, cfg: &AppEmbedding) -> Result<(), RuntimeError> {
        if self.version != 1
            || self.binding != cfg.binding()?
            || self.usage_reported_requests > self.requests
            || self
                .settled_micros
                .checked_add(self.reserved_or_unknown_micros)
                .is_none()
        {
            return Err(config_error(
                "embedding ledger binding/version is invalid; retain it and migrate explicitly",
            ));
        }
        Ok(())
    }
}
#[derive(Clone)]
enum Resources {
    File(PathBuf),
    #[cfg(feature = "database")]
    Sql {
        objects: crate::runtime::database::SqlObjectStore,
        identity: String,
    },
}
enum BuildGuard {
    File {
        _lock: Lock,
    },
    #[cfg(feature = "database")]
    Sql(crate::runtime::database::SqlObjectLease),
}
impl Resources {
    fn new(config: &AppConfig, stores: &AppStores) -> Result<Self, RuntimeError> {
        #[cfg(feature = "database")]
        if let Some((db, ns)) = &stores.sql {
            return Ok(Self::Sql {
                objects: db.objects(ns.clone()),
                identity: stores.identity().into(),
            });
        }
        let _ = stores;
        Ok(Self::File(root(config)?))
    }
    fn model_identity(&self, cfg: &AppEmbedding) -> Result<String, RuntimeError> {
        Ok(match self {
            Self::File(_) => format!("app-embedding-v1:{}", cfg.binding()?),
            #[cfg(feature = "database")]
            Self::Sql { identity, .. } => format!(
                "app-embedding-sql-v1:{}",
                fingerprint(&(cfg.binding()?, identity))?
            ),
        })
    }
    async fn spend(&self, cfg: &AppEmbedding) -> Result<Spend, RuntimeError> {
        match self {
            Self::File(root) => {
                let root = root.clone();
                let cfg = cfg.clone();
                blocking(move || spend(&root, &cfg)).await
            }
            #[cfg(feature = "database")]
            Self::Sql { objects, .. } => {
                let record = objects
                    .load::<Spend>("embedding-spend")
                    .await?
                    .unwrap_or(Spend::empty(cfg)?);
                record.validate(cfg)?;
                Ok(record)
            }
        }
    }
    async fn update<R: Send + 'static>(
        &self,
        cfg: &AppEmbedding,
        operation: impl FnOnce(&mut Spend) -> Result<R, RuntimeError> + Send + 'static,
    ) -> Result<R, RuntimeError> {
        let cfg = cfg.clone();
        match self {
            Self::File(root) => {
                let root = root.clone();
                blocking(move || {
                    let _lock = lock(&root, "spend.lock")?;
                    let mut record = spend(&root, &cfg)?;
                    let result = operation(&mut record)?;
                    record.validate(&cfg)?;
                    atomic_write(&root.join("spend.json"), &record, 8192)?;
                    Ok(result)
                })
                .await
            }
            #[cfg(feature = "database")]
            Self::Sql { objects, .. } => {
                objects
                    .modify("embedding-spend", Spend::empty(&cfg)?, move |record| {
                        record.validate(&cfg)?;
                        let result = operation(record)?;
                        record.validate(&cfg)?;
                        Ok(result)
                    })
                    .await
            }
        }
    }
    async fn index(&self) -> Result<Option<EmbeddingIndex>, RuntimeError> {
        match self {
            Self::File(root) => {
                let path = root.join("index.json");
                blocking(move || read_json(&path, INDEX_LIMIT)).await
            }
            #[cfg(feature = "database")]
            Self::Sql { objects, .. } => objects.load("embedding-index").await,
        }
    }
    async fn acquire(&self) -> Result<BuildGuard, RuntimeError> {
        match self {
            Self::File(root) => {
                let root = root.clone();
                Ok(BuildGuard::File {
                    _lock: blocking(move || lock(&root, "index.lock")).await?,
                })
            }
            #[cfg(feature = "database")]
            Self::Sql { objects, .. } => {
                Ok(BuildGuard::Sql(objects.acquire("embedding-index").await?))
            }
        }
    }
    async fn save(&self, index: EmbeddingIndex, guard: &BuildGuard) -> Result<(), RuntimeError> {
        match (self, guard) {
            (Self::File(root), BuildGuard::File { .. }) => {
                let path = root.join("index.json");
                blocking(move || atomic_write(&path, &index, INDEX_LIMIT)).await
            }
            #[cfg(feature = "database")]
            (Self::Sql { .. }, BuildGuard::Sql(lease)) => lease.save(&index).await,
            #[cfg(feature = "database")]
            _ => Err(config_error(
                "index build guard differs from storage backend",
            )),
        }
    }
}
struct MeteredEmbedding {
    config: AppEmbedding,
    resources: Resources,
    #[cfg(feature = "database")]
    guard: Option<crate::runtime::database::SqlObjectLease>,
    provider: Arc<dyn crate::Provider>,
}
impl MeteredEmbedding {
    fn new(
        config: AppEmbedding,
        resources: Resources,
        guard: Option<&BuildGuard>,
    ) -> Result<Self, RuntimeError> {
        let _ = guard;
        config.validate()?;
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(storage)?;
        let provider: Arc<dyn crate::Provider> = match &config.provider {
            AppProvider::Openai {
                base_url, key_env, ..
            } => Arc::new(
                crate::openai::OpenAiCompatible::new(base_url, secret(key_env)?)
                    .with_client(client),
            ),
            AppProvider::Native {
                protocol,
                base_url,
                key_env,
                ..
            } => Arc::new(
                crate::native_providers::NativeProvider::new(*protocol, base_url, secret(key_env)?)
                    .map_err(|error| config_error(error.detail()))?,
            ),
            _ => return Err(config_error("unsupported embedding provider")),
        };
        Ok(Self {
            config,
            resources,
            #[cfg(feature = "database")]
            guard: match guard {
                Some(BuildGuard::Sql(lease)) => Some(lease.clone()),
                _ => None,
            },
            provider,
        })
    }
}
/// Explicit private-memory entry point. Reuses the project's independent,
/// durable embedding ledger; constructing it resolves credentials, so static
/// configuration previews and ordinary lexical memory reads must not call it.
#[cfg(feature = "database")]
pub(super) fn semantic_embedding(
    config: &AppConfig,
    stores: &AppStores,
) -> Result<Arc<dyn EmbeddingModel>, RuntimeError> {
    let embedding =
        config.retrieval.embedding.clone().ok_or_else(|| {
            config_error("semantic memory requires explicit embedding configuration")
        })?;
    Ok(Arc::new(MeteredEmbedding::new(
        embedding,
        Resources::new(config, stores)?,
        None,
    )?))
}
impl EmbeddingModel for MeteredEmbedding {
    fn identity(&self) -> String {
        self.resources
            .model_identity(&self.config)
            .expect("serializable embedding configuration")
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
        Box::pin(async move {
            if texts.is_empty()
                || texts.len() > 32
                || texts.iter().any(|s| s.trim().is_empty())
                || texts
                    .iter()
                    .map(String::len)
                    .try_fold(0usize, usize::checked_add)
                    .is_none_or(|n| n > self.config.max_input_bytes)
            {
                return Err(config_error(
                    "embedding inputs exceed configured request limits",
                ));
            }
            let cfg = self.config.clone();
            self.resources
                .update(&self.config, move |record| {
                    let total = record
                        .settled_micros
                        .checked_add(record.reserved_or_unknown_micros)
                        .and_then(|n| n.checked_add(cfg.max_request_cost_micros));
                    if record.overrun || total.is_none_or(|n| n > cfg.max_total_cost_micros) {
                        return Err(config_error(
                            "embedding spend cap exhausted or reported reservation overrun",
                        ));
                    }
                    record.reserved_or_unknown_micros += cfg.max_request_cost_micros;
                    record.requests = record
                        .requests
                        .checked_add(1)
                        .ok_or_else(|| config_error("embedding request counter overflow"))?;
                    Ok(())
                })
                .await?;
            // Once reservation is saved, cancellation, timeout and protocol
            // errors keep it. There is no retry, fallback or silent refund.
            #[cfg(feature = "database")]
            if let Some(guard) = &self.guard {
                guard.verify_ownership().await?;
            }
            let response = tokio::time::timeout(
                Duration::from_millis(self.config.timeout_ms),
                self.provider.embed(&crate::EmbeddingRequest {
                    model: self.config.model.clone(),
                    inputs: texts.to_vec(),
                }),
            )
            .await
            .map_err(|_| config_error("embedding timed out; charge unknown, reservation retained"))?
            .map_err(|e| RuntimeError::Model(crate::TaskError::Provider(e)))?;
            if let Some(tokens) = response.usage.and_then(|usage| usage.prompt_tokens) {
                let actual = (u128::from(tokens)
                    * u128::from(self.config.input_micros_per_million_tokens))
                .div_ceil(1_000_000);
                let actual = u64::try_from(actual)
                    .map_err(|_| config_error("embedding cost overflow; reservation retained"))?;
                let cfg = self.config.clone();
                let overrun = self
                    .resources
                    .update(&self.config, move |record| {
                        record.reserved_or_unknown_micros = record
                            .reserved_or_unknown_micros
                            .checked_sub(cfg.max_request_cost_micros)
                            .ok_or_else(|| config_error("embedding reservation missing"))?;
                        record.settled_micros = record
                            .settled_micros
                            .checked_add(actual)
                            .ok_or_else(|| config_error("embedding ledger overflow"))?;
                        record.usage_reported_requests = record
                            .usage_reported_requests
                            .checked_add(1)
                            .ok_or_else(|| config_error("embedding usage counter overflow"))?;
                        record.overrun |= actual > cfg.max_request_cost_micros;
                        Ok(record.overrun)
                    })
                    .await?;
                if overrun {
                    return Err(config_error(
                        "embedding reported cost exceeded reservation; further dispatch blocked",
                    ));
                }
            }
            Ok(EmbeddingBatch {
                identity: self.identity(),
                vectors: response.vectors,
            })
        })
    }
}
fn root(config: &AppConfig) -> Result<PathBuf, RuntimeError> {
    std::path::absolute(config.state_dir.join("retrieval")).map_err(storage)
}
#[cfg(feature = "qdrant")]
fn qdrant_target(
    config: &AppConfig,
    stores: &AppStores,
) -> Result<rag::QdrantConfig, RuntimeError> {
    let mut target = config
        .retrieval
        .qdrant
        .clone()
        .ok_or_else(|| config_error("Qdrant is not configured by the host"))?;
    target.validate()?;
    target.namespace = format!(
        "app-qdrant-v1:{}",
        fingerprint(&(&target.namespace, stores.identity()))?
    );
    Ok(target)
}
#[cfg(feature = "qdrant")]
fn qdrant_journal(
    config: &AppConfig,
    target: &rag::QdrantConfig,
    index: &EmbeddingIndex,
) -> Result<rag::FileQdrantPublishJournal, RuntimeError> {
    // No caller-selected journal name. Unknown publication for the same frozen
    // target/index cannot be bypassed by silently choosing a new directory.
    rag::FileQdrantPublishJournal::new(
        root(config)?
            .join("qdrant")
            .join(fingerprint(&(target, index))?),
    )
}
#[cfg(feature = "qdrant")]
struct AppPublicationGuard<'a> {
    stores: &'a AppStores,
    resources: Resources,
    guard: &'a BuildGuard,
    corpus: String,
    index: String,
}
#[cfg(feature = "qdrant")]
impl rag::QdrantPublishGuard for AppPublicationGuard<'_> {
    fn verify(&self) -> BoxFuture<'_, Result<(), RuntimeError>> {
        Box::pin(async move {
            let latest =
                self.resources.index().await?.ok_or_else(|| {
                    config_error("local index disappeared during Qdrant publication")
                })?;
            if fingerprint(&latest)? != self.index
                || fingerprint(&self.stores.corpus.snapshot().await?)? != self.corpus
            {
                return Err(RuntimeError::Conflict("corpus/index changed during explicit Qdrant publication; retain original journal".into()));
            }
            #[cfg(feature = "database")]
            if let BuildGuard::Sql(lease) = self.guard {
                lease.verify_ownership().await?;
            }
            #[cfg(not(feature = "database"))]
            let _ = self.guard;
            Ok(())
        })
    }
}
/// Explicit trusted-host publication only. The HTTP Application router exposes
/// no counterpart, and query/startup never calls this operation.
#[cfg(feature = "qdrant")]
pub async fn publish_qdrant_index(
    config: &AppConfig,
    expected_revision: u64,
    expected_retrieval_binding: &str,
    expected_index_binding: &str,
) -> Result<Value, RuntimeError> {
    let stores = AppStores::open(config).await?;
    let result = publish_qdrant_index_with_stores(
        config,
        &stores,
        expected_revision,
        expected_retrieval_binding,
        expected_index_binding,
    )
    .await;
    stores.close().await;
    result
}
#[cfg(feature = "qdrant")]
pub(super) async fn publish_qdrant_index_with_stores(
    config: &AppConfig,
    stores: &AppStores,
    expected_revision: u64,
    expected_retrieval_binding: &str,
    expected_index_binding: &str,
) -> Result<Value, RuntimeError> {
    config.validate()?;
    if fingerprint(&config.retrieval)? != expected_retrieval_binding {
        return Err(RuntimeError::Conflict(
            "retrieval configuration changed before Qdrant publication".into(),
        ));
    }
    let target = qdrant_target(config, stores)?;
    let resources = Resources::new(config, stores)?;
    let guard = resources.acquire().await?;
    let index = resources
        .index()
        .await?
        .ok_or_else(|| config_error("build the local metered index before Qdrant publication"))?;
    let snapshot = stores.corpus.snapshot().await?;
    if snapshot.revision != expected_revision || fingerprint(&index)? != expected_index_binding {
        return Err(RuntimeError::Conflict(
            "corpus revision or exact index binding changed before Qdrant publication".into(),
        ));
    }
    let embedding = config
        .retrieval
        .embedding
        .as_ref()
        .ok_or_else(|| config_error("explicit embedding configuration required"))?;
    index.validate(&snapshot, &resources.model_identity(embedding)?)?;
    if index.document_context != config.retrieval.embedding_document_context {
        return Err(config_error(
            "embedding document context changed; rebuild explicitly",
        ));
    }
    let journal = qdrant_journal(config, &target, &index)?;
    if journal.snapshot().await?.is_some() {
        return Err(RuntimeError::Conflict("Qdrant publication journal already exists for this exact binding; inspect complete/pending/unknown state, no automatic replay".into()));
    }
    let admission = AppPublicationGuard {
        stores,
        resources: resources.clone(),
        guard: &guard,
        corpus: fingerprint(&snapshot)?,
        index: fingerprint(&index)?,
    };
    let client = rag::QdrantClient::new(target.clone())?;
    client
        .publish_guarded(&snapshot, &index, &journal, &admission)
        .await?;
    Ok(
        json!({"corpus_revision":expected_revision,"index_binding":expected_index_binding,"target_binding":target.binding()?,"published_points":index.vectors.len(),"complete":true,"restart_required":true,"billing_boundary":"existing metered embeddings reused; Qdrant service/infrastructure costs are separate from generation and embedding"}),
    )
}
#[cfg(feature = "qdrant")]
async fn qdrant_status(
    config: &AppConfig,
    stores: &AppStores,
    snapshot: &rag::CorpusSnapshot,
    index: Option<&EmbeddingIndex>,
) -> Result<Value, RuntimeError> {
    let target = qdrant_target(config, stores)?;
    let Some(index) = index else {
        return Ok(
            json!({"ready":false,"state":"local_index_missing","target_binding":target.binding()?}),
        );
    };
    let journal = qdrant_journal(config, &target, index)?;
    let retained = journal.snapshot().await?;
    let validation = retained
        .as_ref()
        .map(|state| {
            let embedding = config
                .retrieval
                .embedding
                .as_ref()
                .ok_or_else(|| config_error("embedding configuration missing"))?;
            index.validate(
                snapshot,
                &Resources::new(config, stores)?.model_identity(embedding)?,
            )?;
            if index.document_context != config.retrieval.embedding_document_context {
                return Err(config_error("embedding document context changed"));
            }
            target.validate_publish_snapshot(snapshot, index, state)
        })
        .transpose();
    let (ready, error) = match validation {
        Ok(Some(_)) => (true, None),
        Ok(None) => (false, None),
        Err(error) => (false, Some(error.to_string())),
    };
    Ok(
        json!({"ready":ready,"state":if ready {"complete"} else if retained.is_some() {"incomplete_or_mismatch"} else {"not_published"},"target_binding":target.binding()?,"journal":retained,"error":error,"authority":"cold local exact-plan validation; no credentials, remote request or query admission"}),
    )
}
pub(super) async fn configured(config: &AppConfig) -> Result<Arc<dyn Retriever>, RuntimeError> {
    let stores = AppStores::open(config).await?;
    configured_with_stores(config, &stores).await
}
pub(super) async fn configured_with_stores(
    config: &AppConfig,
    stores: &AppStores,
) -> Result<Arc<dyn Retriever>, RuntimeError> {
    config.retrieval.validate()?;
    let lexical: Arc<dyn Retriever> = Arc::new(ConfiguredLexicalRetriever::new(
        config.retrieval.lexical.clone(),
    )?);
    if config.retrieval.mode == RetrievalMode::Lexical {
        return constrain_identifiers(config, lexical);
    }
    let resources = Resources::new(config, stores)?;
    let index = resources.index().await?.ok_or_else(|| {
        config_error("embedding index missing; explicitly build it in lexical mode first")
    })?;
    if index.document_context != config.retrieval.embedding_document_context {
        return Err(config_error(
            "embedding document context changed; explicitly rebuild the index in lexical mode",
        ));
    }
    let cfg = config
        .retrieval
        .embedding
        .clone()
        .expect("validated hybrid configuration");
    let snapshot = stores.corpus.snapshot().await?;
    index.validate(&snapshot, &resources.model_identity(&cfg)?)?;
    #[cfg(feature = "qdrant")]
    let ready_qdrant = if config.retrieval.qdrant.is_some() {
        let target = qdrant_target(config, stores)?;
        let journal = qdrant_journal(config, &target, &index)?;
        let state = journal.snapshot().await?.ok_or_else(|| {
            config_error(
                "Qdrant index has not been explicitly published; run host index-publish-qdrant",
            )
        })?;
        let receipt = target.validate_publish_snapshot(&snapshot, &index, &state)?;
        Some((target, receipt))
    } else {
        None
    };
    let model = Arc::new(MeteredEmbedding::new(cfg, resources, None)?);
    #[cfg(feature = "qdrant")]
    let vector: Arc<dyn Retriever> = if let Some((target, receipt)) = ready_qdrant {
        Arc::new(rag::QdrantRetriever::new(
            Arc::new(rag::QdrantClient::new(target)?),
            &snapshot,
            Arc::new(index),
            receipt,
            model,
            config.retrieval.semantic_min_score,
        )?)
    } else {
        local_vector(config, index, model, &snapshot)?
    };
    #[cfg(not(feature = "qdrant"))]
    let vector = local_vector(config, index, model, &snapshot)?;
    constrain_identifiers(
        config,
        Arc::new(
            HybridRetriever::new(lexical, vector)
                .with_fusion(
                    config.retrieval.lexical_weight,
                    config.retrieval.semantic_weight,
                    config.retrieval.rank_constant,
                    config.retrieval.candidate_k,
                )?
                .with_semantic_gate(config.retrieval.require_semantic_match)?,
        ),
    )
}
fn local_vector(
    config: &AppConfig,
    index: EmbeddingIndex,
    model: Arc<dyn EmbeddingModel>,
    snapshot: &rag::CorpusSnapshot,
) -> Result<Arc<dyn Retriever>, RuntimeError> {
    Ok(Arc::new(
        VectorRetriever::new(index, model, snapshot)?
            .with_min_score(config.retrieval.semantic_min_score)?
            .with_query_prefix(config.retrieval.query_prefix.clone())?
            .with_lexical_support(
                config.retrieval.lexical.clone(),
                config.retrieval.semantic_min_lexical_terms,
            )?,
    ))
}
fn constrain_identifiers(
    config: &AppConfig,
    inner: Arc<dyn Retriever>,
) -> Result<Arc<dyn Retriever>, RuntimeError> {
    let inner: Arc<dyn Retriever> = if config.retrieval.exact_identifiers {
        Arc::new(IdentifierConstrainedRetriever::new(
            inner,
            config.retrieval.lexical.metadata_keys.clone(),
        )?)
    } else {
        inner
    };
    let inner: Arc<dyn Retriever> = if let Some(rerank) = &config.retrieval.rerank {
        Arc::new(rag::RerankingRetriever::new(
            inner,
            Arc::new(rag::LexicalReranker::new(rerank.lexical.clone())?),
            rerank.candidates,
            rerank.max_candidate_bytes,
        )?)
    } else if let Some(rerank) = &config.retrieval.http_rerank {
        Arc::new(rag::RerankingRetriever::new(
            inner,
            Arc::new(rag::HttpReranker::new(rerank.provider.clone())?),
            rerank.candidates,
            rerank.max_candidate_bytes,
        )?)
    } else {
        inner
    };
    if config.retrieval.max_chunks_per_document != 0 {
        Ok(Arc::new(rag::PerDocumentRetriever::new(
            inner,
            config.retrieval.max_chunks_per_document,
            config.retrieval.candidate_k,
        )?))
    } else {
        Ok(inner)
    }
}

/// Explicit full rebuild/update. Expected revision AND configuration identity
/// prevent a stale UI from approving different source/service parameters.
pub async fn build_index(
    config: &AppConfig,
    expected_revision: u64,
    expected_binding: &str,
) -> Result<Value, RuntimeError> {
    let stores = AppStores::open(config).await?;
    let result =
        build_index_with_stores(config, &stores, expected_revision, expected_binding).await;
    stores.close().await;
    result
}
pub(super) async fn build_index_with_stores(
    config: &AppConfig,
    stores: &AppStores,
    expected_revision: u64,
    expected_binding: &str,
) -> Result<Value, RuntimeError> {
    config.validate()?;
    if expected_binding != fingerprint(&config.retrieval)? {
        return Err(RuntimeError::Conflict(
            "retrieval configuration changed".into(),
        ));
    }
    let cfg = config
        .retrieval
        .embedding
        .clone()
        .ok_or_else(|| config_error("embedding service not configured"))?;
    let resources = Resources::new(config, stores)?;
    let guard = resources.acquire().await?;
    let snapshot = stores.corpus.snapshot().await?;
    if snapshot.revision != expected_revision {
        return Err(RuntimeError::Conflict(
            "corpus revision changed before index build".into(),
        ));
    }
    let model = MeteredEmbedding::new(cfg, resources.clone(), Some(&guard))?;
    // Validate every batch before spending on the first one.
    for batch in snapshot.chunks.chunks(32) {
        if batch
            .iter()
            .map(|c| {
                EmbeddingIndex::document_text(c, config.retrieval.embedding_document_context).len()
            })
            .sum::<usize>()
            > model.config.max_input_bytes
        {
            return Err(config_error(
                "index batch exceeds embedding input limit; adjust chunking/request limit",
            ));
        }
    }
    let index = EmbeddingIndex::build_with_document_context(
        &snapshot,
        &model,
        config.retrieval.embedding_document_context,
    )
    .await?;
    let current = stores.corpus.snapshot().await?;
    if fingerprint(&current)? != fingerprint(&snapshot)? {
        return Err(RuntimeError::Conflict(
            "corpus changed during build; old index retained, charges recorded".into(),
        ));
    }
    let count = index.vectors.len();
    let binding = fingerprint(&index)?;
    resources.save(index, &guard).await?;
    Ok(
        json!({"corpus_revision":expected_revision,"chunks":count,"index_binding":binding,"restart_required":true}),
    )
}
/// Status inspection does not load credentials or contact a provider.
pub async fn index_status(config: &AppConfig) -> Result<Value, RuntimeError> {
    let stores = AppStores::open(config).await?;
    let result = index_status_with_stores(config, &stores).await;
    stores.close().await;
    result
}
pub(super) async fn index_status_with_stores(
    config: &AppConfig,
    stores: &AppStores,
) -> Result<Value, RuntimeError> {
    config.validate()?;
    let resources = Resources::new(config, stores)?;
    let index = resources.index().await?;
    let snapshot = stores.corpus.snapshot().await?;
    let validity = match (&index, &config.retrieval.embedding) {
        (Some(index), Some(cfg))
            if index.document_context == config.retrieval.embedding_document_context =>
        {
            index.validate(&snapshot, &resources.model_identity(cfg)?)
        }
        (Some(_), Some(_)) => Err(config_error(
            "embedding document context changed; explicit rebuild required",
        )),
        _ => Err(config_error("embedding index or configuration missing")),
    };
    let ledger = if let Some(cfg) = config.retrieval.embedding.clone() {
        Some(resources.spend(&cfg).await?)
    } else {
        None
    };
    // Reading spending must not resolve a credential or instantiate transport.
    let rerank_spend = if let Some(cfg) = &config.retrieval.http_rerank {
        let snapshot = rag::FileRerankLedger::new(&cfg.provider.ledger_dir)?
            .snapshot(&cfg.provider)
            .await?;
        Some(json!({"totals":snapshot.totals(),"ledger":snapshot,
            "authority":"separate host-configured filesystem ledger; not project generation quota"}))
    } else {
        None
    };
    let output = json!({"configuration":config.retrieval,"configuration_binding":fingerprint(&config.retrieval)?,
        "corpus_revision":snapshot.revision,"index_present":index.is_some(),"index_valid":validity.is_ok(),
        "index_error":validity.err().map(|e| e.to_string()),"index_binding":index.as_ref().map(fingerprint).transpose()?,
        "embedding_spend":ledger,"rerank_spend":rerank_spend,"billing_boundary":"embedding and reranking have separate ledgers; generation and tool-service costs excluded"});
    #[cfg(feature = "qdrant")]
    let output = if config.retrieval.qdrant.is_some() {
        let mut output = output;
        output["qdrant"] = qdrant_status(config, stores, &snapshot, index.as_ref()).await?;
        output
    } else {
        output
    };
    Ok(output)
}
