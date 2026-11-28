#![cfg(all(feature = "app", feature = "database"))]
//! Real HTTP/SQL billing and owner isolation; vectors are protocol fixtures,
//! never a claim about learned semantic quality or external cloud deployment.
use axum::{extract::State, routing::post, Json, Router};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use taskloom::runtime::{
    app::{host::HostPrivateMemory, multiuser::*, *},
    database::*,
    memory::*,
    *,
};

#[derive(Default)]
struct SourceFixture(AtomicUsize);
impl Model for SourceFixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text("explicit fixture source closed"),
            })
        })
    }
}
enum Action {
    Revoke(String),
    ExpireInFlight,
    Forget(MemoryAccess),
}
struct Endpoint {
    db: SqlDatabase,
    namespace: DatabaseNamespace,
    app: Arc<Application>,
    calls: AtomicUsize,
    inputs: Mutex<Vec<Vec<String>>>,
    action: Mutex<Option<(usize, Action)>>,
    missing_usage: AtomicBool,
}
async fn embeddings(State(state): State<Arc<Endpoint>>, Json(body): Json<Value>) -> Json<Value> {
    let before = state.calls.load(Ordering::SeqCst);
    // Real durable SQL reservation must be present before this owned HTTP
    // provider receives any reviewed text or query.
    let ledger: Value = state
        .db
        .objects(state.namespace.clone())
        .load("embedding-spend")
        .await
        .unwrap()
        .unwrap();
    assert!(ledger["requests"].as_u64().unwrap() > before as u64);
    assert!(ledger["reserved_or_unknown_micros"].as_u64().unwrap() >= 10);
    let call = state.calls.fetch_add(1, Ordering::SeqCst) + 1;
    assert_eq!(body["model"], "semantic-memory-http-fixture");
    let texts: Vec<String> = serde_json::from_value(body["input"].clone()).unwrap();
    state.inputs.lock().unwrap().push(texts.clone());
    let action = {
        let mut slot = state.action.lock().unwrap();
        if slot.as_ref().is_some_and(|(at, _)| *at == call) {
            slot.take().map(|(_, action)| action)
        } else {
            None
        }
    };
    match action {
        Some(Action::Revoke(token)) => {
            state
                .db
                .identities()
                .revoke_token(&token, "security-admin")
                .await
                .unwrap();
        }
        Some(Action::ExpireInFlight) => tokio::time::sleep(Duration::from_millis(1200)).await,
        Some(Action::Forget(access)) => {
            let manager = MemoryManager::new(&state.app.memory);
            let snapshot = manager.snapshot(&access).await.unwrap();
            manager
                .forget(&access, snapshot.revision, MemoryKind::Fact, "fact-00")
                .await
                .unwrap();
        }
        None => (),
    }
    let mut value = json!({"data":texts.iter().enumerate().map(|(index,_)|json!({"index":index,"embedding":[1.0,0.5]})).collect::<Vec<_>>()});
    if !state.missing_usage.load(Ordering::SeqCst) {
        value["usage"] = json!({"prompt_tokens":3,"total_tokens":3});
    }
    Json(value)
}
struct HttpHost {
    base: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for HttpHost {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn serve(
    db: &SqlDatabase,
    app: &Arc<Application>,
    project: &str,
    policy: PrivateMemoryPolicy,
) -> HttpHost {
    let service_router = router_with_private_memory(
        db.identities(),
        vec![ProjectApplication {
            tenant: "semantic".into(),
            project: project.into(),
            application: app.clone(),
        }],
        policy,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/tenants/semantic/projects/{project}/api",
        listener.local_addr().unwrap()
    );
    HttpHost {
        base,
        server: tokio::spawn(async move { axum::serve(listener, service_router).await.unwrap() }),
    }
}
async fn member(db: &SqlDatabase, user: &str, project: &str, role: ProjectRole) -> IssuedToken {
    let identities = db.identities();
    let revision = identities.snapshot().await.unwrap().revision;
    identities
        .put_user(
            ServiceUser {
                id: user.into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "semantic".into(),
                    project: project.into(),
                    role,
                }],
            },
            revision,
            "bootstrap",
        )
        .await
        .unwrap();
    identities
        .issue_token(user, Duration::from_secs(120), "bootstrap")
        .await
        .unwrap()
}
fn semantic() -> SemanticMemoryPolicy {
    SemanticMemoryPolicy {
        max_records: 64,
        max_input_bytes: 64 * 1024,
        min_score: 0.8,
        recall: MemoryRecallPolicy {
            max_items: 20,
            max_bytes: 64 * 1024,
            include_preferences: false,
        },
    }
}
fn policy() -> PrivateMemoryPolicy {
    PrivateMemoryPolicy {
        curate: true,
        semantic: Some(semantic()),
        ..Default::default()
    }
}
fn access(project: &str, user: &str) -> MemoryAccess {
    MemoryAccess::curator(
        MemoryScope {
            tenant: "semantic".into(),
            project: project.into(),
            owner: user.into(),
        },
        user,
    )
    .unwrap()
}
async fn seed(app: &Arc<Application>, project: &str, user: &str, count: usize) -> MemoryAccess {
    let access = access(project, user);
    let session_id = format!("source-{user}");
    let text = "Owned fixture source for each exact proposal.";
    app.create_session(&session_id).await.unwrap();
    app.start_session_turn(&session_id, "one", text)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.session_job(&session_id).unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let source = app.session(&session_id).await.unwrap();
    let manager = MemoryManager::new(&app.memory);
    let mut snapshot = manager.snapshot(&access).await.unwrap();
    let proposals = (0..count + 2)
        .map(|i| MemoryProposal {
            kind: MemoryKind::Fact,
            key: format!("fact-{i:02}"),
            value: format!("{user}-private-value-{i:02}"),
            quote: text.into(),
        })
        .collect::<Vec<_>>();
    for batch in proposals.chunks(16) {
        snapshot = manager
            .propose_from_turn(
                &access,
                snapshot.revision,
                &source,
                "one",
                "trusted-seed",
                batch.to_vec(),
            )
            .await
            .unwrap();
    }
    let ids = snapshot
        .records
        .values()
        .map(|record| (record.key.clone(), record.id.clone()))
        .collect::<Vec<_>>();
    for (key, id) in ids {
        if key == format!("fact-{:02}", count) {
            continue;
        }
        snapshot = manager
            .review(
                &access,
                snapshot.revision,
                &id,
                MemoryReviewDecision {
                    accept: key != format!("fact-{:02}", count + 1),
                    replace: false,
                    note: "explicit trusted fixture review".into(),
                },
            )
            .await
            .unwrap();
    }
    access
}
async fn request(
    client: &reqwest::Client,
    host: &HttpHost,
    token: &IssuedToken,
    body: Value,
) -> reqwest::Response {
    client
        .post(format!("{}/memory/semantic-recall", host.base))
        .bearer_auth(token.bearer.as_str())
        .json(&body)
        .send()
        .await
        .unwrap()
}
async fn ledger(db: &SqlDatabase, namespace: &DatabaseNamespace) -> Value {
    db.objects(namespace.clone())
        .load("embedding-spend")
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn private_semantic_policy_is_default_off_omitted_and_statically_bounded() {
    let legacy: HostPrivateMemory = serde_json::from_value(json!({"curate":true})).unwrap();
    assert!(legacy.semantic.is_none());
    assert!(serde_json::to_value(legacy)
        .unwrap()
        .get("semantic")
        .is_none());
    assert!(PrivateMemoryPolicy::default().semantic.is_none());
    let explicit: HostPrivateMemory =
        serde_json::from_value(json!({"semantic":semantic()})).unwrap();
    assert!(explicit.semantic.is_some());
    let mut invalid = policy();
    invalid.semantic.as_mut().unwrap().max_records = 257;
    assert!(invalid.validate().is_err());
}

async fn controls(db: &SqlDatabase, url: &str, root: &Path) {
    let project = uuid::Uuid::new_v4().simple().to_string();
    let alice_id = format!("alice-{project}");
    let bob_id = format!("bob-{project}");
    let alice = member(db, &alice_id, &project, ProjectRole::Runner).await;
    let bob = member(db, &bob_id, &project, ProjectRole::Operator).await;
    let reader = member(
        db,
        &format!("reader-{project}"),
        &project,
        ProjectRole::Reader,
    )
    .await;
    let foreign = member(
        db,
        &format!("foreign-{project}"),
        "different-project",
        ProjectRole::Operator,
    )
    .await;
    let namespace = DatabaseNamespace::new("semantic", &project).unwrap();
    let database_env = format!("TASKLOOM_SEMANTIC_DB_{project}");
    let key_env = format!("TASKLOOM_SEMANTIC_KEY_{project}");
    std::env::set_var(&database_env, url);
    let embed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model = Arc::new(SourceFixture::default());
    let config = AppConfig {
        state_dir: root.join("state"),
        knowledge: false,
        database: Some(AppDatabase {
            url_env: database_env.clone(),
            tenant: "semantic".into(),
            project: project.clone(),
            max_connections: 4,
            max_record_bytes: 64 * 1024 * 1024,
            lease_ms: 10_000,
        }),
        retrieval: AppRetrieval {
            embedding: Some(AppEmbedding {
                provider: AppProvider::Openai {
                    base_url: format!("http://{}/v1", embed_listener.local_addr().unwrap()),
                    key_env: key_env.clone(),
                    namespace: "semantic-fixture".into(),
                },
                model: "semantic-memory-http-fixture".into(),
                currency: "USD".into(),
                input_micros_per_million_tokens: 1_000_000,
                max_request_cost_micros: 10,
                max_total_cost_micros: 5000,
                max_input_bytes: 128 * 1024,
                timeout_ms: 5000,
            }),
            ..Default::default()
        },
        limits: RunLimits {
            max_cost_micros: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    let app = Application::with_model(
        config.clone(),
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    let alice_access = seed(&app, &project, &alice_id, 33).await;
    let bob_access = seed(&app, &project, &bob_id, 1).await;
    let generation_calls = model.0.load(Ordering::SeqCst);
    let state = Arc::new(Endpoint {
        db: db.clone(),
        namespace: namespace.clone(),
        app: app.clone(),
        calls: AtomicUsize::new(0),
        inputs: Mutex::new(Vec::new()),
        action: Mutex::new(None),
        missing_usage: AtomicBool::new(false),
    });
    let endpoint = state.clone();
    let embed_server = tokio::spawn(async move {
        axum::serve(
            embed_listener,
            Router::new()
                .route("/v1/embeddings", post(embeddings))
                .with_state(endpoint),
        )
        .await
        .unwrap()
    });
    let host = serve(db, &app, &project, policy()).await;
    let client = reqwest::Client::new();
    // Actual host cold validation succeeds with missing embedding credentials.
    app.configure_queue(
        Some(QueueLimits {
            max_open_jobs: 4,
            max_running: 1,
        }),
        0,
        "bootstrap",
        "cold semantic policy check",
    )
    .await
    .unwrap();
    let app_path = root.join("app.json");
    let host_path = root.join("host.json");
    std::fs::write(&app_path, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::write(&host_path,serde_json::to_vec(&json!({"listen":"127.0.0.1:0","database_url_env":database_env,"projects":[{"tenant":"semantic","project":project,"config":app_path}],"private_memory":{"semantic":semantic()}})).unwrap()).unwrap();
    host::check(&host_path).await.unwrap();
    assert!(std::env::var(&key_env).is_err());
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    assert!(db
        .objects(namespace.clone())
        .load::<Value>("embedding-spend")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        request(&client, &host, &alice, json!({"text":"reviewed memories"}))
            .await
            .status(),
        400
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    let disabled = serve(db, &app, &project, PrivateMemoryPolicy::default()).await;
    for (service, enabled) in [(&host, true), (&disabled, false)] {
        let identity: Value = client
            .get(format!("{}/identity", service.base))
            .bearer_auth(alice.bearer.as_str())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(identity["semantic_memory_enabled"], enabled);
    }
    assert_eq!(
        request(
            &client,
            &disabled,
            &alice,
            json!({"text":"reviewed memories"})
        )
        .await
        .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{}/memory/recall", host.base))
            .bearer_auth(reader.bearer.as_str())
            .json(&json!({"text":"private"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    std::env::set_var(&key_env, "owned-loopback-fixture");
    for token in [&reader, &foreign] {
        assert_eq!(
            request(&client, &host, token, json!({"text":"reviewed memories"}))
                .await
                .status(),
            403
        );
    }
    for body in [
        json!({"text":"reviewed memories","owner":alice_id}),
        json!({"text":""}),
        json!({"text":"x".repeat(4097)}),
    ] {
        assert_eq!(request(&client, &host, &bob, body).await.status(), 400);
    }
    assert_eq!(
        client
            .post(format!(
                "{}/memory/semantic-recall?owner={alice_id}",
                host.base
            ))
            .bearer_auth(bob.bearer.as_str())
            .json(&json!({"text":"reviewed memories"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut tiny = policy();
    tiny.semantic.as_mut().unwrap().recall.max_bytes = 512;
    let tiny_host = serve(db, &app, &project, tiny).await;
    assert_eq!(
        request(&client, &tiny_host, &alice, json!({"text":"q".repeat(700)}))
            .await
            .status(),
        400
    );
    let mut over_records = policy();
    over_records.semantic.as_mut().unwrap().max_records = 1;
    let bounded = serve(db, &app, &project, over_records).await;
    assert_eq!(
        request(
            &client,
            &bounded,
            &alice,
            json!({"text":"reviewed memories"})
        )
        .await
        .status(),
        400
    );
    let mut over_bytes = policy();
    over_bytes.semantic.as_mut().unwrap().max_input_bytes = 1;
    let bounded_bytes = serve(db, &app, &project, over_bytes).await;
    assert_eq!(
        request(
            &client,
            &bounded_bytes,
            &alice,
            json!({"text":"reviewed memories"})
        )
        .await
        .status(),
        400
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);

    let expected = MemoryManager::new(&app.memory)
        .snapshot(&alice_access)
        .await
        .unwrap()
        .records
        .values()
        .filter(|r| r.status == MemoryStatus::Active)
        .map(|r| format!("{}\n{}", r.key, r.value))
        .collect::<BTreeSet<_>>();
    let response = request(&client, &host, &alice, json!({"text":"reviewed memories"})).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let result: SemanticMemoryRecall = response.json().await.unwrap();
    assert_eq!(result.memory.scope.owner, alice_id);
    assert!(!result.memory.hits.is_empty());
    assert_eq!(state.calls.load(Ordering::SeqCst), 3); // 32+1 reviewed records, then query.
    let sent = state.inputs.lock().unwrap()[..2]
        .iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(sent, expected);
    assert_eq!(expected.len(), 33);
    for hit in &result.memory.hits {
        assert!(expected.contains(&format!("{}\n{}", hit.record.key, hit.record.value)));
    }
    let settled = ledger(db, &namespace).await;
    assert_eq!(settled["settled_micros"], 9);
    assert_eq!(settled["reserved_or_unknown_micros"], 0);
    let bob_result: SemanticMemoryRecall =
        request(&client, &host, &bob, json!({"text":"reviewed memories"}))
            .await
            .json()
            .await
            .unwrap();
    assert_eq!(bob_result.memory.scope.owner, bob_id);
    assert_eq!(bob_result.memory.hits.len(), 1);
    assert!(!serde_json::to_string(&bob_result)
        .unwrap()
        .contains(&alice_id));
    let before = state.calls.load(Ordering::SeqCst);
    assert_eq!(
        request(&client, &host, &alice, json!({"text":"reviewed memories"}))
            .await
            .status(),
        200
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), before + 3); // Explicit ephemeral rebuild.

    // Revoke during the first document batch: no second batch or query upload.
    let before = state.calls.load(Ordering::SeqCst);
    *state.action.lock().unwrap() = Some((before + 1, Action::Revoke(alice.id.clone())));
    let refused = request(&client, &host, &alice, json!({"text":"reviewed memories"})).await;
    assert_eq!(refused.status(), 403);
    assert!(!refused.text().await.unwrap().contains("private-value"));
    assert_eq!(state.calls.load(Ordering::SeqCst), before + 1);
    let short = db
        .identities()
        .issue_token(&alice_id, Duration::from_secs(1), "bootstrap")
        .await
        .unwrap();
    let before = state.calls.load(Ordering::SeqCst);
    *state.action.lock().unwrap() = Some((before + 1, Action::ExpireInFlight));
    assert_eq!(
        request(&client, &host, &short, json!({"text":"reviewed memories"}))
            .await
            .status(),
        403
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), before + 1);
    let alice = db
        .identities()
        .issue_token(&alice_id, Duration::from_secs(120), "bootstrap")
        .await
        .unwrap();
    let before = state.calls.load(Ordering::SeqCst);
    *state.action.lock().unwrap() = Some((before + 3, Action::Revoke(alice.id.clone())));
    assert_eq!(
        request(&client, &host, &alice, json!({"text":"reviewed memories"}))
            .await
            .status(),
        403
    );
    assert_eq!(state.calls.load(Ordering::SeqCst), before + 3); // Paid query result withheld.
    let alice = db
        .identities()
        .issue_token(&alice_id, Duration::from_secs(120), "bootstrap")
        .await
        .unwrap();
    let before = state.calls.load(Ordering::SeqCst);
    *state.action.lock().unwrap() = Some((before + 3, Action::Forget(alice_access)));
    let stale = request(&client, &host, &alice, json!({"text":"reviewed memories"})).await;
    assert_eq!(stale.status(), 409);
    assert!(!stale.text().await.unwrap().contains("private-value"));
    assert_eq!(state.calls.load(Ordering::SeqCst), before + 3);
    let before = ledger(db, &namespace).await;
    state.missing_usage.store(true, Ordering::SeqCst);
    assert_eq!(
        request(&client, &host, &bob, json!({"text":"reviewed memories"}))
            .await
            .status(),
        200
    );
    let after = ledger(db, &namespace).await;
    assert_eq!(
        after["requests"].as_u64().unwrap(),
        before["requests"].as_u64().unwrap() + 2
    );
    assert_eq!(
        after["reserved_or_unknown_micros"].as_u64().unwrap(),
        before["reserved_or_unknown_micros"].as_u64().unwrap() + 20
    );
    assert_eq!(after["settled_micros"], before["settled_micros"]);
    assert_eq!(
        MemoryManager::new(&app.memory)
            .snapshot(&bob_access)
            .await
            .unwrap()
            .records
            .values()
            .filter(|r| r.status == MemoryStatus::Active)
            .count(),
        1
    );
    assert_eq!(model.0.load(Ordering::SeqCst), generation_calls);
    drop(host);
    drop(disabled);
    drop(tiny_host);
    drop(bounded);
    drop(bounded_bytes);
    embed_server.abort();
    let _ = embed_server.await;
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    std::env::remove_var(key_env);
    std::env::remove_var(database_env);
}
#[tokio::test]
async fn sqlite_explicit_semantic_memory_http_is_scoped_metered_and_revocable() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("semantic.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; actual HTTP/SQL, fixture embeddings"]
async fn postgres_explicit_semantic_memory_http_is_scoped_metered_and_revocable() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PostgreSQL required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
