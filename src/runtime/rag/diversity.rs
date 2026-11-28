//! Explicit bounded source diversity, after complete backend validation.
use super::{invalid, CorpusSnapshot, RetrievalQuery, Retriever, ScoredChunk};
use crate::runtime::state::fingerprint;
use futures_util::future::BoxFuture;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Limits how many chunks one document can occupy in the final result. The
/// host explicitly chooses a wider candidate pool; this may increase the
/// configured backend's work. Sources, scores and relative order are preserved.
pub struct PerDocumentRetriever {
    inner: Arc<dyn Retriever>,
    max_per_document: usize,
    candidates: usize,
}
impl PerDocumentRetriever {
    pub fn new(
        inner: Arc<dyn Retriever>,
        max_per_document: usize,
        candidates: usize,
    ) -> Result<Self, super::RuntimeError> {
        if !(1..=20).contains(&candidates) || !(1..=candidates).contains(&max_per_document) {
            return Err(invalid(
                "source diversity requires 1..20 candidates and a bounded per-document cap",
            ));
        }
        Ok(Self {
            inner,
            max_per_document,
            candidates,
        })
    }
}
impl Retriever for PerDocumentRetriever {
    fn identity(&self) -> Option<String> {
        Some(format!(
            "per-document-v1:{}",
            fingerprint(&(
                self.inner.identity()?,
                self.max_per_document,
                self.candidates
            ))
            .expect("serializable diversity policy")
        ))
    }
    fn retrieve<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, super::RuntimeError>> {
        Box::pin(async move {
            snapshot.validate()?;
            query.validate()?;
            if query.top_k > self.candidates {
                return Err(invalid(
                    "requested top_k exceeds source-diversity candidate pool",
                ));
            }
            let mut widened = query.clone();
            widened.top_k = self.candidates;
            let ranking = self.inner.retrieve(snapshot, &widened).await?;
            let known = snapshot
                .chunks
                .iter()
                .map(|chunk| (chunk.id.as_str(), chunk.document_id.as_str()))
                .collect::<BTreeMap<_, _>>();
            let mut seen = BTreeSet::new();
            if ranking.len() > self.candidates
                || ranking.iter().any(|hit| {
                    !hit.score.is_finite()
                        || hit.score < 0.0
                        || !known.contains_key(hit.chunk_id.as_str())
                        || !seen.insert(hit.chunk_id.as_str())
                })
            {
                return Err(invalid(
                    "invalid complete backend ranking before source diversity",
                ));
            }
            let mut counts = BTreeMap::new();
            let mut selected = Vec::new();
            for hit in ranking {
                let count = counts.entry(known[hit.chunk_id.as_str()]).or_insert(0usize);
                if *count < self.max_per_document {
                    *count += 1;
                    selected.push(hit);
                    if selected.len() == query.top_k {
                        break;
                    }
                }
            }
            Ok(selected)
        })
    }
}
