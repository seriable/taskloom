#![cfg(feature = "runtime")]
use futures_util::future::BoxFuture;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use taskloom::runtime::rag::*;
use taskloom::runtime::RuntimeError;

struct FixtureEmbeddings {
    calls: Arc<AtomicUsize>,
    identity: &'static str,
}
impl EmbeddingModel for FixtureEmbeddings {
    fn identity(&self) -> String {
        self.identity.into()
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingBatch {
                identity: self.identity(),
                vectors: texts
                    .iter()
                    .map(|s| {
                        if s.contains("car") || s.contains("automobile") {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        }
                    })
                    .collect(),
            })
        })
    }
}
async fn corpus(root: &std::path::Path) -> FileCorpusStore {
    let store = FileCorpusStore::new(root);
    for (id, text) in [
        ("vehicle", "A car transports passengers."),
        ("fruit", "Apples are a fruit."),
    ] {
        ingest(
            &store,
            Document {
                id: id.into(),
                title: id.into(),
                source: format!("fixture:{id}"),
                text: text.into(),
                metadata: BTreeMap::new(),
            },
            &WindowChunker::default(),
            None,
        )
        .await
        .unwrap();
    }
    store
}
fn model() -> Arc<FixtureEmbeddings> {
    Arc::new(FixtureEmbeddings {
        calls: Arc::new(AtomicUsize::new(0)),
        identity: "explicit-fixture-v1",
    })
}

struct RankedFixture {
    id: Option<&'static str>,
    ranking: Vec<ScoredChunk>,
}
impl Retriever for RankedFixture {
    fn identity(&self) -> Option<String> {
        self.id.map(str::to_owned)
    }
    fn retrieve<'a>(
        &'a self,
        _: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move { Ok(self.ranking.clone()) })
    }
}

#[tokio::test]
async fn semantic_admission_prevents_lexical_resurrection_without_changing_sources() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let embeddings = model();
    let mut index = EmbeddingIndex::build(&snapshot, embeddings.as_ref())
        .await
        .unwrap();
    let fruit = snapshot
        .chunks
        .iter()
        .find(|c| c.document_id == "fruit")
        .unwrap();
    index.vectors.insert(fruit.id.clone(), vec![0.2, 1.0]);
    let vector = Arc::new(
        VectorRetriever::new(index, embeddings, &snapshot)
            .unwrap()
            .with_min_score(0.7)
            .unwrap(),
    );
    let baseline = HybridRetriever::new(Arc::new(LexicalRetriever), vector.clone());
    let legacy_id = baseline.identity();
    let explicit_false = HybridRetriever::new(Arc::new(LexicalRetriever), vector.clone())
        .with_semantic_gate(false)
        .unwrap();
    assert_eq!(legacy_id, explicit_false.identity());
    let query = RetrievalQuery::new("fruit automobile");
    assert_eq!(
        search(&store, &baseline, &query).await.unwrap().hits.len(),
        2
    );
    let gated = HybridRetriever::new(Arc::new(LexicalRetriever), vector)
        .with_semantic_gate(true)
        .unwrap();
    assert_ne!(legacy_id, gated.identity());
    let result = search(&store, &gated, &query).await.unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].chunk.document_id, "vehicle");
    let chunk = &result.hits[0].chunk;
    SearchResult::verify_all(
        std::slice::from_ref(&result),
        CitedReport {
            text: format!("A car transports passengers [[{}]].", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: chunk.text.clone(),
            }],
        },
    )
    .unwrap();
    let empty = Arc::new(RankedFixture {
        id: Some("empty-semantic-v1"),
        ranking: vec![],
    });
    let abstain = HybridRetriever::new(Arc::new(LexicalRetriever), empty)
        .with_semantic_gate(true)
        .unwrap();
    assert!(search(&store, &abstain, &query)
        .await
        .unwrap()
        .hits
        .is_empty());
    let invalid = Arc::new(RankedFixture {
        id: Some("bad-lexical-v1"),
        ranking: vec![ScoredChunk {
            chunk_id: "not-a-source".into(),
            score: 1.0,
        }],
    });
    let empty = Arc::new(RankedFixture {
        id: Some("empty-v1"),
        ranking: vec![],
    });
    let invalid = HybridRetriever::new(invalid, empty)
        .with_semantic_gate(true)
        .unwrap();
    assert!(
        search(&store, &invalid, &query).await.is_err(),
        "filtering must not hide corrupt backend rankings"
    );
}

