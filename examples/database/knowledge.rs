use serde_json::{json, Value};
use taskloom::runtime::{database::*, memory::*, rag::*, *};

fn access(tenant: &str, project: &str) -> Result<MemoryAccess, RuntimeError> {
    // Local example arguments are trusted host input, not HTTP ownership claims.
    MemoryAccess::curator(
        MemoryScope {
            tenant: tenant.into(),
            project: project.into(),
            owner: "alice".into(),
        },
        "local-operator",
    )
}
pub async fn snapshot(
    db: &SqlDatabase,
    tenant: &str,
    project: &str,
) -> Result<Value, RuntimeError> {
    let ns = DatabaseNamespace::new(tenant, project)?;
    let memory = db.memory(ns.clone());
    let corpus = db.corpus(ns.clone());
    let manager = MemoryManager::new(&memory);
    let auth = access(tenant, project)?;
    let bob = MemoryAccess::curator(
        MemoryScope {
            owner: "bob".into(),
            ..auth.scope().clone()
        },
        "local-operator",
    )?;
    let sessions = db.sessions(ns.clone());
    let runs = db.runs(ns);
    let source = sessions.load("knowledge-source").await?;
    let source_run = match &source {
        Some(session) => match session.turns.first() {
            Some(turn) => runs.inspect(&turn.binding.run_id).await?,
            None => None,
        },
        None => None,
    };
    Ok(json!({"corpus":corpus.snapshot().await?,
        "retrieval":search(&corpus,&LexicalRetriever,&RetrievalQuery::new("Zephyr retention")).await?,
        "memory":manager.snapshot(&auth).await?,"recall":manager.recall(&auth,"reply language",&Default::default()).await?,
        "other_owner":manager.snapshot(&bob).await?,"session":source,"source_run":source_run}))
}
pub async fn run(
    db: &SqlDatabase,
    tenant: &str,
    project: &str,
    agent: &AgentLoop<'_>,
) -> Result<(), RuntimeError> {
    let ns = DatabaseNamespace::new(tenant, project)?;
    let corpus = db.corpus(ns.clone());
    ingest(
        &corpus,
        Document {
            id: "retention".into(),
            title: "Zephyr retention policy".into(),
            source: "fixture://source-admitted-by-local-host".into(),
            text: "Retain approved audit events for thirty days. Unicode 原文 remains exact."
                .into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        Some(0),
    )
    .await?;
    let sessions = db.sessions(ns.clone());
    let runs = db.runs(ns.clone());
    let manager = SessionManager::new(&sessions);
    manager
        .create(
            "knowledge-source",
            vec!["Acknowledge in one short sentence. No tools.".into()],
        )
        .await?;
    let source = manager
        .run_turn(
            &DurableAgent::new(agent, &runs),
            "knowledge-source",
            "first",
            "I prefer English replies.",
        )
        .await?;
    if source.run.report.stop_reason != StopReason::Completed {
        return Err(RuntimeError::Protocol("native source turn failed".into()));
    }
    let memory = db.memory(ns);
    let memories = MemoryManager::new(&memory);
    let auth = access(tenant, project)?;
    let extraction = memories
        .extract_from_turn(
            &auth,
            &source.session,
            &MemoryExtractionRequest {
                expected_revision: 0,
                turn_id: "first".into(),
                request_id: "native-extract".into(),
            },
            agent,
            &runs,
        )
        .await?;
    // Retain original model extraction separately from host review/edit actions.
    println!(
        "{}",
        json!({"stage":"native_extraction","result":extraction})
    );
    let record = extraction
        .memory
        .records
        .values()
        .find(|r| r.kind == MemoryKind::Preference && r.value.to_lowercase().contains("english"))
        .ok_or_else(|| {
            RuntimeError::Protocol(
                "native model did not extract the declared English preference".into(),
            )
        })?;
    let active = memories
        .review(
            &auth,
            extraction.memory.revision,
            &record.id,
            MemoryReviewDecision {
                accept: true,
                replace: false,
                note: "Local operator verifies the original exact User quote.".into(),
            },
        )
        .await?;
    let other_owner_while_live = memory
        .load(&MemoryScope {
            owner: "bob".into(),
            ..auth.scope().clone()
        })
        .await?;
    let edited = memories
        .edit(
            &auth,
            active.revision,
            &record.id,
            "Bilingual English and Chinese",
            "Explicit local operator edit, not model extraction.",
        )
        .await?;
    let erased = memories
        .forget(&auth, edited.revision, record.kind, &record.key)
        .await?;
    let replay = memories
        .propose_from_turn(
            &auth,
            erased.revision,
            &source.session,
            "first",
            "operator-replay-probe",
            vec![MemoryProposal {
                kind: record.kind,
                key: record.key.clone(),
                value: record.value.clone(),
                quote: record.source.quote.clone(),
            }],
        )
        .await?;
    println!(
        "{}",
        json!({"stage":"operator_lifecycle","active":active,"other_owner_while_live":other_owner_while_live,"edited":edited,"erased":erased,"replay":replay})
    );
    Ok(())
}
