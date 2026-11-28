#![cfg(feature = "research")]
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;
use taskloom::runtime::{rag::*, RuntimeError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const KEY_NAME: &str = "TASKLOOM_RERANK_FIXTURE_KEY";
const KEY_VALUE: &str = "local-rerank-fixture-token-only";
fn credential() {
    static SET: Once = Once::new();
    SET.call_once(|| std::env::set_var(KEY_NAME, KEY_VALUE));
}
fn config(endpoint: String, root: std::path::PathBuf) -> HttpRerankConfig {
    credential();
    HttpRerankConfig {
        endpoint,
        key_env: KEY_NAME.into(),
        namespace: "fixture-account".into(),
        model: "explicit-rerank-model".into(),
        currency: "USD".into(),
        input_micros_per_million_tokens: 1_000_000,
        max_input_bytes: 16384,
        max_response_bytes: 16384,
        max_request_cost_micros: 1000,
        max_total_cost_micros: 10000,
        max_requests: 10,
        timeout_ms: 1000,
        ledger_dir: root,
    }
}
fn corpus() -> CorpusSnapshot {
    let mut snapshot = CorpusSnapshot::default();
    for (id, text) in [
        ("one", "alpha weak evidence"),
        ("two", "alpha beta strongest original evidence"),
    ] {
        let document = Document {
            id: id.into(),
            title: id.into(),
            source: format!("fixture:{id}"),
            text: text.into(),
            metadata: BTreeMap::new(),
        };
        snapshot
            .chunks
            .push(Chunk::from_range(&document, 0, document.text.len()).unwrap());
        snapshot.documents.insert(id.into(), document);
    }
    snapshot
}
fn candidates(snapshot: &CorpusSnapshot) -> Vec<ScoredChunk> {
    snapshot
        .chunks
        .iter()
        .map(|c| ScoredChunk {
            chunk_id: c.id.clone(),
            score: 1.0,
        })
        .collect()
}
fn good() -> Value {
    json!({"model":"explicit-rerank-model","results":[{"index":0,"relevance_score":0.1},{"index":1,"relevance_score":0.9}],"usage":{"total_tokens":17}})
}
async fn server(
    body: Value,
    status: u16,
    delay: Duration,
    ledger: std::path::PathBuf,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/rerank", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let saved = seen.clone();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 2048];
        let end = loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                break at + 4;
            }
            assert!(bytes.len() < 65536);
        };
        let header = std::str::from_utf8(&bytes[..end]).unwrap();
        assert!(header.starts_with("POST /rerank "));
        assert!(header
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY_VALUE}")));
        let length = header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while bytes.len() < end + length {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let ledger: RerankLedgerSnapshot =
            serde_json::from_slice(&std::fs::read(ledger.join("rerank-ledger.json")).unwrap())
                .unwrap();
        assert_eq!(
            ledger.attempts.len(),
            1,
            "durable reservation must already exist before HTTP reception"
        );
        assert!(ledger.attempts[0].settled_micros.is_none());
        let request: Value = serde_json::from_slice(&bytes[end..end + length]).unwrap();
        assert_eq!(request["model"], "explicit-rerank-model");
        assert_eq!(request["top_n"], 2);
        saved.lock().unwrap().push(request);
        tokio::time::sleep(delay).await;
        let raw = serde_json::to_vec(&body).unwrap();
        let redirect = if status == 302 {
            "Location: http://127.0.0.1:9/never-follow\r\n"
        } else {
            ""
        };
        let header=format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{redirect}\r\n",raw.len());
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(&raw).await;
        // A second hidden retry/redirect to this endpoint would be observable.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    (endpoint, seen, worker)
}
struct Fixed(Vec<ScoredChunk>);
impl Retriever for Fixed {
    fn identity(&self) -> Option<String> {
        Some("fixture-backend".into())
    }
    fn retrieve<'a>(
        &'a self,
        _: &'a CorpusSnapshot,
        _: &'a RetrievalQuery,
    ) -> BoxFuture<'a, Result<Vec<ScoredChunk>, RuntimeError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}
