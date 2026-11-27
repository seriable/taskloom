#![cfg(all(feature = "runtime", any(feature = "openai", feature = "anthropic")))]

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use taskloom::runtime::*;
use taskloom::{Gateway, GatewayTransport, Provider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Each response closes the connection, so the second model turn proves a
/// fresh request contains the actual prior tool call and execution result.
async fn server(replies: Vec<Value>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        requests.push(
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                        );
                        break;
                    }
                }
            }
            let body = reply.to_string();
            let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (format!("http://{address}"), task)
}

async fn execute(provider: impl Provider) -> RunReport {
    let model = GatewayTransport::new(Arc::new(Gateway::default()), provider);
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {
        name:"lookup".into(),description:"Find a fact.".into(),effect:ToolEffect::ReadOnly,
        permissions:BTreeSet::new(),input_schema:json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
    }, |args:Value,_:ToolContext|async move {assert_eq!(args["query"],"meaning");Ok(json!({"answer":42}))})).unwrap();
    let report = AgentLoop::new(&model, &tools, "test-model")
        .with_policy(ToolPolicy::default().allow("lookup"))
        .run(
            "http-test",
            vec![
                Message::system("Find the answer."),
                Message::user("What is the meaning?"),
            ],
        )
        .await;
    assert_eq!(
        report.stop_reason,
        StopReason::Completed,
        "{:?}",
        report.detail
    );
    assert_eq!(report.final_text(), Some("42"));
    assert_eq!((report.steps, report.tool_calls), (2, 1));
    assert_eq!(
        (report.usage.input_tokens, report.usage.output_tokens),
        (20, 4)
    );
    report
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_native_tool_loop_over_http() {
    let (url,task) = server(vec![
        json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"find-1","type":"function","function":{"name":"lookup","arguments":"{\"query\":\"meaning\"}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":"42"}}],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
    ]).await;
    execute(taskloom::openai::OpenAiCompatible::new(url, "test-key")).await;
    let requests = task.await.unwrap();
    assert_eq!(requests[0]["tools"][0]["function"]["name"], "lookup");
    assert_eq!(requests[1]["messages"][2]["tool_calls"][0]["id"], "find-1");
    assert_eq!(requests[1]["messages"][3]["tool_call_id"], "find-1");
    let result: Value =
        serde_json::from_str(requests[1]["messages"][3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["output"]["answer"], 42);
}

#[cfg(feature = "anthropic")]
#[tokio::test]
async fn anthropic_native_tool_loop_over_http() {
    let (url,task) = server(vec![
        json!({"stop_reason":"tool_use","content":[{"type":"tool_use","id":"find-1","name":"lookup","input":{"query":"meaning"}}],"usage":{"input_tokens":10,"output_tokens":2}}),
        json!({"stop_reason":"end_turn","content":[{"type":"text","text":"42"}],"usage":{"input_tokens":10,"output_tokens":2}}),
    ]).await;
    execute(taskloom::anthropic::Anthropic::with_base_url(
        url, "test-key",
    ))
    .await;
    let requests = task.await.unwrap();
    assert_eq!(requests[0]["tools"][0]["name"], "lookup");
    assert_eq!(requests[1]["system"], "Find the answer.");
    assert_eq!(requests[1]["messages"][1]["content"][0]["id"], "find-1");
    assert_eq!(
        requests[1]["messages"][2]["content"][0]["tool_use_id"],
        "find-1"
    );
    let result: Value = serde_json::from_str(
        requests[1]["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["answer"], 42);
}
