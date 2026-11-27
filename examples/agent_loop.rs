//! Offline: cargo run --locked --features runtime --example agent_loop
use std::collections::BTreeSet;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::gateway::TokenUsage;
use taskloom::runtime::*;
use taskloom::{
    ChatRequest, ChatResponse, Gateway, GatewayTransport, ModelError, Provider, Streaming,
};

/// Deterministic model for an offline demonstration. Real providers supply
/// these same turns through their native tool-calling protocol.
struct OfflineProvider;

impl Provider for OfflineProvider {
    fn id(&self) -> &str {
        "offline://agent-example"
    }
    fn complete<'a>(
        &'a self,
        _: &'a ChatRequest,
        _: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        Box::pin(async { Err(ModelError::misconfigured("use native agent turns")) })
    }
    fn converse<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelReply, ModelError>> {
        Box::pin(async move {
            let results = request
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::Tool { result } => Some(result),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let turn = match results.as_slice() {
                [] => AssistantTurn {
                    content: "First double 3.".into(),
                    tool_calls: vec![ToolCall {
                        id: "double-1".into(),
                        name: "double".into(),
                        arguments: json!({"n":3}),
                    }],
                },
                [first] => AssistantTurn {
                    content: "Double the result again.".into(),
                    tool_calls: vec![ToolCall {
                        id: "double-2".into(),
                        name: "double".into(),
                        arguments: json!({"n":first.output}),
                    }],
                },
                [_, last] => AssistantTurn::text(format!("The answer is {}.", last.output)),
                _ => return Err(ModelError::schema_mismatch("unexpected transcript")),
            };
            Ok(ModelReply {
                turn: Ok(turn),
                usage: Some(TokenUsage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    total_tokens: Some(15),
                }),
            })
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = GatewayTransport::new(Arc::new(Gateway::default()), OfflineProvider);
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {
        name:"double".into(), description:"Double a nonnegative integer.".into(),
        input_schema:json!({"type":"object","properties":{"n":{"type":"integer","minimum":0,"maximum":1_000_000}},"required":["n"],"additionalProperties":false}),
        effect:ToolEffect::ReadOnly, permissions:BTreeSet::new(),
    }, |arguments:Value,_:ToolContext| async move { Ok(json!(arguments["n"].as_u64().unwrap()*2)) }))?;
    let sink = |event: &RunEvent| println!("{}: {:?}", event.sequence, event.kind);
    let report = AgentLoop::new(&model, &tools, "offline")
        .with_policy(ToolPolicy::default().allow("double"))
        .with_events(&sink)
        .run(
            "double-twice",
            vec![Message::user("Double 3, then double that result.")],
        )
        .await;
    assert_eq!(report.stop_reason, StopReason::Completed);
    assert_eq!(report.final_text(), Some("The answer is 12."));
    assert_eq!((report.steps, report.tool_calls), (3, 2));
    println!("{}", report.final_text().unwrap());
    println!(
        "{} model turns, {} tools, {} reported tokens",
        report.steps,
        report.tool_calls,
        report.usage.input_tokens + report.usage.output_tokens
    );
    Ok(())
}
