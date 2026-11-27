use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};

use super::WorkflowState;
use crate::runtime::{
    store::{absolute_root, atomic_write, blocking, read_json, storage},
    RuntimeError,
};

pub trait WorkflowLease: Send + Sync {
    fn load(&self) -> BoxFuture<'_, Result<Option<WorkflowState>, RuntimeError>>;
    fn save<'a>(&'a self, state: &'a WorkflowState) -> BoxFuture<'a, Result<(), RuntimeError>>;
}
pub trait WorkflowStore: Send + Sync {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn WorkflowLease>, RuntimeError>>;
    fn inspect<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WorkflowState>, RuntimeError>>;
}

#[derive(Debug, Clone)]
pub struct FileWorkflowStore {
    root: PathBuf,
    root_error: Option<String>,
    max_bytes: u64,
}
impl FileWorkflowStore {
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
    pub fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }
}

impl WorkflowStore for FileWorkflowStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn WorkflowLease>, RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            let key = key(id)?;
            let root = self.root.clone();
            let id = id.to_string();
            let max = self.max_bytes;
            blocking(move || {
                std::fs::create_dir_all(&root).map_err(storage)?;
                let lock = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(root.join(format!("{key}.workflow.lock")))
                    .map_err(storage)?;
                fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || matches!(error.raw_os_error(), Some(11 | 33))
                    {
                        RuntimeError::Conflict("workflow is owned by another worker".into())
                    } else {
                        storage(error)
                    }
                })?;
                Ok(Arc::new(Lease(Arc::new(Inner {
                    root,
                    key,
                    id,
                    lock,
                    writes: Mutex::new(()),
                    max,
                }))) as Arc<dyn WorkflowLease>)
            })
            .await
        })
    }
    fn inspect<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WorkflowState>, RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            let path = self.root.join(format!("{}.workflow.json", key(id)?));
            let id = id.to_string();
            let max = self.max_bytes;
            blocking(move || read(&path, max, &id)).await
        })
    }
}

struct Inner {
    root: PathBuf,
    key: String,
    id: String,
    lock: File,
    writes: Mutex<()>,
    max: u64,
}
impl Drop for Inner {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}
struct Lease(Arc<Inner>);
impl WorkflowLease for Lease {
    fn load(&self) -> BoxFuture<'_, Result<Option<WorkflowState>, RuntimeError>> {
        let inner = Arc::clone(&self.0);
        Box::pin(blocking(move || {
            read(
                &inner.root.join(format!("{}.workflow.json", inner.key)),
                inner.max,
                &inner.id,
            )
        }))
    }
    fn save<'a>(&'a self, state: &'a WorkflowState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        let state = state.clone();
        let inner = Arc::clone(&self.0);
        Box::pin(blocking(move || {
            state.validate()?;
            if state.id != inner.id {
                return Err(storage("workflow id does not match ownership"));
            }
            let _guard = inner
                .writes
                .lock()
                .map_err(|_| storage("workflow writer panicked"))?;
            let path = inner.root.join(format!("{}.workflow.json", inner.key));
            if let Some(previous) = read(&path, inner.max, &inner.id)? {
                if previous.revision > state.revision {
                    // A newer workflow revision is stored: this state is stale
                    // and must not be reported as durably saved.
                    return Err(RuntimeError::Conflict(
                        "a newer workflow revision is already stored".into(),
                    ));
                }
                if previous.revision == state.revision {
                    return if serde_json::to_value(previous).map_err(storage)?
                        == serde_json::to_value(&state).map_err(storage)?
                    {
                        Ok(())
                    } else {
                        Err(storage("conflicting workflow revision"))
                    };
                }
            }
            atomic_write(&path, &state, inner.max)
        }))
    }
}
fn read(path: &Path, max: u64, id: &str) -> Result<Option<WorkflowState>, RuntimeError> {
    let state = read_json::<WorkflowState>(path, max)?;
    if let Some(state) = &state {
        state.validate()?;
        if state.id != id {
            return Err(storage("stored workflow identity differs"));
        }
    }
    Ok(state)
}
fn key(id: &str) -> Result<String, RuntimeError> {
    if id.trim().is_empty() {
        return Err(super::graph::config("workflow id must be nonempty"));
    }
    Ok(hex::encode(Sha256::digest(id.as_bytes())))
}
