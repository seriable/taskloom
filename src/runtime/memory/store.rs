use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;

use super::{MemoryScope, MemorySnapshot};
use crate::runtime::{
    store::{absolute_root, atomic_write, blocking, read_json, storage},
    RuntimeError,
};

/// Trusted persistence primitive; enforce authorization through MemoryManager.
/// save must perform atomic CAS and reject a mismatched scope or revision.
pub trait MemoryStore: Send + Sync {
    fn load<'a>(
        &'a self,
        scope: &'a MemoryScope,
    ) -> BoxFuture<'a, Result<MemorySnapshot, RuntimeError>>;
    fn save<'a>(
        &'a self,
        snapshot: &'a MemorySnapshot,
        expected: u64,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
}
#[derive(Debug, Clone)]
pub struct FileMemoryStore {
    root: PathBuf,
    root_error: Option<String>,
    max_bytes: u64,
}
impl FileMemoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let (root, root_error) = absolute_root(root.into());
        Self {
            root,
            root_error,
            max_bytes: 16 * 1024 * 1024,
        }
    }
    pub fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}
fn read(path: &Path, max: u64, scope: &MemoryScope) -> Result<MemorySnapshot, RuntimeError> {
    let snapshot = read_json::<MemorySnapshot>(path, max)?
        .unwrap_or_else(|| MemorySnapshot::empty(scope.clone()));
    snapshot.validate().map_err(storage)?;
    if &snapshot.scope != scope {
        return Err(storage("memory scope does not match its filename"));
    }
    Ok(snapshot)
}
impl MemoryStore for FileMemoryStore {
    fn load<'a>(
        &'a self,
        scope: &'a MemoryScope,
    ) -> BoxFuture<'a, Result<MemorySnapshot, RuntimeError>> {
        Box::pin(async move {
            if let Some(e) = &self.root_error {
                return Err(storage(e));
            }
            let path = self.root.join(format!("{}.memory.json", scope.identity()?));
            let scope = scope.clone();
            let max = self.max_bytes;
            blocking(move || read(&path, max, &scope)).await
        })
    }
    fn save<'a>(
        &'a self,
        snapshot: &'a MemorySnapshot,
        expected: u64,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            if let Some(e) = &self.root_error {
                return Err(storage(e));
            }
            snapshot.validate()?;
            let admitted = serde_json::to_vec(snapshot).map_err(storage)?.len() as u64;
            // Reserve the worst bounded audit actor/event growth per record.
            // Forgetting removes the larger source/value record and pays down
            // this allowance; a full store must still permit erasure.
            let erasure_reserve = (snapshot.records.len() as u64).saturating_mul(2048);
            if admitted.saturating_add(erasure_reserve) > self.max_bytes {
                return Err(RuntimeError::StorageLimit(
                    "memory snapshot exceeds the configured byte limit".into(),
                ));
            }
            let snapshot = snapshot.clone();
            let root = self.root.clone();
            let max = self.max_bytes;
            blocking(move || {
                std::fs::create_dir_all(&root).map_err(storage)?;
                let key = snapshot.scope.identity()?;
                let lock = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(root.join(format!("{key}.memory.lock")))
                    .map_err(storage)?;
                fs2::FileExt::try_lock_exclusive(&lock).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || matches!(e.raw_os_error(), Some(11 | 33))
                    {
                        RuntimeError::Conflict("memory is owned by another writer".into())
                    } else {
                        storage(e)
                    }
                })?;
                let _lock = MemoryLock(lock);
                let path = root.join(format!("{key}.memory.json"));
                let previous = read(&path, max, &snapshot.scope)?;
                if previous.revision != expected
                    || snapshot.revision
                        != expected
                            .checked_add(1)
                            .ok_or_else(|| storage("memory revision overflow"))?
                    || !snapshot.events.starts_with(&previous.events)
                    || !snapshot
                        .forgotten_sources
                        .is_superset(&previous.forgotten_sources)
                {
                    return Err(RuntimeError::Conflict(
                        "memory revision or immutable audit prefix changed".into(),
                    ));
                }
                atomic_write(&path, &snapshot, max)
            })
            .await
        })
    }
}
struct MemoryLock(File);
impl Drop for MemoryLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
