#![cfg(feature = "runtime")]
use futures_util::future::BoxFuture;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use taskloom::runtime::{rag::*, RuntimeError};

struct Fixed {
    ranks: Vec<ScoredChunk>,
    observed: Arc<AtomicUsize>,
}
impl Retriever for Fixed {
    fn identity(&self) -> Option<String> {
        Some("synthetic-diversity-fixture".into())
    }
    fn retrieve<'a>(
        &'a self,
        _: &'a CorpusSnapshot,
        query: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            self.observed.store(query.top_k, Ordering::SeqCst);
            Ok(self.ranks.clone())
        })
    }
}
fn fixture() -> CorpusSnapshot {
    let mut snapshot = CorpusSnapshot::default();
    for (id, text, ranges) in [
        ("a", "first second third", vec![(0, 5), (6, 12), (13, 18)]),
        ("b", "other source", vec![(0, 12)]),
    ] {
        let doc = Document {
            id: id.into(),
            title: id.into(),
            source: format!("fixture:{id}"),
            text: text.into(),
            metadata: BTreeMap::new(),
        };
        for (start, end) in ranges {
            snapshot
                .chunks
                .push(Chunk::from_range(&doc, start, end).unwrap());
        }
        snapshot.documents.insert(id.into(), doc);
    }
    snapshot.validate().unwrap();
    snapshot
}
fn backend(snapshot: &CorpusSnapshot, observed: Arc<AtomicUsize>) -> Fixed {
    Fixed {
        ranks: snapshot
            .chunks
            .iter()
            .enumerate()
            .map(|(i, c)| ScoredChunk {
                chunk_id: c.id.clone(),
                score: 10.0 - i as f64,
            })
            .collect(),
        observed,
    }
}
#[tokio::test]
async fn explicit_diversity_widens_once_and_preserves_source_scores_order() {
    let snapshot = fixture();
    let observed = Arc::new(AtomicUsize::new(0));
    let wrapper =
        PerDocumentRetriever::new(Arc::new(backend(&snapshot, observed.clone())), 1, 4).unwrap();
    let mut query = RetrievalQuery::new("source");
    query.top_k = 2;
    let hits = wrapper.retrieve(&snapshot, &query).await.unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), 4);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].chunk_id, snapshot.chunks[0].id);
    assert_eq!(hits[1].chunk_id, snapshot.chunks[3].id);
    assert_eq!(hits[1].score, 7.0);
    assert!(wrapper.identity().unwrap().starts_with("per-document-v1:"));
}
#[tokio::test]
async fn corrupt_late_result_cannot_hide_behind_top_one_or_document_cap() {
    let snapshot = fixture();
    for corrupt in [
        ScoredChunk {
            chunk_id: "not-in-snapshot".into(),
            score: 1.0,
        },
        ScoredChunk {
            chunk_id: snapshot.chunks[0].id.clone(),
            score: 1.0,
        },
        ScoredChunk {
            chunk_id: snapshot.chunks[3].id.clone(),
            score: f64::NAN,
        },
    ] {
        let mut fixed = backend(&snapshot, Arc::new(AtomicUsize::new(0)));
        fixed.ranks[3] = corrupt;
        let wrapper = PerDocumentRetriever::new(Arc::new(fixed), 1, 4).unwrap();
        let mut query = RetrievalQuery::new("source");
        query.top_k = 1;
        assert!(wrapper.retrieve(&snapshot, &query).await.is_err());
    }
}
#[tokio::test]
async fn query_above_authorized_pool_refuses_before_backend_dispatch() {
    let snapshot = fixture();
    let observed = Arc::new(AtomicUsize::new(0));
    let wrapper =
        PerDocumentRetriever::new(Arc::new(backend(&snapshot, observed.clone())), 1, 2).unwrap();
    assert!(wrapper
        .retrieve(&snapshot, &RetrievalQuery::new("source"))
        .await
        .is_err());
    assert_eq!(observed.load(Ordering::SeqCst), 0);
}
#[cfg(feature = "app")]
#[test]
fn default_app_identity_omits_diversity_and_rejects_unfunded_pool_expansion() {
    use taskloom::runtime::app::{AppConfig, AppLexicalRerank};
    let mut config = AppConfig::default();
    assert!(serde_json::to_value(&config.retrieval)
        .unwrap()
        .get("max_chunks_per_document")
        .is_none());
    config.retrieval.max_chunks_per_document = 1;
    config.retrieval.rerank = Some(AppLexicalRerank {
        candidates: 5,
        max_candidate_bytes: 65536,
        lexical: LexicalConfig::default(),
    });
    assert!(config.validate().is_err());
    config.retrieval.rerank.as_mut().unwrap().candidates = 20;
    config.validate().unwrap();
}
