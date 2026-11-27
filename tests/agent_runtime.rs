#![cfg(feature = "runtime")]

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::gateway::{ModelPrice, ModelProfileOverride, ModelRegistry, PriceTable, TokenUsage};
use taskloom::runtime::*;
use taskloom::{
    CancelToken, ChatRequest, ChatResponse, Gateway, GatewayConfig, GatewayTransport, ModelError,
    Provider, Streaming,
};

struct Scripted {
    replies: Mutex<VecDeque<Result<ModelReply, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl Scripted {
    fn new(turns: Vec<AssistantTurn>) -> Arc<Self> {
        Self::replies(turns.into_iter().map(|turn| reply(Ok(turn))).collect())
    }
    fn replies(replies: Vec<Result<ModelReply, ModelError>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Provider for Scripted {
    fn id(&self) -> &str {
        "https://runtime.test/v1"
    }
    fn complete<'a>(
        &'a self,
        _: &'a ChatRequest,
        _: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        Box::pin(async { panic!("runtime must use native provider turns") })
    }
    fn converse<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelReply, ModelError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra model call")
        })
    }
}

fn reply(turn: Result<AssistantTurn, ModelError>) -> Result<ModelReply, ModelError> {
    Ok(ModelReply {
        turn,
        usage: Some(TokenUsage {
            prompt_tokens: Some(10),
            completion_tokens: Some(2),
            total_tokens: Some(12),
        }),
    })
}

fn gateway() -> Arc<Gateway> {
    Arc::new(Gateway::new(GatewayConfig {
        prices: PriceTable::new([ModelPrice::new("primary", 1.0, 1.0)]),
        ..Default::default()
    }))
}

fn call(id: &str, n: i32) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "double".into(),
        arguments: json!({"n":n}),
    }
}
fn turn(calls: Vec<ToolCall>) -> AssistantTurn {
    AssistantTurn {
        content: String::new(),
        tool_calls: calls,
    }
}
fn history() -> Vec<Message> {
    vec![
        Message::system("Use tools to answer."),
        Message::user("Double twice."),
    ]
}
fn definition(effect: ToolEffect) -> ToolDefinition {
    ToolDefinition {
        name: "double".into(),
        description: "Double a nonnegative integer".into(),
        effect,
        permissions: BTreeSet::from(["compute".into()]),
        input_schema: json!({"type":"object", "properties":{"n":{"type":"integer","minimum":0}}, "required":["n"], "additionalProperties":false}),
    }
}
fn policy() -> ToolPolicy {
    ToolPolicy::default().allow("double").grant("compute")
}
fn registry(counter: Arc<AtomicUsize>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools
        .register(FnTool::new(
            definition(ToolEffect::ReadOnly),
            move |arguments: Value, context: ToolContext| {
                let counter = Arc::clone(&counter);
                async move {
                    assert!(!context.run_id.is_empty());
                    assert!(!context.call_id.is_empty());
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(json!(arguments["n"].as_i64().unwrap() * 2))
                }
            },
        ))
        .unwrap();
    tools
}

#[tokio::test]
async fn successive_native_tool_turns_have_correlated_results_events_and_usage() {
    let provider = Scripted::new(vec![
        turn(vec![call("a", 3)]),
        turn(vec![call("b", 6)]),
        AssistantTurn::text("12"),
    ]);
    let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
    let counter = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&counter));
    let observed = Mutex::new(Vec::new());
    let sink = |event: &RunEvent| observed.lock().unwrap().push(event.clone());
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .with_events(&sink)
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(131_000),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Completed);
    assert_eq!(report.final_text(), Some("12"));
    assert_eq!((report.steps, report.tool_calls), (3, 2));
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert_eq!((report.usage.requests, report.usage.cost_micros), (3, 36));
    assert_eq!(report.budget.committed_micros, 36);
    assert_eq!(report.budget.in_flight_micros, 0);
    assert_eq!(observed.into_inner().unwrap(), report.events);
    for (index, event) in report.events.iter().enumerate() {
        assert_eq!(event.sequence, index);
    }
    assert_eq!(
        report.events.last().unwrap().kind,
        RunEventKind::Stopped {
            reason: StopReason::Completed
        }
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests[1].tools.len(), 1);
    assert!(
        matches!(&requests[2].messages[5], Message::Tool { result } if result.call_id == "b" && result.output == json!(12))
    );
    let saved = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<RunReport>(&saved)
            .unwrap()
            .final_text(),
        Some("12")
    );
}

