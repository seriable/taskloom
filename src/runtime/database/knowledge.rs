use super::*;
use crate::runtime::{
    memory::{MemoryScope, MemorySnapshot, MemoryStore},
    rag::{validate_chunks, Chunk, CorpusSnapshot, CorpusStore, Document},
};

/// Namespace-bound, bounded whole-corpus snapshots with transactional CAS.
/// This is not a server-side vector search engine.
#[derive(Clone)]
pub struct SqlCorpusStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlCorpusStore {
    pub(super) fn new(db: SqlDatabase, namespace: DatabaseNamespace) -> Self {
        Self { db, namespace }
    }
    fn key(&self) -> Result<RecordKey, RuntimeError> {
        RecordKey::new(self.namespace.clone(), "corpus", "default")
    }
    async fn update(
        &self,
        expected: Option<u64>,
        operation: impl FnOnce(&mut CorpusSnapshot) -> Result<(), RuntimeError> + Send,
    ) -> Result<CorpusSnapshot, RuntimeError> {
        self.db.schema().await?;
        let key = self.key()?;
        let mut tx = self.db.write_transaction().await?;
        let row = self
            .db
            .lock_record(&mut tx, &key, &CorpusSnapshot::default())
            .await?;
        let mut snapshot = parse_corpus(row)?;
        if expected.is_some_and(|revision| revision != snapshot.revision) {
            return Err(conflict("corpus revision changed"));
        }
        operation(&mut snapshot)?;
        snapshot.revision = snapshot
            .revision
            .checked_add(1)
            .ok_or_else(|| storage("corpus revision overflow"))?;
        snapshot.validate()?;
        let body = self.db.serialize(&key, &snapshot)?;
        self.db
            .replace(&mut tx, &key, snapshot.revision, &body)
            .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }
}
impl CorpusStore for SqlCorpusStore {
    fn snapshot(&self) -> BoxFuture<'_, Result<CorpusSnapshot, RuntimeError>> {
        Box::pin(async move {
            self.db
                .read(&self.key()?)
                .await?
                .map(parse_corpus)
                .transpose()
                .map(Option::unwrap_or_default)
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
            self.update(expected_revision, move |snapshot| {
                snapshot
                    .chunks
                    .retain(|chunk| chunk.document_id != document.id);
                snapshot.chunks.extend(chunks);
                snapshot.documents.insert(document.id.clone(), document);
                Ok(())
            })
            .await
        })
    }
    fn remove<'a>(
        &'a self,
        id: &'a str,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'a, Result<CorpusSnapshot, RuntimeError>> {
        Box::pin(self.update(expected_revision, move |snapshot| {
            if snapshot.documents.remove(id).is_none() {
                return Err(config("document does not exist"));
            }
            snapshot.chunks.retain(|chunk| chunk.document_id != id);
            Ok(())
        }))
    }
}
fn parse_corpus(row: Record) -> Result<CorpusSnapshot, RuntimeError> {
    let snapshot: CorpusSnapshot = row.parse()?;
    snapshot.validate()?;
    if snapshot.revision != row.revision {
        return Err(storage("database corpus revision differs"));
    }
    Ok(snapshot)
}

/// Persistence primitive for MemoryManager. It binds both project namespace
/// and owner scope; authentication/curator grants still belong to the host.
#[derive(Clone)]
pub struct SqlMemoryStore {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
}
impl SqlMemoryStore {
    pub(super) fn new(db: SqlDatabase, namespace: DatabaseNamespace) -> Self {
        Self { db, namespace }
    }
    fn key(&self, scope: &MemoryScope) -> Result<RecordKey, RuntimeError> {
        scope.validate()?;
        if DatabaseNamespace::new(&scope.tenant, &scope.project)? != self.namespace {
            return Err(RuntimeError::Permission(
                "memory scope differs from host database namespace".into(),
            ));
        }
        RecordKey::new(self.namespace.clone(), "memory", &scope.identity()?)
    }
}
impl MemoryStore for SqlMemoryStore {
    fn load<'a>(
        &'a self,
        scope: &'a MemoryScope,
    ) -> BoxFuture<'a, Result<MemorySnapshot, RuntimeError>> {
        Box::pin(async move {
            self.db
                .read(&self.key(scope)?)
                .await?
                .map(|row| parse_memory(row, scope))
                .transpose()
                .map(|snapshot| snapshot.unwrap_or_else(|| MemorySnapshot::empty(scope.clone())))
        })
    }
    fn save<'a>(
        &'a self,
        snapshot: &'a MemorySnapshot,
        expected: u64,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            let key = self.key(&snapshot.scope)?;
            snapshot.validate()?;
            let body = self.db.serialize(&key, snapshot)?;
            // Match file storage's retained capacity for explicit erasure.
            let reserve = (snapshot.records.len() as u64).saturating_mul(2048);
            if (body.len() as u64).saturating_add(reserve) > self.db.0.options.max_record_bytes {
                return Err(RuntimeError::StorageLimit(
                    "memory snapshot exceeds byte limit with erasure reserve".into(),
                ));
            }
            self.db.schema().await?;
            let mut tx = self.db.write_transaction().await?;
            let row = self
                .db
                .lock_record(
                    &mut tx,
                    &key,
                    &MemorySnapshot::empty(snapshot.scope.clone()),
                )
                .await?;
            let previous = parse_memory(row, &snapshot.scope)?;
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
                return Err(conflict(
                    "memory revision or immutable audit prefix changed",
                ));
            }
            self.db
                .replace(&mut tx, &key, snapshot.revision, &body)
                .await?;
            tx.commit().await.map_err(database_error)
        })
    }
}
fn parse_memory(row: Record, scope: &MemoryScope) -> Result<MemorySnapshot, RuntimeError> {
    let snapshot: MemorySnapshot = row.parse()?;
    snapshot.validate()?;
    if &snapshot.scope != scope || snapshot.revision != row.revision {
        return Err(storage("database memory scope/revision differs"));
    }
    Ok(snapshot)
}
