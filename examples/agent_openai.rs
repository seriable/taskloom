//! Live model: OPENAI_API_KEY and TASKLOOM_MODEL are required.
//! cargo run --locked --features runtime,openai --example agent_openai
use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use taskloom::openai::OpenAiCompatible;
use taskloom::runtime::*;
use taskloom::{Gateway, GatewayTransport};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("OPENAI_API_KEY")?;
    let model_id = std::env::var("TASKLOOM_MODEL")?;
    let base =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = GatewayTransport::new(
        Arc::new(Gateway::default()),
        OpenAiCompatible::new(base, key),
    );
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {
        name:"multiply".into(),description:"Multiply two integers.".into(),
        input_schema:json!({"type":"object","properties":{"a":{"type":"integer","minimum":-1_000_000,"maximum":1_000_000},"b":{"type":"integer","minimum":-1_000_000,"maximum":1_000_000}},"required":["a","b"],"additionalProperties":false}),
        effect:ToolEffect::ReadOnly,permissions:BTreeSet::new(),
    }, |args:Value,_:ToolContext| async move {Ok(json!(args["a"].as_i64().unwrap()*args["b"].as_i64().unwrap()))}))?;
    let report = AgentLoop::new(&model, &tools, model_id)
        .with_policy(ToolPolicy::default().allow("multiply"))
        .run(
            "multiply-example",
            vec![
                Message::system("Use the multiply tool for each calculation."),
                Message::user("Calculate 17 times 23, then multiply the result by 5."),
            ],
        )
        .await;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.stop_reason != StopReason::Completed {
        return Err(format!("agent stopped: {:?}", report.stop_reason).into());
    }
    Ok(())
}
