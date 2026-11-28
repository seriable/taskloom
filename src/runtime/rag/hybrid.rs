use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::{invalid, CorpusSnapshot, RetrievalQuery, Retriever, ScoredChunk};
use crate::runtime::{state::fingerprint, RuntimeError};

/// Host-supplied embeddings. Identity must bind model/version, endpoint and
/// tenant, without credentials. Charges belong to this service, not RunBudget.
/// There is deliberately no automatic network service or synthetic embedding.
pub trait EmbeddingModel: Send + Sync {
    fn identity(&self) -> String;
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>>;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatch {
    pub identity: String,
    /// Exactly one vector per input, in input order.
    pub vectors: Vec<Vec<f32>>,
}
/// One embedding request through an existing provider adapter, without retry
/// or model fallback. Namespace is a host-declared tenant/version identity.
/// Configure billing at the service boundary; these calls are not charged to
/// an agent's generation RunBudget. Timeouts may still incur service charges.
pub struct ProviderEmbeddingModel {
    provider: Arc<dyn crate::Provider>,
    model: String,
    namespace: String,
    max_input_bytes: usize,
    timeout: std::time::Duration,
}
impl ProviderEmbeddingModel {
    pub fn new(
        provider: Arc<dyn crate::Provider>,
        model: impl Into<String>,
        namespace: impl Into<String>,
        max_input_bytes: usize,
        timeout: std::time::Duration,
    ) -> Result<Self, RuntimeError> {
        let model = model.into();
        let namespace = namespace.into();
        if model.trim().is_empty()
            || model.len() > 256
            || namespace.trim().is_empty()
            || namespace.len() > 1024
            || !(1..=512 * 1024).contains(&max_input_bytes)
            || timeout.is_zero()
            || timeout > std::time::Duration::from_secs(120)
        {
            return Err(invalid("invalid embedding provider configuration"));
        }
        Ok(Self {
            provider,
            model,
            namespace,
            max_input_bytes,
            timeout,
        })
    }
}
impl EmbeddingModel for ProviderEmbeddingModel {
    fn identity(&self) -> String {
        format!(
            "provider-embedding-v1:{}",
            fingerprint(&(
                self.provider.cache_namespace(),
                &self.model,
                &self.namespace,
                self.max_input_bytes,
                self.timeout
            ))
            .expect("serializable embedding binding")
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
                    .is_none_or(|n| n > self.max_input_bytes)
            {
                return Err(invalid("embedding inputs exceed configured limits"));
            }
            let identity = self.identity();
            let request = crate::EmbeddingRequest {
                model: self.model.clone(),
                inputs: texts.to_vec(),
            };
            let response = tokio::time::timeout(self.timeout, self.provider.embed(&request))
                .await
                .map_err(|_| invalid("embedding request timed out; service charge may be unknown"))?
                .map_err(|e| RuntimeError::Model(crate::TaskError::Provider(e)))?;
            if self.identity() != identity {
                return Err(invalid(
                    "embedding provider configuration changed during request",
                ));
            }
            Ok(EmbeddingBatch {
                identity,
                vectors: response.vectors,
            })
        })
    }
}
/// Serializable small-corpus index, bound to the complete immutable snapshot.
/// Persist atomically in the host; a stale index fails rather than mixing
/// old vectors with new source text. Format changes require a new version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingIndex {
    pub version: u32,
    pub corpus_fingerprint: String,
    pub model_identity: String,
    pub dimensions: usize,
    pub vectors: BTreeMap<String, Vec<f32>>,
}
impl EmbeddingIndex {
    /// Explicitly embeds all chunks in bounded batches. No background rebuild
    /// or retry occurs, so callers can manage ingestion/service spending.
    pub async fn build(
        snapshot: &CorpusSnapshot,
        model: &dyn EmbeddingModel,
    ) -> Result<Self, RuntimeError> {
        snapshot.validate()?;
        if snapshot.chunks.is_empty() || snapshot.chunks.len() > 10_000 {
            return Err(invalid("vector index requires 1..=10000 chunks"));
        }
        let identity = model.identity();
        if identity.trim().is_empty() || identity.len() > 4096 {
            return Err(invalid("invalid embedding model identity"));
        }
        let mut index = Self {
            version: 1,
            corpus_fingerprint: fingerprint(snapshot)?,
            model_identity: identity,
            dimensions: 0,
            vectors: BTreeMap::new(),
        };
        for chunks in snapshot.chunks.chunks(32) {
            let texts = chunks.iter().map(|c| c.text.clone()).collect::<Vec<_>>();
            let batch = model.embed(&texts).await?;
            if batch.identity != index.model_identity
                || model.identity() != index.model_identity
                || batch.vectors.len() != chunks.len()
            {
                return Err(invalid("embedding response identity or count mismatch"));
            }
            if index.dimensions == 0 {
                index.dimensions = batch.vectors.first().map_or(0, Vec::len);
            }
            check_size(index.dimensions, snapshot.chunks.len())?;
            for (chunk, vector) in chunks.iter().zip(batch.vectors) {
                check_vector(&vector, index.dimensions)?;
                index.vectors.insert(chunk.id.clone(), vector);
            }
        }
        index.validate(snapshot, &model.identity())?;
        Ok(index)
    }
    pub fn validate(&self, snapshot: &CorpusSnapshot, identity: &str) -> Result<(), RuntimeError> {
        snapshot.validate()?;
        check_size(self.dimensions, self.vectors.len())?;
        if self.version != 1
            || identity.trim().is_empty()
            || identity.len() > 4096
            || self.model_identity != identity
            || self.corpus_fingerprint != fingerprint(snapshot)?
            || self.vectors.len() != snapshot.chunks.len()
            || snapshot
                .chunks
                .iter()
                .any(|c| !self.vectors.contains_key(&c.id))
        {
            return Err(invalid(
                "vector index version, source snapshot or model identity changed",
            ));
        }
        for vector in self.vectors.values() {
            check_vector(vector, self.dimensions)?;
        }
        Ok(())
    }
}
fn check_size(dimensions: usize, count: usize) -> Result<(), RuntimeError> {
    if dimensions == 0
        || dimensions > 4096
        || count == 0
        || count > 10_000
        || dimensions.saturating_mul(count) > 4_000_000
    {
        return Err(invalid("vector index exceeds dimension/chunk/value limits"));
    }
    Ok(())
}
fn check_vector(vector: &[f32], dimensions: usize) -> Result<(), RuntimeError> {
    if vector.len() != dimensions || vector.iter().any(|n| !n.is_finite()) || norm(vector) == 0.0 {
        return Err(invalid(
            "embedding vector has wrong dimensions, nonfinite values or zero norm",
        ));
    }
    Ok(())
}
fn norm(vector: &[f32]) -> f64 {
    vector
        .iter()
        .map(|n| f64::from(*n).powi(2))
        .sum::<f64>()
        .sqrt()
}

