#![cfg(feature = "mcp")]
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::{mcp::*, *};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
async fn read(socket: &mut tokio::net::TcpStream) -> (String, BTreeMap<String, String>, Value) {
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
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let length = headers
        .get("content-length")
        .map_or(0, |v| v.parse::<usize>().unwrap());
    while raw.len() < end + length {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        raw.extend_from_slice(&buffer[..n]);
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&raw[end..end + length]).unwrap()
    };
    (first, headers, body)
}
async fn json_reply(socket: &mut tokio::net::TcpStream, id: Value, result: Value) {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nMcp-Session-Id: original-session\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
}
async fn sse(socket: &mut tokio::net::TcpStream, session: &str, event: Option<&str>, value: Value) {
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: {session}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let id = event.map_or_else(String::new, |id| format!("id: {id}\n"));
    socket
        .write_all(format!("{id}data: {value}\n\n").as_bytes())
        .await
        .unwrap();
}
fn final_value(mode: &str, id: Value) -> Value {
    json!({"jsonrpc":if mode.contains("version"){ "1.0" }else{"2.0"},"id":id,
        "result":if mode.contains("content"){json!({"isError":false})}else{json!({"content":[{"type":"text","text":"original completed result"}],"isError":mode=="tool-error"})}})
}
async fn fixture(
    mode: &'static str,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let effects = Arc::new(AtomicUsize::new(0));
    let gets = Arc::new(AtomicUsize::new(0));
    let effect = effects.clone();
    let get = gets.clone();
    let worker = tokio::spawn(async move {
        let mut original_id = Value::Null;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (first, headers, request) = read(&mut socket).await;
            if first.starts_with("GET /mcp ") {
                let attempt = get.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["mcp-session-id"], "original-session");
                assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
                assert_eq!(
                    headers["last-event-id"],
                    if attempt == 1 && mode == "new-cursor" {
                        "cursor-2"
                    } else {
                        "cursor-1"
                    }
                );
                assert!(request.is_null());
                if mode == "denied" {
                    socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                } else if mode == "replay" {
                    sse(&mut socket,"original-session",Some("cursor-1"),json!({"jsonrpc":"2.0","id":"host-replay","method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"never dispatch"}}],"maxTokens":4}})).await;
                } else if mode == "new-cursor" && attempt == 0 {
                    sse(
                        &mut socket,
                        "original-session",
                        Some("cursor-2"),
                        json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}}),
                    )
                    .await;
                } else {
                    sse(
                        &mut socket,
                        if mode == "session" {
                            "changed-session"
                        } else {
                            "original-session"
                        },
                        Some("result-1"),
                        final_value(
                            if mode.starts_with("get-") {
                                mode
                            } else {
                                "normal"
                            },
                            if mode == "request" {
                                json!(999999)
                            } else {
                                original_id.clone()
                            },
                        ),
                    )
                    .await;
                }
            } else {
                assert!(first.starts_with("POST /mcp "));
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        json_reply(
                            &mut socket,
                            request["id"].clone(),
                            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}),
                        )
                        .await
                    }
                    "notifications/initialized" => {
                        socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    }
                    "tools/list" => {
                        json_reply(
                            &mut socket,
                            request["id"].clone(),
                            json!({"tools":[{"name":"write","inputSchema":{"type":"object"}}]}),
                        )
                        .await
                    }
                    "tools/call" => {
                        effect.fetch_add(1, Ordering::SeqCst);
                        original_id = request["id"].clone();
                        if mode == "original-json-content" {
                            json_reply(&mut socket, original_id.clone(), json!({"isError":false}))
                                .await;
                            continue;
                        }
                        sse(&mut socket,"original-session",if mode=="no-id"{None}else{Some("cursor-1")},json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}})).await;
                        if mode.starts_with("original-") || mode == "tool-error" {
                            socket
                                .write_all(
                                    format!(
                                        "id: result-1\ndata: {}\n\n",
                                        final_value(mode, original_id.clone())
                                    )
                                    .as_bytes(),
                                )
                                .await
                                .unwrap();
                        }
                        if mode == "timeout" {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                    }
                    _ => panic!("unexpected protocol method"),
                }
            }
        }
    });
    (endpoint, effects, gets, worker)
}
async fn connect(endpoint: &str, policy: Option<McpSsePolicy>) -> McpClient {
    let mut config = McpHttpConfig::new(endpoint);
    config.timeout = Duration::from_millis(500);
    McpClient::connect_with_extensions(
        config,
        McpHttpExtensions {
            resumption: policy,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
async fn execute(client: &McpClient) -> Result<Value, ToolError> {
    client
        .bind(McpToolBinding {
            name: "write".into(),
            remote_name: "write".into(),
            effect: ToolEffect::NonIdempotent,
            permissions: BTreeSet::from(["explicit-write".into()]),
            description: Some("host-approved fixture effect".into()),
        })
        .unwrap()
        .execute(
            json!({"effect":"only-once"}),
            ToolContext {
                run_id: "resume-fixture".into(),
                call_id: "original-write".into(),
            },
        )
        .await
}
#[tokio::test]
async fn explicit_sse_get_recovers_original_response_and_never_reposts_effect() {
    let (endpoint, effects, gets, worker) = fixture("drop").await;
    let client = connect(&endpoint, Some(McpSsePolicy::default())).await;
    assert!(execute(&client).await.is_err());
    assert_eq!(gets.load(Ordering::SeqCst), 0);
    let handle = client.pending_resumptions().unwrap().pop().unwrap();
    assert_eq!(handle.last_event_id(), Some("cursor-1"));
    let result = client.resume_tool_call(&handle).await.unwrap();
    assert_eq!(result["content"][0]["text"], "original completed result");
    assert!(client.pending_resumptions().unwrap().is_empty());
    assert!(client.resume_tool_call(&handle).await.is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(gets.load(Ordering::SeqCst), 1);
    worker.abort();
}
#[tokio::test]
async fn default_connection_and_missing_cursor_cannot_make_recovery_get() {
    for (mode, policy) in [("drop", None), ("no-id", Some(McpSsePolicy::default()))] {
        let (endpoint, effects, gets, worker) = fixture(mode).await;
        let client = connect(&endpoint, policy).await;
        assert!(execute(&client).await.is_err());
        let handles = client.pending_resumptions().unwrap();
        if mode == "no-id" {
            assert_eq!(handles.len(), 1);
            assert!(handles[0].last_event_id().is_none());
            assert!(client.resume_tool_call(&handles[0]).await.is_err());
            client.discard_resumption(&handles[0]).unwrap();
        } else {
            assert!(handles.is_empty());
        }
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(gets.load(Ordering::SeqCst), 0);
        worker.abort();
    }
}
#[tokio::test]
async fn get_wrong_session_request_denial_or_replayed_event_fails_closed_without_post_retry() {
    for mode in ["session", "request", "denied", "replay"] {
        let (endpoint, effects, gets, worker) = fixture(mode).await;
        let client = connect(&endpoint, Some(McpSsePolicy::default())).await;
        assert!(execute(&client).await.is_err());
        let handle = client.pending_resumptions().unwrap().pop().unwrap();
        assert!(client.resume_tool_call(&handle).await.is_err(), "{mode}");
        let still = client.pending_resumptions().unwrap().pop().unwrap();
        assert!(client.resume_tool_call(&still).await.is_err());
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(gets.load(Ordering::SeqCst), 1);
        worker.abort();
    }
}
#[tokio::test]
async fn checkpoint_is_connection_local_and_pending_limit_is_before_second_effect_post() {
    let (endpoint, effects, gets, worker) = fixture("drop").await;
    let client = connect(
        &endpoint,
        Some(McpSsePolicy {
            max_pending: 1,
            ..Default::default()
        }),
    )
    .await;
    assert!(execute(&client).await.is_err());
    let handle = client.pending_resumptions().unwrap().pop().unwrap();
    let second = connect(&endpoint, Some(McpSsePolicy::default())).await;
    assert!(second.resume_tool_call(&handle).await.is_err());
    assert!(second.discard_resumption(&handle).is_err());
    assert!(execute(&client).await.is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(gets.load(Ordering::SeqCst), 0);
    client.discard_resumption(&handle).unwrap();
    worker.abort();
}
#[tokio::test]
async fn explicit_second_get_uses_new_cursor_and_rejects_stale_handle_before_network() {
    let (endpoint, effects, gets, worker) = fixture("new-cursor").await;
    let client = connect(
        &endpoint,
        Some(McpSsePolicy {
            max_resumptions: 2,
            ..Default::default()
        }),
    )
    .await;
    assert!(execute(&client).await.is_err());
    let original = client.pending_resumptions().unwrap().pop().unwrap();
    assert!(client.resume_tool_call(&original).await.is_err());
    assert!(client.resume_tool_call(&original).await.is_err());
    assert_eq!(gets.load(Ordering::SeqCst), 1);
    let latest = client.pending_resumptions().unwrap().pop().unwrap();
    assert_eq!(latest.last_event_id(), Some("cursor-2"));
    assert!(client.resume_tool_call(&latest).await.is_ok());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(gets.load(Ordering::SeqCst), 2);
    worker.abort();
}
#[tokio::test]
async fn caller_timeout_leaves_original_cursor_available_for_explicit_get() {
    let (endpoint, effects, gets, worker) = fixture("timeout").await;
    let client = connect(&endpoint, Some(McpSsePolicy::default())).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), execute(&client))
            .await
            .is_err()
    );
    let handle = client.pending_resumptions().unwrap().pop().unwrap();
    assert_eq!(handle.last_event_id(), Some("cursor-1"));
    assert!(client.resume_tool_call(&handle).await.is_ok());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(gets.load(Ordering::SeqCst), 1);
    worker.abort();
}
#[tokio::test]
async fn malformed_final_post_or_get_never_advances_or_deletes_previous_checkpoint() {
    for mode in [
        "original-version",
        "original-content",
        "get-version",
        "get-content",
        "original-json-content",
    ] {
        let (endpoint, effects, gets, worker) = fixture(mode).await;
        let client = connect(&endpoint, Some(McpSsePolicy::default())).await;
        assert!(execute(&client).await.is_err());
        let handle = client.pending_resumptions().unwrap().pop().unwrap();
        assert_eq!(
            handle.last_event_id(),
            if mode == "original-json-content" {
                None
            } else {
                Some("cursor-1")
            },
            "{mode}"
        );
        if mode.starts_with("get-") {
            assert!(client.resume_tool_call(&handle).await.is_err());
            let after = client.pending_resumptions().unwrap().pop().unwrap();
            assert_eq!(after.last_event_id(), Some("cursor-1"));
            assert_eq!(gets.load(Ordering::SeqCst), 1);
        } else if mode != "original-json-content" {
            assert!(client.resume_tool_call(&handle).await.is_ok());
        }
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        worker.abort();
    }
    let (endpoint, effects, _, worker) = fixture("tool-error").await;
    let client = connect(&endpoint, Some(McpSsePolicy::default())).await;
    assert!(execute(&client).await.is_err());
    assert!(client.pending_resumptions().unwrap().is_empty());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    worker.abort();
}
