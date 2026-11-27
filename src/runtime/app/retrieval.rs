//! Explicit local index lifecycle and separately journaled embedding spending.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    config::{config_error, secret},
    AppConfig, AppProvider,
};
use crate::runtime::{
    rag::{
        self, ConfiguredLexicalRetriever, CorpusStore, EmbeddingBatch, EmbeddingIndex,
        EmbeddingModel, HybridRetriever, LexicalConfig, Retriever, VectorRetriever,
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
    pub lexical_weight: f64,
    pub semantic_weight: f64,
    /// Explicitly calibrated cosine cutoff; zero preserves positive-only behavior.
    pub semantic_min_score: f64,
    pub rank_constant: f64,
    pub candidate_k: usize,
}
impl Default for AppRetrieval {
    fn default() -> Self {
        Self {
            mode: RetrievalMode::Lexical,
            lexical: LexicalConfig::default(),
            embedding: None,
            lexical_weight: 1.0,
            semantic_weight: 1.0,
            semantic_min_score: 0.0,
            rank_constant: 60.0,
            candidate_k: 20,
        }
    }
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
        self.lexical.validate()?;
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
        )?;
        if self.mode == RetrievalMode::Hybrid && self.embedding.is_none() {
            return Err(config_error(
                "hybrid retrieval requires embedding configuration",
            ));
        }
        if let Some(embedding) = &self.embedding {
            embedding.validate()?;
        }
        Ok(())
    }
}
impl AppEmbedding {
    fn validate(&self) -> Result<(), RuntimeError> {
        if !matches!(self.provider, AppProvider::Openai { .. }) {
            return Err(config_error(
                "embeddings require an explicit OpenAI-compatible provider",
            ));
        }
        // Reuse endpoint/key-name validation without initializing any service.
        let cfg = AppConfig {
            provider: self.provider.clone(),
            ..Default::default()
        };
        cfg.validate_provider()?;
        if let AppProvider::Openai {
            base_url,
            key_env,
            namespace,
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
    let binding = cfg.binding()?;
    let record = read_json::<Spend>(&root.join("spend.json"), 8192)?.unwrap_or_else(|| Spend {
        version: 1,
        binding: binding.clone(),
        ..Default::default()
    });
    if record.version != 1
        || record.binding != binding
        || record.usage_reported_requests > record.requests
        || record
            .settled_micros
            .checked_add(record.reserved_or_unknown_micros)
            .is_none()
    {
        return Err(config_error(
            "embedding ledger binding/version is invalid; retain it and migrate explicitly",
        ));
    }
    Ok(record)
}
struct MeteredEmbedding {
    config: AppEmbedding,
    root: PathBuf,
    provider: Arc<dyn crate::Provider>,
}
impl MeteredEmbedding {
    fn new(config: AppEmbedding, root: PathBuf) -> Result<Self, RuntimeError> {
        config.validate()?;
        let AppProvider::Openai {
            base_url, key_env, ..
        } = &config.provider
        else {
            unreachable!()
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(storage)?;
        let provider = Arc::new(
            crate::openai::OpenAiCompatible::new(base_url, secret(key_env)?).with_client(client),
        );
        Ok(Self {
            config,
            root,
            provider,
        })
    }
}
impl EmbeddingModel for MeteredEmbedding {
    fn identity(&self) -> String {
        format!(
            "app-embedding-v1:{}",
            self.config
                .binding()
                .expect("serializable embedding configuration")
        )
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
            let root = self.root.clone();
            let cfg = self.config.clone();
            blocking(move || {
                let _lock = lock(&root, "spend.lock")?;
                let mut record = spend(&root, &cfg)?;
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
                atomic_write(&root.join("spend.json"), &record, 8192)
            })
            .await?;
            // Once reservation is saved, cancellation, timeout and protocol
            // errors keep it. There is no retry, fallback or silent refund.
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
                let root = self.root.clone();
                let cfg = self.config.clone();
                blocking(move || {
                    let _lock = lock(&root, "spend.lock")?;
                    let mut record = spend(&root, &cfg)?;
                    record.reserved_or_unknown_micros = record.reserved_or_unknown_micros.checked_sub(cfg.max_request_cost_micros)
                        .ok_or_else(|| config_error("embedding reservation missing"))?;
                    record.settled_micros = record.settled_micros.checked_add(actual)
                        .ok_or_else(|| config_error("embedding ledger overflow"))?;
                    record.usage_reported_requests += 1;
                    record.overrun |= actual > cfg.max_request_cost_micros;
                    atomic_write(&root.join("spend.json"), &record, 8192)?;
                    if record.overrun { return Err(config_error("embedding reported cost exceeded reservation; further dispatch blocked")); }
                    Ok(())
                }).await?;
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
pub(super) async fn configured(config: &AppConfig) -> Result<Arc<dyn Retriever>, RuntimeError> {
    config.retrieval.validate()?;
    let lexical: Arc<dyn Retriever> = Arc::new(ConfiguredLexicalRetriever::new(
        config.retrieval.lexical.clone(),
    )?);
    if config.retrieval.mode == RetrievalMode::Lexical {
        return Ok(lexical);
    }
    let root = root(config)?;
    let corpus = rag::FileCorpusStore::new(
        root.parent()
            .expect("retrieval directory has parent")
            .join("knowledge"),
    );
    let path = root.join("index.json");
    let index: EmbeddingIndex = blocking(move || {
        read_json(&path, INDEX_LIMIT)?.ok_or_else(|| {
            config_error("embedding index missing; explicitly build it in lexical mode first")
        })
    })
    .await?;
    let cfg = config
        .retrieval
        .embedding
        .clone()
        .expect("validated hybrid configuration");
    let model = Arc::new(MeteredEmbedding::new(cfg, root)?);
    let snapshot = corpus.snapshot().await?;
    let vector = Arc::new(
        VectorRetriever::new(index, model, &snapshot)?
            .with_min_score(config.retrieval.semantic_min_score)?,
    );
    Ok(Arc::new(
        HybridRetriever::new(lexical, vector).with_fusion(
            config.retrieval.lexical_weight,
            config.retrieval.semantic_weight,
            config.retrieval.rank_constant,
            config.retrieval.candidate_k,
        )?,
    ))
}

/// Explicit full rebuild/update. Expected revision AND configuration identity
/// prevent a stale UI from approving different source/service parameters.
pub async fn build_index(
    config: &AppConfig,
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
    let root = root(config)?;
    let corpus = rag::FileCorpusStore::new(
        root.parent()
            .expect("retrieval directory has parent")
            .join("knowledge"),
    );
    let lock_root = root.clone();
    let _lock = blocking(move || lock(&lock_root, "index.lock")).await?;
    let snapshot = corpus.snapshot().await?;
    if snapshot.revision != expected_revision {
        return Err(RuntimeError::Conflict(
            "corpus revision changed before index build".into(),
        ));
    }
    let model = MeteredEmbedding::new(cfg, root.clone())?;
    // Validate every batch before spending on the first one.
    for batch in snapshot.chunks.chunks(32) {
        if batch.iter().map(|c| c.text.len()).sum::<usize>() > model.config.max_input_bytes {
            return Err(config_error(
                "index batch exceeds embedding input limit; adjust chunking/request limit",
            ));
        }
    }
    let index = EmbeddingIndex::build(&snapshot, &model).await?;
    let current = corpus.snapshot().await?;
    if fingerprint(&current)? != fingerprint(&snapshot)? {
        return Err(RuntimeError::Conflict(
            "corpus changed during build; old index retained, charges recorded".into(),
        ));
    }
    let count = index.vectors.len();
    let binding = fingerprint(&index)?;
    blocking(move || atomic_write(&root.join("index.json"), &index, INDEX_LIMIT)).await?;
    Ok(
        json!({"corpus_revision":expected_revision,"chunks":count,"index_binding":binding,"restart_required":true}),
    )
}
/// Status inspection does not load credentials or contact a provider.
pub async fn index_status(config: &AppConfig) -> Result<Value, RuntimeError> {
    config.validate()?;
    let root = root(config)?;
    let corpus = rag::FileCorpusStore::new(
        root.parent()
            .expect("retrieval directory has parent")
            .join("knowledge"),
    );
    let path = root.join("index.json");
    let index: Option<EmbeddingIndex> = blocking(move || read_json(&path, INDEX_LIMIT)).await?;
    let snapshot = corpus.snapshot().await?;
    let validity = match (&index, &config.retrieval.embedding) {
        (Some(index), Some(cfg)) => {
            index.validate(&snapshot, &format!("app-embedding-v1:{}", cfg.binding()?))
        }
        _ => Err(config_error("embedding index or configuration missing")),
    };
    let ledger = if let Some(cfg) = config.retrieval.embedding.clone() {
        Some(blocking(move || spend(&root, &cfg)).await?)
    } else {
        None
    };
    Ok(
        json!({"configuration":config.retrieval,"configuration_binding":fingerprint(&config.retrieval)?,
        "corpus_revision":snapshot.revision,"index_present":index.is_some(),"index_valid":validity.is_ok(),
        "index_error":validity.err().map(|e| e.to_string()),"index_binding":index.as_ref().map(fingerprint).transpose()?,
        "embedding_spend":ledger,"billing_boundary":"embedding only; generation and tool-service costs excluded"}),
    )
}
