#![cfg(feature = "mcp")]
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::mcp::*;
use taskloom::runtime::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
enum Reply {
    Result(Value),
    Initialized,
    List(Value),
    Sse(Value),
    Status(u16),
    RpcError,
    BadId,
    Raw(String),
}
#[derive(Debug)]
struct Recorded {
    method: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
async fn server(
    replies: Vec<Reply>,
) -> (
    String,
    Arc<Mutex<Vec<Recorded>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let saved = Arc::clone(&requests);
    let worker = tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            let end = loop {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(n) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break n + 4;
                }
                assert!(bytes.len() < 65536);
            };
            let head = std::str::from_utf8(&bytes[..end]).unwrap();
            let method = head
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .to_string();
            let headers = head
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
                .collect::<BTreeMap<_, _>>();
            let length = headers
                .get("content-length")
                .map_or(0, |s| s.parse::<usize>().unwrap());
            while bytes.len() < end + length {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            let body = if length > 0 {
                serde_json::from_slice::<Value>(&bytes[end..end + length]).unwrap()
            } else {
                Value::Null
            };
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            saved.lock().unwrap().push(Recorded {
                method,
                headers,
                body,
            });
            let (status, kind, body, session) = match reply {
                Reply::Initialized => (202, "application/json", String::new(), false),
                Reply::Status(code) => (code, "application/json", String::new(), false),
                Reply::Result(result) | Reply::List(result) => (
                    200,
                    "application/json",
                    json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
                    true,
                ),
                Reply::Sse(result) => (
                    200,
                    "text/event-stream",
                    format!(
                        "event: message\r\ndata: {}\r\n\r\nevent: message\r\ndata: {}\r\n\r\n",
                        json!({"jsonrpc":"2.0","method":"notifications/progress","params":{"message":"等待"}}),
                        json!({"jsonrpc":"2.0","id":id,"result":result})
                    ),
                    true,
                ),
                Reply::RpcError => (
                    200,
                    "application/json",
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"failed"}})
                        .to_string(),
                    true,
                ),
                Reply::BadId => (
                    200,
                    "application/json",
                    json!({"jsonrpc":"2.0","id":99999,"result":{}}).to_string(),
                    true,
                ),
                Reply::Raw(body) => (200, "application/json", body, true),
            };
            let head=format!("HTTP/1.1 {status} Test\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",body.len(),if session {"Mcp-Session-Id: test-session\r\n"}else{""});
            socket.write_all(head.as_bytes()).await.unwrap();
            // Split through UTF-8 and SSE delimiters to exercise incremental parsing.
            for part in body.as_bytes().chunks(7) {
                if socket.write_all(part).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
    });
    (url, requests, worker)
}
fn initialize() -> Reply {
    Reply::Result(
        json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
    )
}
fn info() -> Value {
    json!({"name":"remote/save","description":"save an item","inputSchema":{"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false},"annotations":{"readOnlyHint":true}})
}
fn catalog() -> Reply {
    Reply::List(json!({"tools":[info()]}))
}
fn binding() -> McpToolBinding {
    McpToolBinding {
        name: "save".into(),
        remote_name: "remote/save".into(),
        effect: ToolEffect::NonIdempotent,
        permissions: BTreeSet::from(["remote:write".into()]),
    }
}
fn context() -> ToolContext {
    ToolContext {
        run_id: "run".into(),
        call_id: "call".into(),
    }
}

#[tokio::test]
async fn paginated_discovery_preserves_headers_explicit_effects_and_structured_results() {
    let (url,requests,worker)=server(vec![initialize(),Reply::Initialized,Reply::List(json!({"tools":[],"nextCursor":"next"})),catalog(),Reply::Result(json!({"content":[{"type":"text","text":"saved"}],"structuredContent":{"saved":true},"isError":false})),Reply::Status(204)]).await;
    let mut config = McpHttpConfig::new(url);
    config.bearer_token = Some("fixture-token".into());
    let client = McpClient::connect(config).await.unwrap();
    assert_eq!(client.tools().count(), 1);
    assert_eq!(client.protocol_version(), "2025-06-18");
    let tool = client.bind(binding()).unwrap();
    assert_eq!(tool.definition().effect, ToolEffect::NonIdempotent);
    assert!(tool.definition().permissions.contains("remote:write"));
    let result = tool.execute(json!({"value":7}), context()).await.unwrap();
    assert_eq!(result["structuredContent"]["saved"], true);
    client.close().await.unwrap();
    assert!(tool.execute(json!({"value":7}), context()).await.is_err());
    worker.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0].body["method"], "initialize");
    assert_eq!(requests[0].headers["authorization"], "Bearer fixture-token");
    assert!(!requests[0].headers.contains_key("mcp-protocol-version"));
    for request in &requests[1..] {
        assert_eq!(request.headers["mcp-session-id"], "test-session");
        assert_eq!(request.headers["mcp-protocol-version"], "2025-06-18");
    }
    assert_eq!(requests[3].body["params"]["cursor"], "next");
    assert_eq!(requests[4].body["params"]["name"], "remote/save");
    assert_eq!(requests[5].method, "DELETE");
}

#[tokio::test]
async fn finite_sse_notifications_and_split_utf8_return_correlated_tool_response() {
    let (url, _, worker) = server(vec![
        initialize(),
        Reply::Initialized,
        catalog(),
        Reply::Sse(json!({"content":[{"type":"text","text":"完成"}]})),
    ])
    .await;
    let client = McpClient::connect(McpHttpConfig::new(url)).await.unwrap();
    let result = client
        .bind(binding())
        .unwrap()
        .execute(json!({"value":1}), context())
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "完成");
    worker.await.unwrap();
}