pub struct VectorRetriever {
    index: EmbeddingIndex,
    model: Arc<dyn EmbeddingModel>,
    binding: String,
    min_score: f64,
    query_prefix: String,
}
impl VectorRetriever {
    pub fn new(
        index: EmbeddingIndex,
        model: Arc<dyn EmbeddingModel>,
        snapshot: &CorpusSnapshot,
    ) -> Result<Self, RuntimeError> {
        index.validate(snapshot, &model.identity())?;
        let binding = fingerprint(&index)?;
        Ok(Self {
            index,
            model,
            binding,
            min_score: 0.0,
            query_prefix: String::new(),
        })
    }
    /// Host-calibrated minimum cosine similarity, in 0..=1. Positive
    /// similarity alone is not a relevance judgment. This filter does not
    /// assert factual support and does not remove lexical candidates in fusion.
    pub fn with_min_score(mut self, minimum: f64) -> Result<Self, RuntimeError> {
        if !minimum.is_finite() || !(0.0..=1.0).contains(&minimum) {
            return Err(invalid(
                "minimum vector similarity must be finite and in 0..=1",
            ));
        }
        self.min_score = minimum;
        Ok(self)
    }
    /// Explicit host/publisher query formatting; document vectors are unchanged.
    /// The prefix is embedding input data and cannot grant tools or authority.
    pub fn with_query_prefix(mut self, prefix: impl Into<String>) -> Result<Self, RuntimeError> {
        let prefix = prefix.into();
        if prefix.len() > 4096 || prefix.contains('\0') {
            return Err(invalid(
                "embedding query prefix exceeds4096 bytes or contains NUL",
            ));
        }
        self.query_prefix = prefix;
        Ok(self)
    }
}
impl Retriever for VectorRetriever {
    fn identity(&self) -> Option<String> {
        let legacy = if self.min_score == 0.0 {
            format!("vector-v1:{}", self.binding)
        } else {
            format!(
                "vector-v2:{}",
                fingerprint(&(&self.binding, self.min_score))
                    .expect("validated vector configuration")
            )
        };
        Some(if self.query_prefix.is_empty() {
            legacy
        } else {
            format!(
                "vector-query-prefix-v1:{}",
                fingerprint(&(&legacy, &self.query_prefix)).expect("bounded query prefix")
            )
        })
    }
    fn retrieve<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            query.validate()?;
            self.index.validate(snapshot, &self.model.identity())?;
            let text = format!("{}{}", self.query_prefix, query.text);
            let batch = self.model.embed(std::slice::from_ref(&text)).await?;
            if batch.identity != self.index.model_identity
                || self.model.identity() != self.index.model_identity
                || batch.vectors.len() != 1
            {
                return Err(invalid("query embedding identity/count mismatch"));
            }
            let vector = &batch.vectors[0];
            check_vector(vector, self.index.dimensions)?;
            let query_norm = norm(vector);
            let mut ranked = self
                .index
                .vectors
                .iter()
                .filter_map(|(id, v)| {
                    let dot = vector
                        .iter()
                        .zip(v)
                        .map(|(a, b)| f64::from(*a) * f64::from(*b))
                        .sum::<f64>();
                    let score = (dot / (query_norm * norm(v))).clamp(-1.0, 1.0);
                    (score > 0.0 && score >= self.min_score).then(|| ScoredChunk {
                        chunk_id: id.clone(),
                        score,
                    })
                })
                .collect::<Vec<_>>();
            rank(&mut ranked);
            ranked.truncate(query.top_k);
            Ok(ranked)
        })
    }
}

