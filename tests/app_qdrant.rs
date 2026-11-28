#![cfg(all(feature = "app", feature = "qdrant", feature = "database"))]
//! Real HTTP and file/SQL billing. Qdrant/embedding responses are protocol
//! fixtures; actual Qdrant CLI acceptance is a separate owned-container run.
use axum::{
    extract::{Path as RoutePath, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use taskloom::runtime::{app::*, database::*, rag::*, *};
const TOKEN: &str = "qdrant-app-local-operator-token";
const KEY: &str = "owned-qdrant-loopback-fixture";
struct Host {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn host(config: &AppConfig) -> Host {
    let app = Application::open(config.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    Host {
        base: format!("http://{}/api", listener.local_addr().unwrap()),
        task: tokio::spawn(async move { serve(listener, app, TOKEN.into()).await.unwrap() }),
    }
}
async fn query(host: &Host, text: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/knowledge/search", host.base))
        .bearer_auth(TOKEN)
        .json(&RetrievalQuery::new(text))
        .send()
        .await
        .unwrap()
}
enum Hook {
    Fence { url: String, namespace: String },
    Source(Box<AppConfig>),
}
#[derive(Default)]
struct VectorState {
    points: Mutex<BTreeMap<String, Value>>,
    requests: AtomicUsize,
    writes: AtomicUsize,
    queries: AtomicUsize,
    fail_write: AtomicBool,
    scopes: Mutex<Vec<String>>,
    hook: Mutex<Option<Hook>>,
}
async fn collection(
    State(state): State<Arc<VectorState>>,
    RoutePath(collection): RoutePath<String>,
    headers: HeaderMap,
) -> Json<Value> {
    assert_eq!(headers["api-key"], KEY);
    state.requests.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"status":"ok","result":{"config":{"params":{"vectors":{"size":if collection=="dimension32" {32}else{2},"distance":"Cosine"}}}}}),
    )
}
async fn upsert(
    State(state): State<Arc<VectorState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(headers["api-key"], KEY);
    state.requests.fetch_add(1, Ordering::SeqCst);
    state.writes.fetch_add(1, Ordering::SeqCst);
    for point in body["points"].as_array().unwrap() {
        state
            .points
            .lock()
            .unwrap()
            .insert(point["id"].as_str().unwrap().into(), point.clone());
    }
    let hook = state.hook.lock().unwrap().take();
    match hook {
        Some(Hook::Fence { url, namespace }) => {
            let pool = sqlx::AnyPool::connect(&url).await.unwrap();
            let changed=sqlx::query("UPDATE taskloom_leases SET owner='explicit-app-qdrant-fence',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='app_object' AND object_id='embedding-index'").bind(namespace).execute(&pool).await.unwrap();
            assert_eq!(changed.rows_affected(), 1);
            pool.close().await;
        }
        Some(Hook::Source(config)) => {
            let stores = AppStores::open(&config).await.unwrap();
            ingest(
                &stores.corpus,
                Document {
                    id: "changed".into(),
                    title: "changed".into(),
                    source: "fixture:changed".into(),
                    text: "Concurrent source change requires explicit new indexing.".into(),
                    metadata: Default::default(),
                },
                &WindowChunker::default(),
                None,
            )
            .await
            .unwrap();
            stores.close().await;
        }
        None => (),
    }
    if state.fail_write.swap(false, Ordering::SeqCst) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({"status":{"error":"fixture applied write, then lost successful acknowledgement"}}),
            ),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"status":"ok","result":{"operation_id":1,"status":"completed"}})),
    )
}
async fn vectors(
    State(state): State<Arc<VectorState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["api-key"], KEY);
    state.requests.fetch_add(1, Ordering::SeqCst);
    state.queries.fetch_add(1, Ordering::SeqCst);
    let must = body["filter"]["must"].as_array().unwrap();
    assert_eq!(must.len(), 5);
    let scope = must.iter().find(|v| v["key"] == "taskloom_scope").unwrap()["match"]["value"]
        .as_str()
        .unwrap()
        .to_owned();
    state.scopes.lock().unwrap().push(scope);
    let points = state
        .points
        .lock()
        .unwrap()
        .values()
        .filter(|point| {
            must.iter()
                .all(|p| point["payload"][p["key"].as_str().unwrap()] == p["match"]["value"])
        })
        .take(body["limit"].as_u64().unwrap() as usize)
        .map(|p| json!({"id":p["id"],"score":0.9,"payload":p["payload"]}))
        .collect::<Vec<_>>();
    Json(json!({"status":"ok","result":{"points":points}}))
}
struct Embeddings {
    path: PathBuf,
    sql: Option<(SqlDatabase, DatabaseNamespace)>,
    calls: AtomicUsize,
    missing_usage: AtomicBool,
    delay: AtomicBool,
    dimensions: usize,
}
async fn embed(State(state): State<Arc<Embeddings>>, Json(body): Json<Value>) -> Json<Value> {
    let ledger: Value = if let Some((db, scope)) = &state.sql {
        db.objects(scope.clone())
            .load("embedding-spend")
            .await
            .unwrap()
            .unwrap()
    } else {
        serde_json::from_slice(&std::fs::read(state.path.join("retrieval/spend.json")).unwrap())
            .unwrap()
    };
    assert!(ledger["reserved_or_unknown_micros"].as_u64().unwrap() >= 10);
    let before = state.calls.fetch_add(1, Ordering::SeqCst);
    assert!(ledger["requests"].as_u64().unwrap() > before as u64);
    if state.delay.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let mut value = json!({"data":body["input"].as_array().unwrap().iter().enumerate().map(|(index,_)|json!({"index":index,"embedding":vec![1.0;state.dimensions]})).collect::<Vec<_>>()});
    if !state.missing_usage.load(Ordering::SeqCst) {
        value["usage"] = json!({"prompt_tokens":3,"total_tokens":3});
    }
    Json(value)
}
struct Project {
    config: AppConfig,
    embedding: Arc<Embeddings>,
    server: Host,
    key_env: String,
    db_env: Option<String>,
}
impl Drop for Project {
    fn drop(&mut self) {
        std::env::remove_var(&self.key_env);
        if let Some(name) = &self.db_env {
            std::env::remove_var(name);
        }
    }
}
async fn project(
    root: &Path,
    vector_url: &str,
    qkey: &str,
    database: Option<(&SqlDatabase, &str)>,
    dimensions: usize,
) -> Project {
    std::fs::create_dir_all(root).unwrap();
    let project = uuid::Uuid::new_v4().simple().to_string();
    let key_env = format!("TASKLOOM_APP_QDRANT_EMBED_{project}");
    let db_env = database.map(|_| format!("TASKLOOM_APP_QDRANT_DB_{project}"));
    if let (Some((_, url)), Some(name)) = (database, &db_env) {
        std::env::set_var(name, url);
    }
    let state = Arc::new(Embeddings {
        path: root.into(),
        sql: database.map(|(db, _)| {
            (
                db.clone(),
                DatabaseNamespace::new("qdrant-app", &project).unwrap(),
            )
        }),
        calls: AtomicUsize::new(0),
        missing_usage: AtomicBool::new(false),
        delay: AtomicBool::new(false),
        dimensions,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let cloned = state.clone();
    let endpoint = Host {
        base: base.clone(),
        task: tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/embeddings", post(embed))
                    .with_state(cloned),
            )
            .await
            .unwrap()
        }),
    };
    let config = AppConfig {
        state_dir: root.into(),
        database: db_env.as_ref().map(|name| AppDatabase {
            url_env: name.clone(),
            tenant: "qdrant-app".into(),
            project: project.clone(),
            max_connections: 4,
            max_record_bytes: 64 * 1024 * 1024,
            lease_ms: 10_000,
        }),
        retrieval: AppRetrieval {
            embedding: Some(AppEmbedding {
                provider: AppProvider::Openai {
                    base_url: base,
                    key_env: key_env.clone(),
                    namespace: format!("protocol-fixture-{project}"),
                },
                model: "app-qdrant-fixture".into(),
                currency: "USD".into(),
                input_micros_per_million_tokens: 1_000_000,
                max_request_cost_micros: 10,
                max_total_cost_micros: 1000,
                max_input_bytes: 128 * 1024,
                timeout_ms: 5000,
            }),
            qdrant: Some(QdrantConfig {
                endpoint: vector_url.into(),
                collection: if dimensions == 32 {
                    "dimension32"
                } else {
                    "host_collection"
                }
                .into(),
                namespace: "same-declared-host-namespace".into(),
                api_key_env: Some(qkey.into()),
                timeout_ms: 5000,
                max_response_bytes: 1024 * 1024,
                max_request_bytes: if dimensions == 32 { 1024 } else { 1024 * 1024 },
                batch_points: 1,
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    Project {
        config,
        embedding: state,
        server: endpoint,
        key_env,
        db_env,
    }
}
async fn ingest_sources(config: &AppConfig) {
    let stores = AppStores::open(config).await.unwrap();
    for (id, text) in [
        (
            "alpha",
            "Source alpha includes exact invoice AB-2345 for the local owner.",
        ),
        ("beta", "Source beta keeps distinct original evidence."),
    ] {
        ingest(
            &stores.corpus,
            Document {
                id: id.into(),
                title: id.into(),
                source: format!("fixture:{id}"),
                text: text.into(),
                metadata: Default::default(),
            },
            &WindowChunker::default(),
            None,
        )
        .await
        .unwrap();
    }
    stores.close().await;
}
async fn build(config: &AppConfig) -> Value {
    let before = retrieval::index_status(config).await.unwrap();
    retrieval::build_index(
        config,
        before["corpus_revision"].as_u64().unwrap(),
        before["configuration_binding"].as_str().unwrap(),
    )
    .await
    .unwrap();
    retrieval::index_status(config).await.unwrap()
}
async fn publish(config: &AppConfig, status: &Value) -> Result<Value, RuntimeError> {
    retrieval::publish_qdrant_index(
        config,
        status["corpus_revision"].as_u64().unwrap(),
        status["configuration_binding"].as_str().unwrap(),
        status["index_binding"].as_str().unwrap(),
    )
    .await
}
fn journal(config: &AppConfig) -> PathBuf {
    let root = config.state_dir.join("retrieval/qdrant");
    let entries = std::fs::read_dir(root)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    entries[0].path().join("publish.json")
}

#[test]
fn qdrant_opt_in_is_omitted_and_unsupported_hybrid_controls_are_rejected() {
    assert!(serde_json::to_value(AppRetrieval::default())
        .unwrap()
        .get("qdrant")
        .is_none());
}
async fn controls(root: &Path, database: Option<(&SqlDatabase, &str)>) {
    let vector_state = Arc::new(VectorState::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vector_url = format!("http://{}/", listener.local_addr().unwrap());
    let state = vector_state.clone();
    let vector_host = Host {
        base: vector_url.clone(),
        task: tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/collections/{collection}", get(collection))
                    .route(
                        "/collections/{collection}/points",
                        axum::routing::put(upsert),
                    )
                    .route("/collections/{collection}/points/query", post(vectors))
                    .with_state(state),
            )
            .await
            .unwrap()
        }),
    };
    let qkey = format!("TASKLOOM_APP_QDRANT_KEY_{}", uuid::Uuid::new_v4().simple());
    let main = project(&root.join("main"), &vector_url, &qkey, database, 2).await;
    ingest_sources(&main.config).await;
    preview_configuration(main.config.clone()).unwrap();
    let cold = retrieval::index_status(&main.config).await.unwrap();
    assert_eq!(cold["qdrant"]["ready"], false);
    let lexical = host(&main.config).await;
    assert_eq!(query(&lexical, "source").await.status(), 200);
    assert_eq!(main.embedding.calls.load(Ordering::SeqCst), 0);
    assert_eq!(vector_state.requests.load(Ordering::SeqCst), 0);
    drop(lexical);
    std::env::set_var(&main.key_env, "owned-embedding-protocol-fixture");
    let built = build(&main.config).await;
    assert_eq!(main.embedding.calls.load(Ordering::SeqCst), 1);
    assert_eq!(vector_state.requests.load(Ordering::SeqCst), 0);
    let mut hybrid = main.config.clone();
    hybrid.retrieval.mode = RetrievalMode::Hybrid;
    assert!(Application::open(hybrid.clone()).await.is_err());
    assert_eq!(main.embedding.calls.load(Ordering::SeqCst), 1);
    assert_eq!(vector_state.requests.load(Ordering::SeqCst), 0);
    assert!(
        retrieval::publish_qdrant_index(&main.config, 999, "bad", "bad")
            .await
            .is_err()
    );
    assert!(retrieval::publish_qdrant_index(
        &main.config,
        built["corpus_revision"].as_u64().unwrap(),
        built["configuration_binding"].as_str().unwrap(),
        "bad-index"
    )
    .await
    .is_err());
    std::env::set_var(&qkey, KEY);
    publish(&main.config, &built).await.unwrap();
    assert_eq!(vector_state.writes.load(Ordering::SeqCst), 2);
    assert_eq!(main.embedding.calls.load(Ordering::SeqCst), 1);
    std::env::remove_var(&qkey);
    std::env::remove_var(&main.key_env);
    let before = vector_state.requests.load(Ordering::SeqCst);
    let cold = retrieval::index_status(&hybrid).await.unwrap();
    assert_eq!(cold["qdrant"]["ready"], true);
    assert_eq!(vector_state.requests.load(Ordering::SeqCst), before);
    let path = journal(&main.config);
    let original = std::fs::read(&path).unwrap();
    let mut corrupted: Value = serde_json::from_slice(&original).unwrap();
    corrupted["complete"] = json!(true);
    corrupted["batches"][0]["request_sha256"] = json!("0".repeat(64));
    std::fs::write(&path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
    assert_eq!(
        retrieval::index_status(&hybrid).await.unwrap()["qdrant"]["ready"],
        false
    );
    let error = match Application::open(hybrid.clone()).await {
        Err(e) => e,
        Ok(_) => panic!("corrupt receipt accepted"),
    };
    assert!(error.to_string().contains("incomplete"));
    assert_eq!(main.embedding.calls.load(Ordering::SeqCst), 1);
    std::fs::write(&path, original).unwrap();
    std::env::set_var(&qkey, KEY);
    std::env::set_var(&main.key_env, "owned-embedding-protocol-fixture");
    for (prefix, minimum) in [("query instruction", 0), ("", 1)] {
        let mut invalid = hybrid.clone();
        invalid.retrieval.query_prefix = prefix.into();
        invalid.retrieval.semantic_min_lexical_terms = minimum;
        assert!(invalid.validate().is_err());
    }
    hybrid.retrieval.exact_identifiers = true;
    hybrid.retrieval.rerank = Some(AppLexicalRerank {
        candidates: 20,
        max_candidate_bytes: 64 * 1024,
        lexical: LexicalConfig::default(),
    });
    let running = host(&hybrid).await;
    let found: SearchResult = query(&running, "source").await.json().await.unwrap();
    assert_eq!(found.hits.len(), 2);
    assert!(found
        .hits
        .iter()
        .all(|h| h.chunk.source.starts_with("fixture:")));
    let identified: SearchResult = query(&running, "AB-2345").await.json().await.unwrap();
    assert_eq!(identified.hits.len(), 1);
    assert_eq!(identified.hits[0].chunk.document_id, "alpha");
    let writes = vector_state.writes.load(Ordering::SeqCst);
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/retrieval/qdrant/publish", running.base))
            .bearer_auth(TOKEN)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(vector_state.writes.load(Ordering::SeqCst), writes);
    let foreign = project(&root.join("foreign"), &vector_url, &qkey, database, 2).await;
    ingest_sources(&foreign.config).await;
    std::env::set_var(&foreign.key_env, "owned-embedding-protocol-fixture");
    let foreign_built = build(&foreign.config).await;
    publish(&foreign.config, &foreign_built).await.unwrap();
    assert_ne!(
        retrieval::index_status(&main.config).await.unwrap()["qdrant"]["target_binding"],
        retrieval::index_status(&foreign.config).await.unwrap()["qdrant"]["target_binding"]
    );
    let mut foreign_hybrid = foreign.config.clone();
    foreign_hybrid.retrieval.mode = RetrievalMode::Hybrid;
    let foreign_host = host(&foreign_hybrid).await;
    assert_eq!(
        query(&foreign_host, "source")
            .await
            .json::<SearchResult>()
            .await
            .unwrap()
            .hits
            .len(),
        2
    );
    assert_eq!(
        query(&running, "source")
            .await
            .json::<SearchResult>()
            .await
            .unwrap()
            .hits
            .len(),
        2
    );
    let scopes = vector_state.scopes.lock().unwrap().clone();
    assert_ne!(scopes[scopes.len() - 1], scopes[scopes.len() - 2]);
    let before = retrieval::index_status(&main.config).await.unwrap()["embedding_spend"].clone();
    main.embedding.missing_usage.store(true, Ordering::SeqCst);
    assert_eq!(query(&running, "source").await.status(), 200);
    let after = retrieval::index_status(&main.config).await.unwrap()["embedding_spend"].clone();
    assert_eq!(
        after["reserved_or_unknown_micros"].as_u64().unwrap(),
        before["reserved_or_unknown_micros"].as_u64().unwrap() + 10
    );
    assert_eq!(after["settled_micros"], before["settled_micros"]);
    let unknown = project(&root.join("unknown"), &vector_url, &qkey, database, 2).await;
    ingest_sources(&unknown.config).await;
    std::env::set_var(&unknown.key_env, "owned-embedding-protocol-fixture");
    let built = build(&unknown.config).await;
    vector_state.fail_write.store(true, Ordering::SeqCst);
    assert!(publish(&unknown.config, &built).await.is_err());
    let writes = vector_state.writes.load(Ordering::SeqCst);
    let bytes = std::fs::read(journal(&unknown.config)).unwrap();
    assert!(publish(&unknown.config, &built).await.is_err());
    assert_eq!(vector_state.writes.load(Ordering::SeqCst), writes);
    assert_eq!(std::fs::read(journal(&unknown.config)).unwrap(), bytes);
    assert_eq!(
        retrieval::index_status(&unknown.config).await.unwrap()["qdrant"]["ready"],
        false
    );
    let mut falsely_complete: Value = serde_json::from_slice(&bytes).unwrap();
    falsely_complete["complete"] = json!(true);
    std::fs::write(
        journal(&unknown.config),
        serde_json::to_vec(&falsely_complete).unwrap(),
    )
    .unwrap();
    assert_eq!(
        retrieval::index_status(&unknown.config).await.unwrap()["qdrant"]["ready"],
        false
    );
    std::fs::write(journal(&unknown.config), bytes).unwrap();
    let limited = project(&root.join("limited"), &vector_url, &qkey, database, 32).await;
    ingest_sources(&limited.config).await;
    std::env::set_var(&limited.key_env, "owned-embedding-protocol-fixture");
    let built = build(&limited.config).await;
    publish(&limited.config, &built).await.unwrap();
    let mut limited_hybrid = limited.config.clone();
    limited_hybrid.retrieval.mode = RetrievalMode::Hybrid;
    let limited_host = host(&limited_hybrid).await;
    let before = limited.embedding.calls.load(Ordering::SeqCst);
    let requests = vector_state.requests.load(Ordering::SeqCst);
    assert_eq!(query(&limited_host, "source").await.status(), 400);
    assert_eq!(limited.embedding.calls.load(Ordering::SeqCst), before);
    assert_eq!(vector_state.requests.load(Ordering::SeqCst), requests);
    let mut timeout = project(&root.join("timeout"), &vector_url, &qkey, database, 2).await;
    timeout
        .config
        .retrieval
        .embedding
        .as_mut()
        .unwrap()
        .timeout_ms = 100;
    ingest_sources(&timeout.config).await;
    std::env::set_var(&timeout.key_env, "owned-embedding-protocol-fixture");
    let built = build(&timeout.config).await;
    publish(&timeout.config, &built).await.unwrap();
    let mut timeout_hybrid = timeout.config.clone();
    timeout_hybrid.retrieval.mode = RetrievalMode::Hybrid;
    let timeout_host = host(&timeout_hybrid).await;
    timeout.embedding.delay.store(true, Ordering::SeqCst);
    let before = vector_state.queries.load(Ordering::SeqCst);
    assert!(!query(&timeout_host, "source").await.status().is_success());
    assert_eq!(timeout.embedding.calls.load(Ordering::SeqCst), 2);
    assert_eq!(vector_state.queries.load(Ordering::SeqCst), before);
    assert_eq!(
        retrieval::index_status(&timeout.config).await.unwrap()["embedding_spend"]
            ["reserved_or_unknown_micros"],
        10
    );
    let fenced = project(&root.join("fenced"), &vector_url, &qkey, database, 2).await;
    ingest_sources(&fenced.config).await;
    std::env::set_var(&fenced.key_env, "owned-embedding-protocol-fixture");
    let built = build(&fenced.config).await;
    *vector_state.hook.lock().unwrap() = Some(if let Some((_, url)) = database {
        let db = fenced.config.database.as_ref().unwrap();
        Hook::Fence {
            url: url.into(),
            namespace: serde_json::to_string(&(&db.tenant, &db.project)).unwrap(),
        }
    } else {
        Hook::Source(Box::new(fenced.config.clone()))
    });
    let writes = vector_state.writes.load(Ordering::SeqCst);
    assert!(publish(&fenced.config, &built).await.is_err());
    assert_eq!(vector_state.writes.load(Ordering::SeqCst), writes + 1);
    assert_eq!(
        retrieval::index_status(&fenced.config).await.unwrap()["qdrant"]["ready"],
        false
    );
    drop((
        running,
        foreign_host,
        limited_host,
        timeout_host,
        vector_host,
    ));
    std::env::remove_var(qkey);
    // Retain the endpoint handles until assertions above finish.
    assert!(!main.server.base.is_empty());
}
#[tokio::test]
async fn file_app_qdrant_scoping_billing_receipts_and_authorized_publication() {
    let dir = tempfile::tempdir().unwrap();
    controls(dir.path(), None).await;
}
#[tokio::test]
async fn sqlite_app_qdrant_scoping_billing_receipts_and_authorized_publication() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("qdrant.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(dir.path(), Some((&db, &url))).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires owned TASKLOOM_TEST_POSTGRES_URL; actual PostgreSQL and loopback protocol fixtures"]
async fn postgres_app_qdrant_scoping_billing_receipts_and_authorized_publication() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PostgreSQL required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(dir.path(), Some((&db, &url))).await;
    db.close().await;
}
