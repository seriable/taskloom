//! A typed task on the Anthropic Messages API with a JSON Schema
//! (structured outputs), streaming progress and an idle timeout:
//!
//! ```text
//! ANTHROPIC_API_KEY=... cargo run -p taskloom --features anthropic --example anthropic
//! ```

use std::sync::Arc;
use std::time::Duration;

use taskloom::anthropic::Anthropic;
use taskloom::gateway::{ModelPrice, PriceTable};
use taskloom::{
    Agent, Environment, Gateway, GatewayConfig, GatewayTransport, PromptTask, Protocol, TaskSpec,
};

#[tokio::main]
async fn main() {
    let gateway = Arc::new(Gateway::new(GatewayConfig {
        // Claude Opus 5.5: $4 / $20 per million input / output tokens.
        prices: PriceTable::new([ModelPrice::new("claude-opus-5-5", 4.0, 20.0)]),
        ..GatewayConfig::default()
    }));
    let provider = Anthropic::new(std::env::var("ANTHROPIC_API_KEY").unwrap_or_default());
    let transport = GatewayTransport::new(gateway, provider)
        // Stream, and give up on an attempt that goes quiet for a minute.
        .with_idle_timeout(Some(Duration::from_secs(60)))
        .with_attempt_timeout(Some(Duration::from_secs(600)));
    let environment = Environment::new();
    let progress = |_label: &str, _model: &str, delta: &str| eprint!("{delta}");
    let agent = Agent::new(&transport, &environment).with_progress(&progress);

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "keywords": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["summary", "keywords"],
        "additionalProperties": false
    });
    let task = PromptTask::new(
        "summary",
        TaskSpec::new("claude-opus-5-5", Protocol::Json, 0.0, 2_000)
            .schema(schema)
            .repair(1),
        "Summarize the text in one sentence and list three keywords.",
        "taskloom is a Rust library that runs typed LLM tasks with model fallback, \
         resilient JSON, hedging, circuit breakers, call ledgers and record/replay.",
        taskloom::json_value,
    );
    match agent.run_traced(&task).await {
        Ok(outcome) => {
            eprintln!();
            println!("{}", outcome.output);
            println!(
                "{} in / {} out tokens, ${:.4}",
                outcome.usage.input_tokens,
                outcome.usage.output_tokens,
                outcome.usage.cost()
            );
        }
        Err(error) => eprintln!("failed: {error}"),
    }
}
