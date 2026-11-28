use super::*;
use crate::runtime::{AssistantTurn, Message, ModelReply, ModelRequest, ToolCall};

pub(super) async fn converse(
    provider: &Anthropic,
    request: &ModelRequest,
) -> Result<ModelReply, ModelError> {
    if request.model.trim().is_empty() {
        return Err(ModelError::misconfigured("configure the model"));
    }
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        let (role, blocks) = match message {
            Message::System { content } => {
                system.push(content.as_str());
                continue;
            }
            Message::User { content } => ("user", vec![json!({"type":"text", "text":content})]),
            Message::Assistant { turn } => {
                let mut blocks = Vec::new();
                if !turn.content.is_empty() {
                    blocks.push(json!({"type":"text", "text":turn.content}));
                }
                blocks.extend(turn.tool_calls.iter().map(|call| json!({"type":"tool_use", "id":call.id, "name":call.name, "input":call.arguments})));
                ("assistant", blocks)
            }
            Message::Tool { result } => (
                "user",
                vec![json!({
                    "type":"tool_result", "tool_use_id":result.call_id,
                    "content":result.output.to_string(), "is_error":result.is_error,
                })],
            ),
        };
        // Consecutive tool results belong to a single native user turn.
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                last["content"].as_array_mut().unwrap().extend(blocks);
            }
            _ => messages.push(json!({"role":role, "content":blocks})),
        }
    }
    let mut body =
        json!({"model":request.model, "max_tokens":request.max_output_tokens, "messages":messages});
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if provider.sampling {
        body["temperature"] = json!(request.temperature);
    }
    if !request.tools.is_empty() {
        body["tools"] =
            json!(request.tools.iter().map(|tool| json!({
            "name":tool.name, "description":tool.description, "input_schema":tool.input_schema,
        })).collect::<Vec<_>>());
    }
    let response = provider.post(&body).await?;
    let value: Value = response.json().await.map_err(|error| {
        ModelError::schema_mismatch(format!("agent message is not JSON: {error}"))
    })?;
    let usage = parse_message(&value).usage;
    Ok(ModelReply {
        turn: parse_turn(&value),
        usage,
    })
}

fn parse_turn(value: &Value) -> Result<AssistantTurn, ModelError> {
    let reason = value.get("stop_reason").and_then(Value::as_str);
    if reason == Some("refusal") {
        return Err(ModelError::ContentRejected {
            detail: "model refused the agent turn".into(),
        });
    }
    if !matches!(reason, Some("end_turn" | "tool_use")) {
        return Err(ModelError::schema_mismatch(
            "agent message is truncated or has no supported stop reason",
        ));
    }
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| ModelError::schema_mismatch("agent message has no content blocks"))?;
    let mut turn = AssistantTurn::default();
    for block in blocks {
        let string = |key: &str| {
            block.get(key).and_then(Value::as_str).ok_or_else(|| {
                ModelError::schema_mismatch(format!("agent content block is missing {key}"))
            })
        };
        match string("type")? {
            "text" => turn.content.push_str(string("text")?),
            "tool_use" => turn.tool_calls.push(ToolCall {
                id: string("id")?.into(),
                name: string("name")?.into(),
                arguments: block
                    .get("input")
                    .cloned()
                    .ok_or_else(|| ModelError::schema_mismatch("tool_use is missing input"))?,
            }),
            // Reasoning blocks are not tool output or final user-facing text.
            "thinking" | "redacted_thinking" => {}
            _ => {
                return Err(ModelError::schema_mismatch(
                    "unsupported agent content block",
                ))
            }
        }
    }
    if (reason == Some("tool_use")) == turn.tool_calls.is_empty() {
        return Err(ModelError::schema_mismatch(
            "tool calls do not match the stop reason",
        ));
    }
    turn.validate()
        .map_err(|error| ModelError::schema_mismatch(error.to_string()))?;
    Ok(turn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ToolResult;

    #[tokio::test]
    async fn groups_results_and_keeps_usage_on_truncation() {
        let value = json!({"stop_reason":"max_tokens", "content":[{"type":"text", "text":"partial"}], "usage":{"input_tokens":13,"output_tokens":4}});
        let (url, server) =
            crate::test_http::serve_once(200, false, vec![value.to_string()], Duration::ZERO).await;
        let mut messages = vec![
            Message::system("instructions"),
            Message::user("question"),
            Message::Assistant {
                turn: AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![
                        ToolCall {
                            id: "a".into(),
                            name: "lookup".into(),
                            arguments: json!({}),
                        },
                        ToolCall {
                            id: "b".into(),
                            name: "lookup".into(),
                            arguments: json!({}),
                        },
                    ],
                },
            },
        ];
        for id in ["a", "b"] {
            messages.push(Message::Tool {
                result: ToolResult {
                    call_id: id.into(),
                    output: json!("found"),
                    is_error: false,
                },
            });
        }
        let request = ModelRequest {
            label: "test".into(),
            model: "test".into(),
            messages,
            tools: vec![],
            temperature: 0.0,
            max_output_tokens: 100,
        };
        let reply = Anthropic::with_base_url(url, "test-key")
            .converse(&request)
            .await
            .unwrap();
        assert!(reply.turn.is_err());
        assert_eq!(reply.usage.unwrap().completion_tokens, Some(4));
        let wire = server.await.unwrap();
        let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["system"], "instructions");
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][2]["content"][1]["tool_use_id"], "b");
    }

    #[test]
    fn validates_native_tool_use() {
        let value = json!({"stop_reason":"tool_use","content":[{"type":"tool_use", "id":"a","name":"lookup", "input":{}}]});
        assert_eq!(parse_turn(&value).unwrap().tool_calls[0].id, "a");
        let value = json!({"stop_reason":"end_turn","content":[{"type":"tool_use", "id":"a","name":"lookup", "input":{}}]});
        assert!(parse_turn(&value).is_err());
    }
}
