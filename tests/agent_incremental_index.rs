#![cfg(feature = "runtime")]
use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use taskloom::runtime::{rag::*, RuntimeError};

struct FixtureModel {
    identity: Mutex<String>,
    calls: Mutex<Vec<Vec<String>>>,
    mode: AtomicUsize,
}
impl FixtureModel {
    fn new() -> Self {
        Self {
            identity: Mutex::new("incremental-fixture-model-v1".into()),
            calls: Mutex::new(Vec::new()),
            mode: AtomicUsize::new(0),
        }
    }
    fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}
impl EmbeddingModel for FixtureModel {
    fn identity(&self) -> String {
        self.identity.lock().unwrap().clone()
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
        Box::pin(async move {
            let identity = self.identity();
            self.calls.lock().unwrap().push(texts.to_vec());
            let mode = self.mode.load(Ordering::SeqCst);
            if mode == 4 {
                return Err(RuntimeError::Protocol(
                    "fixture provider failure; usage unknown".into(),
                ));
            }
            let vectors = if mode == 1 {
                Vec::new()
            } else {
                texts
                    .iter()
                    .map(|text| {
                        let hash = Sha256::digest(text.as_bytes());
                        match mode {
                            2 => vec![f32::NAN, 1.0, 1.0],
                            3 => vec![1.0, 2.0, 3.0, 4.0],
                            _ => hash[..3].iter().map(|b| f32::from(*b) + 1.0).collect(),
                        }
                    })
                    .collect()
            };
            if mode == 5 {
                *self.identity.lock().unwrap() = "changed-during-embedding".into();
            }
            Ok(EmbeddingBatch { identity, vectors })
        })
    }
}
fn chunker() -> WindowChunker {
    WindowChunker {
        max_chars: 40,
        overlap_chars: 0,
    }
}
fn doc(id: &str, title: &str, text: String) -> Document {
    Document {
        id: id.into(),
        title: title.into(),
        source: format!("reviewed://tenant-one/{id}"),
        text,
        metadata: BTreeMap::new(),
    }
}
async fn fixture() -> (
    tempfile::TempDir,
    FileCorpusStore,
    CorpusSnapshot,
    FixtureModel,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(dir.path());
    ingest(
        &store,
        doc(
            "a",
            "Alpha",
            format!("{}{}{}", "a".repeat(40), "b".repeat(40), "c".repeat(40)),
        ),
        &chunker(),
        Some(0),
    )
    .await
    .unwrap();
    let old = ingest(
        &store,
        doc("b", "Beta", format!("{}{}", "d".repeat(40), "e".repeat(40))),
        &chunker(),
        Some(1),
    )
    .await
    .unwrap();
    (dir, store, old, FixtureModel::new())
}
#[tokio::test]
async fn unchanged_or_revision_only_update_reuses_every_vector_without_changing_legacy_json() {
    for context in [false, true] {
        let (_dir, _store, old, model) = fixture().await;
        let index = EmbeddingIndex::build_with_document_context(&old, &model, context)
            .await
            .unwrap();
        let before = serde_json::to_value(&index).unwrap();
        model.clear();
        let updated = index.update_incremental(&old, &old, &model).await.unwrap();
        assert_eq!(model.count(), 0);
        assert_eq!(updated.receipt.reused, old.chunks.len());
        assert_eq!(updated.receipt.embedded, 0);
        assert_eq!(updated.receipt.removed, 0);
        assert!(!updated.receipt.index_absent);
        assert_eq!(
            updated.receipt.old_index_binding,
            updated.receipt.new_index_binding.clone().unwrap()
        );
        assert_eq!(
            serde_json::to_value(updated.index.unwrap()).unwrap(),
            before
        );
        if !context {
            assert!(before.get("document_context").is_none());
            assert_eq!(before["version"], 1);
        }
        let mut revision_only = old.clone();
        revision_only.revision += 3;
        let updated = index
            .update_incremental(&old, &revision_only, &model)
            .await
            .unwrap();
        updated
            .index
            .as_ref()
            .unwrap()
            .validate(&revision_only, &model.identity())
            .unwrap();
        assert_ne!(
            updated.receipt.old_corpus_binding,
            updated.receipt.new_corpus_binding
        );
        assert_eq!(updated.receipt.reused, old.chunks.len());
        assert_eq!(model.count(), 0);
        assert_eq!(serde_json::to_value(&index).unwrap(), before);
    }
}
#[tokio::test]
async fn one_source_edit_embeds_only_exact_new_chunk_then_deletes_drop_vectors_without_dispatch() {
    let (_dir, store, old, model) = fixture().await;
    let index = EmbeddingIndex::build(&old, &model).await.unwrap();
    let before = serde_json::to_value(&index).unwrap();
    let mut changed = old.documents["a"].clone();
    changed.text = format!("{}{}{}", "a".repeat(40), "z".repeat(40), "c".repeat(40));
    let new = ingest(&store, changed, &chunker(), Some(old.revision))
        .await
        .unwrap();
    model.clear();
    let updated = index.update_incremental(&old, &new, &model).await.unwrap();
    assert_eq!(
        (
            updated.receipt.reused,
            updated.receipt.embedded,
            updated.receipt.removed
        ),
        (4, 1, 1)
    );
    assert_eq!(*model.calls.lock().unwrap(), vec![vec!["z".repeat(40)]]);
    let changed_index = updated.index.unwrap();
    changed_index.validate(&new, &model.identity()).unwrap();
    for chunk in old.chunks.iter().filter(|c| new.chunks.contains(c)) {
        assert_eq!(changed_index.vectors[&chunk.id], index.vectors[&chunk.id]);
    }
    assert_eq!(serde_json::to_value(&index).unwrap(), before);
    let deleted = store.remove("b", Some(new.revision)).await.unwrap();
    model.clear();
    let updated = changed_index
        .update_incremental(&new, &deleted, &model)
        .await
        .unwrap();
    assert_eq!(
        (
            updated.receipt.reused,
            updated.receipt.embedded,
            updated.receipt.removed
        ),
        (3, 0, 2)
    );
    assert_eq!(model.count(), 0);
    let reduced = updated.index.unwrap();
    assert_eq!(reduced.vectors.len(), 3);
    reduced.validate(&deleted, &model.identity()).unwrap();
    let empty = store.remove("a", Some(deleted.revision)).await.unwrap();
    let cleared = reduced
        .update_incremental(&deleted, &empty, &model)
        .await
        .unwrap();
    assert!(cleared.index.is_none());
    assert!(cleared.receipt.index_absent);
    assert!(cleared.receipt.new_index_binding.is_none());
    assert_eq!(
        (
            cleared.receipt.reused,
            cleared.receipt.embedded,
            cleared.receipt.removed
        ),
        (0, 0, 3)
    );
    assert_eq!(model.count(), 0);
    assert_eq!(reduced.vectors.len(), 3);
    assert!(reduced.validate(&empty, &model.identity()).is_err());
}
#[tokio::test]
async fn source_and_title_context_changes_never_reuse_same_body_from_another_provenance() {
    for context in [false, true] {
        let (_dir, store, old, model) = fixture().await;
        let index = EmbeddingIndex::build_with_document_context(&old, &model, context)
            .await
            .unwrap();
        let mut changed = old.documents["a"].clone();
        changed.source = "reviewed://other-tenant/a".into();
        changed.title = "Changed title".into();
        let new = ingest(&store, changed, &chunker(), Some(old.revision))
            .await
            .unwrap();
        model.clear();
        let updated = index.update_incremental(&old, &new, &model).await.unwrap();
        assert_eq!(
            (
                updated.receipt.reused,
                updated.receipt.embedded,
                updated.receipt.removed
            ),
            (2, 3, 3)
        );
        let calls = model.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 3);
        assert_eq!(calls[0][0].starts_with("Title: Changed title\n\n"), context);
        let index = updated.index.unwrap();
        index.validate(&new, &model.identity()).unwrap();
    }
}
#[tokio::test]
async fn stale_source_revision_model_context_and_malformed_old_vectors_refuse_before_dispatch() {
    let (_dir, store, old, model) = fixture().await;
    let index = EmbeddingIndex::build(&old, &model).await.unwrap();
    let mut doc = old.documents["a"].clone();
    doc.text = "changed body".repeat(10);
    let new = ingest(&store, doc, &chunker(), Some(old.revision))
        .await
        .unwrap();
    model.clear();
    let mut stale_old = old.clone();
    stale_old.revision += 1;
    assert!(index
        .update_incremental(&stale_old, &new, &model)
        .await
        .is_err());
    let mut rollback = new.clone();
    rollback.revision = old.revision - 1;
    assert!(index
        .update_incremental(&old, &rollback, &model)
        .await
        .is_err());
    let mut same_revision = new.clone();
    same_revision.revision = old.revision;
    assert!(index
        .update_incremental(&old, &same_revision, &model)
        .await
        .is_err());
    let mut invalid = new.clone();
    invalid.chunks[0].text.push_str("unbound");
    assert!(index
        .update_incremental(&old, &invalid, &model)
        .await
        .is_err());
    for mode in 0..4 {
        let mut bad = index.clone();
        match mode {
            0 => bad.vectors.values_mut().next().unwrap()[0] = f32::NAN,
            1 => bad.vectors.values_mut().next().unwrap().fill(0.0),
            2 => bad.document_context = true,
            _ => {
                bad.vectors.pop_first();
            }
        }
        assert!(bad.update_incremental(&old, &new, &model).await.is_err());
    }
    *model.identity.lock().unwrap() = "another-model".into();
    assert!(index.update_incremental(&old, &new, &model).await.is_err());
    assert_eq!(model.count(), 0);
}
#[tokio::test]
async fn malformed_or_failed_changed_batch_has_no_retry_partial_receipt_or_mutation() {
    let (_dir, store, old, model) = fixture().await;
    let index = EmbeddingIndex::build(&old, &model).await.unwrap();
    let before = serde_json::to_value(&index).unwrap();
    let mut doc = old.documents["a"].clone();
    doc.source = "reviewed://new-version/a".into();
    let new = ingest(&store, doc, &chunker(), Some(old.revision))
        .await
        .unwrap();
    for mode in 1..=5 {
        model.clear();
        *model.identity.lock().unwrap() = "incremental-fixture-model-v1".into();
        model.mode.store(mode, Ordering::SeqCst);
        assert!(index.update_incremental(&old, &new, &model).await.is_err());
        assert_eq!(model.count(), 1, "one explicit batch, no fallback/retry");
        assert_eq!(serde_json::to_value(&index).unwrap(), before);
    }
}
#[tokio::test]
async fn changed_batches_remain_bounded_and_final_capacity_is_checked_before_dispatch() {
    let (_dir, store, old, model) = fixture().await;
    let index = EmbeddingIndex::build(&old, &model).await.unwrap();
    let new = ingest(
        &store,
        doc("c", "Many chunks", "f".repeat(40 * 70)),
        &chunker(),
        Some(old.revision),
    )
    .await
    .unwrap();
    model.clear();
    let updated = index.update_incremental(&old, &new, &model).await.unwrap();
    assert_eq!(
        (
            updated.receipt.reused,
            updated.receipt.embedded,
            updated.receipt.removed
        ),
        (5, 70, 0)
    );
    assert_eq!(
        model
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![32, 32, 6]
    );
    let huge = ingest(
        &store,
        doc("d", "Too many chunks", "g".repeat(10_001)),
        &WindowChunker {
            max_chars: 1,
            overlap_chars: 0,
        },
        Some(new.revision),
    )
    .await
    .unwrap();
    model.clear();
    assert!(updated
        .index
        .unwrap()
        .update_incremental(&new, &huge, &model)
        .await
        .is_err());
    assert_eq!(model.count(), 0);
}
