//! The production features together, offline: costs in the ledger, a
//! progress callback, a repair turn, and cancellation.
//! `cargo run -p taskloom --example production`

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use taskloom::gateway::{ModelPrice, PriceTable, TokenUsage};
use taskloom::ledger::CallLedger;
use taskloom::{
    Agent, Answer, CancelToken, ChatRequest, ChatResponse, Environment, Gateway, GatewayConfig,
    GatewayTransport, ModelError, PromptTask, Protocol, Provider, Rejection, Streaming, TaskError,
    TaskSpec,
};

/// A stand-in provider: streams its answer in two pieces, reports usage, and
/// forgets a field on its first answer.
#[derive(Default)]
struct Demo {
    calls: AtomicUsize,
}

impl Provider for Demo {
    fn id(&self) -> &str {
        "https://demo.invalid/v1"
    }

    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        streaming: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if request.user_prompt.contains("slow") {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            let content = if call == 0 {
                r#"{"title": "Typed tasks"}"#
            } else {
                r#"{"title": "Typed tasks", "tags": ["rust", "llm"]}"#
            };
            if let Some(on_delta) = streaming.and_then(|streaming| streaming.on_delta) {
                let (head, tail) = content.split_at(content.len() / 2);
                on_delta(head);
                on_delta(tail);
            }
            Ok(ChatResponse {
                content: content.to_string(),
                usage: Some(TokenUsage {
                    prompt_tokens: Some(1_200),
                    completion_tokens: Some(40),
                    total_tokens: Some(1_240),
                }),
                finish_reason: Some("stop".into()),
            })
        })
    }
}

fn decode(answer: Answer) -> Result<serde_json::Value, Rejection> {
    let value = answer.into_json()?;
    if value.get("tags").and_then(|tags| tags.as_array()).is_none() {
        return Err(Rejection::new("the answer has no \"tags\" array"));
    }
    Ok(value)
}

#[tokio::main]
async fn main() {
    let gateway = Arc::new(Gateway::new(GatewayConfig {
        prices: PriceTable::new([ModelPrice::new("demo", 2.0, 8.0)]),
        ..GatewayConfig::default()
    }));
    let transport = GatewayTransport::new(gateway.clone(), Demo::default());
    let environment = Environment::new();
    let progress = |_label: &str, _model: &str, delta: &str| print!("{delta}");
    let agent = Agent::new(&transport, &environment).with_progress(&progress);

    // A repair turn: the first answer misses "tags", the model is told why.
    let task = PromptTask::new(
        "title_and_tags",
        TaskSpec::new("demo-model", Protocol::Json, 0.2, 200).repair(1),
        r#"Title the text and tag it. Answer {"title": "...", "tags": ["..."]}."#,
        "taskloom runs typed LLM tasks.",
        decode,
    );
    let ledger = CallLedger::new();
    let outcome = ledger
        .scope(agent.run_traced(&task))
        .await
        .expect("an answer");
    println!();
    println!("answer: {}", outcome.output);
    println!(
        "usage: {} in / {} out tokens, cost {:.6}",
        outcome.usage.input_tokens,
        outcome.usage.output_tokens,
        outcome.usage.cost()
    );
    println!("ledger: {:?}", ledger.snapshot().calls);

    // Cancellation: a slow request stopped after 100 ms.
    let token = CancelToken::new();
    let slow = PromptTask::new(
        "slow",
        TaskSpec::new("demo-model", Protocol::Json, 0.2, 200),
        "system",
        "slow request",
        decode,
    );
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });
    let error = Agent::new(&transport, &environment)
        .with_cancel(&token)
        .run(&slow)
        .await
        .unwrap_err();
    assert!(matches!(error, TaskError::Cancelled));
    println!("slow task: {error}");
}
