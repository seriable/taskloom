//! Pluggable ingestion, chunking and retrieval with verifiable source excerpts.
//! Retrieved text is untrusted data, never tool permission or system policy.
mod corpus;
mod hybrid;
#[cfg(feature = "research")]
mod pdf;
mod retrieval;
#[cfg(feature = "research")]
mod web;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::RuntimeError;
pub use corpus::{CorpusSnapshot, CorpusStore, FileCorpusStore, CORPUS_VERSION};
pub use hybrid::{
    EmbeddingBatch, EmbeddingIndex, EmbeddingModel, HybridRetriever, ProviderEmbeddingModel,
    VectorRetriever,
};
#[cfg(feature = "research")]
pub use pdf::{PdfConfig, PdfLoader};
pub use retrieval::{
    search, Citation, CitedReport, ConfiguredLexicalRetriever, Evidence, LexicalConfig,
    LexicalRetriever, QuotePolicy, RagTool, RetrievalQuery, Retriever, ScoredChunk, SearchResult,
    VerifiedReport,
};
#[cfg(feature = "research")]
pub use web::{
    WebAccess, WebClient, WebFetchTool, WebSearch, WebSearchConfig, WebSearchHit, WebSearchTool,
};

pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Stable host-assigned identity; importing it again replaces its chunks.
    pub id: String,
    pub title: String,
    /// A display/source URI. This module never fetches or executes it.
    pub source: String,
    pub text: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}
impl Document {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.id.trim().is_empty()
            || self.id.len() > 512
            || self.title.trim().is_empty()
            || self.title.len() > 4096
            || self.source.trim().is_empty()
            || self.source.len() > 4096
            || self.text.trim().is_empty()
            || self.text.len() > MAX_DOCUMENT_BYTES
            || self.metadata.len() > 64
            || self
                .metadata
                .iter()
                .any(|(k, v)| k.len() > 256 || v.len() > 4096)
        {
            return Err(invalid(
                "invalid document identity, source, text or metadata size",
            ));
        }
        Ok(())
    }
}

/// Host implementations may extract PDF/HTML/etc. Explicitly decide whether
/// network access, scripts or external references are permitted in that loader.
pub trait DocumentLoader: Send + Sync {
    fn load<'a>(
        &'a self,
        path: &'a Path,
        id: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Document, RuntimeError>>;
}

/// Bounded UTF-8 plain-text/Markdown loader. No network or format conversion.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextLoader;
impl DocumentLoader for TextLoader {
    fn load<'a>(
        &'a self,
        path: &'a Path,
        id: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Document, RuntimeError>> {
        Box::pin(async move {
            let path = path.to_path_buf();
            let id = id.to_string();
            let title = title.to_string();
            super::store::blocking(move || {
                use std::io::Read;
                let path = std::fs::canonicalize(path).map_err(super::store::storage)?;
                let file = std::fs::File::open(&path).map_err(super::store::storage)?;
                if !file.metadata().map_err(super::store::storage)?.is_file() {
                    return Err(invalid("source is not a regular file"));
                }
                let mut bytes = Vec::new();
                file.take((MAX_DOCUMENT_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(super::store::storage)?;
                if bytes.len() > MAX_DOCUMENT_BYTES {
                    return Err(invalid("document exceeds byte limit"));
                }
                let document = Document {
                    id,
                    title,
                    source: path.to_string_lossy().into_owned(),
                    text: String::from_utf8(bytes)
                        .map_err(|_| invalid("source is not UTF-8 text"))?,
                    metadata: BTreeMap::new(),
                };
                document.validate()?;
                Ok(document)
            })
            .await
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// Content/provenance hash, never a document's mutable ordinal.
    pub id: String,
    pub document_id: String,
    pub title: String,
    pub source: String,
    /// UTF-8 byte offsets into the original document, end exclusive.
    pub start: usize,
    pub end: usize,
    pub text: String,
}
impl Chunk {
    pub fn from_range(document: &Document, start: usize, end: usize) -> Result<Self, RuntimeError> {
        let text = document
            .text
            .get(start..end)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("invalid UTF-8 chunk range"))?;
        let mut chunk = Self {
            id: String::new(),
            document_id: document.id.clone(),
            title: document.title.clone(),
            source: document.source.clone(),
            start,
            end,
            text: text.into(),
        };
        chunk.id = chunk.fingerprint()?;
        Ok(chunk)
    }
    fn fingerprint(&self) -> Result<String, RuntimeError> {
        let bytes = serde_json::to_vec(&(
            &self.document_id,
            &self.title,
            &self.source,
            self.start,
            self.end,
            &self.text,
        ))
        .map_err(super::store::storage)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
    pub fn validate(&self, document: &Document) -> Result<(), RuntimeError> {
        if self.document_id != document.id
            || self.title != document.title
            || self.source != document.source
            || self.text.is_empty()
            || document.text.get(self.start..self.end) != Some(self.text.as_str())
            || self.id != self.fingerprint()?
        {
            return Err(invalid("chunk differs from its source range/provenance"));
        }
        Ok(())
    }
}

pub trait Chunker: Send + Sync {
    fn split(&self, document: &Document) -> Result<Vec<Chunk>, RuntimeError>;
}

/// Character-count windows preserve exact text and UTF-8 byte provenance.
#[derive(Debug, Clone, Copy)]
pub struct WindowChunker {
    pub max_chars: usize,
    pub overlap_chars: usize,
}
impl Default for WindowChunker {
    fn default() -> Self {
        Self {
            max_chars: 1200,
            overlap_chars: 150,
        }
    }
}
impl Chunker for WindowChunker {
    fn split(&self, document: &Document) -> Result<Vec<Chunk>, RuntimeError> {
        document.validate()?;
        if self.max_chars == 0 || self.max_chars > 64 * 1024 || self.overlap_chars >= self.max_chars
        {
            return Err(invalid(
                "chunk window must be 1..=65536 chars with smaller overlap",
            ));
        }
        let offsets = document
            .text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(document.text.len()))
            .collect::<Vec<_>>();
        let total = offsets.len() - 1;
        let count = total.div_ceil(self.max_chars - self.overlap_chars);
        if count > 100_000 {
            return Err(invalid("too many document chunks"));
        }
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < total {
            let end = (start + self.max_chars).min(total);
            chunks.push(Chunk::from_range(document, offsets[start], offsets[end])?);
            if end == total {
                break;
            }
            start = end - self.overlap_chars;
        }
        Ok(chunks)
    }
}

/// Validate plugin chunks before committing any change to a corpus.
pub async fn ingest(
    store: &dyn CorpusStore,
    document: Document,
    chunker: &dyn Chunker,
    expected_revision: Option<u64>,
) -> Result<CorpusSnapshot, RuntimeError> {
    document.validate()?;
    let chunks = chunker.split(&document)?;
    validate_chunks(&document, &chunks)?;
    store.replace(document, chunks, expected_revision).await
}

fn validate_chunks(document: &Document, chunks: &[Chunk]) -> Result<(), RuntimeError> {
    if chunks.is_empty() || chunks.len() > 100_000 {
        return Err(invalid("empty or oversized chunk set"));
    }
    let mut ids = BTreeSet::new();
    for chunk in chunks {
        chunk.validate(document)?;
        if !ids.insert(&chunk.id) {
            return Err(invalid("duplicate chunk ID"));
        }
    }
    Ok(())
}
fn invalid(detail: &str) -> RuntimeError {
    RuntimeError::Protocol(format!("knowledge: {detail}"))
}