#[test]
fn semantic_admission_needs_a_live_lane_and_cannot_invent_backend_identity() {
    let make = || {
        Arc::new(RankedFixture {
            id: None,
            ranking: vec![],
        }) as Arc<dyn Retriever>
    };
    assert!(HybridRetriever::new(make(), make()).identity().is_none());
    assert!(HybridRetriever::new(make(), make())
        .with_semantic_gate(true)
        .unwrap()
        .identity()
        .is_none());
    assert!(HybridRetriever::new(make(), make())
        .with_fusion(1.0, 0.0, 60.0, 20)
        .unwrap()
        .with_semantic_gate(true)
        .is_err());
    assert!(HybridRetriever::new(make(), make())
        .with_semantic_gate(true)
        .unwrap()
        .with_fusion(1.0, 0.0, 60.0, 20)
        .is_err());
}

#[tokio::test]
async fn explicit_query_prefix_is_versioned_without_touching_document_vectors() {
    struct RecordingModel(std::sync::Mutex<Vec<Vec<String>>>);
    impl EmbeddingModel for RecordingModel {
        fn identity(&self) -> String {
            "recording-fixture-v1".into()
        }
        fn embed<'a>(
            &'a self,
            texts: &'a [String],
        ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(texts.to_vec());
                Ok(EmbeddingBatch {
                    identity: self.identity(),
                    vectors: vec![vec![1.0, 0.0]; texts.len()],
                })
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let model = Arc::new(RecordingModel(std::sync::Mutex::new(vec![])));
    let index = EmbeddingIndex::build(&snapshot, model.as_ref())
        .await
        .unwrap();
    let before = serde_json::to_value(&index).unwrap();
    let baseline = VectorRetriever::new(index.clone(), model.clone(), &snapshot).unwrap();
    let empty = VectorRetriever::new(index.clone(), model.clone(), &snapshot)
        .unwrap()
        .with_query_prefix("")
        .unwrap();
    assert_eq!(baseline.identity(), empty.identity());
    let prefix = "Instruct: Retrieve supporting passages.\nQuery: ";
    let instructed = VectorRetriever::new(index.clone(), model.clone(), &snapshot)
        .unwrap()
        .with_query_prefix(prefix)
        .unwrap();
    assert_ne!(baseline.identity(), instructed.identity());
    search(&store, &instructed, &RetrievalQuery::new("automobile"))
        .await
        .unwrap();
    let calls = model.0.lock().unwrap();
    assert_eq!(
        calls[0],
        snapshot
            .chunks
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(calls.last().unwrap(), &vec![format!("{prefix}automobile")]);
    assert_eq!(serde_json::to_value(&index).unwrap(), before);
    drop(calls);
    for bad in ["x".repeat(4097), "bad\0prefix".into()] {
        assert!(
            VectorRetriever::new(index.clone(), model.clone(), &snapshot)
                .unwrap()
                .with_query_prefix(bad)
                .is_err()
        );
    }
}

#[tokio::test]
async fn vector_cutoff_is_explicit_bound_and_does_not_turn_similarity_into_citation_support() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let embeddings = model();
    let mut index = EmbeddingIndex::build(&snapshot, embeddings.as_ref())
        .await
        .unwrap();
    // Both documents have positive similarity, but fruit is a weak match.
    let fruit = snapshot
        .chunks
        .iter()
        .find(|c| c.document_id == "fruit")
        .unwrap();
    index.vectors.insert(fruit.id.clone(), vec![0.2, 1.0]);
    let baseline = VectorRetriever::new(index.clone(), embeddings.clone(), &snapshot).unwrap();
    assert_eq!(
        search(&store, &baseline, &RetrievalQuery::new("automobile"))
            .await
            .unwrap()
            .hits
            .len(),
        2
    );
    let filtered = VectorRetriever::new(index.clone(), embeddings.clone(), &snapshot)
        .unwrap()
        .with_min_score(0.7)
        .unwrap();
    assert_ne!(baseline.identity(), filtered.identity());
    let result = search(&store, &filtered, &RetrievalQuery::new("automobile"))
        .await
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].chunk.document_id, "vehicle");
    let zero = VectorRetriever::new(index.clone(), embeddings.clone(), &snapshot)
        .unwrap()
        .with_min_score(0.0)
        .unwrap();
    assert_eq!(baseline.identity(), zero.identity());
    for minimum in [f64::NAN, -0.1, 1.1, f64::INFINITY] {
        assert!(
            VectorRetriever::new(index.clone(), embeddings.clone(), &snapshot)
                .unwrap()
                .with_min_score(minimum)
                .is_err()
        );
    }
}

