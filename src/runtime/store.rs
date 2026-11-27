use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{BudgetSnapshot, RunState, RuntimeError};

/// Exclusive ownership is held for the entire invocation, including all
/// asynchronous checkpoint writes. Implementations must provide equivalent
/// exclusion and atomic, monotonic replacement semantics.
pub trait RunLease: Send + Sync {
    fn load(&self) -> BoxFuture<'_, Result<Option<RunState>, RuntimeError>>;
    fn save<'a>(&'a self, state: &'a RunState) -> BoxFuture<'a, Result<(), RuntimeError>>;
    fn load_budget(&self) -> BoxFuture<'_, Result<Option<BudgetSnapshot>, RuntimeError>>;
    fn save_budget<'a>(
        &'a self,
        snapshot: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
}

pub trait RunStore: Send + Sync {
    fn acquire<'a>(
        &'a self,
        run_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RunLease>, RuntimeError>>;
    /// Atomic read without ownership, for status inspection while a worker is
    /// active. The result is the last committed checkpoint, not live memory.
    fn inspect<'a>(
        &'a self,
        run_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RunState>, RuntimeError>>;
}

/// Local filesystem store with persistent lock files, per-run OS locks,
/// flushed temporary files and atomic replacement. Hashes keep all file names
/// within the trusted root even when an external run id contains path syntax.
#[derive(Debug, Clone)]
pub struct FileRunStore {
    root: PathBuf,
    root_error: Option<String>,
    max_bytes: u64,
}

impl FileRunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let (root, root_error) = absolute_root(root.into());
        Self {
            root,
            root_error,
            max_bytes: 64 * 1024 * 1024,
        }
    }
    pub fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn key(run_id: &str) -> Result<String, RuntimeError> {
        if run_id.trim().is_empty() {
            return Err(RuntimeError::Configuration(
                "run id must be nonempty".into(),
            ));
        }
        Ok(hex::encode(Sha256::digest(run_id.as_bytes())))
    }
}

impl RunStore for FileRunStore {
    fn acquire<'a>(
        &'a self,
        run_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RunLease>, RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            let key = Self::key(run_id)?;
            // Bind this lease to an absolute path before crossing an await;
            // a host tool changing process cwd cannot redirect its writes.
            let root = std::path::absolute(&self.root).map_err(storage)?;
            let run_id = run_id.to_string();
            let max_bytes = self.max_bytes;
            blocking(move || {
                std::fs::create_dir_all(&root).map_err(storage)?;
                let lock = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(root.join(format!("{key}.lock")))
                    .map_err(storage)?;
                fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || matches!(error.raw_os_error(), Some(33 | 11))
                    {
                        RuntimeError::Conflict(format!("run is owned by another worker: {run_id}"))
                    } else {
                        storage(error)
                    }
                })?;
                let lease = FileLease {
                    inner: Arc::new(LeaseInner {
                        root,
                        key,
                        run_id,
                        lock,
                        writes: Mutex::new(()),
                        max_bytes,
                    }),
                };
                Ok(Arc::new(lease) as Arc<dyn RunLease>)
            })
            .await
        })
    }

    fn inspect<'a>(
        &'a self,
        run_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RunState>, RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            let path = std::path::absolute(&self.root)
                .map_err(storage)?
                .join(format!("{}.json", Self::key(run_id)?));
            let max_bytes = self.max_bytes;
            let run_id = run_id.to_string();
            blocking(move || read_state(&path, max_bytes, &run_id)).await
        })
    }
}

struct LeaseInner {
    root: PathBuf,
    key: String,
    run_id: String,
    lock: File,
    writes: Mutex<()>,
    max_bytes: u64,
}

