//! A real provider behind the gateway (budget check, breaker, retries,
//! fallback chain):
//!
//! ```text
//! OPENAI_BASE_URL=https://api.openai.com/v1 OPENAI_API_KEY=sk-... \
//! OPENAI_MODEL=gpt-4o-mini OPENAI_FALLBACK_MODEL=gpt-4o \
//! cargo run -p taskloom --features openai --example openai
//! ```

use std::sync::Arc;

use taskloom::gateway::{ModelProfileOverride, ModelRegistry, TaskEntry, TaskRegistry};
use taskloom::openai::OpenAiCompatible;
use taskloom::{
    json_value, Agent, Environment, Gateway, GatewayConfig, GatewayTransport, PromptTask, Protocol,
    TaskSpec,
};

#[tokio::main]
async fn main() {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let model = var("OPENAI_MODEL");
    let fallback = var("OPENAI_FALLBACK_MODEL");
    let gateway = Arc::new(Gateway::new(GatewayConfig {
        // The task's budget: refused before sending when the input is larger.
        tasks: TaskRegistry::new([TaskEntry::bounded_text("summary", 400, 0.2)]),
        models: ModelRegistry::new().with_overrides(if fallback.is_empty() {
            Vec::new()
        } else {
            vec![ModelProfileOverride {
                id: model.clone(),
                fallback_chain: Some(vec![fallback]),
                ..ModelProfileOverride::default()
            }]
        }),
        ..GatewayConfig::default()
    }));
    let provider = OpenAiCompatible::new(var("OPENAI_BASE_URL"), var("OPENAI_API_KEY"));
    let transport = GatewayTransport::new(gateway.clone(), provider);
    let environment = Environment::new();
    let task = PromptTask::new(
        "summary",
        TaskSpec::new(model.as_str(), Protocol::ResilientJson, 0.2, 400),
        r#"Summarize the text in one sentence. Answer {"summary": "..."}."#,
        "taskloom is a Rust library that runs typed LLM tasks with model fallback, \
         resilient JSON, hedging, circuit breakers, call ledgers and record/replay.",
        json_value,
    );
    match Agent::new(&transport, &environment).run_traced(&task).await {
        Ok(outcome) => println!("{} (model {})", outcome.output, outcome.model),
        Err(error) => eprintln!("failed: {error}"),
    }
    println!("{:#?}", gateway.stats());
}
