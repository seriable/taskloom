use super::*;
use crate::runtime::{AssistantTurn, Message, ModelReply, ModelRequest, ToolCall};

impl NativeProvider {
    pub(super) async fn native_turn(
        &self,
        request: &ModelRequest,
    ) -> Result<ModelReply, ModelError> {
        if request.max_output_tokens == 0 || !request.temperature.is_finite() {
            return Err(ModelError::misconfigured(
                "native turn requires positive output bound",
            ));
        }
        // Validate correlation before encoding protocol-specific messages.
        crate::runtime::types::validate_history(&request.messages)
            .map_err(|_| ModelError::misconfigured("invalid native conversation history"))?;
        let body = self.turn_body(request)?;
        let value = self.post(&request.model, &body).await?;
        let turn = self.parse_turn(&value).and_then(|turn| {
            if turn
                .tool_calls
                .iter()
                .any(|call| !request.tools.iter().any(|tool| tool.name == call.name))
            {
                return Err(ModelError::schema_mismatch(
                    "native model requested an unadvertised tool",
                ));
            }
            Ok(turn)
        });
        Ok(ModelReply {
            usage: self.usage(&value),
            turn,
        })
    }
    fn turn_body(&self, request: &ModelRequest) -> Result<Value, ModelError> {
        let mut systems = Vec::new();
        let mut input = Vec::new();
        let mut call_names = std::collections::BTreeMap::new();
        for message in &request.messages {
            match self.protocol {
                NativeProtocol::Responses => match message {
                    Message::System{content}=>input.push(json!({"role":"system","content":content})),
                    Message::User{content}=>input.push(json!({"role":"user","content":content})),
                    Message::Assistant{turn}=>{
                        if !turn.content.is_empty(){input.push(json!({"role":"assistant","content":turn.content}));}
                        for call in &turn.tool_calls{input.push(json!({"type":"function_call","call_id":call.id,"name":call.name,"arguments":call.arguments.to_string()}));}
                    }
                    Message::Tool{result}=>input.push(json!({"type":"function_call_output","call_id":result.call_id,"output":json!({"output":result.output,"is_error":result.is_error}).to_string()})),
                },
                NativeProtocol::Gemini|NativeProtocol::Vertex => match message {
                    Message::System{content}=>systems.push(json!({"text":content})),
                    Message::User{content}=>input.push(json!({"role":"user","parts":[{"text":content}]})),
                    Message::Assistant{turn}=>{
                        let mut parts=Vec::new(); if !turn.content.is_empty(){parts.push(json!({"text":turn.content}));}
                        for call in &turn.tool_calls {call_names.insert(call.id.clone(),call.name.clone());parts.push(json!({"functionCall":{"id":call.id,"name":call.name,"args":call.arguments}}));}
                        input.push(json!({"role":"model","parts":parts}));
                    }
                    Message::Tool{result}=>{
                        let name=call_names.get(&result.call_id).ok_or_else(||ModelError::misconfigured("missing Gemini tool name"))?;
                        let part=json!({"functionResponse":{"id":result.call_id,"name":name,"response":{"output":result.output,"is_error":result.is_error}}});
                        // Parallel function responses must occupy one user turn.
                        if let Some(last)=input.last_mut().filter(|v|v.get("role").and_then(Value::as_str)==Some("user") && v.pointer("/parts/0/functionResponse").is_some()) {
                            last["parts"].as_array_mut().unwrap().push(part);
                        }else{input.push(json!({"role":"user","parts":[part]}));}
                    }
                },
                NativeProtocol::Bedrock => match message {
                    Message::System{content}=>systems.push(json!({"text":content})),
                    Message::User{content}=>input.push(json!({"role":"user","content":[{"text":content}]})),
                    Message::Assistant{turn}=>{
                        let mut parts=Vec::new();if !turn.content.is_empty(){parts.push(json!({"text":turn.content}));}
                        for call in &turn.tool_calls{parts.push(json!({"toolUse":{"toolUseId":call.id,"name":call.name,"input":call.arguments}}));}
                        input.push(json!({"role":"assistant","content":parts}));
                    }
                    Message::Tool{result}=>{
                        let part=json!({"toolResult":{"toolUseId":result.call_id,"content":[{"json":result.output}],"status":if result.is_error{"error"}else{"success"}}});
                        if let Some(last)=input.last_mut().filter(|v|v.get("role").and_then(Value::as_str)==Some("user") && v.pointer("/content/0/toolResult").is_some()){
                            last["content"].as_array_mut().unwrap().push(part);
                        }else{input.push(json!({"role":"user","content":[part]}));}
                    }
                },
                NativeProtocol::Azure => match message {
                    Message::System{content}=>input.push(json!({"role":"system","content":content})),
                    Message::User{content}=>input.push(json!({"role":"user","content":content})),
                    Message::Assistant{turn}=>{
                        let mut value=json!({"role":"assistant","content":turn.content});
                        if !turn.tool_calls.is_empty(){value["tool_calls"]=json!(turn.tool_calls.iter().map(|c|json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments.to_string()}})).collect::<Vec<_>>());}
                        input.push(value);
                    }
                    Message::Tool{result}=>input.push(json!({"role":"tool","tool_call_id":result.call_id,"content":json!({"output":result.output,"is_error":result.is_error}).to_string()})),
                },
            }
        }
        let mut body = match self.protocol {
            NativeProtocol::Responses => {
                json!({"model":request.model,"store":false,"stream":false,"input":input,"max_output_tokens":request.max_output_tokens,"temperature":request.temperature})
            }
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                json!({"systemInstruction":{"parts":systems},"contents":input,"generationConfig":{"temperature":request.temperature,"maxOutputTokens":request.max_output_tokens}})
            }
            NativeProtocol::Bedrock => {
                json!({"system":systems,"messages":input,"inferenceConfig":{"temperature":request.temperature,"maxTokens":request.max_output_tokens}})
            }
            NativeProtocol::Azure => {
                json!({"messages":input,"max_tokens":request.max_output_tokens,"temperature":request.temperature,"stream":false})
            }
        };
        if !request.tools.is_empty() {
            match self.protocol {
                NativeProtocol::Responses=>body["tools"]=json!(request.tools.iter().map(|t|json!({"type":"function","name":t.name,"description":t.description,"parameters":t.input_schema,"strict":false})).collect::<Vec<_>>()),
                NativeProtocol::Gemini|NativeProtocol::Vertex=>body["tools"]=json!([{"functionDeclarations":request.tools.iter().map(|t|json!({"name":t.name,"description":t.description,"parametersJsonSchema":t.input_schema})).collect::<Vec<_>>()}]),
                NativeProtocol::Bedrock=>body["toolConfig"]=json!({"tools":request.tools.iter().map(|t|json!({"toolSpec":{"name":t.name,"description":t.description,"inputSchema":{"json":t.input_schema}}})).collect::<Vec<_>>()}),
                NativeProtocol::Azure=>body["tools"]=json!(request.tools.iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.input_schema}})).collect::<Vec<_>>()),
            }
        }
        Ok(body)
    }
    fn parse_turn(&self, value: &Value) -> Result<AssistantTurn, ModelError> {
        let (content, finish_reason) = self.response_text(value, true)?;
        if finish_reason == "refusal" {
            return Err(ModelError::ContentRejected {
                detail: "native model refused the turn".into(),
            });
        }
        if finish_reason != "stop" {
            return Err(ModelError::schema_mismatch(
                "native response is truncated or incomplete",
            ));
        }
        let mut calls = Vec::new();
        let parse_string = |value: &Value, path: &str| {
            value
                .pointer(path)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    ModelError::schema_mismatch(
                        "native function call is missing an identifier or name",
                    )
                })
        };
        let finished = match self.protocol {
            NativeProtocol::Responses => {
                for item in value
                    .get("output")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match item.get("type").and_then(Value::as_str) {
                        Some("function_call") => {
                            calls.push(ToolCall {
                                id: parse_string(item, "/call_id")?,
                                name: parse_string(item, "/name")?,
                                arguments: serde_json::from_str(
                                    item.get("arguments").and_then(Value::as_str).ok_or_else(
                                        || {
                                            ModelError::schema_mismatch(
                                                "native arguments must be JSON text",
                                            )
                                        },
                                    )?,
                                )
                                .map_err(|_| {
                                    ModelError::schema_mismatch("invalid native tool arguments")
                                })?,
                            });
                        }
                        Some("message") => {}
                        _ => {
                            return Err(ModelError::schema_mismatch(
                                "unsupported Responses output item; hosted tools are not enabled",
                            ))
                        }
                    }
                }
                value.get("status").and_then(Value::as_str) == Some("completed")
            }
            NativeProtocol::Gemini | NativeProtocol::Vertex => {
                for (index, part) in value
                    .pointer("/candidates/0/content/parts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    // Thought signatures cannot be represented in the legacy
                    // Message contract. Refuse rather than silently drop them.
                    if part.get("thoughtSignature").is_some() {
                        return Err(ModelError::schema_mismatch(
                            "Gemini thoughtSignature requires a signature-preserving model adapter",
                        ));
                    }
                    if let Some(call) = part.get("functionCall") {
                        if call.get("id").is_some_and(|value| {
                            value.as_str().is_none_or(|id| id.trim().is_empty())
                        }) {
                            return Err(ModelError::schema_mismatch(
                                "Gemini function identifier is malformed",
                            ));
                        }
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                format!(
                                    "gemini-{}-{index}",
                                    hex::encode(
                                        &sha2::Sha256::digest(value.to_string().as_bytes())[..8]
                                    )
                                )
                            });
                        calls.push(ToolCall {
                            id,
                            name: parse_string(call, "/name")?,
                            arguments: call.get("args").cloned().ok_or_else(|| {
                                ModelError::schema_mismatch("Gemini function arguments are missing")
                            })?,
                        });
                    }
                }
                value
                    .pointer("/candidates/0/finishReason")
                    .and_then(Value::as_str)
                    == Some("STOP")
            }
            NativeProtocol::Bedrock => {
                for part in value
                    .pointer("/output/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(call) = part.get("toolUse") {
                        calls.push(ToolCall {
                            id: parse_string(call, "/toolUseId")?,
                            name: parse_string(call, "/name")?,
                            arguments: call.get("input").cloned().ok_or_else(|| {
                                ModelError::schema_mismatch("Bedrock function input is missing")
                            })?,
                        });
                    }
                }
                let reason = value.get("stopReason").and_then(Value::as_str);
                if (reason == Some("tool_use")) == calls.is_empty() {
                    return Err(ModelError::schema_mismatch(
                        "Bedrock tool use does not match stop reason",
                    ));
                }
                matches!(reason, Some("end_turn" | "stop_sequence" | "tool_use"))
            }
            NativeProtocol::Azure => {
                for call in value
                    .pointer("/choices/0/message/tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if call.get("type").and_then(Value::as_str) != Some("function") {
                        return Err(ModelError::schema_mismatch("unsupported Azure tool type"));
                    }
                    calls.push(ToolCall {
                        id: parse_string(call, "/id")?,
                        name: parse_string(call, "/function/name")?,
                        arguments: serde_json::from_str(
                            call.pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    ModelError::schema_mismatch("Azure tool arguments missing")
                                })?,
                        )
                        .map_err(|_| ModelError::schema_mismatch("invalid Azure tool arguments"))?,
                    });
                }
                let reason = value
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str);
                if (reason == Some("tool_calls")) == calls.is_empty() {
                    return Err(ModelError::schema_mismatch(
                        "Azure tool use does not match finish reason",
                    ));
                }
                matches!(reason, Some("stop" | "tool_calls"))
            }
        };
        if !finished {
            return Err(ModelError::schema_mismatch(
                "native response is truncated or incomplete",
            ));
        }
        let turn = AssistantTurn {
            content,
            tool_calls: calls,
        };
        turn.validate().map_err(|_| {
            ModelError::schema_mismatch("native turn has empty or invalid tool calls")
        })?;
        Ok(turn)
    }
}
use sha2::Digest;