struct ToolModel;
impl Model for ToolModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let turn = if request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }))
            {
                AssistantTurn::text("done")
            } else {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "remote-call".into(),
                        name: "save".into(),
                        arguments: json!({"value":7}),
                    }],
                }
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
#[tokio::test]
async fn durable_mcp_call_waits_for_approval_and_completed_reentry_sends_nothing() {
    let (url, requests, worker) = server(vec![
        initialize(),
        Reply::Initialized,
        catalog(),
        Reply::Result(json!({"content":[{"type":"text","text":"saved"}]})),
    ])
    .await;
    let client = McpClient::connect(McpHttpConfig::new(url)).await.unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(client.bind(binding()).unwrap()).unwrap();
    let model = ToolModel;
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_policy(ToolPolicy::default().allow("save").grant("remote:write"));
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let durable = DurableAgent::new(&agent, &store);
    let waiting = durable
        .start("mcp", vec![Message::user("save")])
        .await
        .unwrap();
    assert_eq!(waiting.status, RunStatus::WaitingApproval);
    assert_eq!(requests.lock().unwrap().len(), 3);
    durable
        .decide(
            "mcp",
            &waiting.pending_approval().unwrap().request_id,
            ApprovalDecision {
                approved: true,
                actor: "operator".into(),
                note: "approved remote write".into(),
            },
        )
        .await
        .unwrap();
    let done = durable.resume("mcp").await.unwrap();
    assert_eq!(done.report.stop_reason, StopReason::Completed);
    assert_eq!(durable.resume("mcp").await.unwrap().revision, done.revision);
    worker.await.unwrap();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.body["method"] == "tools/call")
            .count(),
        1
    );
}

#[tokio::test]
async fn remote_failures_and_response_limits_never_retry_tool_calls() {
    for reply in [
        Reply::Status(500),
        Reply::RpcError,
        Reply::BadId,
        Reply::Raw("not json".into()),
        Reply::Result(json!({"content":[],"isError":true})),
        Reply::Result(json!({"content":"invalid"})),
        Reply::Result(json!({"content":[{"type":"text","text":"x".repeat(4096)}]})),
    ] {
        let (url, requests, worker) =
            server(vec![initialize(), Reply::Initialized, catalog(), reply]).await;
        let mut config = McpHttpConfig::new(url);
        config.max_response_bytes = 1024;
        let client = McpClient::connect(config).await.unwrap();
        assert!(client
            .bind(binding())
            .unwrap()
            .execute(json!({"value":7}), context())
            .await
            .is_err());
        worker.await.unwrap();
        assert_eq!(requests.lock().unwrap().len(), 4);
    }
}

#[tokio::test]
async fn negotiation_redirect_and_repeated_pagination_fail_closed() {
    let (url, _, worker) = server(vec![Reply::Result(
        json!({"protocolVersion":"2099-01-01","capabilities":{"tools":{}}}),
    )])
    .await;
    assert!(McpClient::connect(McpHttpConfig::new(url)).await.is_err());
    worker.await.unwrap();
    let (url, _, worker) = server(vec![Reply::Status(307)]).await;
    assert!(McpClient::connect(McpHttpConfig::new(url)).await.is_err());
    worker.await.unwrap();
    let page = Reply::List(json!({"tools":[],"nextCursor":"same"}));
    let (url, requests, worker) =
        server(vec![initialize(), Reply::Initialized, page.clone(), page]).await;
    assert!(McpClient::connect(McpHttpConfig::new(url)).await.is_err());
    worker.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn tenant_namespace_changes_frozen_tool_identity_without_recording_token() {
    let (url, _, worker) = server(vec![
        initialize(),
        Reply::Initialized,
        catalog(),
        initialize(),
        Reply::Initialized,
        catalog(),
    ])
    .await;
    let mut config = McpHttpConfig::new(&url);
    config.namespace = "tenant-a".into();
    config.bearer_token = Some("secret-a".into());
    let a = McpClient::connect(config)
        .await
        .unwrap()
        .bind(binding())
        .unwrap()
        .definition();
    let mut config = McpHttpConfig::new(&url);
    config.namespace = "tenant-b".into();
    config.bearer_token = Some("secret-b".into());
    let b = McpClient::connect(config)
        .await
        .unwrap()
        .bind(binding())
        .unwrap()
        .definition();
    assert_ne!(a, b);
    assert!(!serde_json::to_string(&a).unwrap().contains("secret-a"));
    worker.await.unwrap();
}
