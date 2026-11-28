use super::*;
use crate::runtime::{AssistantTurn, Message, ModelReply, ModelRequest, ToolCall};

pub(super) async fn converse(
    provider: &OpenAiCompatible,
    request: &ModelRequest,
) -> Result<ModelReply, ModelError> {
    provider.check(&request.model)?;
    let messages = request.messages.iter().map(|message| match message {
        Message::System { content } => json!({"role": "system", "content": content}),
        Message::User { content } => json!({"role": "user", "content": content}),
        Message::Assistant { turn } => {
            let mut message = json!({"role": "assistant", "content": turn.content});
            if !turn.tool_calls.is_empty() {
                message["tool_calls"] = json!(turn.tool_calls.iter().map(|call| json!({
                    "id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments.to_string()},
                })).collect::<Vec<_>>());
            }
            message
        }
        Message::Tool { result } => json!({
            "role": "tool", "tool_call_id": result.call_id,
            "content": json!({"output": result.output, "is_error": result.is_error}).to_string(),
        }),
    }).collect::<Vec<_>>();
    let mut body = json!({
        "model": request.model, "messages": messages, "temperature": request.temperature,
        "max_tokens": request.max_output_tokens,
    });
    provider.generation_options(&mut body);
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(|tool| json!({
            "type": "function", "function": {"name": tool.name, "description": tool.description, "parameters": tool.input_schema},
        })).collect::<Vec<_>>());
    }
    let response = provider.post("chat/completions", &body).await?;
    let value: Value = response.json().await.map_err(|error| {
        ModelError::schema_mismatch(format!("agent completion is not JSON: {error}"))
    })?;
    let usage = value
        .get("usage")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    Ok(ModelReply {
        turn: parse_turn(&value),
        usage,
    })
}

fn parse_turn(value: &Value) -> Result<AssistantTurn, ModelError> {
    let choice = value
        .pointer("/choices/0")
        .ok_or_else(|| ModelError::schema_mismatch("agent completion has no choice"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ModelError::schema_mismatch("agent completion has no message"))?;
    let reason = choice.get("finish_reason").and_then(Value::as_str);
    if has_refusal(message) || matches!(reason, Some("refusal" | "content_filter")) {
        return Err(ModelError::ContentRejected {
            detail: "model refused the agent turn".into(),
        });
    }
    if !matches!(reason, Some("stop" | "tool_calls")) {
        return Err(ModelError::schema_mismatch(
            "agent completion is truncated or has no supported finish reason",
        ));
    }
    let content = match message.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        _ => return Err(ModelError::schema_mismatch("unsupported assistant content")),
    };
    let calls = match message.get("tool_calls") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(calls)) => calls
            .iter()
            .map(|call| {
                if call.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(ModelError::schema_mismatch("unsupported tool call type"));
                }
                let string = |path: &str| {
                    call.pointer(path).and_then(Value::as_str).ok_or_else(|| {
                        ModelError::schema_mismatch(format!("tool call is missing {path}"))
                    })
                };
                let arguments =
                    serde_json::from_str(string("/function/arguments")?).map_err(|error| {
                        ModelError::schema_mismatch(format!("invalid tool arguments: {error}"))
                    })?;
                Ok(ToolCall {
                    id: string("/id")?.into(),
                    name: string("/function/name")?.into(),
                    arguments,
                })
            })
            .collect::<Result<Vec<_>, ModelError>>()?,
        _ => return Err(ModelError::schema_mismatch("tool_calls must be an array")),
    };
    if (reason == Some("tool_calls")) == calls.is_empty() {
        return Err(ModelError::schema_mismatch(
            "tool calls do not match the finish reason",
        ));
    }
    let turn = AssistantTurn {
        content,
        tool_calls: calls,
    };
    turn.validate()
        .map_err(|error| ModelError::schema_mismatch(error.to_string()))?;
    Ok(turn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ToolResult;

    #[tokio::test]
    async fn native_history_preserves_tool_ids_and_usage_on_invalid_arguments() {
        let body = json!({"choices":[{"finish_reason":"tool_calls", "message":{"tool_calls":[{
            "id":"next", "type":"function", "function":{"name":"lookup", "arguments":"{broken"}
        }]}}], "usage":{"prompt_tokens":11,"completion_tokens":3}});
        let (url, server) =
            crate::test_http::serve_once(200, false, vec![body.to_string()], Duration::ZERO).await;
        let request = ModelRequest {
            label: "test".into(),
            model: "test".into(),
            temperature: 0.0,
            max_output_tokens: 100,
            tools: vec![],
            messages: vec![
                Message::system("instructions"),
                Message::user("question"),
                Message::Assistant {
                    turn: AssistantTurn {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "first".into(),
                            name: "lookup".into(),
                            arguments: json!({}),
                        }],
                    },
                },
                Message::Tool {
                    result: ToolResult {
                        call_id: "first".into(),
                        output: json!("found"),
                        is_error: false,
                    },
                },
            ],
        };
        let reply = OpenAiCompatible::new(url, "test-key")
            .with_enable_thinking(Some(false))
            .converse(&request)
            .await
            .unwrap();
        assert!(reply.turn.is_err());
        assert_eq!(reply.usage.unwrap().prompt_tokens, Some(11));
        let wire = server.await.unwrap();
        let body: Value = serde_json::from_str(wire.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["enable_thinking"], false);
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "first");
        assert_eq!(body["messages"][3]["tool_call_id"], "first");
        assert_eq!(body["messages"][3]["role"], "tool");
    }

    #[test]
    fn refuses_incomplete_or_inconsistent_turns() {
        for reason in ["length", "stop", "", "tool_calls"] {
            let value = json!({"choices":[{"finish_reason":reason,"message":{"content":null}}]});
            assert!(parse_turn(&value).is_err());
        }
        let value = json!({"choices":[{"finish_reason":"stop","message":{"content":"ok", "refusal":"refused"}}]});
        assert!(matches!(
            parse_turn(&value),
            Err(ModelError::ContentRejected { .. })
        ));
    }
}
