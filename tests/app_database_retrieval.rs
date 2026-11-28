#![cfg(all(feature = "app", feature = "database"))]
//! Actual HTTP/SQL controls with fixture embedding vectors and usage. These
//! synthetic vectors are not native relevance or service-price measurements.
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use taskloom::runtime::{app::*, database::*, rag::*};

#[derive(Clone)]
struct Embeddings {
    objects: SqlObjectStore,
    calls: Arc<AtomicUsize>,
    usage: Option<u64>,
    fence: Option<(sqlx::AnyPool, String)>,
}
async fn embed(State(state): State<Embeddings>, Json(body): Json<Value>) -> Json<Value> {
    let ledger = state
        .objects
        .load::<Value>("embedding-spend")
        .await
        .unwrap()
        .unwrap();
    assert!(ledger["requests"].as_u64().unwrap() > state.calls.load(Ordering::SeqCst) as u64);
    assert!(
        ledger["reserved_or_unknown_micros"].as_u64().unwrap() >= 10,
        "durable reserve must precede HTTP dispatch"
    );
    state.calls.fetch_add(1, Ordering::SeqCst);
    if let Some((pool, namespace)) = &state.fence {
        sqlx::query("UPDATE taskloom_leases SET expires_ms=0 WHERE namespace=$1 AND family='app_object' AND object_id='embedding-index'")
            .bind(namespace).execute(pool).await.unwrap();
    }
    let mut reply = json!({"data":body["input"].as_array().unwrap().iter().enumerate().map(|(i,_)|json!({"index":i,"embedding":[1.0,0.5]})).collect::<Vec<_>>()});
    if let Some(tokens) = state.usage {
        reply["usage"] = json!({"prompt_tokens":tokens,"total_tokens":tokens});
    }
    Json(reply)
}
async fn setup(
    db: &SqlDatabase,
    url: &str,
    root: &Path,
    usage: Option<u64>,
    fence: bool,
) -> (AppConfig, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let project = uuid::Uuid::new_v4().to_string();
    let ns = DatabaseNamespace::new("sql-retrieval", &project).unwrap();
    let env = format!("TASKLOOM_SQL_RETRIEVAL_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&env, url);
    let key = format!("TASKLOOM_SQL_EMBED_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&key, "fixture-loopback-key");
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    sqlx::any::install_default_drivers();
    let fence = if fence {
        Some((
            sqlx::AnyPool::connect(url).await.unwrap(),
            serde_json::to_string(&("sql-retrieval", &project)).unwrap(),
        ))
    } else {
        None
    };
    let state = Embeddings {
        objects: db.objects(ns),
        calls: calls.clone(),
        usage,
        fence,
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/embeddings", post(embed))
                .with_state(state),
        )
        .await
        .unwrap();
    });
    let cfg = AppConfig {
        state_dir: root.into(),
        database: Some(AppDatabase {
            url_env: env,
            tenant: "sql-retrieval".into(),
            project,
            max_connections: 4,
            max_record_bytes: 64 * 1024 * 1024,
            lease_ms: 10_000,
        }),
        retrieval: AppRetrieval {
            embedding: Some(AppEmbedding {
                provider: AppProvider::Openai {
                    base_url: endpoint,
                    key_env: key,
                    namespace: "sql-protocol-fixture".into(),
                },
                model: "fixture-embedding".into(),
                currency: "USD".into(),
                input_micros_per_million_tokens: 1_000_000,
                max_request_cost_micros: 10,
                max_total_cost_micros: 30,
                max_input_bytes: 128 * 1024,
                timeout_ms: 5000,
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    (cfg, calls, server)
}
async fn ingest_docs(cfg: &AppConfig, count: usize) {
    let stores = AppStores::open(cfg).await.unwrap();
    for i in 0..count {
        ingest(
            &stores.corpus,
            Document {
                id: format!("doc-{i}"),
                title: "Known policy".into(),
                source: "fixture://policy".into(),
                text: format!("Unicode 原文 policy {i}."),
                metadata: Default::default(),
            },
            &WindowChunker::default(),
            Some(i as u64),
        )
        .await
        .unwrap();
    }
    stores.close().await;
}
async fn build(cfg: &AppConfig) -> Result<Value, taskloom::runtime::RuntimeError> {
    let status = retrieval::index_status(cfg).await?;
    retrieval::build_index(
        cfg,
        status["corpus_revision"].as_u64().unwrap(),
        status["configuration_binding"].as_str().unwrap(),
    )
    .await
}
async fn host(cfg: AppConfig) -> (Arc<Application>, String, tokio::task::JoinHandle<()>) {
    let app = Application::open(cfg).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
    let cloned = app.clone();
    let task = tokio::spawn(async move {
        serve(listener, cloned, "sql-retrieval-token".into())
            .await
            .unwrap();
    });
    (app, endpoint, task)
}
async fn query(url: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{url}/knowledge/search"))
        .bearer_auth("sql-retrieval-token")
        .json(&RetrievalQuery::new("unrelated terms"))
        .send()
        .await
        .unwrap()
}
async fn controls(db: &SqlDatabase, url: &str, root: &Path) {
    let (mut cfg, calls, provider) = setup(db, url, &root.join("normal"), Some(3), false).await;
    ingest_docs(&cfg, 1).await;
    build(&cfg).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        !cfg.state_dir.join("retrieval/spend.json").exists()
            && !cfg.state_dir.join("retrieval/index.json").exists()
    );
    cfg.retrieval.mode = RetrievalMode::Hybrid;
    let (app, api, server) = host(cfg.clone()).await;
    let result = query(&api).await;
    assert_eq!(result.status(), 200);
    let result: Value = result.json().await.unwrap();
    assert!(result["hits"][0]["chunk"]["text"]
        .as_str()
        .unwrap()
        .contains("Unicode 原文"));
    // Actual built-in offline model invokes the same configured hybrid tool.
    app.start("research", "Known policy").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.job("research").unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(app.inspect("research").await.unwrap().report.tool_calls, 1);
    let status = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(status["embedding_spend"]["requests"], 3);
    assert_eq!(status["embedding_spend"]["settled_micros"], 9);
    server.abort();
    let _ = server.await;
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    cfg.retrieval
        .embedding
        .as_mut()
        .unwrap()
        .max_total_cost_micros = 19;
    let (one, one_url, one_server) = host(cfg.clone()).await;
    let (two, two_url, two_server) = host(cfg.clone()).await;
    let (a, b) = tokio::join!(query(&one_url), query(&two_url));
    assert_eq!(
        usize::from(a.status().is_success()) + usize::from(b.status().is_success()),
        1,
        "separate SQL pools must share remaining cap"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let key = match &cfg.retrieval.embedding.as_ref().unwrap().provider {
        AppProvider::Openai { key_env, .. } => key_env.clone(),
        _ => unreachable!(),
    };
    std::env::remove_var(&key);
    let cold = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(cold["embedding_spend"]["requests"], 4);
    assert_eq!(cold["embedding_spend"]["settled_micros"], 12);
    std::env::set_var(&key, "fixture-loopback-key");
    let stores = AppStores::open(&cfg).await.unwrap();
    ingest(
        &stores.corpus,
        Document {
            id: "changed".into(),
            title: "Changed".into(),
            source: "fixture://changed".into(),
            text: "Changed corpus after frozen index.".into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        Some(1),
    )
    .await
    .unwrap();
    stores.close().await;
    assert!(!query(&one_url).await.status().is_success());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "stale index fails before another service dispatch"
    );
    assert!(Application::open(cfg.clone()).await.is_err());
    one_server.abort();
    two_server.abort();
    let _ = one_server.await;
    let _ = two_server.await;
    one.shutdown(Duration::from_secs(1)).await.unwrap();
    two.shutdown(Duration::from_secs(1)).await.unwrap();
    provider.abort();
    let _ = provider.await;
    let (mut unknown, calls, provider) = setup(db, url, &root.join("unknown"), None, false).await;
    unknown
        .retrieval
        .embedding
        .as_mut()
        .unwrap()
        .max_total_cost_micros = 20;
    ingest_docs(&unknown, 1).await;
    build(&unknown).await.unwrap();
    unknown.retrieval.mode = RetrievalMode::Hybrid;
    let (app, api, server) = host(unknown.clone()).await;
    assert_eq!(query(&api).await.status(), 200);
    assert!(!query(&api).await.status().is_success());
    let ledger = retrieval::index_status(&unknown).await.unwrap();
    assert_eq!(ledger["embedding_spend"]["reserved_or_unknown_micros"], 20);
    assert_eq!(ledger["embedding_spend"]["usage_reported_requests"], 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
    let _ = server.await;
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    provider.abort();
    let _ = provider.await;
    let (overrun, calls, provider) = setup(db, url, &root.join("overrun"), Some(20), false).await;
    ingest_docs(&overrun, 1).await;
    assert!(build(&overrun).await.is_err());
    let status = retrieval::index_status(&overrun).await.unwrap();
    assert_eq!(status["embedding_spend"]["overrun"], true);
    assert_eq!(status["embedding_spend"]["settled_micros"], 20);
    assert_eq!(status["index_present"], false);
    assert!(build(&overrun).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    provider.abort();
    let _ = provider.await;
    let (fenced, calls, provider) = setup(db, url, &root.join("fenced"), Some(3), true).await;
    ingest_docs(&fenced, 33).await;
    assert!(build(&fenced).await.is_err());
    let status = retrieval::index_status(&fenced).await.unwrap();
    assert_eq!(status["index_present"], false);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "fenced build must not send the second native protocol batch"
    );
    assert_eq!(status["embedding_spend"]["requests"], 2);
    assert_eq!(status["embedding_spend"]["reserved_or_unknown_micros"], 10);
    provider.abort();
    let _ = provider.await;
}
#[tokio::test]
async fn sqlite_app_index_ledger_concurrent_cap_and_fenced_build() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retrieval.sqlite");
    let db = SqlDatabase::sqlite(&path, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &SqlDatabase::sqlite_url(&path).unwrap(), dir.path()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_app_index_ledger_concurrent_cap_and_fenced_build() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
