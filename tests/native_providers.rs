#![cfg(all(feature = "openai", feature = "runtime"))]
use serde_json::{json, Value};
use std::time::Duration;
use taskloom::runtime::{
    AssistantTurn, Message, ModelRequest, ToolCall, ToolDefinition, ToolEffect, ToolResult,
};
use taskloom::{
    native_providers::{NativeProtocol, NativeProvider},
    ChatRequest, Provider,
};

async fn serve_once(value: Value, status: u16) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        format!("http://{address}"),
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some(end) = text.find("\r\n\r\n") {
                    let size = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            let body = value.to_string();
            let header=format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
            String::from_utf8(bytes).unwrap()
        }),
    )
}
fn turn() -> ModelRequest {
    ModelRequest {
        label: "native-test".into(),
        model: "test-model".into(),
        messages: vec![
            Message::system("host"),
            Message::user("question"),
            Message::Assistant {
                turn: AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "old".into(),
                        name: "lookup".into(),
                        arguments: json!({"q":"test"}),
                    }],
                },
            },
            Message::Tool {
                result: ToolResult {
                    call_id: "old".into(),
                    output: json!({"value":7}),
                    is_error: false,
                },
            },
        ],
        tools: vec![ToolDefinition {
            name: "lookup".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
            effect: ToolEffect::ReadOnly,
            permissions: Default::default(),
        }],
        temperature: 0.0,
        max_output_tokens: 100,
    }
}
fn response(protocol: NativeProtocol) -> Value {
    match protocol {
        NativeProtocol::Responses => {
            json!({"status":"completed","output":[{"type":"function_call","call_id":"next","name":"lookup","arguments":"{\"q\":\"next\"}"}],"usage":{"input_tokens":9,"output_tokens":3}})
        }
        NativeProtocol::Gemini | NativeProtocol::Vertex => {
            json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"functionCall":{"id":"next","name":"lookup","args":{"q":"next"}}}]}}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":3,"thoughtsTokenCount":2}})
        }
        NativeProtocol::Bedrock => {
            json!({"stopReason":"tool_use","output":{"message":{"role":"assistant","content":[{"toolUse":{"toolUseId":"next","name":"lookup","input":{"q":"next"}}}]}},"usage":{"inputTokens":9,"outputTokens":3}})
        }
        NativeProtocol::Azure => {
            json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":"next","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"next\"}"}}]}}],"usage":{"prompt_tokens":9,"completion_tokens":3}})
        }
    }
}
#[tokio::test]
async fn protocols_preserve_tool_history_credentials_and_usage() {
    for protocol in [
        NativeProtocol::Responses,
        NativeProtocol::Gemini,
        NativeProtocol::Vertex,
        NativeProtocol::Bedrock,
        NativeProtocol::Azure,
    ] {
        let (mut endpoint, server) = serve_once(response(protocol), 200).await;
        if protocol == NativeProtocol::Azure {
            endpoint
                .push_str("/openai/deployments/test-model/chat/completions?api-version=2024-10-21");
        }
        let provider = NativeProvider::new(protocol, &endpoint, "local-test-key").unwrap();
        assert!(!format!("{provider:?}").contains("local-test-key"));
        let reply = provider.converse(&turn()).await.unwrap();
        let usage = reply.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(9));
        assert_eq!(
            usage.completion_tokens,
            Some(
                if matches!(protocol, NativeProtocol::Gemini | NativeProtocol::Vertex) {
                    5
                } else {
                    3
                }
            )
        );
        assert_eq!(reply.turn.unwrap().tool_calls[0].id, "next");
        let wire = server.await.unwrap();
        let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(wire.contains("local-test-key"));
        assert!(!body.to_string().contains("local-test-key"));
        match protocol {
            NativeProtocol::Responses => {
                assert!(wire.starts_with("POST /responses "));
                assert_eq!(body["store"], false);
                assert_eq!(body["input"][3]["call_id"], "old");
            }
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                assert!(wire.starts_with("POST /models/test-model:generateContent "));
                assert_eq!(
                    body["contents"][2]["parts"][0]["functionResponse"]["name"],
                    "lookup"
                );
            }
            NativeProtocol::Bedrock => {
                assert!(wire.starts_with("POST /model/test-model/converse "));
                assert_eq!(
                    body["messages"][2]["content"][0]["toolResult"]["toolUseId"],
                    "old"
                );
            }
            NativeProtocol::Azure => {
                assert!(wire.contains("api-version=2024-10-21"));
                assert_eq!(body["messages"][3]["tool_call_id"], "old");
            }
        }
    }
}
#[tokio::test]
async fn protocol_failures_retain_observed_usage_and_never_replay() {
    let mut cases = Vec::new();
    let mut truncated = response(NativeProtocol::Responses);
    truncated["status"] = json!("incomplete");
    cases.push((NativeProtocol::Responses, truncated));
    let mut invalid = response(NativeProtocol::Bedrock);
    invalid["output"]["message"]["content"][0]["toolUse"]["input"] = json!("invalid");
    cases.push((NativeProtocol::Bedrock, invalid));
    let mut signature = response(NativeProtocol::Gemini);
    signature["candidates"][0]["content"]["parts"][0]["thoughtSignature"] = json!("opaque");
    cases.push((NativeProtocol::Gemini, signature));
    for (protocol, value) in cases {
        let (endpoint, server) = serve_once(value, 200).await;
        let provider = NativeProvider::new(protocol, endpoint, "test").unwrap();
        let reply = provider.converse(&turn()).await.unwrap();
        assert!(reply.turn.is_err());
        assert_eq!(reply.usage.unwrap().prompt_tokens, Some(9));
        server.await.unwrap();
    }
}
#[tokio::test]
async fn refusal_and_http_errors_are_typed_without_secret_body_echo() {
    let (endpoint,server)=serve_once(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}],"usage":{"input_tokens":2,"output_tokens":1}}),200).await;
    let reply = NativeProvider::new(NativeProtocol::Responses, endpoint, "test")
        .unwrap()
        .converse(&turn())
        .await
        .unwrap();
    assert!(matches!(
        reply.turn,
        Err(taskloom::ModelError::ContentRejected { .. })
    ));
    assert_eq!(reply.usage.unwrap().completion_tokens, Some(1));
    server.await.unwrap();
    let (endpoint, server) = serve_once(
        json!({"error":{"message":"secret-example","code":"auth"}}),
        401,
    )
    .await;
    let error = NativeProvider::new(NativeProtocol::Responses, endpoint, "test")
        .unwrap()
        .converse(&turn())
        .await
        .unwrap_err();
    assert!(matches!(error, taskloom::ModelError::Auth { .. }));
    assert!(!error.to_string().contains("secret-example"));
    server.await.unwrap();
}
#[tokio::test]
async fn azure_endpoint_version_and_redirects_fail_closed() {
    assert!(NativeProvider::new(NativeProtocol::Responses, "http://example.com/v1", "x").is_err());
    assert!(NativeProvider::new(
        NativeProtocol::Gemini,
        "https://example.com?key=secret",
        "x"
    )
    .is_err());
    assert!(NativeProvider::new(
        NativeProtocol::Azure,
        "https://example.com/openai/deployments/x/chat/completions",
        "x"
    )
    .is_err());
    let (endpoint, server) = serve_once(json!({"location":"https://example.com"}), 302).await;
    assert!(
        NativeProvider::new(NativeProtocol::Responses, endpoint, "test")
            .unwrap()
            .converse(&turn())
            .await
            .is_err()
    );
    server.await.unwrap();
}
#[tokio::test]
async fn typed_responses_schema_and_stateless_chat() {
    let (endpoint,server)=serve_once(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":true}"}]}],"usage":{"input_tokens":4,"output_tokens":2}}),200).await;
    let request = ChatRequest {
        model: "test".into(),
        system_prompt: "host".into(),
        user_prompt: "hello".into(),
        images: vec![],
        temperature: 0.0,
        max_output_tokens: Some(32),
        json: true,
        schema: Some(json!({"type":"object"})),
        schema_name: "answer".into(),
        seed: None,
    };
    let response = NativeProvider::new(NativeProtocol::Responses, endpoint, "test")
        .unwrap()
        .complete(&request, None)
        .await
        .unwrap();
    assert_eq!(response.content, "{\"ok\":true}");
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    let wire = server.await.unwrap();
    let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["text"]["format"]["type"], "json_schema");
    assert_eq!(body["store"], false);
    assert!(body.get("previous_response_id").is_none());
}
#[test]
fn app_native_configuration_is_pure_and_frozen() {
    // No actual credential resolution in validation or planning.
    #[cfg(feature = "app")]
    {
        let mut config = taskloom::runtime::app::AppConfig {
            provider: taskloom::runtime::app::AppProvider::Native {
                protocol: NativeProtocol::Vertex,
                base_url: "https://example.com/v1/projects/p/locations/global/publishers/google"
                    .into(),
                key_env: "TASKLOOM_TEST_MISSING_NATIVE_KEY".into(),
                namespace: "project-a".into(),
            },
            ..Default::default()
        };
        config.limits.max_cost_micros = Some(0);
        config.prices = vec![taskloom::gateway::ModelPrice::new(
            config.model.clone(),
            0.0,
            0.0,
        )];
        config.validate().unwrap();
        let preview = taskloom::runtime::app::preview_configuration(config.clone()).unwrap();
        assert!(serde_json::to_string(&preview)
            .unwrap()
            .contains("TASKLOOM_TEST_MISSING_NATIVE_KEY"));
        config.enable_thinking = Some(false);
        assert!(config.validate().is_err());
    }
    let _ = Duration::from_secs(1);
}

