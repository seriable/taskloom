//! End-to-end accounting and serving-model regressions, without network calls.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use taskloom::gateway::{ModelPrice, ModelProfileOverride, ModelRegistry, PriceTable, TokenUsage};
use taskloom::ledger::CallLedger;
use taskloom::{
    json_value, Agent, Answer, ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse,
    Environment, Gateway, GatewayConfig, GatewayTransport, HedgePolicy, ModelError, PromptTask,
    Protocol, Provider, Recording, Rejection, Request, Response, Streaming, TaskError, TaskSpec,
    Transport,
};

struct Scripted {
    replies: Mutex<VecDeque<Result<ChatResponse, ModelError>>>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(replies: Vec<Result<ChatResponse, ModelError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            asked: Mutex::new(Vec::new()),
        }
    }
}

fn usage() -> TokenUsage {
    TokenUsage {
        prompt_tokens: Some(10),
        completion_tokens: Some(2),
        total_tokens: Some(12),
    }
}

fn reply(content: &str) -> Result<ChatResponse, ModelError> {
    Ok(ChatResponse {
        content: content.into(),
        usage: Some(usage()),
        finish_reason: Some("stop".into()),
    })
}

fn unavailable() -> Result<ChatResponse, ModelError> {
    Err(ModelError::transient(Some(503), "unavailable"))
}

impl Provider for Scripted {
    fn id(&self) -> &str {
        "https://regressions.test/v1"
    }

    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        _: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        self.asked.lock().unwrap().push(request.model.clone());
        let result = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        Box::pin(async move { result })
    }

    fn embed<'a>(
        &'a self,
        _: &'a EmbeddingRequest,
    ) -> BoxFuture<'a, Result<EmbeddingResponse, ModelError>> {
        Box::pin(async {
            Ok(EmbeddingResponse {
                vectors: vec![],
                usage: Some(usage()),
            })
        })
    }
}

fn gateway() -> Arc<Gateway> {
    Arc::new(Gateway::new(GatewayConfig {
        models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
            id: "primary".into(),
            fallback_chain: Some(vec!["backup".into()]),
            ..Default::default()
        }]),
        prices: PriceTable::new([
            ModelPrice::new("primary", 1.0, 2.0),
            ModelPrice::new("backup", 2.0, 4.0),
        ]),
        ..Default::default()
    }))
}

type Decode = fn(Answer) -> Result<serde_json::Value, Rejection>;

fn task(protocol: Protocol) -> PromptTask<'static, serde_json::Value, Decode> {
    PromptTask::new(
        "regression",
        TaskSpec::new("primary", protocol, 0.0, 100).step_cache(true),
        "system",
        "user",
        json_value as Decode,
    )
}