#[tokio::test]
async fn hybrid_can_return_vector_only_evidence_and_verify_unchanged_sources() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let model = model();
    let index = EmbeddingIndex::build(&snapshot, model.as_ref())
        .await
        .unwrap();
    let serialized = serde_json::to_vec(&index).unwrap();
    let index: EmbeddingIndex = serde_json::from_slice(&serialized).unwrap();
    index.validate(&snapshot, &model.identity()).unwrap();
    let vector = Arc::new(VectorRetriever::new(index, model.clone(), &snapshot).unwrap());
    let hybrid = HybridRetriever::new(Arc::new(LexicalRetriever), vector);
    let query = RetrievalQuery::new("automobile");
    assert!(search(&store, &LexicalRetriever, &query)
        .await
        .unwrap()
        .hits
        .is_empty());
    let result = search(&store, &hybrid, &query).await.unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].chunk.document_id, "vehicle");
    let chunk = &result.hits[0].chunk;
    SearchResult::verify_all(
        std::slice::from_ref(&result),
        CitedReport {
            text: format!("Vehicle evidence [[{}]]", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: chunk.text.clone(),
            }],
        },
    )
    .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    // A deterministic vocabulary fixture validates plumbing, not semantic recall.
    assert!(hybrid.identity().unwrap().starts_with("hybrid-v1:"));
}

#[tokio::test]
async fn stale_or_wrong_model_index_fails_before_query_service_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let model = model();
    let index = EmbeddingIndex::build(&snapshot, model.as_ref())
        .await
        .unwrap();
    assert!(index.validate(&snapshot, "different-tenant").is_err());
    let vector = VectorRetriever::new(index, model.clone(), &snapshot).unwrap();
    store
        .remove("vehicle", Some(snapshot.revision))
        .await
        .unwrap();
    assert!(search(&store, &vector, &RetrievalQuery::new("automobile"))
        .await
        .is_err());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn corrupt_vectors_and_unknown_index_versions_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let model = model();
    let original = EmbeddingIndex::build(&snapshot, model.as_ref())
        .await
        .unwrap();
    for value in [
        vec![0.0, 0.0],
        vec![f32::NAN, 1.0],
        vec![1.0],
        vec![f32::INFINITY, 0.0],
    ] {
        let mut index = original.clone();
        *index.vectors.values_mut().next().unwrap() = value;
        assert!(index.validate(&snapshot, &model.identity()).is_err());
    }
    let mut index = original.clone();
    index.version = 2;
    assert!(index.validate(&snapshot, &model.identity()).is_err());
    let mut index = original.clone();
    index
        .vectors
        .remove(index.vectors.keys().next().unwrap().clone().as_str());
    assert!(index.validate(&snapshot, &model.identity()).is_err());
}

struct Ranking(Vec<ScoredChunk>);
impl Retriever for Ranking {
    fn retrieve<'a>(
        &'a self,
        _: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}