#[tokio::test]
async fn preflights_entire_batch_before_any_effect() {
    for (calls, expected) in [
        (vec![call("a", 1), call("b", -2)], StopReason::ToolError),
        (
            vec![call("same", 1), call("same", 2)],
            StopReason::ModelError,
        ),
    ] {
        let provider = Scripted::new(vec![turn(calls)]);
        let transport = GatewayTransport::new(gateway(), provider);
        let counter = Arc::new(AtomicUsize::new(0));
        let tools = registry(Arc::clone(&counter));
        let report = AgentLoop::new(&transport, &tools, "primary")
            .with_policy(policy())
            .run("run", history())
            .await;
        assert_eq!(report.stop_reason, expected);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        assert_eq!(report.usage.requests, 1);
    }
}

#[tokio::test]
async fn explicit_permission_and_grant_are_both_required() {
    for policy in [ToolPolicy::default(), ToolPolicy::default().allow("double")] {
        let provider = Scripted::new(vec![turn(vec![call("a", 1)])]);
        let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
        let counter = Arc::new(AtomicUsize::new(0));
        let tools = registry(Arc::clone(&counter));
        let report = AgentLoop::new(&transport, &tools, "primary")
            .with_policy(policy)
            .run("run", history())
            .await;
        assert_eq!(report.stop_reason, StopReason::PermissionDenied);
        assert!(provider.requests.lock().unwrap()[0].tools.is_empty());
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn step_and_tool_limits_stop_before_dispatch() {
    let counter = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&counter));
    let provider = Scripted::new(vec![]);
    let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .with_limits(RunLimits {
            max_steps: 0,
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::StepLimit);
    assert_eq!(provider.calls(), 0);
    let provider = Scripted::new(vec![turn(vec![call("a", 1), call("b", 2)])]);
    let transport = GatewayTransport::new(gateway(), provider);
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .with_limits(RunLimits {
            max_tool_calls: 1,
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::ToolLimit);
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reused_id_across_turns_cannot_execute_twice() {
    let provider = Scripted::new(vec![turn(vec![call("a", 1)]), turn(vec![call("a", 1)])]);
    let transport = GatewayTransport::new(gateway(), provider);
    let counter = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&counter));
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_history_and_precancelled_runs_send_nothing() {
    let provider = Scripted::new(vec![]);
    let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
    let tools = ToolRegistry::new();
    let report = AgentLoop::new(&transport, &tools, "primary")
        .run(
            "run",
            vec![Message::Tool {
                result: ToolResult {
                    call_id: "unknown".into(),
                    output: json!(1),
                    is_error: false,
                },
            }],
        )
        .await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    let cancel = CancelToken::new();
    cancel.cancel();
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_cancel(&cancel)
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Cancelled);
    assert_eq!(provider.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn total_deadline_interrupts_tool_and_preserves_prior_usage() {
    let provider = Scripted::new(vec![turn(vec![call("a", 1)])]);
    let transport = GatewayTransport::new(gateway(), provider);
    let mut tools = ToolRegistry::new();
    tools
        .register(FnTool::new(
            definition(ToolEffect::ReadOnly),
            |_: Value, _: ToolContext| async {
                std::future::pending::<Result<Value, ToolError>>().await
            },
        ))
        .unwrap();
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .with_limits(RunLimits {
            max_duration: Duration::from_secs(2),
            tool_timeout: Duration::from_secs(30),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Deadline);
    assert_eq!(report.usage.cost_micros, 12);
    assert_eq!(report.tool_calls, 1);
    assert!(!report
        .events
        .iter()
        .any(|event| matches!(event.kind, RunEventKind::ToolFinished { .. })));
}

#[tokio::test(start_paused = true)]
async fn tool_timeout_can_be_handled_for_reads_but_stops_uncertain_writes() {
    for effect in [ToolEffect::ReadOnly, ToolEffect::NonIdempotent] {
        let provider = Scripted::new(vec![
            turn(vec![call("a", 1)]),
            AssistantTurn::text("tool unavailable"),
        ]);
        let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
        let mut tools = ToolRegistry::new();
        tools
            .register(FnTool::new(
                definition(effect),
                |_: Value, _: ToolContext| async {
                    std::future::pending::<Result<Value, ToolError>>().await
                },
            ))
            .unwrap();
        let report = AgentLoop::new(&transport, &tools, "primary")
            .with_policy(policy())
            .with_limits(RunLimits {
                tool_timeout: Duration::from_secs(1),
                ..Default::default()
            })
            .run("run", history())
            .await;
        let expected = if effect == ToolEffect::ReadOnly {
            StopReason::Completed
        } else {
            StopReason::ToolError
        };
        assert_eq!(report.stop_reason, expected);
        assert_eq!(
            provider.calls(),
            if effect == ToolEffect::ReadOnly { 2 } else { 1 }
        );
        assert!(matches!(&report.messages[3],Message::Tool {result} if result.is_error));
    }
}

#[tokio::test]
async fn spend_reservations_block_later_turns_without_sending() {
    let provider = Scripted::new(vec![turn(vec![call("a", 1)])]);
    let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
    let tools = registry(Arc::new(AtomicUsize::new(0)));
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(128_100),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Budget);
    assert_eq!(provider.calls(), 1);
    assert_eq!(report.budget.committed_micros, 12);
    assert_eq!(report.usage.requests, 1);
}

#[tokio::test]
async fn missing_prices_and_unpriced_fallbacks_fail_closed_with_budget() {
    let provider = Scripted::new(vec![]);
    let transport = GatewayTransport::new(Arc::new(Gateway::default()), Arc::clone(&provider));
    let tools = ToolRegistry::new();
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_limits(RunLimits {
            max_cost_micros: Some(u64::MAX),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Budget);
    assert_eq!(provider.calls(), 0);
    let configured = Arc::new(Gateway::new(GatewayConfig {
        models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
            id: "primary".into(),
            fallback_chain: Some(vec!["backup".into()]),
            ..Default::default()
        }]),
        prices: PriceTable::new([ModelPrice::new("primary", 1.0, 1.0)]),
        ..Default::default()
    }));
    let provider = Scripted::replies(vec![Err(ModelError::transient(Some(503), "unavailable"))]);
    let transport =
        GatewayTransport::new(configured, Arc::clone(&provider)).with_transient_attempts(1);
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(1_000_000),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Budget);
    assert_eq!(provider.calls(), 1);
    assert_eq!(report.budget.uncertain_micros, 128_100);
}

