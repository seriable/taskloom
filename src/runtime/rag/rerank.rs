//! Explicit host-injected reranking without hidden provider calls or source mutation.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::{
    invalid, retrieval::terms, CorpusSnapshot, LexicalConfig, RetrievalQuery, Retriever,
    ScoredChunk,
};
use crate::runtime::{state::fingerprint, RuntimeError};

/// Reorder or filter only the provided validated candidate IDs. Nonnegative,
/// finite scores and unique IDs are required. The host owns any external
/// service credentials, immutable model identity and durable spending admission.
/// This SDK does not silently supply a remote reranking service.
pub trait Reranker: Send + Sync {
    fn identity(&self) -> Option<String> {
        None
    }
    fn rerank<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
        candidates: &'a [ScoredChunk],
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>>;
}

/// Bounded composition: validate the complete backend result, select its first
/// candidates, rerank/filter, validate again and only then take requested top_k.
/// A reranker cannot resurrect a chunk rejected by the backend's host filters.
pub struct RerankingRetriever {
    inner: Arc<dyn Retriever>,
    reranker: Arc<dyn Reranker>,
    candidates: usize,
    max_candidate_bytes: usize,
}
impl RerankingRetriever {
    pub fn new(
        inner: Arc<dyn Retriever>,
        reranker: Arc<dyn Reranker>,
        candidates: usize,
        max_candidate_bytes: usize,
    ) -> Result<Self, RuntimeError> {
        if !(1..=20).contains(&candidates)
            || !(128..=16 * 1024 * 1024).contains(&max_candidate_bytes)
        {
            return Err(invalid("invalid reranking candidate count/byte limits"));
        }
        Ok(Self {
            inner,
            reranker,
            candidates,
            max_candidate_bytes,
        })
    }
}
impl Retriever for RerankingRetriever {
    fn identity(&self) -> Option<String> {
        let inner = self.inner.identity()?;
        let reranker = self.reranker.identity()?;
        Some(format!(
            "reranking-v1:{}",
            fingerprint(&(inner, reranker, self.candidates, self.max_candidate_bytes))
                .expect("serializable reranking policy")
        ))
    }
    fn retrieve<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            query.validate()?;
            snapshot.validate()?;
            if query.top_k > self.candidates {
                return Err(invalid(
                    "requested top_k exceeds configured reranking candidates",
                ));
            }
            let mut widened = query.clone();
            widened.top_k = 20;
            let ranking = self.inner.retrieve(snapshot, &widened).await?;
            let known = snapshot
                .chunks
                .iter()
                .map(|c| (c.id.as_str(), c))
                .collect::<BTreeMap<_, _>>();
            validate_ranking(&ranking, &known.keys().copied().collect(), widened.top_k)?;
            // Check all backend results before reducing the candidates. A corrupt
            // later rank cannot hide behind the candidate or output cutoff.
            let candidates = ranking
                .into_iter()
                .take(self.candidates)
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return Ok(Vec::new());
            }
            let mut bytes = 0usize;
            for item in &candidates {
                let chunk = known[item.chunk_id.as_str()];
                let document = &snapshot.documents[&chunk.document_id];
                bytes = bytes.saturating_add(
                    serde_json::to_vec(&(chunk, &document.metadata))
                        .map_err(|_| invalid("cannot serialize reranking candidate"))?
                        .len(),
                );
                if bytes > self.max_candidate_bytes {
                    return Err(invalid("reranking candidate bytes exceed limit"));
                }
            }
            let eligible = candidates
                .iter()
                .map(|c| c.chunk_id.as_str())
                .collect::<BTreeSet<_>>();
            let mut reranked = self.reranker.rerank(snapshot, query, &candidates).await?;
            validate_ranking(&reranked, &eligible, candidates.len())?;
            reranked.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
            reranked.truncate(query.top_k);
            Ok(reranked)
        })
    }
}
fn validate_ranking(
    ranking: &[ScoredChunk],
    eligible: &BTreeSet<&str>,
    maximum: usize,
) -> Result<(), RuntimeError> {
    let mut seen = BTreeSet::new();
    if ranking.len() > maximum
        || ranking.iter().any(|r| {
            !r.score.is_finite()
                || r.score < 0.0
                || !eligible.contains(r.chunk_id.as_str())
                || !seen.insert(r.chunk_id.as_str())
        })
    {
        return Err(invalid("invalid reranking IDs/scores/candidate binding"));
    }
    Ok(())
}

/// Reproducible local overlap baseline, not a semantic cross-encoder. Uses the
/// same host field weights, allowed metadata, aliases and stop words as lexical
/// retrieval. Scores sum weighted fractions of distinct query terms per field.
/// Zero-overlap candidates are omitted; ties use content/provenance chunk IDs.
#[derive(Debug, Clone, Default)]
pub struct LexicalReranker {
    config: LexicalConfig,
}
impl LexicalReranker {
    pub fn new(config: LexicalConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        Ok(Self { config })
    }
}
impl Reranker for LexicalReranker {
    fn identity(&self) -> Option<String> {
        Some(format!(
            "lexical-rerank-v1:{}",
            fingerprint(&self.config).expect("validated lexical reranker")
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
            let known = snapshot
                .chunks
                .iter()
                .map(|c| (c.id.as_str(), c))
                .collect::<BTreeMap<_, _>>();
            validate_ranking(candidates, &known.keys().copied().collect(), 20)?;
            let query_terms = self.config.query_terms(&query.text)?;
            if query_terms.is_empty() {
                return Ok(Vec::new());
            }
            let score_field = |text: &str| {
                let present = terms(text);
                query_terms
                    .iter()
                    .filter(|term| present.contains_key(*term))
                    .count() as f64
                    / query_terms.len() as f64
            };
            let mut scored = Vec::new();
            for candidate in candidates {
                let chunk = known[candidate.chunk_id.as_str()];
                let metadata = &snapshot.documents[&chunk.document_id].metadata;
                let meta = self
                    .config
                    .metadata_keys
                    .iter()
                    .filter_map(|key| metadata.get(key))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                let score = self.config.body_weight * score_field(&chunk.text)
                    + self.config.title_weight * score_field(&chunk.title)
                    + self.config.metadata_weight * score_field(&meta);
                if score > 0.0 {
                    scored.push(ScoredChunk {
                        chunk_id: chunk.id.clone(),
                        score,
                    });
                }
            }
            scored.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
            Ok(scored)
        })
    }
}