/// Weighted reciprocal-rank fusion. Backend scores are deliberately not
/// compared across scales. Each input ranking is validated before fusion;
/// a service/index failure is surfaced, never silently replaced with lexical.
pub struct HybridRetriever {
    lexical: Arc<dyn Retriever>,
    semantic: Arc<dyn Retriever>,
    lexical_weight: f64,
    semantic_weight: f64,
    rank_constant: f64,
    candidate_k: usize,
    require_semantic_match: bool,
}
impl HybridRetriever {
    pub fn new(lexical: Arc<dyn Retriever>, semantic: Arc<dyn Retriever>) -> Self {
        Self {
            lexical,
            semantic,
            lexical_weight: 1.0,
            semantic_weight: 1.0,
            rank_constant: 60.0,
            candidate_k: 20,
            require_semantic_match: false,
        }
    }
    pub fn with_fusion(
        mut self,
        lexical_weight: f64,
        semantic_weight: f64,
        rank_constant: f64,
        candidate_k: usize,
    ) -> Result<Self, RuntimeError> {
        if !lexical_weight.is_finite()
            || !semantic_weight.is_finite()
            || !rank_constant.is_finite()
            || !(0.0..=100.0).contains(&lexical_weight)
            || !(0.0..=100.0).contains(&semantic_weight)
            || lexical_weight + semantic_weight <= 0.0
            || !(1.0..=1000.0).contains(&rank_constant)
            || !(1..=20).contains(&candidate_k)
            || (self.require_semantic_match && semantic_weight == 0.0)
        {
            return Err(invalid("invalid hybrid fusion limits/weights"));
        }
        self.lexical_weight = lexical_weight;
        self.semantic_weight = semantic_weight;
        self.rank_constant = rank_constant;
        self.candidate_k = candidate_k;
        Ok(self)
    }
    /// Require admission by the semantic lane before either lane contributes
    /// to fusion. The semantic backend owns its explicit cutoff. This is
    /// bounded candidate admission, not source entailment verification.
    pub fn with_semantic_gate(mut self, required: bool) -> Result<Self, RuntimeError> {
        if required && self.semantic_weight == 0.0 {
            return Err(invalid(
                "semantic admission requires an enabled semantic lane",
            ));
        }
        self.require_semantic_match = required;
        Ok(self)
    }
}
impl Retriever for HybridRetriever {
    fn identity(&self) -> Option<String> {
        let lexical_identity = self.lexical.identity();
        let semantic_identity = self.semantic.identity();
        if (self.lexical_weight > 0.0 && lexical_identity.is_none())
            || (self.semantic_weight > 0.0 && semantic_identity.is_none())
        {
            return None;
        }
        let legacy = format!(
            "hybrid-v1:{}",
            fingerprint(&(
                lexical_identity,
                semantic_identity,
                self.lexical_weight,
                self.semantic_weight,
                self.rank_constant,
                self.candidate_k
            ))
            .expect("finite fusion configuration")
        );
        Some(if self.require_semantic_match {
            format!("hybrid-semantic-gate-v1:{legacy}")
        } else {
            legacy
        })
    }
    fn retrieve<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            query.validate()?;
            snapshot.validate()?;
            let mut candidates = query.clone();
            candidates.top_k = self.candidate_k.max(query.top_k);
            let (lexical, semantic) = futures_util::future::try_join(
                retrieve_if(
                    self.lexical.as_ref(),
                    snapshot,
                    &candidates,
                    self.lexical_weight,
                ),
                retrieve_if(
                    self.semantic.as_ref(),
                    snapshot,
                    &candidates,
                    self.semantic_weight,
                ),
            )
            .await?;
            let admitted = semantic
                .iter()
                .map(|item| item.chunk_id.clone())
                .collect::<BTreeSet<_>>();
            let known = snapshot
                .chunks
                .iter()
                .map(|c| c.id.as_str())
                .collect::<BTreeSet<_>>();
            let mut fused = BTreeMap::<String, f64>::new();
            for (mut ranking, weight) in [
                (lexical, self.lexical_weight),
                (semantic, self.semantic_weight),
            ] {
                let mut ids = BTreeSet::new();
                if ranking.len() > candidates.top_k
                    || ranking.iter().any(|v| {
                        !v.score.is_finite()
                            || v.score < 0.0
                            || !known.contains(v.chunk_id.as_str())
                            || !ids.insert(v.chunk_id.clone())
                    })
                {
                    return Err(invalid("invalid hybrid backend ranking"));
                }
                rank(&mut ranking);
                for (index, item) in ranking.into_iter().enumerate() {
                    if self.require_semantic_match && !admitted.contains(&item.chunk_id) {
                        continue;
                    }
                    *fused.entry(item.chunk_id).or_default() +=
                        weight / (self.rank_constant + (index + 1) as f64);
                }
            }
            let mut ranked = fused
                .into_iter()
                .map(|(chunk_id, score)| ScoredChunk { chunk_id, score })
                .collect::<Vec<_>>();
            rank(&mut ranked);
            ranked.truncate(query.top_k);
            Ok(ranked)
        })
    }
}
fn rank(ranked: &mut [ScoredChunk]) {
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
}
async fn retrieve_if(
    backend: &dyn Retriever,
    snapshot: &CorpusSnapshot,
    query: &RetrievalQuery,
    weight: f64,
) -> Result<Vec<ScoredChunk>, RuntimeError> {
    if weight == 0.0 {
        Ok(Vec::new())
    } else {
        backend.retrieve(snapshot, query).await
    }
}
