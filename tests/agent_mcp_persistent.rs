#![cfg(feature = "mcp-persistence")]
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use taskloom::runtime::{database::*, mcp::*, *};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const SESSION: &str = "private-session-header-not-public";
const TOKEN: &str = "private-bearer-not-persisted";
fn options() -> DatabaseOptions {
    DatabaseOptions {
        lease_duration: Duration::from_millis(700),
        ..Default::default()
    }
}
fn hash<T: serde::Serialize>(value: &T) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(value).unwrap()))
}
fn key(name: &str) -> String {
    format!("mcp-http-v1-{}", hash(&name))
}
fn context() -> ToolContext {
    ToolContext {
        run_id: "private-run-context".into(),
        call_id: "exact-original-call".into(),
    }
}
fn config(endpoint: &str) -> McpPersistentConfig {
    let mut http = McpHttpConfig::new(endpoint);
    http.bearer_token = Some(TOKEN.into());
    http.namespace = "tenant-project".into();
    http.timeout = Duration::from_secs(10);
    let mut config = McpPersistentConfig::new(http, "tenant/project/principal-A");
    config.policy.allow_http_loopback_for_tests = true;
    config
}
async fn database(url: &str) -> SqlDatabase {
    let database = SqlDatabase::connect(url, options()).await.unwrap();
    database.migrate().await.unwrap();
    database
}
fn fsync(path: &Path, value: &Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{value}").unwrap();
    file.sync_all().unwrap();
}
fn count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .count()
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Drop,
    Hold,
    Observed,
    NoCursor,
    Callback,
    WrongSession,
    WrongRequest,
    Get404,
    Malformed,
    Replay,
    Big,
    LargeCatalog,
}
struct Server {
    endpoint: String,
    posts: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
    initializations: Arc<AtomicUsize>,
    effects: PathBuf,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
async fn read_request(
    socket: &mut tokio::net::TcpStream,
) -> (String, BTreeMap<String, String>, Value) {
    let mut raw = Vec::new();
    let mut buffer = [0; 4096];
    let end = loop {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&buffer[..n]);
        if let Some(at) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let text = std::str::from_utf8(&raw[..end]).unwrap();
    let first = text.lines().next().unwrap().to_owned();
    let headers = text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().into()))
        .collect::<BTreeMap<String, String>>();
    let length = headers
        .get("content-length")
        .map_or(0, |v| v.parse::<usize>().unwrap());
    while raw.len() < end + length {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&buffer[..n]);
    }
    (
        first,
        headers,
        if length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&raw[end..end + length]).unwrap()
        },
    )
}
async fn json_response(socket: &mut tokio::net::TcpStream, id: Value, result: Value) {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: {SESSION}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
}
async fn event(socket: &mut tokio::net::TcpStream, id: Option<&str>, message: Value) {
    let prefix = id.map_or_else(String::new, |id| format!("id: {id}\n"));
    let _ = socket
        .write_all(format!("{prefix}data: {message}\n\n").as_bytes())
        .await;
}
async fn server(mode: Mode, objects: SqlObjectStore, logical: &str, root: &Path) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let posts = Arc::new(AtomicUsize::new(0));
    let gets = Arc::new(AtomicUsize::new(0));
    let initializations = Arc::new(AtomicUsize::new(0));
    let copied_posts = posts.clone();
    let copied_gets = gets.clone();
    let copied_init = initializations.clone();
    let effects = root.join("external-effect.jsonl");
    let effect_path = effects.clone();
    let logical = logical.to_owned();
    let worker = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let posts = copied_posts.clone();
            let gets = copied_gets.clone();
            let init = copied_init.clone();
            let objects = objects.clone();
            let logical = logical.clone();
            let effects = effect_path.clone();
            children.spawn(async move {
                let (first,headers,request)=read_request(&mut socket).await;assert_eq!(headers["authorization"],format!("Bearer {TOKEN}"));
                if first.starts_with("GET ") {
                    gets.fetch_add(1,Ordering::SeqCst);assert_eq!(headers["mcp-session-id"],SESSION);assert_eq!(headers["mcp-protocol-version"],"2025-06-18");assert_eq!(headers["last-event-id"],"cursor-1");
                    let stored=objects.load::<Value>(&key(&logical)).await.unwrap().unwrap();let call=stored["calls"].as_object().unwrap().values().next().unwrap();
                    assert_eq!(call["get_attempts"],1);assert_eq!(call["state"],"post_outcome_unknown");
                    if mode==Mode::Get404 {socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();return;}
                    let session=if mode==Mode::WrongSession{"other-private-session"}else{SESSION};
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: {session}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                    if mode==Mode::Replay {event(&mut socket,Some("cursor-1"),json!({"jsonrpc":"2.0","method":"notifications/progress"})).await;return;}
                    let id=if mode==Mode::WrongRequest {json!(999999)}else{call["request_id"].clone()};
                    let result=if mode==Mode::Malformed {json!({"isError":false})}else{json!({"content":[{"type":"text","text":"exact original durable response"}]})};
                    event(&mut socket,Some("result-1"),json!({"jsonrpc":"2.0","id":id,"result":result})).await;return;
                }
                match request["method"].as_str().unwrap() {
                    "initialize"=>{init.fetch_add(1,Ordering::SeqCst);assert_eq!(request["params"]["capabilities"],json!({}));json_response(&mut socket,request["id"].clone(),json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}}})).await;},
                    "notifications/initialized"=>{socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();},
                    "tools/list"=>json_response(&mut socket,request["id"].clone(),json!({"tools":[{"name":"write","description":if mode==Mode::LargeCatalog{"padding".repeat(200)}else{"fixture only".into()},"inputSchema":{"type":"object"}}]})).await,
                    "tools/call"=>{
                        let stored=objects.load::<Value>(&key(&logical)).await.unwrap().unwrap();let call=stored["calls"].as_object().unwrap().values().find(|call|call["request_id"]==request["id"]).unwrap();
                        assert_eq!(call["state"],"post_outcome_unknown");assert!(call["response"].is_null());assert!(stored["next_id"].as_u64().unwrap()>request["id"].as_u64().unwrap());
                        posts.fetch_add(1,Ordering::SeqCst);fsync(&effects,&json!({"id":request["id"],"fsynced_effect":true}));
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: {SESSION}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                        event(&mut socket,if mode==Mode::NoCursor {None}else{Some("cursor-1")},json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}})).await;
                        if mode==Mode::Callback {event(&mut socket,Some("callback-1"),json!({"jsonrpc":"2.0","id":"server-sampling-request","method":"sampling/createMessage","params":{"messages":[],"maxTokens":1}})).await;}
                        if mode==Mode::Observed || mode==Mode::Big {
                            let text=if mode==Mode::Big {"x".repeat(4*1024*1024)}else{"exact original durable response".into()};
                            event(&mut socket,Some("result-1"),json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":text}]}})).await;
                        }
                        if mode==Mode::Hold {tokio::time::sleep(Duration::from_secs(120)).await;}
                    },
                    _=>panic!("unexpected extra POST"),
                }
            });
        }
    });
    Server {
        endpoint,
        posts,
        gets,
        initializations,
        effects,
        worker,
    }
}
async fn reopen(
    db: &SqlDatabase,
    project: &str,
    endpoint: &str,
    logical: &str,
) -> PersistentMcpClient {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        match PersistentMcpClient::open(
            config(endpoint),
            SqlMcpSessionStore::new(db.clone(), "mcp-test", project).unwrap(),
            logical,
        )
        .await
        {
            Ok(client) => return client,
            Err(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "owner never became available"
                );
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_durable_post_cursor_observed_ack_and_scope_guards() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("mcp.sqlite")).unwrap();
    let db = database(&url).await;
    guard_controls(&db, dir.path()).await;
    db.close().await;
}
async fn guard_controls(db: &SqlDatabase, root: &Path) {
    for (ordinal, mode) in [
        Mode::Drop,
        Mode::NoCursor,
        Mode::Callback,
        Mode::WrongSession,
        Mode::WrongRequest,
        Mode::Get404,
        Mode::Malformed,
        Mode::Replay,
        Mode::Big,
    ]
    .into_iter()
    .enumerate()
    {
        let project = format!(
            "guard-{ordinal}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let objects = db.objects(DatabaseNamespace::new("mcp-test", &project).unwrap());
        let case = root.join(&project);
        std::fs::create_dir(&case).unwrap();
        let server = server(mode, objects.clone(), "session", &case).await;
        let client = PersistentMcpClient::create(
            config(&server.endpoint),
            SqlMcpSessionStore::new(db.clone(), "mcp-test", &project).unwrap(),
            "session",
        )
        .await
        .unwrap();
        assert!(client
            .call(
                "write",
                json!({"text":"private-args-never-stored"}),
                &context()
            )
            .await
            .is_err());
        assert_eq!(server.posts.load(Ordering::SeqCst), 1);
        assert_eq!(count(&server.effects), 1);
        let (handle, summary) = client.pending().await.unwrap().pop().unwrap();
        let public = serde_json::to_string(&summary).unwrap();
        for secret in [TOKEN, SESSION, "private-args-never-stored"] {
            assert!(!public.contains(secret));
        }
        let stored = objects
            .load::<Value>(&key("session"))
            .await
            .unwrap()
            .unwrap();
        let body = serde_json::to_string(&stored).unwrap();
        assert!(body.contains(SESSION));
        assert!(!body.contains(TOKEN));
        assert!(!body.contains("private-args-never-stored"));
        assert!(client
            .call("write", json!({"different":true}), &context())
            .await
            .is_err());
        assert!(client
            .call(
                "write",
                json!({"text":"private-args-never-stored"}),
                &context()
            )
            .await
            .is_err());
        let result = client.resume(&handle).await;
        if mode == Mode::Drop || mode == Mode::Big {
            let observed = result.unwrap();
            assert_eq!(
                observed.result().unwrap()["content"][0]["text"],
                "exact original durable response"
            );
            let retained = objects
                .load::<Value>(&key("session"))
                .await
                .unwrap()
                .unwrap();
            let entry = retained["calls"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(entry["state"], "observed_result");
            assert_eq!(entry["response"], *observed.response());
            assert!(client
                .acknowledge(&handle, &"f".repeat(64), "bad-ack")
                .await
                .is_err());
            fsync(&case.join("adopted-result.jsonl"), observed.response());
            client
                .acknowledge(&handle, observed.sha256(), "host-runstore-committed")
                .await
                .unwrap();
            assert!(client.resume(&handle).await.is_err());
            let retained = objects
                .load::<Value>(&key("session"))
                .await
                .unwrap()
                .unwrap();
            let entry = retained["calls"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(entry["state"], "host_acknowledged");
            assert!(entry["response"].is_null());
        } else {
            assert!(result.is_err());
            let latest = client.pending().await.unwrap().pop().unwrap().1;
            if mode == Mode::Malformed {
                assert!(latest.has_cursor);
            }
            assert!(client.resume(&handle).await.is_err());
            client
                .abandon_unknown(&handle, "explicit-host-unknown-abandonment")
                .await
                .unwrap();
        }
        assert_eq!(server.posts.load(Ordering::SeqCst), 1);
        assert_eq!(server.initializations.load(Ordering::SeqCst), 1);
        if mode == Mode::NoCursor || mode == Mode::Callback {
            assert_eq!(server.gets.load(Ordering::SeqCst), 0);
        }
        client.release_owner().unwrap();
        let mut wrong = config(&server.endpoint);
        wrong.principal_namespace = "different-principal".into();
        assert!(PersistentMcpClient::open(
            wrong,
            SqlMcpSessionStore::new(db.clone(), "mcp-test", &project).unwrap(),
            "session"
        )
        .await
        .is_err());
        let restored = reopen(db, &project, &server.endpoint, "session").await;
        assert!(restored
            .call(
                "write",
                json!({"text":"private-args-never-stored"}),
                &context()
            )
            .await
            .is_err());
        restored.release_owner().unwrap();
    }
}
struct Handler;
impl McpInteractionHandler for Handler {
    fn identity(&self) -> String {
        "must-never-be-called".into()
    }
    fn approve<'a>(
        &'a self,
        _: &'a McpInteractionRequest,
    ) -> BoxFuture<'a, Result<McpInteractionApproval, RuntimeError>> {
        Box::pin(async { panic!("persistent v1 dispatched host callback") })
    }
    fn respond<'a>(
        &'a self,
        _: &'a McpInteractionRequest,
        _: &'a McpInteractionApproval,
    ) -> BoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async { panic!("persistent v1 dispatched model") })
    }
}
#[tokio::test]
async fn persistent_extensions_and_capacity_fail_before_any_effect_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("capacity.sqlite")).unwrap();
    let db = database(&url).await;
    let store = SqlMcpSessionStore::new(db.clone(), "mcp-test", "capacity")
        .unwrap()
        .with_capacity(1)
        .unwrap();
    let mut unsupported = config("http://127.0.0.1:1/mcp");
    unsupported.extensions.interactions =
        Some(McpInteractions::new(McpInteractionPolicy::default(), Arc::new(Handler)).unwrap());
    assert!(unsupported.validate().is_err());
    assert!(
        PersistentMcpClient::create(unsupported, store.clone(), "disabled")
            .await
            .is_err()
    );
    let objects = db.objects(DatabaseNamespace::new("mcp-test", "capacity").unwrap());
    assert!(objects
        .load::<Value>(&key("disabled"))
        .await
        .unwrap()
        .is_none());
    let server = server(Mode::Observed, objects.clone(), "one", dir.path()).await;
    let mut cfg = config(&server.endpoint);
    cfg.policy.max_calls = 1;
    let client = PersistentMcpClient::create(cfg, store.clone(), "one")
        .await
        .unwrap();
    assert!(
        PersistentMcpClient::create(config(&server.endpoint), store, "two")
            .await
            .is_err()
    );
    assert_eq!(server.initializations.load(Ordering::SeqCst), 1);
    let result = client.call("write", json!({}), &context()).await.unwrap();
    client
        .acknowledge(result.call(), result.sha256(), "saved")
        .await
        .unwrap();
    let other = ToolContext {
        run_id: "another".into(),
        call_id: "call".into(),
    };
    assert!(client.call("write", json!({}), &other).await.is_err());
    assert_eq!(server.posts.load(Ordering::SeqCst), 1);
    client.release_owner().unwrap();
    db.close().await;
}
async fn sql_failure_controls(db: &SqlDatabase, url: &str, root: &Path) {
    let project = format!(
        "write-failure-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let objects = db.objects(DatabaseNamespace::new("mcp-test", &project).unwrap());
    let server = server(
        Mode::LargeCatalog,
        objects.clone(),
        "sql-write-failure",
        root,
    )
    .await;
    let original = PersistentMcpClient::create(
        config(&server.endpoint),
        SqlMcpSessionStore::new(db.clone(), "mcp-test", &project).unwrap(),
        "sql-write-failure",
    )
    .await
    .unwrap();
    let ready = objects
        .load::<Value>(&key("sql-write-failure"))
        .await
        .unwrap()
        .unwrap();
    assert!(ready["ready"].as_bool().unwrap());
    let inspect = sqlx::AnyPool::connect(url).await.unwrap();
    let physical_body:String=sqlx::query_scalar("SELECT body FROM taskloom_records WHERE namespace=$1 AND family='app_object' AND object_id=$2")
        .bind(serde_json::to_string(&("mcp-test",&project)).unwrap()).bind(key("sql-write-failure")).fetch_one(&inspect).await.unwrap();
    let ready_bytes = physical_body.len();
    inspect.close().await;
    assert!(ready_bytes > 1024);
    original.release_owner().unwrap();
    let restricted = SqlDatabase::connect(
        url,
        DatabaseOptions {
            max_record_bytes: (ready_bytes + 16) as u64,
            ..options()
        },
    )
    .await
    .unwrap();
    let restricted_store =
        SqlMcpSessionStore::new(restricted.clone(), "mcp-test", &project).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let client = loop {
        match PersistentMcpClient::open(
            config(&server.endpoint),
            restricted_store.clone(),
            "sql-write-failure",
        )
        .await
        {
            Ok(client) => break client,
            Err(_) => {
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    assert!(client.call("write", json!({}), &context()).await.is_err());
    assert!(
        client.pending().await.is_err(),
        "unconfirmed save must poison the current owner"
    );
    assert_eq!(server.posts.load(Ordering::SeqCst), 0);
    assert_eq!(count(&server.effects), 0);
    let retained = objects
        .load::<Value>(&key("sql-write-failure"))
        .await
        .unwrap()
        .unwrap();
    assert!(retained["calls"].as_object().unwrap().is_empty());
    client.release_owner().unwrap();
    restricted.close().await;
    let restored = reopen(db, &project, &server.endpoint, "sql-write-failure").await;
    assert!(restored.pending().await.unwrap().is_empty());
    restored.release_owner().unwrap();
    assert_eq!(server.initializations.load(Ordering::SeqCst), 1);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_failed_unknown_reservation_poison_requires_new_epoch_and_sends_no_post() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("save-failure.sqlite")).unwrap();
    let db = database(&url).await;
    sql_failure_controls(&db, &url, dir.path()).await;
    db.close().await;
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn process(url: &str, project: &str, endpoint: &str, mode: &str, marker: &Path) -> Process {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "persistent_mcp_process_worker",
            "--nocapture",
        ])
        .env_clear()
        .env("TASKLOOM_MCP_FIXTURE_URL", url)
        .env("TASKLOOM_MCP_FIXTURE_PROJECT", project)
        .env("TASKLOOM_MCP_FIXTURE_ENDPOINT", endpoint)
        .env("TASKLOOM_MCP_FIXTURE_MODE", mode)
        .env("TASKLOOM_MCP_FIXTURE_MARKER", marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
    }
    Process(command.spawn().unwrap())
}
#[tokio::test]
#[ignore = "internal owned process fixture, invoked by durable MCP crash tests"]
async fn persistent_mcp_process_worker() {
    let url = std::env::var("TASKLOOM_MCP_FIXTURE_URL").unwrap();
    let project = std::env::var("TASKLOOM_MCP_FIXTURE_PROJECT").unwrap();
    let endpoint = std::env::var("TASKLOOM_MCP_FIXTURE_ENDPOINT").unwrap();
    let mode = std::env::var("TASKLOOM_MCP_FIXTURE_MODE").unwrap();
    let marker = PathBuf::from(std::env::var("TASKLOOM_MCP_FIXTURE_MARKER").unwrap());
    let db = database(&url).await;
    let store = SqlMcpSessionStore::new(db.clone(), "mcp-test", &project).unwrap();
    if mode == "compete" {
        match PersistentMcpClient::open(config(&endpoint), store, "session").await {
            Ok(_client) => {
                fsync(&marker, &json!({"acquired":true}));
                tokio::time::sleep(Duration::from_secs(120)).await;
            }
            Err(_) => fsync(&marker, &json!({"acquired":false})),
        }
        return;
    }
    let client = PersistentMcpClient::create(config(&endpoint), store, "session")
        .await
        .unwrap();
    let result = client
        .call(
            "write",
            json!({"exact":"fsynced external effect"}),
            &context(),
        )
        .await;
    if mode == "observed" {
        let observed = result.unwrap();
        fsync(
            &marker,
            &json!({"response_sha256":observed.sha256(),"before_runstore_adoption":true}),
        );
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
}
async fn wait_file(path: &Path, process: &mut Process) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    while !path.exists() {
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "owned process exited before marker"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "owned marker not written"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
async fn process_controls(db: &SqlDatabase, url: &str, root: &Path) {
    for (mode, behavior) in [("post", Mode::Hold), ("observed", Mode::Observed)] {
        let project = format!(
            "proc-{}-{}",
            mode,
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let objects = db.objects(DatabaseNamespace::new("mcp-test", &project).unwrap());
        let dir = root.join(&project);
        std::fs::create_dir(&dir).unwrap();
        let server = server(behavior, objects.clone(), "session", &dir).await;
        let marker = dir.join("observed-before-runstore.jsonl");
        let mut child = process(url, &project, &server.endpoint, mode, &marker);
        if mode == "observed" {
            wait_file(&marker, &mut child).await;
        } else {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
            loop {
                let record = objects.load::<Value>(&key("session")).await.unwrap();
                if record
                    .as_ref()
                    .and_then(|v| v["calls"].as_object())
                    .is_some_and(|calls| calls.values().any(|call| call["cursor"] == "cursor-1"))
                {
                    break;
                }
                assert!(child.0.try_wait().unwrap().is_none());
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        assert_eq!(count(&server.effects), 1);
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let recovered = reopen(db, &project, &server.endpoint, "session").await;
        let (handle, summary) = recovered.pending().await.unwrap().pop().unwrap();
        if mode == "observed" {
            assert_eq!(summary.state, "observed_result");
        }
        let observed = recovered.resume(&handle).await.unwrap();
        assert_eq!(
            observed.result().unwrap()["content"][0]["text"],
            "exact original durable response"
        );
        assert_eq!(server.posts.load(Ordering::SeqCst), 1);
        assert_eq!(server.initializations.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.gets.load(Ordering::SeqCst),
            usize::from(mode == "post")
        );
        fsync(&dir.join("adopted-runstore.jsonl"), observed.response());
        recovered
            .acknowledge(&handle, observed.sha256(), "fsynced-original-runstore")
            .await
            .unwrap();
        recovered.release_owner().unwrap();
        // Two actual independent successors contend for the same durable lease.
        let first_marker = dir.join("first-owner.jsonl");
        let second_marker = dir.join("second-owner.jsonl");
        tokio::time::sleep(Duration::from_millis(800)).await;
        let mut first = process(url, &project, &server.endpoint, "compete", &first_marker);
        let mut second = process(url, &project, &server.endpoint, "compete", &second_marker);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        while !first_marker.exists() || !second_marker.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let a: Value = serde_json::from_str(
            std::fs::read_to_string(&first_marker)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        let b: Value = serde_json::from_str(
            std::fs::read_to_string(&second_marker)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_ne!(a["acquired"], b["acquired"]);
        if a["acquired"] == true {
            first.0.kill().unwrap();
            first.0.wait().unwrap();
        } else {
            second.0.kill().unwrap();
            second.0.wait().unwrap();
        }
        let current = reopen(db, &project, &server.endpoint, "session").await;
        // Force an external epoch change in the owned fixture database. The old
        // SDK owner must be unable to publish/ACK or start any further HTTP.
        let pool = sqlx::AnyPool::connect(url).await.unwrap();
        sqlx::query("UPDATE taskloom_leases SET owner='fixture-new-owner',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='app_object' AND object_id=$2")
            .bind(serde_json::to_string(&("mcp-test",&project)).unwrap()).bind(key("session")).execute(&pool).await.unwrap();
        assert!(current.pending().await.is_err());
        assert!(current
            .call(
                "write",
                json!({}),
                &ToolContext {
                    run_id: "new".into(),
                    call_id: "new".into()
                }
            )
            .await
            .is_err());
        current.release_owner().unwrap();
        let fresh = reopen(db, &project, &server.endpoint, "session").await;
        assert_eq!(
            fresh.pending().await.unwrap().pop().unwrap().1.state,
            "host_acknowledged"
        );
        fresh.release_owner().unwrap();
        pool.close().await;
        assert_eq!(server.posts.load(Ordering::SeqCst), 1);
        assert_eq!(count(&server.effects), 1);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_real_process_death_and_observed_before_runstore_adoption_recover_without_repost() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("process.sqlite")).unwrap();
    let db = database(&url).await;
    process_controls(&db, &url, dir.path()).await;
    db.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; hard-kills only exact owned fixture children"]
async fn postgres_real_process_death_competing_owners_and_stale_epoch_are_fenced() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PG URL required");
    let db = database(&url).await;
    process_controls(&db, &url, dir.path()).await;
    guard_controls(&db, dir.path()).await;
    sql_failure_controls(&db, &url, dir.path()).await;
    db.close().await;
}