#[tokio::test]
async fn gateway_fallback_model_survives_cache_recording_and_replay() {
    let dir = std::env::temp_dir().join(format!("taskloom-serving-model-{}", std::process::id()));
    let recording = dir.join("record");
    let environment = Environment::new()
        .with_step_cache_dir(dir.join("cache"))
        .with_recording(Recording::Record(recording.clone()));
    let gateway = gateway();
    let provider = Arc::new(Scripted::new(vec![unavailable(), reply(r#"{"ok":true}"#)]));
    let transport =
        Arc::new(GatewayTransport::new(gateway, provider.clone()).with_transient_attempts(1));
    // Both reference and Arc transport wrappers must forward metadata.
    let reference = &transport;
    let agent = Agent::new(&reference, &environment);
    let task = task(Protocol::Json);
    let fresh = agent.run_traced(&task).await.unwrap();
    assert_eq!(fresh.model, "backup");
    assert_eq!(fresh.models_tried, 1, "counts the outer task chain");
    assert!(!fresh.reused);
    assert_eq!(fresh.usage.cost_micros, 28);
    let cached = agent.run_traced(&task).await.unwrap();
    assert_eq!(cached.model, "backup");
    assert!(cached.reused);
    assert_eq!(cached.usage.requests, 0);
    let replay = Environment::new().with_recording(Recording::Replay(recording));
    let replayed = Agent::new(&reference, &replay)
        .run_traced(&task)
        .await
        .unwrap();
    assert_eq!(replayed.model, "backup");
    assert_eq!(replayed.output, fresh.output);
    assert!(replayed.reused);
    assert_eq!(*provider.asked.lock().unwrap(), ["primary", "backup"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn invalid_json_and_refusals_keep_usage_and_error_classification() {
    for refusal_reason in [None, Some("refusal"), Some("content_filter")] {
        let refused = refusal_reason.is_some();
        let gateway = gateway();
        let mut response = reply("invalid JSON").unwrap();
        if let Some(reason) = refusal_reason {
            response.finish_reason = Some(reason.into());
        }
        let transport = GatewayTransport::new(gateway.clone(), Scripted::new(vec![Ok(response)]));
        let environment = Environment::new();
        let ledger = CallLedger::new();
        let task = task(Protocol::Json);
        let agent = Agent::new(&transport, &environment);
        let result = ledger.scope(agent.run(&task)).await;
        if refused {
            assert!(matches!(
                result,
                Err(TaskError::Provider(ModelError::ContentRejected { .. }))
            ));
        } else {
            assert!(matches!(
                result,
                Err(TaskError::Provider(ModelError::SchemaMismatch { .. }))
            ));
        }
        let stats = gateway.stats();
        assert_eq!(
            stats.sent_attempts, 1,
            "no retries or fallback for non-transient errors"
        );
        let usage = stats.usage;
        assert_eq!(usage.requests, 1);
        assert_eq!(
            (usage.input_tokens, usage.output_tokens, usage.cost_micros),
            (10, 2, 14)
        );
        assert_eq!(stats.max_actual_prompt_tokens, Some(10));
        let kind = if refused {
            "content_rejected"
        } else {
            "schema_mismatch"
        };
        assert_eq!(stats.outcomes.get(kind), Some(&1));
        let profile = ledger.snapshot();
        assert_eq!(profile.total_usage().cost_micros, 14);
        assert_eq!(profile.calls["regression"].failures, 1);
    }
}

#[tokio::test]
async fn resilient_json_counts_failed_responses_and_preserves_text_fallback_model() {
    let gateway = gateway();
    let provider = Arc::new(Scripted::new(vec![
        reply("invalid JSON"),
        reply("still invalid"),
        unavailable(),
        reply("Here is the answer: {\"ok\":true}"),
    ]));
    let transport =
        GatewayTransport::new(gateway.clone(), provider.clone()).with_transient_attempts(1);
    let environment = Environment::new();
    let outcome = Agent::new(&transport, &environment)
        .run_traced(&task(Protocol::ResilientJson))
        .await
        .unwrap();
    assert_eq!(outcome.model, "backup");
    assert_eq!(outcome.output, serde_json::json!({"ok":true}));
    assert_eq!(outcome.usage.requests, 3);
    assert_eq!(outcome.usage.cost_micros, 56);
    assert_eq!(gateway.stats().outcomes.get("schema_mismatch"), Some(&2));
    assert_eq!(
        *provider.asked.lock().unwrap(),
        ["primary", "primary", "primary", "backup"]
    );
}

#[tokio::test]
async fn repaired_answer_reports_the_model_that_served_the_repair() {
    let dir = std::env::temp_dir().join(format!("taskloom-repaired-model-{}", std::process::id()));
    let gateway = gateway();
    let provider = Arc::new(Scripted::new(vec![
        reply(r#"{"wrong":true}"#),
        unavailable(),
        reply(r#"{"ok":true}"#),
    ]));
    let transport = GatewayTransport::new(gateway, provider).with_transient_attempts(1);
    let recording = dir.join("record");
    let environment = Environment::new()
        .with_recording(Recording::Record(recording.clone()))
        .with_step_cache_dir(dir.join("cache"));
    let task = PromptTask::new(
        "repair",
        TaskSpec::new("primary", Protocol::Json, 0.0, 100)
            .repair(1)
            .step_cache(true),
        "system",
        "user",
        |answer: Answer| {
            let value = answer.into_json()?;
            if value.get("ok").is_none() {
                return Err(Rejection::new("missing ok"));
            }
            Ok(value)
        },
    );
    let outcome = Agent::new(&transport, &environment)
        .run_traced(&task)
        .await
        .unwrap();
    assert_eq!(outcome.model, "backup");
    assert_eq!(outcome.usage.requests, 2);
    assert_eq!(outcome.usage.cost_micros, 42);
    let cached = Agent::new(&transport, &environment)
        .run_traced(&task)
        .await
        .unwrap();
    assert!(cached.reused);
    assert_eq!(cached.model, "backup");
    let replay = Environment::new().with_recording(Recording::Replay(recording));
    let replayed = Agent::new(&transport, &replay)
        .run_traced(&task)
        .await
        .unwrap();
    assert!(replayed.reused);
    assert_eq!(replayed.model, "backup");
    assert_eq!(replayed.usage.requests, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_hedged_task_keeps_the_winning_responses_model() {
    struct Racing(AtomicUsize);

    impl Transport for Racing {
        fn send<'a>(&'a self, _: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
            Box::pin(async { panic!("the runner must use the metadata-aware method") })
        }

        fn send_with_metadata<'a>(
            &'a self,
            _: Request<'a>,
        ) -> BoxFuture<'a, Result<Response, TaskError>> {
            let copy = self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if copy == 0 {
                    std::future::pending::<()>().await;
                }
                Ok(Response {
                    answer: Answer::Json(serde_json::json!({"ok":true})),
                    model: "hedged-backup".into(),
                })
            })
        }
    }

    let transport = Racing(AtomicUsize::new(0));
    let environment = Environment::new();
    let task = PromptTask::new(
        "hedge",
        TaskSpec::new("primary", Protocol::Json, 0.0, 100)
            .hedge(Some(HedgePolicy {
                after: Duration::from_millis(5),
                copies: 2,
            }))
            .deadline(Duration::from_secs(1)),
        "system",
        "user",
        json_value,
    );
    let outcome = Agent::new(&transport, &environment)
        .run_traced(&task)
        .await
        .unwrap();
    assert_eq!(outcome.model, "hedged-backup");
    assert_eq!(transport.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn invalid_embeddings_keep_provider_usage() {
    let gateway = gateway();
    let transport = GatewayTransport::new(gateway.clone(), Scripted::new(vec![]));
    let environment = Environment::new();
    let agent = Agent::new(&transport, &environment);
    let ledger = CallLedger::new();
    let result = ledger
        .scope(agent.embed("embedding", "primary", &["text".into()], false))
        .await;
    assert!(matches!(
        result,
        Err(TaskError::Provider(ModelError::SchemaMismatch { .. }))
    ));
    assert_eq!(gateway.stats().usage.requests, 1);
    assert_eq!(gateway.stats().usage.cost_micros, 14);
    assert_eq!(ledger.snapshot().total_usage().cost_micros, 14);
    assert_eq!(gateway.stats().outcomes.get("schema_mismatch"), Some(&1));
}
