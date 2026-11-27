#![cfg(feature = "database")]
//! Actual databases, explicitly fixture model decisions; no semantic benchmark.
use futures_util::future::BoxFuture;
use serde_json::json;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use taskloom::runtime::{database::*, memory::*, rag::*, *};

#[derive(Default)]
struct Extractor(AtomicUsize);
impl Model for Extractor {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(5).await?.settle(3);
            self.0.fetch_add(1, Ordering::SeqCst);
            let extracting = matches!(&request.messages[0],Message::System{content} if content.starts_with("Extract durable user facts"));
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text(if extracting {
                    json!({"memories":[{"kind":"preference","key":"language","value":"English","quote":"I prefer English replies."}]}).to_string()
                } else {
                    "acknowledged".into()
                }),
            })
        })
    }
}
fn namespace(project: &str) -> DatabaseNamespace {
    DatabaseNamespace::new("knowledge-tenant", project).unwrap()
}
fn memory_scope(project: &str, owner: &str) -> MemoryScope {
    MemoryScope {
        tenant: "knowledge-tenant".into(),
        project: project.into(),
        owner: owner.into(),
    }
}
fn document(id: &str, text: &str) -> Document {
    Document {
        id: id.into(),
        title: "Authoritative title only zephyr".into(),
        source: "fixture://knowledge".into(),
        text: text.into(),
        metadata: Default::default(),
    }
}
async fn controls(db: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let ns = namespace(&project);
    let corpus = db.corpus(ns.clone());
    assert_eq!(corpus.snapshot().await.unwrap().revision, 0);
    let initial = ingest(
        &corpus,
        document("one", "Unicode 源文 evidence remains byte exact."),
        &WindowChunker::default(),
        Some(0),
    )
    .await
    .unwrap();
    assert_eq!(initial.revision, 1);
    // Two independent SQL connections contend on the same actual aggregate.
    let cold = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    let second = cold.corpus(ns.clone());
    let chunker = WindowChunker::default();
    let (a, b) = tokio::join!(
        ingest(
            &corpus,
            document("a", "First simultaneous update."),
            &chunker,
            Some(1)
        ),
        ingest(
            &second,
            document("b", "Second simultaneous update."),
            &chunker,
            Some(1)
        )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(
        a.as_ref().err().or(b.as_ref().err()),
        Some(RuntimeError::Conflict(_))
    ));
    let snapshot = second.snapshot().await.unwrap();
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.documents.len(), 2);
    let result = search(&second, &LexicalRetriever, &RetrievalQuery::new("zephyr"))
        .await
        .unwrap();
    assert!(!result.hits.is_empty());
    assert!(db
        .corpus(namespace("other-project"))
        .snapshot()
        .await
        .unwrap()
        .documents
        .is_empty());
    let bad = document("bad", "This text differs from the supplied chunks.");
    assert!(corpus
        .replace(bad, initial.chunks.clone(), Some(2))
        .await
        .is_err());
    assert_eq!(corpus.snapshot().await.unwrap().revision, 2);
    let updated = ingest(
        &second,
        document("one", "Replacement exact UTF-8 更新."),
        &WindowChunker::default(),
        Some(2),
    )
    .await
    .unwrap();
    assert!(!updated.chunks.iter().any(|c| c.text.contains("remains")));
    assert_eq!(updated.revision, 3);
    assert!(corpus.remove("one", Some(2)).await.is_err());
    assert_eq!(corpus.remove("one", Some(3)).await.unwrap().revision, 4);

    let runs = db.runs(ns.clone());
    let sessions = db.sessions(ns.clone());
    let memory = db.memory(ns.clone());
    let auth = MemoryAccess::curator(memory_scope(&project, "alice"), "host-alice").unwrap();
    let model = Extractor::default();
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "fixture").with_limits(RunLimits {
        max_cost_micros: Some(30),
        ..Default::default()
    });
    let manager = SessionManager::new(&sessions);
    manager
        .create("source", vec!["Host-owned system".into()])
        .await
        .unwrap();
    let source = manager
        .run_turn(
            &DurableAgent::new(&agent, &runs),
            "source",
            "one",
            "I prefer English replies.",
        )
        .await
        .unwrap()
        .session;
    let memories = MemoryManager::new(&memory);
    let request = MemoryExtractionRequest {
        expected_revision: 0,
        turn_id: "one".into(),
        request_id: "learn".into(),
    };
    let extracted = memories
        .extract_from_turn(&auth, &source, &request, &agent, &runs)
        .await
        .unwrap();
    assert!(extracted.proposals_recorded);
    assert_eq!(extracted.memory.revision, 1);
    assert_eq!(extracted.run.report.budget.committed_micros, 3);
    let id = extracted.memory.records.keys().next().unwrap().clone();
    let cold_store = cold.memory(ns.clone());
    let cold_manager = MemoryManager::new(&cold_store);
    assert_eq!(
        cold_manager.snapshot(&auth).await.unwrap(),
        extracted.memory
    );
    let reader = MemoryAccess::reader(auth.scope().clone(), "reader").unwrap();
    let decision = MemoryReviewDecision {
        accept: true,
        replace: false,
        note: "verified source".into(),
    };
    assert!(matches!(
        cold_manager.review(&reader, 1, &id, decision.clone()).await,
        Err(RuntimeError::Permission(_))
    ));
    let (a, b) = tokio::join!(
        memories.review(&auth, 1, &id, decision.clone()),
        cold_manager.review(&auth, 1, &id, decision)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let active = cold_manager.snapshot(&auth).await.unwrap();
    assert_eq!(active.revision, 2);
    assert_eq!(
        memories
            .recall(&auth, "language", &Default::default())
            .await
            .unwrap()
            .hits[0]
            .record
            .value,
        "English"
    );
    let bob = MemoryAccess::curator(memory_scope(&project, "bob"), "host-bob").unwrap();
    assert!(cold_manager
        .snapshot(&bob)
        .await
        .unwrap()
        .records
        .is_empty());
    let foreign =
        MemoryAccess::curator(memory_scope("other-project", "alice"), "host-foreign").unwrap();
    assert!(matches!(
        memories.snapshot(&foreign).await,
        Err(RuntimeError::Permission(_))
    ));
    let edited = cold_manager
        .edit(&auth, 2, &id, "Bilingual", "owner clarified")
        .await
        .unwrap();
    assert_eq!(edited.revision, 3);
    assert_eq!(edited.records[&id].status, MemoryStatus::Superseded);
    let erased = memories
        .forget(&auth, 3, MemoryKind::Preference, "language")
        .await
        .unwrap();
    assert!(erased.records.is_empty());
    assert_eq!(erased.revision, 4);
    let replay = cold_manager
        .propose_from_turn(
            &auth,
            4,
            &source,
            "one",
            "different-extractor",
            vec![MemoryProposal {
                kind: MemoryKind::Preference,
                key: "language".into(),
                value: "English again".into(),
                quote: "I prefer English replies.".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(replay, erased, "erasure tombstone survives cold storage");
    let mut tampered = erased.clone();
    tampered.revision += 1;
    tampered.events.pop();
    assert!(memory.save(&tampered, 4).await.is_err());
    assert_eq!(cold_store.load(auth.scope()).await.unwrap(), erased);
    assert_eq!(sessions.load("source").await.unwrap().unwrap(), source);

    sqlx::any::install_default_drivers();
    let admin = sqlx::AnyPool::connect(url).await.unwrap();
    let key = serde_json::to_string(&("knowledge-tenant", &project)).unwrap();
    let body: String = sqlx::query_scalar(
        "SELECT body FROM taskloom_records WHERE namespace=$1 AND family='memory' AND object_id=$2",
    )
    .bind(&key)
    .bind(auth.scope().identity().unwrap())
    .fetch_one(&admin)
    .await
    .unwrap();
    assert!(!body.contains("Bilingual") && !body.contains("English replies"));
    // Simulate a storage transplant into Bob's row. Envelope/scope checks refuse it.
    sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) VALUES($1,'memory',$2,4,$3)")
        .bind(&key).bind(bob.scope().identity().unwrap()).bind(&body).execute(&admin).await.unwrap();
    assert!(cold_store.load(bob.scope()).await.is_err());
    // Revision/body corruption is separately refused, including cold reads.
    sqlx::query(
        "UPDATE taskloom_records SET revision=revision+1 WHERE namespace=$1 AND family='corpus'",
    )
    .bind(&key)
    .execute(&admin)
    .await
    .unwrap();
    assert!(second.snapshot().await.is_err());
    let small = SqlDatabase::connect(
        url,
        DatabaseOptions {
            max_record_bytes: 1024,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let limited = small.corpus(namespace(&uuid::Uuid::new_v4().to_string()));
    assert!(ingest(
        &limited,
        document("large", &"long content ".repeat(500)),
        &WindowChunker::default(),
        Some(0)
    )
    .await
    .is_err());
    assert_eq!(
        limited.snapshot().await.unwrap().revision,
        0,
        "oversize admission rolls back its seed"
    );
    small.close().await;
    admin.close().await;
    cold.close().await;
}

#[tokio::test]
async fn sqlite_knowledge_cas_cold_reads_and_erasure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("knowledge space ü.sqlite");
    let url = SqlDatabase::sqlite_url(&path).unwrap();
    let db = SqlDatabase::sqlite(
        path,
        DatabaseOptions {
            lease_duration: Duration::from_millis(600),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_knowledge_cas_cold_reads_and_erasure() {
    let url =
        std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("explicit owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url).await;
    db.close().await;
}