#[tokio::test(start_paused = true)]
async fn retries_keep_uncertain_attempts_charged() {
    let provider = Scripted::replies(vec![
        Err(ModelError::transient(Some(503), "unavailable")),
        reply(Ok(AssistantTurn::text("done"))),
    ]);
    let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
    let tools = ToolRegistry::new();
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(256_200),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Completed);
    assert_eq!(provider.calls(), 2);
    assert_eq!(report.budget.uncertain_micros, 128_100);
    assert_eq!(report.budget.committed_micros, 128_112);
    assert_eq!(report.budget.uncertain_attempts, 1);
}

#[tokio::test]
async fn reported_usage_on_rejected_turn_is_not_lost() {
    let provider = Scripted::replies(vec![reply(Err(ModelError::ContentRejected {
        detail: "refused".into(),
    }))]);
    let transport = GatewayTransport::new(gateway(), provider);
    let tools = ToolRegistry::new();
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(128_100),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::ModelError);
    assert_eq!(report.usage.requests, 1);
    assert_eq!(report.usage.cost_micros, 12);
    assert_eq!(report.budget.committed_micros, 12);
}

#[test]
fn reservations_are_shared_and_drops_are_conservative() {
    let budget = RunBudget::new(Some(100));
    let first = budget.reserve(70).unwrap();
    assert!(budget.clone().reserve(31).is_err());
    first.settle(20);
    assert!(
        budget.reserve(1).is_err(),
        "a denied run cannot dispatch again"
    );
    let budget = RunBudget::new(Some(100));
    budget.reserve(70).unwrap().settle(20);
    drop(budget.reserve(80).unwrap());
    let snapshot = budget.snapshot();
    assert_eq!(snapshot.committed_micros, 100);
    assert_eq!(snapshot.uncertain_micros, 80);
    assert_eq!(snapshot.in_flight_micros, 0);
}

#[test]
fn rejects_duplicate_tools_invalid_schemas_and_external_schema_references() {
    let mut tools = registry(Arc::new(AtomicUsize::new(0)));
    let make = |definition| {
        FnTool::new(definition, |_: Value, _: ToolContext| async {
            Ok(Value::Null)
        })
    };
    assert!(tools
        .register(make(definition(ToolEffect::ReadOnly)))
        .is_err());
    for schema in [
        json!({"type":"object","properties":{"n":{"type":"nonsense"}}}),
        json!({"type":"object","$ref":"https://unreachable.invalid/schema"}),
    ] {
        let mut def = definition(ToolEffect::ReadOnly);
        def.name = "invalid".into();
        def.input_schema = schema;
        assert!(tools.register(make(def)).is_err());
    }
}

