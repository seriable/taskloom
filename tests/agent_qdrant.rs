#![cfg(feature = "qdrant")]
//! Actual bounded REST over loopback; vectors are offline protocol fixtures.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};
use taskloom::runtime::{rag::*, RuntimeError};

#[derive(Default)]
struct Model(AtomicUsize);
impl EmbeddingModel for Model {
    fn identity(&self) -> String {
        "qdrant-offline-protocol-fixture-v1".into()
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> BoxFuture<'a, Result<EmbeddingBatch, RuntimeError>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(EmbeddingBatch {
                identity: self.identity(),
                vectors: texts.iter().map(|_| vec![1.0, 0.5]).collect(),
            })
        })
    }
}
#[derive(Clone, Copy, Default, PartialEq)]
enum Fault {
    #[default]
    None,
    DropAck,
    MissingAck,
    Foreign,
    ForeignLowScore,
    Unknown,
    Duplicate,
    BadScore,
    MissingPayload,
    TooMany,
    BigResponse,
    CollectionMismatch,
}
#[derive(Default)]
struct State {
    points: Mutex<BTreeMap<String, Value>>,
    fault: Mutex<Fault>,
    journal: Mutex<Option<PathBuf>>,
    writes: AtomicUsize,
    queries: AtomicUsize,
    dimensions: AtomicUsize,
    last_query: Mutex<Value>,
    stop: AtomicBool,
}
fn state_dimensions(state: &State, fault: Fault) -> usize {
    state.dimensions.load(Ordering::SeqCst) + usize::from(fault == Fault::CollectionMismatch)
}
struct Server {
    address: String,
    state: Arc<State>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let state = Arc::new(State::default());
        state.dimensions.store(2, Ordering::SeqCst);
        let copied = state.clone();
        let worker = thread::spawn(move || {
            for incoming in listener.incoming() {
                let mut stream = incoming.unwrap();
                if copied.stop.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 2048];
                let boundary = loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break i + 4;
                    }
                    assert!(bytes.len() < 65_536);
                };
                let head = String::from_utf8(bytes[..boundary].to_vec()).unwrap();
                let line = head.lines().next().unwrap();
                let path = line.split_whitespace().nth(1).unwrap();
                let size = head
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                assert!(size <= 8 * 1024 * 1024);
                while bytes.len() < boundary + size {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let body = if size > 0 {
                    serde_json::from_slice::<Value>(&bytes[boundary..boundary + size]).unwrap()
                } else {
                    Value::Null
                };
                let fault = *copied.fault.lock().unwrap();
                let value = if line.starts_with("GET ") {
                    json!({"status":"ok","result":{"config":{"params":{"vectors":{"size":state_dimensions(&copied, fault),"distance":"Cosine"}}}}})
                } else if line.starts_with("PUT ") {
                    assert_eq!(path, "/collections/host_collection/points?wait=true");
                    let journal = copied
                        .journal
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("host journal must be selected");
                    let receipt: Value = serde_json::from_slice(
                        &std::fs::read(journal.join("publish.json")).unwrap(),
                    )
                    .unwrap();
                    assert!(receipt["batches"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|batch| batch["state"] == "pending"));
                    copied.writes.fetch_add(1, Ordering::SeqCst);
                    for point in body["points"].as_array().unwrap() {
                        copied
                            .points
                            .lock()
                            .unwrap()
                            .insert(point["id"].as_str().unwrap().into(), point.clone());
                    }
                    if fault == Fault::DropAck {
                        continue;
                    }
                    json!({"status":"ok","result":{"operation_id":1,"status":if fault==Fault::MissingAck{"acknowledged"}else{"completed"}}})
                } else {
                    assert_eq!(path, "/collections/host_collection/points/query");
                    assert!(line.starts_with("POST "));
                    assert_eq!(body["with_vector"], false);
                    copied.queries.fetch_add(1, Ordering::SeqCst);
                    *copied.last_query.lock().unwrap() = body.clone();
                    let predicates = body["filter"]["must"].as_array().unwrap();
                    assert_eq!(predicates.len(), 5);
                    let mut points = copied
                        .points
                        .lock()
                        .unwrap()
                        .values()
                        .filter(|p| {
                            predicates.iter().all(|predicate| {
                                p["payload"][predicate["key"].as_str().unwrap()]
                                    == predicate["match"]["value"]
                            })
                        })
                        .take(body["limit"].as_u64().unwrap() as usize)
                        .map(|p| json!({"id":p["id"],"score":0.9,"payload":p["payload"]}))
                        .collect::<Vec<_>>();
                    match fault {
                        Fault::Foreign => {
                            points[0]["payload"]["taskloom_scope"] = json!("another-owner")
                        }
                        Fault::ForeignLowScore => {
                            points[0]["payload"]["taskloom_scope"] = json!("another-owner");
                            points[0]["score"] = json!(0.01);
                        }
                        Fault::Unknown => {
                            points[0]["id"] = json!("00000000-0000-5000-8000-000000000000")
                        }
                        Fault::Duplicate => points = vec![points[0].clone(), points[0].clone()],
                        Fault::BadScore => points[0]["score"] = json!(1.1),
                        Fault::MissingPayload => points[0]["payload"] = Value::Null,
                        Fault::TooMany => points.extend(vec![points[0].clone(); 21]),
                        Fault::BigResponse => points[0]["padding"] = json!("x".repeat(16 * 1024)),
                        _ => (),
                    }
                    json!({"status":"ok","result":{"points":points}})
                };
                let body = serde_json::to_vec(&value).unwrap();
                let head=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self {
            address,
            state,
            worker: Some(worker),
        }
    }
    fn config(&self) -> QdrantConfig {
        QdrantConfig {
            endpoint: format!("http://{}/", self.address),
            collection: "host_collection".into(),
            namespace: "tenant/project/owner".into(),
            api_key_env: None,
            timeout_ms: 2000,
            max_response_bytes: 8192,
            max_request_bytes: 1024 * 1024,
            batch_points: 1,
        }
    }
}
async fn corpus() -> (
    tempfile::TempDir,
    CorpusSnapshot,
    Arc<Model>,
    Arc<EmbeddingIndex>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = FileCorpusStore::new(dir.path());
    for (id, text) in [
        ("alpha", "Alpha source with exact local provenance."),
        ("beta", "Beta source is retained locally, not remote text."),
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
    let snapshot = store.snapshot().await.unwrap();
    let model = Arc::new(Model::default());
    let index = Arc::new(
        EmbeddingIndex::build(&snapshot, model.as_ref())
            .await
            .unwrap(),
    );
    (dir, snapshot, model, index)
}
#[tokio::test]
async fn qdrant_fixed_scope_publish_query_and_complete_result_validation() {
    let server = Server::start();
    let (dir, snapshot, model, index) = corpus().await;
    let client = Arc::new(QdrantClient::new(server.config()).unwrap());
    let journal = FileQdrantPublishJournal::new(dir.path().join("publish")).unwrap();
    *server.state.journal.lock().unwrap() = Some(dir.path().join("publish"));
    let receipt = client.publish(&snapshot, &index, &journal).await.unwrap();
    assert!(journal.snapshot().await.unwrap().unwrap().complete);
    assert_eq!(server.state.writes.load(Ordering::SeqCst), 2);
    let retriever = QdrantRetriever::new(
        client.clone(),
        &snapshot,
        index.clone(),
        receipt,
        model.clone(),
        0.5,
    )
    .unwrap();
    let query = RetrievalQuery {
        text: "source".into(),
        top_k: 2,
        max_bytes: 4096,
    };
    let result = search(&FileCorpusStore::new(dir.path()), &retriever, &query)
        .await
        .unwrap();
    assert_eq!(result.hits.len(), 2);
    assert!(result
        .hits
        .iter()
        .all(|e| e.chunk.source.starts_with("fixture:")));
    let query_body = server.state.last_query.lock().unwrap().clone();
    assert!(query_body["filter"]["must"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "taskloom_scope" && p["match"]["value"] == "tenant/project/owner"));
    let before = model.0.load(Ordering::SeqCst);
    let mut stale = snapshot.clone();
    stale.revision += 1;
    assert!(retriever.retrieve(&stale, &query).await.is_err());
    assert_eq!(model.0.load(Ordering::SeqCst), before);
    for fault in [
        Fault::Foreign,
        Fault::ForeignLowScore,
        Fault::Unknown,
        Fault::Duplicate,
        Fault::BadScore,
        Fault::MissingPayload,
        Fault::TooMany,
        Fault::BigResponse,
    ] {
        *server.state.fault.lock().unwrap() = fault;
        assert!(retriever.retrieve(&snapshot, &query).await.is_err());
    }
    *server.state.fault.lock().unwrap() = Fault::CollectionMismatch;
    let before = model.0.load(Ordering::SeqCst);
    assert!(retriever.retrieve(&snapshot, &query).await.is_err());
    assert_eq!(model.0.load(Ordering::SeqCst), before);
    *server.state.fault.lock().unwrap() = Fault::None;
    let receipt = client
        .completed_receipt(&snapshot, &index, &journal)
        .await
        .unwrap();
    let mut foreign = server.config();
    foreign.namespace = "foreign".into();
    let foreign = Arc::new(QdrantClient::new(foreign).unwrap());
    assert!(QdrantRetriever::new(foreign, &snapshot, index, receipt, model, 0.5).is_err());
}
#[tokio::test]
async fn qdrant_lost_or_incomplete_ack_retains_durable_pending_and_refuses_query_receipt() {
    for fault in [Fault::DropAck, Fault::MissingAck] {
        let server = Server::start();
        let (dir, snapshot, _model, index) = corpus().await;
        let client = QdrantClient::new(server.config()).unwrap();
        let root = dir.path().join("publish");
        let journal = FileQdrantPublishJournal::new(&root).unwrap();
        *server.state.journal.lock().unwrap() = Some(root);
        *server.state.fault.lock().unwrap() = fault;
        assert!(client.publish(&snapshot, &index, &journal).await.is_err());
        let retained = journal.snapshot().await.unwrap().unwrap();
        assert!(!retained.complete);
        let json = serde_json::to_value(&retained).unwrap();
        assert_eq!(json["batches"][0]["state"], "pending");
        assert_eq!(json["batches"][1]["state"], "planned");
        assert_eq!(server.state.points.lock().unwrap().len(), 1);
        assert!(client
            .completed_receipt(&snapshot, &index, &journal)
            .await
            .is_err());
        *server.state.fault.lock().unwrap() = Fault::None;
        assert!(client.publish(&snapshot, &index, &journal).await.is_err());
        assert_eq!(server.state.writes.load(Ordering::SeqCst), 1);
        assert_eq!(journal.snapshot().await.unwrap().unwrap(), retained);
    }
}
#[tokio::test]
async fn qdrant_impossible_query_envelope_is_rejected_before_embedding_or_query_http() {
    let server = Server::start();
    server.state.dimensions.store(32, Ordering::SeqCst);
    let (dir, snapshot, model, original) = corpus().await;
    let mut index = (*original).clone();
    index.dimensions = 32;
    for vector in index.vectors.values_mut() {
        *vector = vec![1.0; 32];
    }
    let index = Arc::new(index);
    index.validate(&snapshot, &model.identity()).unwrap();
    let mut config = server.config();
    config.max_request_bytes = 1024;
    let client = Arc::new(QdrantClient::new(config).unwrap());
    let root = dir.path().join("publish");
    let journal = FileQdrantPublishJournal::new(&root).unwrap();
    *server.state.journal.lock().unwrap() = Some(root);
    let receipt = client.publish(&snapshot, &index, &journal).await.unwrap();
    let retriever =
        QdrantRetriever::new(client, &snapshot, index, receipt, model.clone(), 0.5).unwrap();
    let before = model.0.load(Ordering::SeqCst);
    assert!(retriever
        .retrieve(&snapshot, &RetrievalQuery::new("source"))
        .await
        .is_err());
    assert_eq!(model.0.load(Ordering::SeqCst), before);
    assert_eq!(server.state.queries.load(Ordering::SeqCst), 0);
}
#[test]
fn qdrant_static_target_validation_resolves_no_credentials_and_refuses_ambiguous_targets() {
    let server = Server::start();
    let mut config = server.config();
    config.api_key_env = Some(format!(
        "TASKLOOM_MISSING_QDRANT_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    config.validate().unwrap();
    assert!(QdrantClient::new(config).is_err());
    for endpoint in [
        "http://example.com/",
        "http://127.0.0.1/?key=secret",
        "http://user:pass@127.0.0.1/",
        "http://127.0.0.1/path",
        "http://127.0.0.1/#fragment",
    ] {
        let mut config = server.config();
        config.endpoint = endpoint.into();
        assert!(config.validate().is_err());
    }
    let mut config = server.config();
    config.collection = "../escape".into();
    assert!(config.validate().is_err());
    let mut config = server.config();
    config.api_key_env = Some("1INVALID_ENVIRONMENT_NAME".into());
    assert!(config.validate().is_err());
}
