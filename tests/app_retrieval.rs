#![cfg(feature = "app")]
//! Real HTTP and filesystem integration using a protocol fixture, NOT a
//! learned embedding model or retrieval-quality benchmark.
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use taskloom::runtime::{app::*, rag::*, *};

const TOKEN: &str = "retrieval-test-operator-token";
#[derive(Clone)]
struct Endpoint {
    root: std::path::PathBuf,
    calls: Arc<AtomicUsize>,
    usage: Option<u64>,
    delay: bool,
}
async fn embeddings(State(state): State<Endpoint>, Json(body): Json<Value>) -> Json<Value> {
    // A request must never reach the provider ahead of its durable reserve.
    let ledger: Value =
        serde_json::from_slice(&std::fs::read(state.root.join("retrieval/spend.json")).unwrap())
            .unwrap();
    assert!(ledger["requests"].as_u64().unwrap() > state.calls.load(Ordering::SeqCst) as u64);
    assert!(ledger["reserved_or_unknown_micros"].as_u64().unwrap() >= 10);
    state.calls.fetch_add(1, Ordering::SeqCst);
    if state.delay {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(body["model"], "fixture-embedding");
    let vectors = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, _)| json!({"index":i,"embedding":[1.0,0.5]}))
        .collect::<Vec<_>>();
    let mut response = json!({"data":vectors});
    if let Some(n) = state.usage {
        response["usage"] = json!({"prompt_tokens":n,"total_tokens":n});
    }
    Json(response)
}
async fn setup(
    usage: Option<u64>,
    delay: bool,
) -> (
    tempfile::TempDir,
    AppConfig,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let state = Endpoint {
        root: dir.path().into(),
        calls: calls.clone(),
        usage,
        delay,
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/embeddings", post(embeddings))
                .with_state(state),
        )
        .await
        .unwrap()
    });
    // Dummy credential for this loopback fixture, never an external credential.
    let key_env = format!(
        "TASKLOOM_TEST_EMBED_{}",
        dir.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
    );
    std::env::set_var(&key_env, "loopback-fixture-key");
    let config = AppConfig {
        state_dir: dir.path().into(),
        retrieval: AppRetrieval {
            embedding: Some(AppEmbedding {
                provider: AppProvider::Openai {
                    base_url,
                    key_env,
                    namespace: "protocol-fixture".into(),
                },
                model: "fixture-embedding".into(),
                currency: "USD".into(),
                input_micros_per_million_tokens: 1_000_000,
                max_request_cost_micros: 10,
                max_total_cost_micros: 30,
                max_input_bytes: 128 * 1024,
                timeout_ms: if delay { 100 } else { 5000 },
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    ingest(
        &FileCorpusStore::new(dir.path().join("knowledge")),
        Document {
            id: "policy".into(),
            title: "Recovery policy".into(),
            source: "fixture:policy".into(),
            text: "Interrupted writes require reconciliation.".into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        Some(0),
    )
    .await
    .unwrap();
    (dir, config, calls, server)
}
async fn build(config: &AppConfig) -> Result<Value, RuntimeError> {
    let status = retrieval::index_status(config).await?;
    retrieval::build_index(
        config,
        status["corpus_revision"].as_u64().unwrap(),
        status["configuration_binding"].as_str().unwrap(),
    )
    .await
}
async fn host(config: AppConfig) -> (Arc<Application>, String, tokio::task::JoinHandle<()>) {
    let app = Application::open(config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api", listener.local_addr().unwrap());
    let cloned = app.clone();
    let server = tokio::spawn(async move { serve(listener, cloned, TOKEN.into()).await.unwrap() });
    (app, url, server)
}
async fn query(url: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{url}/knowledge/search"))
        .bearer_auth(TOKEN)
        .json(&RetrievalQuery::new("no lexical overlap"))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn explicit_index_then_hybrid_operator_and_model_share_frozen_binding() {
    let (_dir, mut cfg, calls, provider) = setup(Some(3), false).await;
    let initial = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(initial["index_present"], false);
    assert!(
        retrieval::build_index(&cfg, 0, initial["configuration_binding"].as_str().unwrap())
            .await
            .is_err()
    );
    assert!(retrieval::build_index(&cfg, 1, "stale-config")
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (_lexical, lexical_url, lexical_server) = host(cfg.clone()).await;
    let response = reqwest::Client::new()
        .post(format!("{lexical_url}/retrieval/index"))
        .bearer_auth(TOKEN)
        .json(&json!({"expected_revision":1,"expected_binding":initial["configuration_binding"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["restart_required"],
        true
    );
    lexical_server.abort();
    cfg.retrieval.mode = RetrievalMode::Hybrid;
    let (app, url, server) = host(cfg.clone()).await;
    let response = query(&url).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["hits"][0]["chunk"]["document_id"],
        "policy"
    );
    app.start("native-loop-fixture", "no lexical overlap")
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while app.job("native-loop-fixture").unwrap().active {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3); // index + operator + native tool
    let state = app.inspect("native-loop-fixture").await.unwrap();
    let cap: Value = reqwest::Client::new()
        .get(format!("{url}/research/capabilities"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(state.config.tools[0]
        .description
        .contains(cap["retrieval_binding"].as_str().unwrap()));
    let status = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(status["embedding_spend"]["settled_micros"], 9);
    assert_eq!(status["embedding_spend"]["reserved_or_unknown_micros"], 0);
    assert!(app.verified_report("native-loop-fixture").await.is_ok());
    // Changing sources invalidates active and reopened indexes before payment.
    app.corpus.remove("policy", Some(1)).await.unwrap();
    assert_eq!(query(&url).await.status(), 400);
    assert!(Application::open(cfg).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    provider.abort();
    server.abort();
}

#[tokio::test]
async fn missing_usage_and_restart_keep_reservations_until_cap_blocks_dispatch() {
    let (_dir, mut cfg, calls, provider) = setup(None, false).await;
    cfg.retrieval
        .embedding
        .as_mut()
        .unwrap()
        .max_total_cost_micros = 20;
    build(&cfg).await.unwrap();
    cfg.retrieval.mode = RetrievalMode::Hybrid;
    let (_app, url, server) = host(cfg.clone()).await;
    assert_eq!(query(&url).await.status(), 200);
    server.abort();
    let (_app, url, server) = host(cfg.clone()).await;
    assert_eq!(query(&url).await.status(), 400);
    let status = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(status["embedding_spend"]["reserved_or_unknown_micros"], 20);
    assert_eq!(status["embedding_spend"]["usage_reported_requests"], 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    provider.abort();
    server.abort();
}

#[tokio::test]
async fn timeout_is_not_retried_and_reservation_survives_failed_build() {
    let (_dir, cfg, calls, provider) = setup(Some(3), true).await;
    assert!(build(&cfg).await.is_err());
    let status = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(status["index_present"], false);
    assert_eq!(status["embedding_spend"]["reserved_or_unknown_micros"], 10);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    provider.abort();
}

#[tokio::test]
async fn reported_cost_overrun_blocks_future_requests_including_after_restart() {
    let (_dir, cfg, calls, provider) = setup(Some(11), false).await;
    assert!(build(&cfg).await.is_err());
    assert!(build(&cfg).await.is_err());
    let status = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(status["embedding_spend"]["settled_micros"], 11);
    assert_eq!(status["embedding_spend"]["overrun"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    provider.abort();
}

#[tokio::test]
async fn validation_explanation_and_index_auth_do_not_load_credentials_or_dispatch() {
    let (_dir, mut cfg, calls, provider) = setup(Some(3), false).await;
    if let AppProvider::Openai { key_env, .. } =
        &mut cfg.retrieval.embedding.as_mut().unwrap().provider
    {
        *key_env = "TASKLOOM_TEST_EMBED_NONEXISTENT_CREDENTIAL".into();
    }
    cfg.retrieval
        .lexical
        .query_aliases
        .insert("restart".into(), vec!["reconciliation".into()]);
    cfg.retrieval.lexical.stop_words.insert("the".into());
    cfg.retrieval.exact_identifiers = true;
    cfg.retrieval.query_prefix = "Instruct: Retrieve supporting evidence.\nQuery: ".into();
    cfg.retrieval.require_semantic_match = true;
    let (_app, url, server) = host(cfg.clone()).await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(format!("{url}/retrieval/index"))
            .json(&json!({"expected_revision":1,"expected_binding":"bad"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .post(format!("{url}/retrieval/validate"))
        .bearer_auth(TOKEN)
        .json(&cfg.retrieval)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["applied"], false);
    let response = client
        .post(format!("{url}/retrieval/explain"))
        .bearer_auth(TOKEN)
        .json(&RetrievalQuery::new("restart"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(response["lexical_query"], "restart reconciliation");
    assert_eq!(
        response["embedding_query"],
        "Instruct: Retrieve supporting evidence.\nQuery: restart"
    );
    assert_eq!(response["require_semantic_match"], true);
    assert_eq!(
        response["lexical_terms"],
        json!(["reconciliation", "restart"])
    );
    let absent = client
        .post(format!("{url}/knowledge/search"))
        .bearer_auth(TOKEN)
        .json(&RetrievalQuery::new("the unregistered ABC123456 operation"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(absent["hits"].as_array().unwrap().is_empty());
    let response = client
        .post(format!("{url}/knowledge/search"))
        .bearer_auth(TOKEN)
        .json(&RetrievalQuery::new("restart"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(response["hits"][0]["chunk"]["document_id"], "policy");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    provider.abort();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn separate_cli_processes_cannot_spend_the_same_remaining_reservation() {
    let (dir, mut cfg, calls, provider) = setup(None, false).await;
    cfg.retrieval
        .embedding
        .as_mut()
        .unwrap()
        .max_total_cost_micros = 20;
    build(&cfg).await.unwrap(); // ten reserved, ten remaining
    cfg.retrieval.mode = RetrievalMode::Hybrid;
    let path = dir.path().join("config.json");
    std::fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let launch = || async {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_taskloom"))
            .arg("--config")
            .arg(&path)
            .arg("search")
            .arg("no lexical overlap")
            .kill_on_drop(true)
            .output()
            .await
            .unwrap()
    };
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(launch(), launch())
    })
    .await
    .unwrap();
    assert_ne!(first.status.success(), second.status.success());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let status = retrieval::index_status(&cfg).await.unwrap();
    assert_eq!(status["embedding_spend"]["requests"], 2);
    assert_eq!(status["embedding_spend"]["reserved_or_unknown_micros"], 20);
    provider.abort();
}
