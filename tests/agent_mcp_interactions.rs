#![cfg(feature = "mcp")]
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::{mcp::*, *};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn schema() -> Value {
    json!({"type":"object","properties":{"project":{"type":"string","maxLength":64}},"required":["project"],"additionalProperties":false})
}
struct Handler {
    approved: Arc<AtomicUsize>,
    responded: Arc<AtomicUsize>,
    deny: bool,
    wrong_binding: bool,
    slow: bool,
    wrong_reply: bool,
}
impl McpInteractionHandler for Handler {
    fn identity(&self) -> String {
        "reviewed-manual-fixture-host-v1".into()
    }
    fn approve<'a>(
        &'a self,
        request: &'a McpInteractionRequest,
    ) -> BoxFuture<'a, Result<McpInteractionApproval, RuntimeError>> {
        Box::pin(async move {
            self.approved.fetch_add(1, Ordering::SeqCst);
            if self.deny {
                return Ok(McpInteractionApproval::deny());
            }
            McpInteractionApproval::allow(
                if self.wrong_binding {
                    "f".repeat(64)
                } else {
                    request.binding().to_owned()
                },
                "fixture-operator",
                "explicit fixture source-bound review",
            )
        })
    }
    fn respond<'a>(
        &'a self,
        request: &'a McpInteractionRequest,
        _: &'a McpInteractionApproval,
    ) -> BoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move {
            self.responded.fetch_add(1, Ordering::SeqCst);
            if self.slow {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if request.method() == "sampling/createMessage" {
                Ok(
                    json!({"role":"assistant","content":{"type":"text","text":"Host-provided local fixture text; no model call"},"model":if self.wrong_reply{"unapproved-model"}else{"host-fixture"},"stopReason":"endTurn"}),
                )
            } else {
                Ok(json!({"action":"accept","content":{"project":"reviewed"}}))
            }
        })
    }
}
fn interactions(
    deny: bool,
    wrong_binding: bool,
    slow: bool,
    wrong_reply: bool,
) -> (McpInteractions, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let mut policy = McpInteractionPolicy {
        sampling_model: Some("host-fixture".into()),
        max_sampling_tokens: 8,
        ..Default::default()
    };
    policy.elicitation_schemas.insert(
        McpInteractions::schema_fingerprint(&schema()).unwrap(),
        BTreeSet::from(["project".into()]),
    );
    with_policy(policy, deny, wrong_binding, slow, wrong_reply)
}
fn with_policy(
    policy: McpInteractionPolicy,
    deny: bool,
    wrong_binding: bool,
    slow: bool,
    wrong_reply: bool,
) -> (McpInteractions, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let approved = Arc::new(AtomicUsize::new(0));
    let responded = Arc::new(AtomicUsize::new(0));
    let handler = Handler {
        approved: approved.clone(),
        responded: responded.clone(),
        deny,
        wrong_binding,
        slow,
        wrong_reply,
    };
    (
        McpInteractions::new(policy, Arc::new(handler)).unwrap(),
        approved,
        responded,
    )
}
fn binding() -> McpToolBinding {
    McpToolBinding {
        name: "echo".into(),
        remote_name: "echo".into(),
        effect: ToolEffect::ReadOnly,
        permissions: BTreeSet::from(["mcp:read".into()]),
        description: Some("Explicit fixture read".into()),
    }
}
fn context() -> ToolContext {
    ToolContext {
        run_id: "mcp-interaction-fixture".into(),
        call_id: "read".into(),
    }
}
fn executable() -> &'static std::path::Path {
    static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    static PROGRAM: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let directory = DIRECTORY.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(if cfg!(windows) {
            "mcp-interactions.exe"
        } else {
            "mcp-interactions"
        });
        assert!(std::process::Command::new("rustc")
            .args([
                "--edition=2021",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/data/mcp_interactions_fixture.rs"
                ),
                "-o"
            ])
            .arg(&output)
            .status()
            .unwrap()
            .success());
        directory
    });
    PROGRAM
        .get_or_init(|| {
            directory.path().join(if cfg!(windows) {
                "mcp-interactions.exe"
            } else {
                "mcp-interactions"
            })
        })
        .as_path()
}
fn config(mode: &str) -> McpStdioConfig {
    let mut config = McpStdioConfig::new(executable(), std::env::current_dir().unwrap());
    config.args = vec![mode.into()];
    config.timeout = Duration::from_secs(2);
    config
}
#[tokio::test]
async fn stdio_host_optin_sampling_and_form_require_exact_approval_and_keep_original_grants() {
    for mode in ["sample", "form", "duplicate"] {
        let (host, approved, responded) = interactions(false, false, false, false);
        let client = McpClient::connect_stdio_with_interactions(config(mode), host)
            .await
            .unwrap();
        let tool = client.bind(binding()).unwrap();
        assert_eq!(tool.definition().effect, ToolEffect::ReadOnly);
        assert_eq!(
            tool.definition().permissions,
            BTreeSet::from(["mcp:read".into()])
        );
        assert!(tool.execute(json!({}), context()).await.is_ok());
        assert_eq!(approved.load(Ordering::SeqCst), 1);
        assert_eq!(responded.load(Ordering::SeqCst), 1);
        let audit = client.interaction_audit().unwrap();
        assert_eq!(audit.len(), 1);
        assert!(audit[0].approved && audit[0].response_prepared);
        assert_eq!(audit[0].state, "response_prepared_not_delivery");
        assert!(audit[0].approval_fingerprint.is_some());
        client.close().await.unwrap();
    }
    let legacy = McpClient::connect_stdio(config("normal")).await.unwrap();
    let (host, _, _) = interactions(false, false, false, false);
    let enabled = McpClient::connect_stdio_with_interactions(config("normal"), host)
        .await
        .unwrap();
    assert_ne!(
        legacy.bind(binding()).unwrap().definition().description,
        enabled.bind(binding()).unwrap().definition().description
    );
    legacy.close().await.unwrap();
    enabled.close().await.unwrap();
}
#[tokio::test]
async fn default_denial_policy_limits_secret_form_and_old_protocol_never_dispatch_host_response() {
    let client = McpClient::connect_stdio(config("sample")).await.unwrap();
    assert!(client
        .bind(binding())
        .unwrap()
        .execute(json!({}), context())
        .await
        .is_err());
    assert!(client.interaction_audit().unwrap().is_empty());
    client.close().await.unwrap();
    for mode in ["tokens", "context", "secret", "url", "old-form"] {
        let (host, approved, responded) = interactions(false, false, false, false);
        let client = McpClient::connect_stdio_with_interactions(config(mode), host)
            .await
            .unwrap();
        assert!(client
            .bind(binding())
            .unwrap()
            .execute(json!({}), context())
            .await
            .is_err());
        assert_eq!(approved.load(Ordering::SeqCst), 0, "{mode}");
        assert_eq!(responded.load(Ordering::SeqCst), 0);
        client.close().await.unwrap();
    }
    for (deny, wrong_binding) in [(true, false), (false, true)] {
        let (host, approved, responded) = interactions(deny, wrong_binding, false, false);
        let client = McpClient::connect_stdio_with_interactions(config("sample"), host)
            .await
            .unwrap();
        assert!(client
            .bind(binding())
            .unwrap()
            .execute(json!({}), context())
            .await
            .is_err());
        assert_eq!(approved.load(Ordering::SeqCst), 1);
        assert_eq!(responded.load(Ordering::SeqCst), 0);
        assert!(!client.interaction_audit().unwrap()[0].approved);
        client.close().await.unwrap();
    }
}
#[tokio::test]
async fn callback_cancellation_retains_approved_pending_audit_and_wrong_host_model_is_rejected() {
    let (host, approved, responded) = interactions(false, false, true, false);
    let mut cfg = config("sample");
    cfg.timeout = Duration::from_millis(100);
    let client = McpClient::connect_stdio_with_interactions(cfg, host)
        .await
        .unwrap();
    assert!(client
        .bind(binding())
        .unwrap()
        .execute(json!({}), context())
        .await
        .is_err());
    assert_eq!(approved.load(Ordering::SeqCst), 1);
    assert_eq!(responded.load(Ordering::SeqCst), 1);
    let audit = client.interaction_audit().unwrap();
    assert!(audit[0].approved);
    assert!(!audit[0].response_prepared);
    assert_eq!(audit[0].state, "approved_pending_response");
    client.close().await.unwrap();
    let (host, _, _) = interactions(false, false, false, true);
    let client = McpClient::connect_stdio_with_interactions(config("sample"), host)
        .await
        .unwrap();
    assert!(client
        .bind(binding())
        .unwrap()
        .execute(json!({}), context())
        .await
        .is_err());
    let audit = client.interaction_audit().unwrap();
    assert!(audit[0].approved && !audit[0].response_prepared);
    client.close().await.unwrap();
}
#[tokio::test]
async fn callback_timeout_retains_unknown_outcome_and_lifetime_limit_stops_third_host_dispatch() {
    let policy = McpInteractionPolicy {
        sampling_model: Some("host-fixture".into()),
        max_sampling_tokens: 8,
        timeout: Duration::from_millis(50),
        ..Default::default()
    };
    let (host, approved, responded) = with_policy(policy, false, false, true, false);
    let client = McpClient::connect_stdio_with_interactions(config("sample"), host)
        .await
        .unwrap();
    assert!(client
        .bind(binding())
        .unwrap()
        .execute(json!({}), context())
        .await
        .is_err());
    assert_eq!(approved.load(Ordering::SeqCst), 1);
    assert_eq!(responded.load(Ordering::SeqCst), 1);
    let audit = client.interaction_audit().unwrap();
    assert!(audit[0].approved && !audit[0].response_prepared);
    assert_eq!(audit[0].state, "timed_out_callback_outcome_unknown");
    client.close().await.unwrap();

    let policy = McpInteractionPolicy {
        sampling_model: Some("host-fixture".into()),
        max_sampling_tokens: 8,
        max_server_requests: 2,
        ..Default::default()
    };
    let (host, approved, responded) = with_policy(policy, false, false, false, false);
    let client = McpClient::connect_stdio_with_interactions(config("many"), host)
        .await
        .unwrap();
    assert!(client
        .bind(binding())
        .unwrap()
        .execute(json!({}), context())
        .await
        .is_err());
    assert_eq!(approved.load(Ordering::SeqCst), 2);
    assert_eq!(responded.load(Ordering::SeqCst), 2);
    assert_eq!(client.interaction_audit().unwrap().len(), 2);
    client.close().await.unwrap();
}
#[test]
fn unsafe_elicitation_schema_or_unbounded_policy_cannot_be_host_pinned() {
    for schema in [
        json!({"type":"object","properties":{"password":{"type":"string"}},"additionalProperties":false}),
        json!({"type":"object","properties":{"project":{"$ref":"https://never-fetch.invalid/schema"}},"additionalProperties":false}),
        json!({"type":"object","properties":{},"additionalProperties":true}),
    ] {
        assert!(McpInteractions::schema_fingerprint(&schema).is_err());
    }
    let policy = McpInteractionPolicy {
        max_server_requests: 0,
        ..Default::default()
    };
    assert!(policy.validate().is_err());
}
async fn read_request(socket: &mut tokio::net::TcpStream) -> (BTreeMap<String, String>, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let end = loop {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..end])
        .unwrap()
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().into()))
        .collect::<BTreeMap<String, String>>();
    let length = headers["content-length"].parse::<usize>().unwrap();
    while bytes.len() < end + length {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
    }
    (
        headers,
        serde_json::from_slice(&bytes[end..end + length]).unwrap(),
    )
}
async fn reply(socket: &mut tokio::net::TcpStream, id: Value, result: Value) {
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).unwrap();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nMcp-Session-Id: session\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
}
#[tokio::test]
async fn http_finite_sse_host_requests_post_only_correlated_replies_not_repeated_tools_call() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, init) = read_request(&mut socket).await;
        assert!(init["params"]["capabilities"]["sampling"].is_object());
        assert!(init["params"]["capabilities"]["elicitation"].is_object());
        reply(
            &mut socket,
            init["id"].clone(),
            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}}}),
        )
        .await;
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, notification) = read_request(&mut socket).await;
        assert_eq!(notification["method"], "notifications/initialized");
        socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, listed) = read_request(&mut socket).await;
        reply(
            &mut socket,
            listed["id"].clone(),
            json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
        )
        .await;
        let (mut stream, _) = listener.accept().await.unwrap();
        let (_, call) = read_request(&mut stream).await;
        assert_eq!(call["method"], "tools/call");
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: session\r\nConnection: close\r\n\r\n").await.unwrap();
        let requests = [
            json!({"jsonrpc":"2.0","id":"sample-http","method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"untrusted remote sampling request"}}],"maxTokens":4}}),
            json!({"jsonrpc":"2.0","id":"form-http","method":"elicitation/create","params":{"message":"Choose project","requestedSchema":schema()}}),
        ];
        for request in requests {
            stream
                .write_all(format!("data: {}\n\n", request).as_bytes())
                .await
                .unwrap();
            let (mut response, _) = listener.accept().await.unwrap();
            let (headers, reply) = read_request(&mut response).await;
            assert_eq!(headers["mcp-session-id"], "session");
            assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
            assert_eq!(reply["id"], request["id"]);
            assert!(reply.get("method").is_none());
            assert!(reply["result"].is_object());
            response
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
        stream.write_all(format!("data: {}\n\n",json!({"jsonrpc":"2.0","id":call["id"],"result":{"content":[{"type":"text","text":"tool completed"}]}})).as_bytes()).await.unwrap();
    });
    let (host, approved, responded) = interactions(false, false, false, false);
    let client = McpClient::connect_with_interactions(McpHttpConfig::new(endpoint), host)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        client
            .bind(binding())
            .unwrap()
            .execute(json!({}), context()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["content"][0]["text"], "tool completed");
    worker.await.unwrap();
    assert_eq!(approved.load(Ordering::SeqCst), 2);
    assert_eq!(responded.load(Ordering::SeqCst), 2);
    assert_eq!(client.interaction_audit().unwrap().len(), 2);
}
