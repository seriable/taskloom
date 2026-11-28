#![cfg(feature = "app")]
//! Actual Application HTTP/RagTool chain with fixture reranking, never cloud inference.
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use taskloom::runtime::{app::*, rag::*, *};
const TOKEN: &str = "http-rerank-app-fixture-operator";
#[derive(Clone)]
struct Fixture {
    root: std::path::PathBuf,
    calls: Arc<AtomicUsize>,
    usage: bool,
}
async fn rerank(State(fixture): State<Fixture>, Json(body): Json<Value>) -> Json<Value> {
    let ledger: RerankLedgerSnapshot =
        serde_json::from_slice(&std::fs::read(fixture.root.join("rerank-ledger.json")).unwrap())
            .unwrap();
    let prior = fixture.calls.fetch_add(1, Ordering::SeqCst);
    assert_eq!(ledger.attempts.len(), prior + 1);
    assert!(ledger.attempts.last().unwrap().settled_micros.is_none());
    assert_eq!(body["model"], "app-crossencoder-fixture");
    let results=body["documents"].as_array().unwrap().iter().enumerate().map(|(index,text)|json!({"index":index,"relevance_score":if text.as_str().unwrap().contains("strongest"){0.9}else{0.1}})).collect::<Vec<_>>();
    let mut result = json!({"model":"app-crossencoder-fixture","results":results});
    if fixture.usage {
        result["usage"] = json!({"total_tokens":7});
    }
    Json(result)
}
async fn setup(
    usage: bool,
) -> (
    tempfile::TempDir,
    AppConfig,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/rerank", listener.local_addr().unwrap());
    let ledger = directory.path().join("rerank-ledger");
    let fixture = Fixture {
        root: ledger.clone(),
        calls: calls.clone(),
        usage,
    };
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/rerank", post(rerank))
                .with_state(fixture),
        )
        .await
        .unwrap()
    });
    let key_env = format!(
        "TASKLOOM_APP_RERANK_{}",
        directory
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_uppercase()
    );
    std::env::set_var(&key_env, "app-loopback-reranker-fixture-key");
    let config = AppConfig {
        state_dir: directory.path().join("state"),
        retrieval: AppRetrieval {
            http_rerank: Some(AppHttpRerank {
                candidates: 20,
                max_candidate_bytes: 16384,
                provider: HttpRerankConfig {
                    endpoint,
                    key_env,
                    namespace: "app-loopback-fixture".into(),
                    model: "app-crossencoder-fixture".into(),
                    currency: "USD".into(),
                    input_micros_per_million_tokens: 1_000_000,
                    max_input_bytes: 16384,
                    max_response_bytes: 16384,
                    max_request_cost_micros: 1000,
                    max_total_cost_micros: if usage { 10000 } else { 1000 },
                    max_requests: 10,
                    timeout_ms: 1000,
                    ledger_dir: ledger,
                },
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let store = FileCorpusStore::new(config.state_dir.join("knowledge"));
    for (id, text) in [
        ("weak", "alpha ordinary original source sentence."),
        ("strong", "alpha strongest original source evidence."),
    ] {
        ingest(
            &store,
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
    (directory, config, calls, server)
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
        .json(&RetrievalQuery::new("alpha"))
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn app_api_and_model_ragtool_share_explicit_http_reranker_and_separate_spend() {
    let (_directory, config, calls, provider) = setup(true).await;
    config.validate().unwrap();
    let cfg = &config.retrieval.http_rerank.as_ref().unwrap().provider;
    assert!(!cfg.ledger_dir.join("rerank-ledger.json").exists());
    let (app, url, server) = host(config.clone()).await;
    let response = query(&url).await;
    assert_eq!(response.status(), 200);
    let output: Value = response.json().await.unwrap();
    assert_eq!(output["hits"][0]["chunk"]["document_id"], "strong");
    assert_eq!(
        output["hits"][0]["chunk"]["text"],
        "alpha strongest original source evidence."
    );
    app.start("http-rerank-loop", "alpha").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.job("http-rerank-loop").unwrap().active {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let state = app.inspect("http-rerank-loop").await.unwrap();
    assert_eq!(state.report.stop_reason, StopReason::Completed);
    assert!(app.verified_report("http-rerank-loop").await.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let totals = FileRerankLedger::new(cfg.ledger_dir.clone())
        .unwrap()
        .snapshot(cfg)
        .await
        .unwrap()
        .totals();
    assert_eq!(totals.requests, 2);
    assert_eq!(totals.settled_micros, 14);
    assert_eq!(totals.reserved_or_unknown_micros, 0);
    assert!(!config.state_dir.join("retrieval/spend.json").exists());
    assert!(state.report.steps >= 2);
    assert_eq!(state.report.usage.requests, 0); // Offline fixture reports no provider usage.
    assert_eq!(state.report.usage.cost_micros, 0);
    std::env::remove_var(&cfg.key_env);
    let status = retrieval::index_status(&config).await.unwrap();
    assert_eq!(status["rerank_spend"]["totals"]["settled_micros"], "14");
    assert_eq!(
        status["rerank_spend"]["totals"]["reserved_or_unknown_micros"],
        "0"
    );
    std::env::set_var(&cfg.key_env, "app-loopback-reranker-fixture-key");
    app.shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    server.abort();
    provider.abort();
}
#[tokio::test]
async fn app_failed_usage_remains_unknown_after_restart_and_prevents_more_provider_dispatch() {
    let (_directory, config, calls, provider) = setup(false).await;
    let (app, url, server) = host(config.clone()).await;
    assert_eq!(query(&url).await.status(), 400);
    app.shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    server.abort();
    let (app, url, server) = host(config.clone()).await;
    assert_eq!(query(&url).await.status(), 400);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let cfg = &config.retrieval.http_rerank.as_ref().unwrap().provider;
    let snapshot = FileRerankLedger::new(cfg.ledger_dir.clone())
        .unwrap()
        .snapshot(cfg)
        .await
        .unwrap();
    assert_eq!(snapshot.totals().reserved_or_unknown_micros, 1000);
    assert_eq!(snapshot.totals().requests, 1);
    assert_eq!(snapshot.model, "app-crossencoder-fixture");
    std::env::remove_var(&cfg.key_env);
    let status = retrieval::index_status(&config).await.unwrap();
    assert_eq!(
        status["rerank_spend"]["totals"]["reserved_or_unknown_micros"],
        "1000"
    );
    std::env::set_var(&cfg.key_env, "app-loopback-reranker-fixture-key");
    app.shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    server.abort();
    provider.abort();
}
#[test]
fn http_rerank_configuration_preview_is_pure_and_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.retrieval.http_rerank = Some(AppHttpRerank {
        candidates: 20,
        max_candidate_bytes: 16384,
        provider: HttpRerankConfig {
            endpoint: "https://reviewed.example/rerank".into(),
            key_env: "TASKLOOM_NO_RERANK_CREDENTIAL_FOR_PURE_VALIDATION".into(),
            namespace: "pure".into(),
            model: "explicit-model".into(),
            currency: "USD".into(),
            input_micros_per_million_tokens: 1,
            max_input_bytes: 16384,
            max_response_bytes: 16384,
            max_request_cost_micros: 1,
            max_total_cost_micros: 1,
            max_requests: 1,
            timeout_ms: 1000,
            ledger_dir: directory.path().join("not-created"),
        },
    });
    config.validate().unwrap();
    assert!(!directory.path().join("not-created").exists());
    config.retrieval.rerank = Some(AppLexicalRerank {
        candidates: 20,
        max_candidate_bytes: 16384,
        lexical: LexicalConfig::default(),
    });
    assert!(config.validate().is_err());
}