#[tokio::test]
async fn cancellation_drops_an_inflight_paid_attempt_but_keeps_reservation() {
    struct OnDrop(Arc<AtomicUsize>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct PendingProvider {
        entered: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicUsize>,
    }
    impl Provider for PendingProvider {
        fn id(&self) -> &str {
            "https://pending.test"
        }
        fn complete<'a>(
            &'a self,
            _: &'a ChatRequest,
            _: Option<Streaming<'a>>,
        ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
            Box::pin(async { unreachable!() })
        }
        fn converse<'a>(
            &'a self,
            _: &'a ModelRequest,
        ) -> BoxFuture<'a, Result<ModelReply, ModelError>> {
            Box::pin(async move {
                let _guard = OnDrop(Arc::clone(&self.dropped));
                self.entered.notify_one();
                std::future::pending().await
            })
        }
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicUsize::new(0));
    let transport = GatewayTransport::new(
        gateway(),
        PendingProvider {
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
        },
    );
    let tools = ToolRegistry::new();
    let cancel = CancelToken::new();
    let token = cancel.clone();
    let canceller = tokio::spawn(async move {
        entered.notified().await;
        token.cancel();
    });
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_cancel(&cancel)
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(128_100),
            ..Default::default()
        })
        .run("run", history())
        .await;
    canceller.await.unwrap();
    assert_eq!(report.stop_reason, StopReason::Cancelled);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(report.usage.requests, 0);
    assert_eq!(report.budget.uncertain_micros, 128_100);
    assert_eq!(report.budget.in_flight_micros, 0);
}

#[tokio::test]
async fn oversized_tool_result_is_bounded_and_reported_as_error() {
    for effect in [ToolEffect::ReadOnly, ToolEffect::NonIdempotent] {
        let provider = Scripted::new(vec![
            turn(vec![call("a", 1)]),
            AssistantTurn::text("too much data"),
        ]);
        let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
        let mut tools = ToolRegistry::new();
        tools
            .register(FnTool::new(
                definition(effect),
                |_: Value, _: ToolContext| async { Ok(json!("x".repeat(1_000))) },
            ))
            .unwrap();
        let report = AgentLoop::new(&transport, &tools, "primary")
            .with_policy(policy())
            .with_limits(RunLimits {
                max_tool_result_bytes: 4,
                ..Default::default()
            })
            .run("run", history())
            .await;
        assert_eq!(
            report.stop_reason,
            if effect == ToolEffect::ReadOnly {
                StopReason::Completed
            } else {
                StopReason::ToolError
            }
        );
        assert!(
            matches!(&report.messages[3],Message::Tool {result} if result.is_error && result.output == Value::Null)
        );
        assert_eq!(
            provider.calls(),
            if effect == ToolEffect::ReadOnly { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn missing_usage_retains_ceiling_instead_of_freeing_budget() {
    let provider = Scripted::replies(vec![Ok(ModelReply {
        turn: Ok(turn(vec![call("a", 1)])),
        usage: None,
    })]);
    let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
    let tools = registry(Arc::new(AtomicUsize::new(0)));
    let report = AgentLoop::new(&transport, &tools, "primary")
        .with_policy(policy())
        .with_output_tokens(100)
        .with_limits(RunLimits {
            max_cost_micros: Some(128_100),
            ..Default::default()
        })
        .run("run", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::Budget);
    assert_eq!(provider.calls(), 1);
    assert_eq!(report.budget.committed_micros, 128_100);
    assert_eq!(report.budget.uncertain_attempts, 1);
    assert_eq!(report.usage.requests, 0);
}

#[tokio::test]
async fn cancellation_from_dispatch_events_prevents_the_notified_operation() {
    for before_model in [true, false] {
        let provider = Scripted::new(vec![turn(vec![call("a", 1)])]);
        let transport = GatewayTransport::new(gateway(), Arc::clone(&provider));
        let count = Arc::new(AtomicUsize::new(0));
        let tools = registry(Arc::clone(&count));
        let cancel = CancelToken::new();
        let sink = |event: &RunEvent| {
            if matches!(event.kind, RunEventKind::ModelStarted { .. }) && before_model
                || matches!(event.kind, RunEventKind::ToolStarted { .. }) && !before_model
            {
                cancel.cancel();
            }
        };
        let report = AgentLoop::new(&transport, &tools, "primary")
            .with_policy(policy())
            .with_cancel(&cancel)
            .with_events(&sink)
            .run("run", history())
            .await;
        assert_eq!(report.stop_reason, StopReason::Cancelled);
        assert_eq!(provider.calls(), if before_model { 0 } else { 1 });
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