#[tokio::test]
async fn real_http_reservation_precedes_wire_and_usage_precedes_result_validation() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = directory.path().join("ledger");
    let (endpoint, requests, worker) = server(good(), 200, Duration::ZERO, ledger.clone()).await;
    let cfg = config(endpoint, ledger);
    let provider = Arc::new(HttpReranker::new(cfg.clone()).unwrap());
    let snapshot = corpus();
    let wrapper = RerankingRetriever::new(
        Arc::new(Fixed(candidates(&snapshot))),
        provider.clone(),
        2,
        16384,
    )
    .unwrap();
    let store = FileCorpusStore::new(directory.path().join("corpus"));
    for document in snapshot.documents.values() {
        ingest(&store, document.clone(), &WindowChunker::default(), None)
            .await
            .unwrap();
    }
    let mut query = RetrievalQuery::new("alpha beta");
    query.top_k = 1;
    let result = search(&store, &wrapper, &query).await.unwrap();
    assert_eq!(result.hits[0].chunk.document_id, "two");
    let chunk = &result.hits[0].chunk;
    result
        .verify(CitedReport {
            text: format!("[[{}]]", chunk.id),
            citations: vec![Citation {
                chunk_id: chunk.id.clone(),
                quote: chunk.text.clone(),
            }],
        })
        .unwrap();
    worker.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
    let state = provider.ledger().snapshot(&cfg).await.unwrap();
    let totals = state.totals();
    assert_eq!(totals.settled_micros, 17);
    assert_eq!(totals.reserved_or_unknown_micros, 0);
    assert_eq!(totals.requests, 1);
    assert_eq!(state.attempts[0].reported_input_tokens, Some(17));
}
#[tokio::test]
async fn invalid_late_index_duplicates_and_scores_never_hide_reported_usage() {
    for invalid in [
        json!([{"index":0,"relevance_score":1.0},{"index":2,"relevance_score":0.1}]),
        json!([{"index":0,"relevance_score":1.0},{"index":0,"relevance_score":0.2}]),
        json!([{"index":0,"relevance_score":1.0},{"index":1,"relevance_score":-0.1}]),
        json!([{"index":0,"relevance_score":1.0},{"index":1,"relevance_score":"NaN"}]),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let ledger = directory.path().join("ledger");
        let mut body = good();
        body["results"] = invalid;
        let (endpoint, _, worker) = server(body, 200, Duration::ZERO, ledger.clone()).await;
        let cfg = config(endpoint, ledger);
        let provider = HttpReranker::new(cfg.clone()).unwrap();
        let snapshot = corpus();
        let mut query = RetrievalQuery::new("alpha beta");
        query.top_k = 1;
        assert!(provider
            .rerank(&snapshot, &query, &candidates(&snapshot))
            .await
            .is_err());
        worker.await.unwrap();
        let total = provider.ledger().snapshot(&cfg).await.unwrap().totals();
        assert_eq!(total.settled_micros, 17);
        assert_eq!(total.reserved_or_unknown_micros, 0);
    }
}
#[tokio::test]
async fn timeout_cancellation_missing_or_output_billing_retains_unknown_ceiling_without_retry() {
    for usage in [
        Value::Null,
        json!({"total_tokens":1,"completion_tokens":0}),
        json!({"total_tokens":"invalid"}),
        json!({"total_tokens":1,"input_tokens":2}),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let ledger = directory.path().join("ledger");
        let mut body = good();
        body["usage"] = usage;
        let (endpoint, _, worker) = server(body, 200, Duration::ZERO, ledger.clone()).await;
        let cfg = config(endpoint, ledger);
        let provider = HttpReranker::new(cfg.clone()).unwrap();
        let snapshot = corpus();
        assert!(provider
            .rerank(
                &snapshot,
                &RetrievalQuery::new("alpha beta"),
                &candidates(&snapshot)
            )
            .await
            .is_err());
        worker.await.unwrap();
        let total = provider.ledger().snapshot(&cfg).await.unwrap().totals();
        assert_eq!(total.requests, 1);
        assert_eq!(total.reserved_or_unknown_micros, 1000);
        assert_eq!(total.usage_reported_requests, 0);
    }
    let directory = tempfile::tempdir().unwrap();
    let ledger = directory.path().join("timeout");
    let (endpoint, _, worker) =
        server(good(), 200, Duration::from_millis(250), ledger.clone()).await;
    let mut cfg = config(endpoint, ledger);
    cfg.timeout_ms = 50;
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    let snapshot = corpus();
    assert!(provider
        .rerank(
            &snapshot,
            &RetrievalQuery::new("alpha beta"),
            &candidates(&snapshot)
        )
        .await
        .is_err());
    worker.await.unwrap();
    assert_eq!(
        provider
            .ledger()
            .snapshot(&cfg)
            .await
            .unwrap()
            .totals()
            .reserved_or_unknown_micros,
        1000
    );
    let ledger = directory.path().join("cancelled");
    let (endpoint, seen, worker) =
        server(good(), 200, Duration::from_millis(250), ledger.clone()).await;
    let cfg = config(endpoint, ledger);
    let provider = Arc::new(HttpReranker::new(cfg.clone()).unwrap());
    let copy = provider.clone();
    let work = tokio::spawn(async move {
        let snapshot = corpus();
        copy.rerank(
            &snapshot,
            &RetrievalQuery::new("alpha beta"),
            &candidates(&snapshot),
        )
        .await
    });
    for _ in 0..100 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!seen.lock().unwrap().is_empty());
    work.abort();
    assert!(work.await.is_err());
    worker.await.unwrap();
    assert_eq!(
        provider
            .ledger()
            .snapshot(&cfg)
            .await
            .unwrap()
            .totals()
            .reserved_or_unknown_micros,
        1000
    );
}
#[tokio::test]
async fn reported_overrun_is_persisted_and_blocks_further_dispatch_even_with_total_headroom() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = directory.path().join("ledger");
    let mut body = good();
    body["usage"]["total_tokens"] = json!(2000);
    let (endpoint, _, worker) = server(body, 200, Duration::ZERO, ledger.clone()).await;
    let cfg = config(endpoint, ledger);
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    let snapshot = corpus();
    for _ in 0..2 {
        assert!(provider
            .rerank(
                &snapshot,
                &RetrievalQuery::new("alpha beta"),
                &candidates(&snapshot)
            )
            .await
            .is_err());
    }
    worker.await.unwrap();
    let total = provider.ledger().snapshot(&cfg).await.unwrap().totals();
    assert_eq!(total.requests, 1);
    assert_eq!(total.settled_micros, 2000);
    assert!(total.overrun);
}
#[tokio::test]
async fn zero_price_still_counts_requests_and_configuration_cannot_reset_old_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = directory.path().join("ledger");
    let (endpoint, _, worker) = server(good(), 200, Duration::ZERO, ledger.clone()).await;
    let mut cfg = config(endpoint, ledger);
    cfg.input_micros_per_million_tokens = 0;
    cfg.max_request_cost_micros = 0;
    cfg.max_total_cost_micros = 0;
    cfg.max_requests = 1;
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    let snapshot = corpus();
    provider
        .rerank(
            &snapshot,
            &RetrievalQuery::new("alpha beta"),
            &candidates(&snapshot),
        )
        .await
        .unwrap();
    assert!(provider
        .rerank(
            &snapshot,
            &RetrievalQuery::new("alpha beta"),
            &candidates(&snapshot)
        )
        .await
        .is_err());
    worker.await.unwrap();
    assert_eq!(
        provider
            .ledger()
            .snapshot(&cfg)
            .await
            .unwrap()
            .totals()
            .requests,
        1
    );
    let mut changed = cfg.clone();
    changed.max_requests = 100;
    assert!(provider.ledger().snapshot(&changed).await.is_err());
    assert_ne!(
        provider.identity(),
        HttpReranker::new(changed).unwrap().identity()
    );
}
#[tokio::test]
async fn preflight_and_credential_echo_fail_before_reservation_and_redirects_never_follow() {
    let directory = tempfile::tempdir().unwrap();
    let cfg = config(
        "http://127.0.0.1:9/rerank".into(),
        directory.path().join("preflight"),
    );
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    let mut snapshot = corpus();
    let candidates = candidates(&snapshot);
    assert!(provider
        .rerank(&snapshot, &RetrievalQuery::new(KEY_VALUE), &candidates)
        .await
        .is_err());
    snapshot.chunks[0].text.push_str("corrupt");
    assert!(provider
        .rerank(&snapshot, &RetrievalQuery::new("alpha beta"), &candidates)
        .await
        .is_err());
    assert!(provider
        .ledger()
        .snapshot(&cfg)
        .await
        .unwrap()
        .attempts
        .is_empty());
    let ledger = directory.path().join("redirect");
    let (endpoint, seen, worker) = server(
        json!({"error":"redirect"}),
        302,
        Duration::ZERO,
        ledger.clone(),
    )
    .await;
    let cfg = config(endpoint, ledger);
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    let snapshot = corpus();
    assert!(provider
        .rerank(&snapshot, &RetrievalQuery::new("alpha beta"), &candidates)
        .await
        .is_err());
    worker.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        provider
            .ledger()
            .snapshot(&cfg)
            .await
            .unwrap()
            .totals()
            .reserved_or_unknown_micros,
        1000
    );
}
#[test]
#[ignore = "owned subprocess entry exercised by two_processes_share_one_remaining_paid_reservation"]
fn rerank_process_entry() {
    let Ok(path) = std::env::var("TASKLOOM_RERANK_CHILD_CONFIG") else {
        return;
    };
    let cfg: HttpRerankConfig = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let go = std::path::PathBuf::from(std::env::var("TASKLOOM_RERANK_GO").unwrap());
    for _ in 0..5000 {
        if go.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(go.exists());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let admitted = runtime.block_on(async move {
        let provider = HttpReranker::new(cfg).unwrap();
        let snapshot = corpus();
        provider
            .rerank(
                &snapshot,
                &RetrievalQuery::new("alpha beta"),
                &candidates(&snapshot),
            )
            .await
            .is_ok()
    });
    std::fs::write(
        std::env::var("TASKLOOM_RERANK_CHILD_RESULT").unwrap(),
        serde_json::to_vec(&json!({"admitted":admitted})).unwrap(),
    )
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_processes_share_one_remaining_paid_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = directory.path().join("ledger");
    let mut body = good();
    body["usage"]["total_tokens"] = json!(1);
    let (endpoint, seen, worker) =
        server(body, 200, Duration::from_millis(50), ledger.clone()).await;
    let mut cfg = config(endpoint, ledger);
    cfg.input_micros_per_million_tokens = 1;
    cfg.max_request_cost_micros = 1;
    cfg.max_total_cost_micros = 1;
    let config_file = directory.path().join("config.json");
    std::fs::write(&config_file, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let go = directory.path().join("go");
    let mut children = Vec::new();
    for id in 0..2 {
        let output = directory.path().join(format!("result-{id}.json"));
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rerank_process_entry",
                "--ignored",
                "--nocapture",
            ])
            .env("TASKLOOM_RERANK_CHILD_CONFIG", &config_file)
            .env("TASKLOOM_RERANK_CHILD_RESULT", &output)
            .env("TASKLOOM_RERANK_GO", &go)
            .env(KEY_NAME, KEY_VALUE)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        children.push((child, output));
    }
    std::fs::write(go, b"ready").unwrap();
    let mut admitted = 0;
    for (mut child, path) in children {
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if std::time::Instant::now() > until {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned reranking process exceeded deadline");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let result: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        admitted += usize::from(result["admitted"] == true);
    }
    worker.await.unwrap();
    assert_eq!(admitted, 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
    let state = FileRerankLedger::new(cfg.ledger_dir.clone())
        .unwrap()
        .snapshot(&cfg)
        .await
        .unwrap();
    assert_eq!(state.totals().requests, 1);
    assert_eq!(state.totals().settled_micros, 1);
}

#[test]
fn pure_configuration_rejects_remote_plain_http_and_debug_hides_host_credentials_paths() {
    let directory = tempfile::tempdir().unwrap();
    let mut cfg = config(
        "http://public.example/rerank".into(),
        directory.path().join("private-ledger"),
    );
    assert!(cfg.validate().is_err());
    cfg.endpoint = "https://public.example/rerank".into();
    cfg.validate().unwrap();
    let shown = format!("{cfg:?}");
    assert!(!shown.contains("public.example"));
    assert!(!shown.contains(KEY_NAME));
    assert!(!shown.contains("private-ledger"));
    cfg.endpoint = "http://[::1]:1234/rerank".into();
    cfg.validate().unwrap();
    cfg.endpoint = "https://public.example/rerank?secret=value".into();
    assert!(cfg.validate().is_err());
    let attempt = RerankAttempt {
        ordinal: 1,
        request_sha256: "a".repeat(64),
        input_bytes: 1,
        reserved_micros: 1,
        reported_input_tokens: Some(1),
        settled_micros: Some(u64::MAX),
    };
    let state = RerankLedgerSnapshot {
        format: 1,
        binding: "fixture".into(),
        model: "model".into(),
        namespace: "namespace".into(),
        currency: "USD".into(),
        attempts: vec![attempt.clone(), attempt],
    };
    let totals = state.totals();
    assert_eq!(totals.settled_micros, u128::from(u64::MAX) * 2);
    let value = serde_json::to_value(totals).unwrap();
    assert_eq!(
        value["settled_micros"],
        (u128::from(u64::MAX) * 2).to_string()
    );
}
#[tokio::test]
async fn unsuccessful_http_with_valid_usage_is_charged_and_unsupported_other_billing_is_unknown() {
    for (status, extra, expected_known) in [(500, false, true), (200, true, false)] {
        let directory = tempfile::tempdir().unwrap();
        let ledger = directory.path().join("ledger");
        let mut body = good();
        if extra {
            body["billed_units"] = json!({"output_tokens":100});
        }
        let (endpoint, seen, worker) = server(body, status, Duration::ZERO, ledger.clone()).await;
        let cfg = config(endpoint, ledger);
        let provider = HttpReranker::new(cfg.clone()).unwrap();
        let snapshot = corpus();
        assert!(provider
            .rerank(
                &snapshot,
                &RetrievalQuery::new("alpha beta"),
                &candidates(&snapshot)
            )
            .await
            .is_err());
        worker.await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        let totals = provider.ledger().snapshot(&cfg).await.unwrap().totals();
        assert_eq!(totals.usage_reported_requests, u64::from(expected_known));
        assert_eq!(
            totals.reserved_or_unknown_micros,
            if expected_known { 0 } else { 1000 }
        );
    }
}

#[tokio::test]
async fn complete_input_response_bounds_and_model_tariff_mismatch_keep_exact_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let mut cfg = config(
        "http://127.0.0.1:9/rerank".into(),
        directory.path().join("input"),
    );
    cfg.max_input_bytes = 128;
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    let snapshot = corpus();
    assert!(provider
        .rerank(
            &snapshot,
            &RetrievalQuery::new("alpha beta"),
            &candidates(&snapshot)
        )
        .await
        .is_err());
    assert!(provider
        .ledger()
        .snapshot(&cfg)
        .await
        .unwrap()
        .attempts
        .is_empty());
    for wrong_model in [false, true] {
        let ledger = directory.path().join(if wrong_model {
            "wrong-model"
        } else {
            "response-bound"
        });
        let mut body = good();
        if wrong_model {
            body["model"] = json!("different-billing-model");
        }
        let (endpoint, _, worker) = server(body, 200, Duration::ZERO, ledger.clone()).await;
        let mut cfg = config(endpoint, ledger);
        if !wrong_model {
            cfg.max_response_bytes = 128;
        }
        let provider = HttpReranker::new(cfg.clone()).unwrap();
        assert!(provider
            .rerank(
                &snapshot,
                &RetrievalQuery::new("alpha beta"),
                &candidates(&snapshot)
            )
            .await
            .is_err());
        worker.await.unwrap();
        let totals = provider.ledger().snapshot(&cfg).await.unwrap().totals();
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.reserved_or_unknown_micros, 1000);
        assert_eq!(totals.usage_reported_requests, 0);
    }
    let cfg = config(
        "http://127.0.0.1:9/rerank".into(),
        directory.path().join("corruption"),
    );
    let ledger = FileRerankLedger::new(cfg.ledger_dir.clone()).unwrap();
    ledger.snapshot(&cfg).await.unwrap();
    std::fs::write(cfg.ledger_dir.join("rerank-ledger.json"), b"{").unwrap();
    assert!(ledger.snapshot(&cfg).await.is_err());
    assert!(HttpReranker::new(cfg)
        .unwrap()
        .rerank(
            &snapshot,
            &RetrievalQuery::new("alpha beta"),
            &candidates(&snapshot)
        )
        .await
        .is_err());
}

