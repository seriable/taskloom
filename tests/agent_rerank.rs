#![cfg(feature = "runtime")]
use futures_util::future::BoxFuture;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use taskloom::runtime::{rag::*, RuntimeError};

fn corpus() -> CorpusSnapshot {
    let mut snapshot = CorpusSnapshot::default();
    for (id, title, text) in [
        ("weak", "General", "alpha irrelevant text"),
        ("strong", "alpha beta", "alpha beta reliable evidence"),
        ("other", "General", "unrelated material"),
    ] {
        let doc = Document {
            id: id.into(),
            title: title.into(),
            source: format!("source:{id}"),
            text: text.into(),
            metadata: BTreeMap::new(),
        };
        snapshot
            .chunks
            .push(Chunk::from_range(&doc, 0, doc.text.len()).unwrap());
        snapshot.documents.insert(id.into(), doc);
    }
    snapshot
}
struct Fixed {
    ranks: Vec<ScoredChunk>,
    calls: Arc<AtomicUsize>,
}
impl Retriever for Fixed {
    fn identity(&self) -> Option<String> {
        Some("fixture-backend".into())
    }
    fn retrieve<'a>(
        &'a self,
        _: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.ranks.clone())
        })
    }
}
fn fixed(snapshot: &CorpusSnapshot) -> Arc<dyn Retriever> {
    Arc::new(Fixed {
        ranks: snapshot
            .chunks
            .iter()
            .enumerate()
            .map(|(i, c)| ScoredChunk {
                chunk_id: c.id.clone(),
                score: 100.0 - i as f64,
            })
            .collect(),
        calls: Arc::new(AtomicUsize::new(0)),
    })
}
#[tokio::test]
async fn lexical_reranking_changes_order_end_to_end_preserving_exact_citations() {
    let snapshot = corpus();
    let temporary = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(temporary.path());
    for document in snapshot.documents.values() {
        ingest(&store, document.clone(), &WindowChunker::default(), None)
            .await
            .unwrap();
    }
    let ranker = RerankingRetriever::new(
        fixed(&snapshot),
        Arc::new(LexicalReranker::default()),
        3,
        16384,
    )
    .unwrap();
    let mut query = RetrievalQuery::new("alpha beta");
    query.top_k = 1;
    let result = search(&store, &ranker, &query).await.unwrap();
    assert_eq!(result.hits[0].chunk.document_id, "strong");
    let chunk = &result.hits[0].chunk;
    assert_eq!(chunk.text, "alpha beta reliable evidence");
    assert!(result
        .verify(CitedReport {
            text: format!("Finding [[{}]]", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: chunk.text.clone()
            }]
        })
        .is_ok());
}
struct Inspect {
    calls: Arc<AtomicUsize>,
    return_extra: bool,
}
impl Reranker for Inspect {
    fn identity(&self) -> Option<String> {
        Some("fixture-reranker".into())
    }
    fn rerank<'a>(
        &'a self,
        snapshot: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
        candidates: &'a [ScoredChunk],
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.return_extra {
                Ok(vec![ScoredChunk {
                    chunk_id: snapshot.chunks[2].id.clone(),
                    score: 5.0,
                }])
            } else {
                Ok(candidates.to_vec())
            }
        })
    }
}
#[tokio::test]
async fn full_backend_and_snapshot_validate_before_limits_or_reranker_dispatch() {
    let snapshot = corpus();
    let calls = Arc::new(AtomicUsize::new(0));
    let backend_calls = Arc::new(AtomicUsize::new(0));
    let invalid = vec![
        ScoredChunk {
            chunk_id: snapshot.chunks[0].id.clone(),
            score: 1.0,
        },
        ScoredChunk {
            chunk_id: "unknown-late-candidate".into(),
            score: 1.0,
        },
    ];
    let backend = Arc::new(Fixed {
        ranks: invalid,
        calls: backend_calls.clone(),
    });
    let reranker = Arc::new(Inspect {
        calls: calls.clone(),
        return_extra: false,
    });
    let wrapper = RerankingRetriever::new(backend, reranker, 1, 128).unwrap();
    let mut query = RetrievalQuery::new("alpha");
    query.top_k = 1;
    assert!(wrapper.retrieve(&snapshot, &query).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut corrupt = snapshot.clone();
    corrupt.chunks[2].text.push_str("tampered");
    assert!(wrapper.retrieve(&corrupt, &query).await.is_err());
    assert_eq!(backend_calls.load(Ordering::SeqCst), 1);
    let wrapper = RerankingRetriever::new(
        fixed(&snapshot),
        Arc::new(Inspect {
            calls: calls.clone(),
            return_extra: false,
        }),
        3,
        128,
    )
    .unwrap();
    assert!(wrapper
        .retrieve(&snapshot, &query)
        .await
        .unwrap_err()
        .to_string()
        .contains("bytes"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn reranker_cannot_resurrect_a_candidate_filtered_by_the_backend() {
    let snapshot = corpus();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut query = RetrievalQuery::new("alpha");
    query.top_k = 1;
    let wrapper = RerankingRetriever::new(
        fixed(&snapshot),
        Arc::new(Inspect {
            calls: calls.clone(),
            return_extra: true,
        }),
        1,
        16384,
    )
    .unwrap();
    assert!(wrapper.retrieve(&snapshot, &query).await.is_err());
    let inner = IdentifierConstrainedRetriever::new(fixed(&snapshot), BTreeSet::new()).unwrap();
    let wrapper = RerankingRetriever::new(
        Arc::new(inner),
        Arc::new(Inspect {
            calls: calls.clone(),
            return_extra: true,
        }),
        3,
        16384,
    )
    .unwrap();
    query.text = "ABSENT99999".into();
    assert!(wrapper
        .retrieve(&snapshot, &query)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn ranking_rejects_duplicate_nonfinite_negative_and_overlarge_results() {
    let snapshot = corpus();
    let mut query = RetrievalQuery::new("alpha");
    query.top_k = 1;
    for scores in [vec![f64::NAN], vec![-1.0], vec![1.0, 2.0], vec![1.0; 21]] {
        let ranks = scores
            .into_iter()
            .map(|score| ScoredChunk {
                chunk_id: snapshot.chunks[0].id.clone(),
                score,
            })
            .collect();
        let wrapper = RerankingRetriever::new(
            Arc::new(Fixed {
                ranks,
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(LexicalReranker::default()),
            1,
            16384,
        )
        .unwrap();
        assert!(wrapper.retrieve(&snapshot, &query).await.is_err());
    }
}
#[tokio::test]
async fn lexical_reranker_honors_aliases_stopwords_enabled_fields_and_identities() {
    let snapshot = corpus();
    let candidates = snapshot
        .chunks
        .iter()
        .map(|c| ScoredChunk {
            chunk_id: c.id.clone(),
            score: 1.0,
        })
        .collect::<Vec<_>>();
    let mut policy = LexicalConfig {
        body_weight: 0.0,
        metadata_weight: 0.0,
        ..Default::default()
    };
    policy.stop_words.insert("the".into());
    policy
        .query_aliases
        .insert("reset the".into(), vec!["beta".into()]);
    let reranker = LexicalReranker::new(policy.clone()).unwrap();
    let query = RetrievalQuery::new("reset the");
    let scores = reranker
        .rerank(&snapshot, &query, &candidates)
        .await
        .unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores[0].chunk_id, snapshot.chunks[1].id);
    assert!(reranker
        .rerank(&snapshot, &RetrievalQuery::new("the"), &candidates)
        .await
        .unwrap()
        .is_empty());
    let a = RerankingRetriever::new(fixed(&snapshot), Arc::new(reranker), 3, 16384).unwrap();
    policy.title_weight = 4.0;
    let b = RerankingRetriever::new(
        fixed(&snapshot),
        Arc::new(LexicalReranker::new(policy).unwrap()),
        3,
        16384,
    )
    .unwrap();
    assert_ne!(a.identity(), b.identity());
    let c = RerankingRetriever::new(
        fixed(&snapshot),
        Arc::new(LexicalReranker::default()),
        2,
        16384,
    )
    .unwrap();
    assert_ne!(a.identity(), c.identity());
}
