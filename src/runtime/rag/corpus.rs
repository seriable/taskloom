use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::{invalid, validate_chunks, Chunk, Document};
use crate::runtime::{
    store::{absolute_root, atomic_write, blocking, read_json, storage},
    RuntimeError,
};

pub const CORPUS_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusSnapshot {
    pub version: u32,
    pub revision: u64,
    pub documents: BTreeMap<String, Document>,
    pub chunks: Vec<Chunk>,
}
impl Default for CorpusSnapshot {
    fn default() -> Self {
        Self {
            version: CORPUS_VERSION,
            revision: 0,
            documents: BTreeMap::new(),
            chunks: Vec::new(),
        }
    }
}
impl CorpusSnapshot {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.version != CORPUS_VERSION
            || self.documents.len() > 10_000
            || self.chunks.len() > 100_000
        {
            return Err(invalid("unsupported or oversized corpus"));
        }
        for (id, document) in &self.documents {
            document.validate()?;
            if *id != document.id {
                return Err(invalid("document identity differs"));
            }
        }
        let mut ids = BTreeSet::new();
        let mut documents = BTreeSet::new();
        for chunk in &self.chunks {
            let document = self
                .documents
                .get(&chunk.document_id)
                .ok_or_else(|| invalid("chunk has no document"))?;
            chunk.validate(document)?;
            if !ids.insert(&chunk.id) {
                return Err(invalid("duplicate corpus chunk"));
            }
            documents.insert(&chunk.document_id);
        }
        if documents.len() != self.documents.len() {
            return Err(invalid("document has no chunks"));
        }
        Ok(())
    }
}

pub trait CorpusStore: Send + Sync {
    fn snapshot(&self) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>>;
    fn replace(
        &self,
        document: Document,
        chunks: Vec<Chunk>,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>>;
    fn remove<'a>(
        &'a self,
        id: &'a str,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'a, Result<CorpusSnapshot, RuntimeError>>;
}

/// Atomic whole-corpus snapshots for small local collections. Implement
/// CorpusStore/Retriever for a database/vector index at larger scale.
#[derive(Debug, Clone)]
pub struct FileCorpusStore {
    root: PathBuf,
    root_error: Option<String>,
    max_bytes: u64,
}
impl FileCorpusStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let (root, root_error) = absolute_root(root.into());
        Self {
            root,
            root_error,
            max_bytes: 64 * 1024 * 1024,
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }
    async fn update(
        &self,
        operation: impl FnOnce(&mut CorpusSnapshot) -> Result<(), RuntimeError> + Send + 'static,
        expected: Option<u64>,
    ) -> Result<CorpusSnapshot, RuntimeError> {
        if let Some(error) = &self.root_error {
            return Err(storage(error));
        }
        let root = self.root.clone();
        let max = self.max_bytes;
        blocking(move || {
            std::fs::create_dir_all(&root).map_err(storage)?;
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(root.join("corpus.lock"))
                .map_err(storage)?;
            fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || matches!(error.raw_os_error(), Some(11 | 33))
                {
                    RuntimeError::Conflict("corpus is owned by another writer".into())
                } else {
                    storage(error)
                }
            })?;
            let _lock = Unlock(file);
            let path = root.join("corpus.json");
            let mut snapshot = read(&path, max)?;
            if expected.is_some_and(|expected| expected != snapshot.revision) {
                return Err(RuntimeError::Conflict("corpus revision changed".into()));
            }
            operation(&mut snapshot)?;
            snapshot.revision = snapshot
                .revision
                .checked_add(1)
                .ok_or_else(|| storage("corpus revision overflow"))?;
            snapshot.validate()?;
            atomic_write(&path, &snapshot, max)?;
            Ok(snapshot)
        })
        .await
    }
}
struct Unlock(File);
impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
impl CorpusStore for FileCorpusStore {
    fn snapshot(&self) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            let path = self.root.join("corpus.json");
            let max = self.max_bytes;
            blocking(move || read(&path, max)).await
        })
    }
    fn replace(
        &self,
        document: Document,
        chunks: Vec<Chunk>,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>> {
        Box::pin(async move {
            document.validate()?;
            validate_chunks(&document, &chunks)?;
            self.update(
                move |snapshot| {
                    snapshot
                        .chunks
                        .retain(|chunk| chunk.document_id != document.id);
                    snapshot.chunks.extend(chunks);
                    snapshot.documents.insert(document.id.clone(), document);
                    Ok(())
                },
                expected_revision,
            )
            .await
        })
    }
    fn remove<'a>(
        &'a self,
        id: &'a str,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'a, Result<CorpusSnapshot, RuntimeError>> {
        let id = id.to_string();
        Box::pin(self.update(
            move |snapshot| {
                if snapshot.documents.remove(&id).is_none() {
                    return Err(invalid("document does not exist"));
                }
                snapshot.chunks.retain(|chunk| chunk.document_id != id);
                Ok(())
            },
            expected_revision,
        ))
    }
}
fn read(path: &Path, max: u64) -> Result<CorpusSnapshot, RuntimeError> {
    let snapshot = read_json::<CorpusSnapshot>(path, max)?.unwrap_or_default();
    snapshot.validate()?;
    Ok(snapshot)
}