#[tokio::test]
async fn empty_candidates_do_not_create_spending_and_journal_metadata_is_bound() {
    let directory = tempfile::tempdir().unwrap();
    let cfg = config(
        "http://127.0.0.1:9/rerank".into(),
        directory.path().join("empty"),
    );
    let provider = HttpReranker::new(cfg.clone()).unwrap();
    assert!(provider
        .rerank(&corpus(), &RetrievalQuery::new("alpha"), &[])
        .await
        .unwrap()
        .is_empty());
    assert!(!cfg.ledger_dir.exists());
    let ledger = provider.ledger();
    ledger.reserve(&cfg, &"a".repeat(64), 128).await.unwrap();
    let path = cfg.ledger_dir.join("rerank-ledger.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["model"] = json!("wrong-model");
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(ledger.snapshot(&cfg).await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn ledger_state_and_lock_symlinks_are_rejected_without_budget_reset() {
    let directory = tempfile::tempdir().unwrap();
    let cfg = config(
        "http://127.0.0.1:9/rerank".into(),
        directory.path().join("ledger"),
    );
    let ledger = FileRerankLedger::new(cfg.ledger_dir.clone()).unwrap();
    ledger.reserve(&cfg, &"a".repeat(64), 128).await.unwrap();
    let state = cfg.ledger_dir.join("rerank-ledger.json");
    let actual = directory.path().join("actual.json");
    std::fs::rename(&state, &actual).unwrap();
    std::os::unix::fs::symlink(&actual, &state).unwrap();
    assert!(ledger.snapshot(&cfg).await.is_err());
    std::fs::remove_file(&state).unwrap();
    std::fs::rename(actual, &state).unwrap();
    let lock = cfg.ledger_dir.join("rerank-ledger.lock");
    let actual = directory.path().join("actual.lock");
    std::fs::rename(&lock, &actual).unwrap();
    std::os::unix::fs::symlink(actual, lock).unwrap();
    assert!(ledger.snapshot(&cfg).await.is_err());
}