struct FileLease {
    inner: Arc<LeaseInner>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        // Closing one descriptor is insufficient when a fork/dup still holds
        // the same open file description. No writer can remain once the final
        // Arc is dropped; release explicitly before closing our descriptor.
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

#[derive(Serialize, Deserialize)]
struct BudgetRecord {
    version: u32,
    run_id: String,
    snapshot: BudgetSnapshot,
}

fn read_budget(inner: &LeaseInner, path: &Path) -> Result<Option<BudgetSnapshot>, RuntimeError> {
    let record = read_json::<BudgetRecord>(path, inner.max_bytes)?;
    match record {
        Some(record) if record.version != 1 || record.run_id != inner.run_id => Err(
            RuntimeError::Storage("budget journal version or run id does not match".into()),
        ),
        Some(record) => Ok(Some(record.snapshot)),
        None => Ok(None),
    }
}

impl RunLease for FileLease {
    fn load(&self) -> BoxFuture<'_, Result<Option<RunState>, RuntimeError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(blocking(move || {
            read_state(
                &inner.root.join(format!("{}.json", inner.key)),
                inner.max_bytes,
                &inner.run_id,
            )
        }))
    }

    fn save<'a>(&'a self, state: &'a RunState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        let state = state.clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(blocking(move || {
            state.validate()?;
            if state.report.run_id != inner.run_id {
                return Err(RuntimeError::Storage(
                    "checkpoint run id does not match lease".into(),
                ));
            }
            let _guard = inner
                .writes
                .lock()
                .map_err(|_| RuntimeError::Storage("checkpoint writer panicked".into()))?;
            let path = inner.root.join(format!("{}.json", inner.key));
            if let Some(previous) = read_state(&path, inner.max_bytes, &inner.run_id)? {
                if previous.revision > state.revision {
                    return Ok(());
                }
                if previous.revision == state.revision {
                    if serde_json::to_value(previous).map_err(storage)?
                        == serde_json::to_value(&state).map_err(storage)?
                    {
                        return Ok(());
                    }
                    return Err(RuntimeError::Storage(
                        "conflicting checkpoint with equal revision".into(),
                    ));
                }
            }
            atomic_write(&path, &state, inner.max_bytes)
        }))
    }

    fn load_budget(&self) -> BoxFuture<'_, Result<Option<BudgetSnapshot>, RuntimeError>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(blocking(move || {
            read_budget(
                &inner,
                &inner.root.join(format!("{}.budget.json", inner.key)),
            )
        }))
    }

    fn save_budget<'a>(
        &'a self,
        snapshot: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        let snapshot = snapshot.clone();
        let inner = Arc::clone(&self.inner);
        Box::pin(blocking(move || {
            let _guard = inner
                .writes
                .lock()
                .map_err(|_| RuntimeError::Storage("budget writer panicked".into()))?;
            let path = inner.root.join(format!("{}.budget.json", inner.key));
            if let Some(previous) = read_budget(&inner, &path)? {
                if previous.generation > snapshot.generation {
                    return Ok(());
                }
                if previous.generation == snapshot.generation {
                    return if previous == snapshot {
                        Ok(())
                    } else {
                        Err(RuntimeError::Storage(
                            "conflicting budget with equal generation".into(),
                        ))
                    };
                }
            }
            atomic_write(
                &path,
                &BudgetRecord {
                    version: 1,
                    run_id: inner.run_id.clone(),
                    snapshot,
                },
                inner.max_bytes,
            )
        }))
    }
}

pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, RuntimeError> + Send + 'static,
) -> Result<T, RuntimeError> {
    tokio::task::spawn_blocking(work).await.map_err(storage)?
}

fn read_state(path: &Path, max_bytes: u64, run_id: &str) -> Result<Option<RunState>, RuntimeError> {
    let state = read_json::<RunState>(path, max_bytes)?;
    if let Some(state) = &state {
        if state.report.run_id != run_id {
            return Err(RuntimeError::Storage(
                "stored run id does not match requested id".into(),
            ));
        }
        state.validate()?;
    }
    Ok(state)
}

pub(super) fn read_json<T: DeserializeOwned>(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<T>, RuntimeError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() as u64 > max_bytes {
        return Err(RuntimeError::Storage(
            "checkpoint exceeds byte limit".into(),
        ));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(storage)
}

pub(super) fn atomic_write<T: Serialize>(
    path: &Path,
    value: &T,
    max_bytes: u64,
) -> Result<(), RuntimeError> {
    let bytes = serde_json::to_vec(value).map_err(storage)?;
    if bytes.len() as u64 > max_bytes {
        return Err(RuntimeError::Storage(
            "checkpoint exceeds byte limit".into(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| RuntimeError::Storage("checkpoint path has no parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(storage)?;
    temporary.write_all(&bytes).map_err(storage)?;
    temporary.as_file().sync_all().map_err(storage)?;
    for attempt in 0..8 {
        match temporary.persist(path) {
            Ok(_) => {
                #[cfg(unix)]
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(storage)?;
                return Ok(());
            }
            Err(error)
                if cfg!(windows)
                    && attempt < 7
                    && (error.error.kind() == std::io::ErrorKind::PermissionDenied
                        || matches!(error.error.raw_os_error(), Some(32 | 33))) =>
            {
                temporary = error.file;
                std::thread::sleep(Duration::from_millis(5 * (attempt + 1)));
            }
            Err(error) => return Err(storage(error.error)),
        }
    }
    unreachable!()
}

pub(super) fn storage(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Storage(error.to_string())
}

pub(super) fn absolute_root(root: PathBuf) -> (PathBuf, Option<String>) {
    match std::path::absolute(&root) {
        Ok(absolute) => (absolute, None),
        Err(error) => (root, Some(format!("cannot resolve store root: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_owner_unlocks_even_if_a_duplicate_descriptor_still_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let duplicate = lock.try_clone().unwrap();
        let owner = Arc::new(LeaseInner {
            root: dir.path().into(),
            key: "test".into(),
            run_id: "test".into(),
            lock,
            writes: Mutex::new(()),
            max_bytes: 1024,
        });
        let pending_writer = Arc::clone(&owner);
        let next = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        drop(owner);
        assert!(fs2::FileExt::try_lock_exclusive(&next).is_err());
        drop(pending_writer);
        fs2::FileExt::try_lock_exclusive(&next).unwrap();
        drop(duplicate);
    }
}