#[tokio::test]
async fn malformed_or_unsupported_native_blocks_never_become_successful_turns() {
    let mut cases = Vec::new();
    let mut unknown = response(NativeProtocol::Responses);
    unknown["output"] = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"plausible"},{"type":"audio","data":"unsupported"}]}]);
    cases.push((NativeProtocol::Responses, unknown));
    let mut reasoning = response(NativeProtocol::Responses);
    reasoning["output"] = json!([{"type":"reasoning","encrypted_content":"opaque-required-state"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"plausible"}]}]);
    cases.push((NativeProtocol::Responses, reasoning));
    let mut missing_args = response(NativeProtocol::Gemini);
    missing_args["candidates"][0]["content"]["parts"][0]["functionCall"]
        .as_object_mut()
        .unwrap()
        .remove("args");
    cases.push((NativeProtocol::Gemini, missing_args));
    let mut unsupported = response(NativeProtocol::Gemini);
    unsupported["candidates"][0]["content"]["parts"] =
        json!([{"text":"plausible"},{"inlineData":{"mimeType":"image/png","data":"x"}}]);
    cases.push((NativeProtocol::Gemini, unsupported));
    let mut unknown_bedrock = response(NativeProtocol::Bedrock);
    unknown_bedrock["stopReason"] = json!("end_turn");
    unknown_bedrock["output"]["message"]["content"] =
        json!([{"text":"plausible"},{"reasoningContent":{"signature":"opaque"}}]);
    cases.push((NativeProtocol::Bedrock, unknown_bedrock));
    let mut wrong_array = response(NativeProtocol::Azure);
    wrong_array["choices"][0]["finish_reason"] = json!("stop");
    wrong_array["choices"][0]["message"]["content"] = json!("plausible");
    wrong_array["choices"][0]["message"]["tool_calls"] = json!({"unexpected":"not an array"});
    cases.push((NativeProtocol::Azure, wrong_array));
    for (protocol, value) in cases {
        let (mut endpoint, server) = serve_once(value, 200).await;
        if protocol == NativeProtocol::Azure {
            endpoint
                .push_str("/openai/deployments/test-model/chat/completions?api-version=2024-10-21");
        }
        let reply = NativeProvider::new(protocol, endpoint, "fixture")
            .unwrap()
            .converse(&turn())
            .await
            .unwrap();
        assert!(reply.turn.is_err(), "{protocol:?}");
        assert_eq!(reply.usage.unwrap().prompt_tokens, Some(9));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn unadvertised_calls_are_not_promoted_to_host_authority() {
    let mut request = turn();
    request.tools.clear();
    let (endpoint, server) = serve_once(response(NativeProtocol::Responses), 200).await;
    let reply = NativeProvider::new(NativeProtocol::Responses, endpoint, "fixture")
        .unwrap()
        .converse(&request)
        .await
        .unwrap();
    assert!(reply.turn.is_err());
    assert_eq!(reply.usage.unwrap().completion_tokens, Some(3));
    server.await.unwrap();
}

#[tokio::test]
async fn native_typed_json_prefix_and_text_truncation_are_rejected_after_usage_accounting() {
    use std::sync::Arc;
    use taskloom::{
        Format, Gateway, GatewayTransport, Phase, Request, Transport, TransportOptions,
    };
    for format in [Format::Json, Format::Text] {
        let value = json!({"status":"incomplete","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":true}"}]}],"usage":{"input_tokens":7,"output_tokens":4}});
        let (endpoint, server) = serve_once(value, 200).await;
        let gateway = Arc::new(Gateway::default());
        let transport = GatewayTransport::new(
            gateway.clone(),
            NativeProvider::new(NativeProtocol::Responses, endpoint, "fixture").unwrap(),
        );
        let options = TransportOptions::default();
        let error = transport
            .send(Request {
                label: "native-typed-truncated",
                model: "test-model",
                system_prompt: "host",
                user_prompt: "question",
                images: &[],
                attachment: None,
                format,
                phase: Phase::Single,
                temperature: 0.0,
                max_output_tokens: 32,
                schema: None,
                options: &options,
                on_delta: None,
            })
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported, malformed or incomplete"));
        assert_eq!(gateway.stats().max_actual_prompt_tokens, Some(7));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn azure_deployment_model_mismatch_is_refused_before_dispatch() {
    assert!(NativeProvider::validate_model_endpoint(NativeProtocol::Azure,"http://127.0.0.1:9/openai/deployments/deployment-a/chat/completions?api-version=2024-10-21","deployment-a").is_ok());
    #[cfg(feature = "app")]
    {
        let config=taskloom::runtime::app::AppConfig{model:"different-model".into(),provider:taskloom::runtime::app::AppProvider::Native{protocol:NativeProtocol::Azure,base_url:"http://127.0.0.1:9/openai/deployments/deployment-a/chat/completions?api-version=2024-10-21".into(),key_env:"MISSING_PREFLIGHT_KEY".into(),namespace:"azure".into()},..Default::default()};
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("exact deployment"));
    }
    let provider=NativeProvider::new(NativeProtocol::Azure,"http://127.0.0.1:9/openai/deployments/deployment-a/chat/completions?api-version=2024-10-21","fixture").unwrap();
    let error = provider.converse(&turn()).await.unwrap_err();
    assert!(matches!(error, taskloom::ModelError::Misconfigured { .. }));
    assert!(error.to_string().contains("exact deployment"));
}