#[tokio::test]
async fn identifier_constraints_apply_to_any_ranking_and_preserve_hidden_metadata_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(dir.path());
    for (id, body) in [
        ("exact", "Order AB123456 is pending."),
        ("generic", "An operation may be pending."),
    ] {
        ingest(
            &store,
            Document {
                id: id.into(),
                title: id.into(),
                source: "fixture:identifiers".into(),
                text: body.into(),
                metadata: [
                    ("tags".into(), "XY987654".into()),
                    ("body_sha256".into(), "f1b8a7c9de0a4455".into()),
                ]
                .into_iter()
                .collect(),
            },
            &WindowChunker::default(),
            None,
        )
        .await
        .unwrap();
    }
    let snapshot = store.snapshot().await.unwrap();
    let ranking = snapshot
        .chunks
        .iter()
        .map(|c| ScoredChunk {
            chunk_id: c.id.clone(),
            score: 0.99,
        })
        .collect();
    let base: Arc<dyn Retriever> = Arc::new(Ranking(ranking));
    let filter =
        IdentifierConstrainedRetriever::new(base.clone(), ["tags".into()].into_iter().collect())
            .unwrap();
    assert!(filter.identity().is_none()); // An unidentified custom backend stays unidentified.
    assert_ne!(
        IdentifierConstrainedRetriever::new(Arc::new(LexicalRetriever), Default::default())
            .unwrap()
            .identity(),
        LexicalRetriever.identity()
    );
    let found = search(
        &store,
        &filter,
        &RetrievalQuery::new("Does order ab123456 have a pending operation?"),
    )
    .await
    .unwrap();
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].chunk.document_id, "exact");
    assert_eq!(
        search(&store, &filter, &RetrievalQuery::new("AB123456 XY987654"))
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    assert!(
        search(&store, &filter, &RetrievalQuery::new("AB123456 ZZ123456"))
            .await
            .unwrap()
            .hits
            .is_empty()
    );
    // No candidate can satisfy this excluded internal hash. Even a failing
    // remote backend is unnecessary and must not be called to establish that.
    let calls = Arc::new(AtomicUsize::new(0));
    let failing = IdentifierConstrainedRetriever::new(
        Arc::new(Failing(calls.clone())),
        ["tags".into()].into_iter().collect(),
    )
    .unwrap();
    assert!(search(
        &store,
        &failing,
        &RetrievalQuery::new("Is operation f1b8a7c9de0a4455 documented?")
    )
    .await
    .unwrap()
    .hits
    .is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(search(&store, &failing, &RetrievalQuery::new("AB123456"))
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let invalid = IdentifierConstrainedRetriever::new(
        Arc::new(Ranking(vec![ScoredChunk {
            chunk_id: "unknown".into(),
            score: 1.0,
        }])),
        ["tags".into()].into_iter().collect(),
    )
    .unwrap();
    assert!(search(&store, &invalid, &RetrievalQuery::new("AB123456"))
        .await
        .is_err());
    assert!(IdentifierConstrainedRetriever::query_identifiers(
        "Qwen3 2026 plainword abcdefgh a1B2c3D4"
    )
    .contains("a1b2c3d4"));
    assert_eq!(
        IdentifierConstrainedRetriever::query_identifiers("Qwen3 2026 plainword abcdefgh").len(),
        0
    );
}

#[tokio::test]
async fn fusion_uses_ranks_and_rejects_invalid_backend_entries_before_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let snapshot = store.snapshot().await.unwrap();
    let left = snapshot.chunks[0].id.clone();
    let right = snapshot.chunks[1].id.clone();
    let lexical = Arc::new(Ranking(vec![
        ScoredChunk {
            chunk_id: left.clone(),
            score: 1e30,
        },
        ScoredChunk {
            chunk_id: right.clone(),
            score: 1.0,
        },
    ]));
    let semantic = Arc::new(Ranking(vec![
        ScoredChunk {
            chunk_id: right.clone(),
            score: 0.02,
        },
        ScoredChunk {
            chunk_id: left.clone(),
            score: 0.01,
        },
    ]));
    let hybrid = HybridRetriever::new(lexical.clone(), semantic)
        .with_fusion(1.0, 2.0, 60.0, 20)
        .unwrap();
    let mut query = RetrievalQuery::new("anything");
    query.top_k = 1;
    assert_eq!(
        search(&store, &hybrid, &query).await.unwrap().hits[0]
            .chunk
            .id,
        right
    );
    for ranking in [
        vec![ScoredChunk {
            chunk_id: "unknown".into(),
            score: 1.0,
        }],
        vec![ScoredChunk {
            chunk_id: left.clone(),
            score: -1.0,
        }],
        vec![ScoredChunk {
            chunk_id: left.clone(),
            score: f64::NAN,
        }],
        vec![
            ScoredChunk {
                chunk_id: left.clone(),
                score: 1.0,
            },
            ScoredChunk {
                chunk_id: left.clone(),
                score: 0.5,
            },
        ],
    ] {
        let hybrid = HybridRetriever::new(lexical.clone(), Arc::new(Ranking(ranking)));
        assert!(search(&store, &hybrid, &query).await.is_err());
    }
    assert!(HybridRetriever::new(lexical.clone(), lexical)
        .with_fusion(0.0, 0.0, 60.0, 20)
        .is_err());
}

struct Failing(Arc<AtomicUsize>);
impl Retriever for Failing {
    fn retrieve<'a>(
        &'a self,
        _: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(RuntimeError::Protocol("service unavailable".into()))
        })
    }
}
#[tokio::test]
async fn semantic_errors_surface_and_zero_weight_performs_no_service_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = corpus(dir.path()).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let failing = Arc::new(Failing(calls.clone()));
    let query = RetrievalQuery::new("car");
    let hybrid = HybridRetriever::new(Arc::new(LexicalRetriever), failing.clone());
    assert!(search(&store, &hybrid, &query).await.is_err());
    let hybrid = HybridRetriever::new(Arc::new(LexicalRetriever), failing)
        .with_fusion(1.0, 0.0, 60.0, 20)
        .unwrap();
    assert_eq!(search(&store, &hybrid, &query).await.unwrap().hits.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct EmbeddingProvider {
    calls: Arc<AtomicUsize>,
    slow: bool,
}
impl taskloom::Provider for EmbeddingProvider {
    fn id(&self) -> &str {
        "fixture:embedding-provider"
    }
    fn complete<'a>(
        &'a self,
        _: &'a taskloom::ChatRequest,
        _: Option<taskloom::Streaming<'a>>,
    ) -> BoxFuture<'a, Result<taskloom::ChatResponse, taskloom::ModelError>> {
        Box::pin(async {
            Err(taskloom::ModelError::misconfigured(
                "embedding fixture only",
            ))
        })
    }
    fn embed<'a>(
        &'a self,
        request: &'a taskloom::EmbeddingRequest,
    ) -> BoxFuture<'a, Result<taskloom::EmbeddingResponse, taskloom::ModelError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.model, "embedding-fixture-v1");
            if self.slow {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Ok(taskloom::EmbeddingResponse {
                vectors: request.inputs.iter().map(|_| vec![1.0, 0.25]).collect(),
                usage: None,
            })
        })
    }
}
#[tokio::test]
async fn provider_adapter_binds_tenant_and_rejects_oversize_before_one_attempt() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(EmbeddingProvider {
        calls: calls.clone(),
        slow: false,
    });
    let adapter = ProviderEmbeddingModel::new(
        provider.clone(),
        "embedding-fixture-v1",
        "tenant-a",
        16,
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let other = ProviderEmbeddingModel::new(
        provider,
        "embedding-fixture-v1",
        "tenant-b",
        16,
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    assert_ne!(adapter.identity(), other.identity());
    let batch = adapter.embed(&["short".into()]).await.unwrap();
    assert_eq!(batch.identity, adapter.identity());
    assert_eq!(batch.vectors, vec![vec![1.0, 0.25]]);
    assert!(adapter.embed(&["x".repeat(17)]).await.is_err());
    assert!(adapter.embed(&vec!["a".into(); 33]).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let slow = ProviderEmbeddingModel::new(
        Arc::new(EmbeddingProvider {
            calls: calls.clone(),
            slow: true,
        }),
        "embedding-fixture-v1",
        "tenant-a",
        16,
        std::time::Duration::from_millis(20),
    )
    .unwrap();
    assert!(slow.embed(&["short".into()]).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
