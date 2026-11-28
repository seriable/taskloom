//! Optional host-owned exact identifier constraints, independent of score scale.
use super::{invalid, retrieval::terms, CorpusSnapshot, RetrievalQuery, Retriever, ScoredChunk};
use crate::runtime::{state::fingerprint, RuntimeError};
use futures_util::future::BoxFuture;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Require every mixed ASCII letter/digit query token of at least eight bytes
/// to occur exactly (case folded) in the candidate body, title or selected
/// metadata. This is a bounded lexical constraint, not semantic verification.
/// Pure words, short model names and pure numbers do not become identifiers.
/// Internal metadata, source paths and document IDs never implicitly match.
pub struct IdentifierConstrainedRetriever {
    inner: Arc<dyn Retriever>,
    metadata_keys: BTreeSet<String>,
}
impl IdentifierConstrainedRetriever {
    pub fn new(
        inner: Arc<dyn Retriever>,
        metadata_keys: BTreeSet<String>,
    ) -> Result<Self, RuntimeError> {
        if metadata_keys.len() > 16
            || metadata_keys
                .iter()
                .any(|k| k.trim().is_empty() || k.len() > 256)
        {
            return Err(invalid("invalid identifier-filter metadata keys"));
        }
        Ok(Self {
            inner,
            metadata_keys,
        })
    }
    /// Detect constraints without contacting a provider. Tokenization matches
    /// local lexical search; punctuation separates components.
    pub fn query_identifiers(text: &str) -> BTreeSet<String> {
        terms(text)
            .into_keys()
            .filter(|term| {
                term.len() >= 8
                    && term.bytes().all(|b| b.is_ascii_alphanumeric())
                    && term.bytes().any(|b| b.is_ascii_alphabetic())
                    && term.bytes().any(|b| b.is_ascii_digit())
            })
            .collect()
    }
}
impl Retriever for IdentifierConstrainedRetriever {
    fn identity(&self) -> Option<String> {
        let inner = self.inner.identity()?;
        Some(format!(
            "identifier-filter-v1:{}",
            fingerprint(&(inner, &self.metadata_keys)).expect("serializable identifier policy")
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
            let required = Self::query_identifiers(&query.text);
            if required.is_empty() {
                return self.inner.retrieve(snapshot, query).await;
            }
            let mut eligible = BTreeSet::new();
            for chunk in &snapshot.chunks {
                let mut present = terms(&chunk.text).into_keys().collect::<BTreeSet<_>>();
                present.extend(terms(&chunk.title).into_keys());
                if let Some(doc) = snapshot.documents.get(&chunk.document_id) {
                    for key in &self.metadata_keys {
                        if let Some(value) = doc.metadata.get(key) {
                            present.extend(terms(value).into_keys());
                        }
                    }
                }
                if required.is_subset(&present) {
                    eligible.insert(chunk.id.as_str());
                }
            }
            // A provably absent exact identifier cannot be repaired by vector
            // similarity. Avoid a separately billed embedding call in this case.
            if eligible.is_empty() {
                return Ok(Vec::new());
            }
            let mut widened = query.clone();
            widened.top_k = 20;
            let ranking = self.inner.retrieve(snapshot, &widened).await?;
            let known = snapshot
                .chunks
                .iter()
                .map(|c| c.id.as_str())
                .collect::<BTreeSet<_>>();
            let mut seen = BTreeSet::new();
            if ranking.len() > widened.top_k
                || ranking.iter().any(|item| {
                    !item.score.is_finite()
                        || item.score < 0.0
                        || !known.contains(item.chunk_id.as_str())
                        || !seen.insert(item.chunk_id.as_str())
                })
            {
                return Err(invalid("invalid identifier-filter backend ranking"));
            }
            // The backend owns scoring/order. Filter before the outer search
            // renders body quotations; no fabricated citation or score exists.
            Ok(ranking
                .into_iter()
                .filter(|item| eligible.contains(item.chunk_id.as_str()))
                .take(query.top_k)
                .collect())
        })
    }
}
