//! Explicit old/new snapshot update; no persistence, fallback, retry or sync.
use super::{
    check_size, check_vector, fingerprint, invalid, CorpusSnapshot, EmbeddingIndex, EmbeddingModel,
    RuntimeError,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingIndexUpdateReceipt {
    pub version: u32,
    pub old_corpus_binding: String,
    pub new_corpus_binding: String,
    pub old_index_binding: String,
    pub new_index_binding: Option<String>,
    pub model_identity: String,
    pub document_context: bool,
    pub reused: usize,
    pub embedded: usize,
    /// Old vectors not reused, including replaced source chunks.
    pub removed: usize,
    /// All chunks were deleted. Caller must explicitly clear its persisted index.
    pub index_absent: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingIndexUpdate {
    pub index: Option<EmbeddingIndex>,
    pub receipt: EmbeddingIndexUpdateReceipt,
}
impl EmbeddingIndex {
    /// Validate both full snapshots and the old index before the first embedding.
    /// Reuse only exact source/content/context matches; caller must retain the
    /// original old snapshot. This creates inert data, never mutates either input,
    /// never persists or clears a file/collection, and never embeds automatically.
    /// Service admission, charges and unknown outcomes remain the host model's
    /// durable responsibility. Failure/cancellation returns no partial receipt.
    pub async fn update_incremental(
        &self,
        old_snapshot: &CorpusSnapshot,
        new_snapshot: &CorpusSnapshot,
        model: &dyn EmbeddingModel,
    ) -> Result<EmbeddingIndexUpdate, RuntimeError> {
        old_snapshot.validate()?;
        new_snapshot.validate()?;
        let identity = model.identity();
        self.validate(old_snapshot, &identity)?;
        let old_binding = fingerprint(old_snapshot)?;
        let new_binding = fingerprint(new_snapshot)?;
        if new_snapshot.revision < old_snapshot.revision
            || new_snapshot.revision == old_snapshot.revision && new_binding != old_binding
        {
            return Err(invalid("incremental index requires increasing corpus revision or exact unchanged snapshot; no dispatch"));
        }
        if !new_snapshot.chunks.is_empty() {
            // Dimensions are already known: reject an oversized final index
            // before paying for even its first changed batch.
            check_size(self.dimensions, new_snapshot.chunks.len())?;
        }
        let previous = old_snapshot
            .chunks
            .iter()
            .map(|c| (&c.id, c))
            .collect::<BTreeMap<_, _>>();
        let mut vectors = BTreeMap::new();
        let mut changed = Vec::new();
        for chunk in &new_snapshot.chunks {
            let reusable = previous.get(&chunk.id).is_some_and(|old| {
                *old == chunk
                    && Self::document_text(old, self.document_context)
                        == Self::document_text(chunk, self.document_context)
            });
            if reusable {
                let vector = self
                    .vectors
                    .get(&chunk.id)
                    .ok_or_else(|| invalid("validated old index lost a chunk"))?;
                vectors.insert(chunk.id.clone(), vector.clone());
            } else {
                changed.push(chunk);
            }
        }
        let reused = vectors.len();
        let embedded = changed.len();
        for chunks in changed.chunks(32) {
            let texts = chunks
                .iter()
                .map(|c| Self::document_text(c, self.document_context))
                .collect::<Vec<_>>();
            if model.identity() != identity {
                return Err(invalid(
                    "incremental embedding model changed before dispatch",
                ));
            }
            let batch = model.embed(&texts).await?;
            if batch.identity != identity
                || model.identity() != identity
                || batch.vectors.len() != chunks.len()
            {
                return Err(invalid(
                    "incremental embedding response model/count mismatch; no partial index",
                ));
            }
            for (chunk, vector) in chunks.iter().zip(batch.vectors) {
                // Never switch vector spaces or silently rebuild reused vectors.
                check_vector(&vector, self.dimensions)?;
                vectors.insert(chunk.id.clone(), vector);
            }
        }
        if model.identity() != identity {
            return Err(invalid(
                "incremental embedding model changed; no result returned",
            ));
        }
        let index = if new_snapshot.chunks.is_empty() {
            None
        } else {
            let index = Self {
                version: self.version,
                corpus_fingerprint: new_binding.clone(),
                model_identity: identity.clone(),
                dimensions: self.dimensions,
                vectors,
                document_context: self.document_context,
            };
            index.validate(new_snapshot, &identity)?;
            Some(index)
        };
        let receipt = EmbeddingIndexUpdateReceipt {
            version: 1,
            old_corpus_binding: old_binding,
            new_corpus_binding: new_binding,
            old_index_binding: fingerprint(self)?,
            new_index_binding: index.as_ref().map(fingerprint).transpose()?,
            model_identity: identity,
            document_context: self.document_context,
            reused,
            embedded,
            removed: old_snapshot.chunks.len() - reused,
            index_absent: index.is_none(),
        };
        Ok(EmbeddingIndexUpdate { index, receipt })
    }
}
