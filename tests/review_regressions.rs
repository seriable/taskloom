#![cfg(all(feature = "runtime", feature = "research"))]
use futures_util::future::BoxFuture;
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use taskloom::gateway::{ModelProfileOverride, ModelRegistry};
use taskloom::runtime::rag::{WebAccess, WebClient};
use taskloom::runtime::*;
use taskloom::{
    json_value, Agent, ChatRequest, ChatResponse, Environment, Gateway, GatewayConfig,
    GatewayTransport, ModelError, PromptTask, Protocol, Provider, Streaming, TaskSpec,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct SchemaEcho(AtomicUsize);
impl Provider for SchemaEcho {
    fn id(&self) -> &str {
        "https://schema-review.invalid"
    }
    fn complete<'a>(
        &'a self,
        request: &'a ChatRequest,
        _: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let content = serde_json::json!({"schema_sent": request.schema.is_some()}).to_string();
        Box::pin(async move {
            Ok(ChatResponse {
                content,
                ..Default::default()
            })
        })
    }
}

#[tokio::test]
async fn schema_settings_invalidate_both_cached_and_recorded_answers() {
    let dir = tempfile::tempdir().unwrap();
    let recordings = dir.path().join("recordings");
    let env = Environment::new()
        .with_step_cache_dir(dir.path().join("cache"))
        .with_recording(taskloom::Recording::Record(recordings.clone()));
    let provider = Arc::new(SchemaEcho(AtomicUsize::new(0)));
    let transport = |enabled| {
        GatewayTransport::new(
            Arc::new(Gateway::new(GatewayConfig {
                models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
                    id: "test".into(),
                    supports_json_schema: Some(enabled),
                    ..Default::default()
                }]),
                ..Default::default()
            })),
            provider.clone(),
        )
    };
    let first = transport(false);
    let second = transport(true);
    let task = PromptTask::new(
        "review",
        TaskSpec::new("test", Protocol::Json, 0.0, 100)
            .schema(serde_json::json!({"type":"object"}))
            .step_cache(true),
        "system",
        "question",
        json_value,
    );
    let old = Agent::new(&first, &env).run_traced(&task).await.unwrap();
    let cached = Agent::new(&second, &env).run_traced(&task).await.unwrap();
    let fresh = Agent::new(&second, &Environment::new())
        .run_traced(&task)
        .await
        .unwrap();
    assert_eq!(old.output["schema_sent"], false);
    assert!(!cached.reused);
    assert_eq!(cached.output["schema_sent"], true);
    assert_eq!(fresh.output["schema_sent"], true);
    let replay = Environment::new().with_recording(taskloom::Recording::Replay(recordings));
    for (transport, expected) in [(&first, false), (&second, true)] {
        let answer = Agent::new(transport, &replay)
            .run_traced(&task)
            .await
            .unwrap();
        assert!(answer.reused);
        assert_eq!(answer.output["schema_sent"], expected);
    }
    assert_eq!(provider.0.load(Ordering::SeqCst), 3);
}

struct SizedModel {
    length: AtomicUsize,
    calls: AtomicUsize,
}
impl Model for SizedModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(2).await?.settle(2);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                turn: AssistantTurn::text("x".repeat(self.length.load(Ordering::SeqCst))),
                model: request.model.clone(),
            })
        })
    }
}

#[tokio::test]
async fn reply_limit_violation_is_terminal_and_never_replays_paid_work() {
    let dir = tempfile::tempdir().unwrap();
    let runs = FileRunStore::new(dir.path()).with_max_bytes(8192);
    let model = SizedModel {
        length: AtomicUsize::new(10000),
        calls: AtomicUsize::new(0),
    };
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test").with_limits(RunLimits {
        max_model_result_bytes: 512,
        ..Default::default()
    });
    let durable = DurableAgent::new(&agent, &runs);
    let stopped = durable
        .start("oversized", vec![Message::user("question")])
        .await
        .unwrap();
    assert_eq!(stopped.status, RunStatus::Finished);
    assert_eq!(stopped.report.stop_reason, StopReason::StorageLimit);
    assert_eq!(stopped.report.budget.committed_micros, 2);
    assert_eq!(stopped.report.budget.uncertain_micros, 0);
    assert!(stopped
        .report
        .detail
        .as_deref()
        .unwrap()
        .contains("reply not retained"));
    let again = durable.resume("oversized").await.unwrap();
    assert_eq!(again.revision, stopped.revision);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bounded_small_stores_still_commit_answers_that_fit() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = FileSessionStore::new(dir.path().join("sessions")).with_max_bytes(8192);
    let runs = FileRunStore::new(dir.path().join("runs")).with_max_bytes(8192);
    let manager = SessionManager::new(&sessions);
    manager.create("small", vec![]).await.unwrap();
    let model = SizedModel {
        length: AtomicUsize::new(400),
        calls: AtomicUsize::new(0),
    };
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test").with_limits(RunLimits {
        max_model_result_bytes: 512,
        ..Default::default()
    });
    let durable = DurableAgent::new(&agent, &runs);
    let completed = manager
        .run_turn(&durable, "small", "one", "q")
        .await
        .unwrap();
    assert_eq!(completed.run.report.stop_reason, StopReason::Completed);
    assert!(completed.session.active.is_none());
    assert_eq!(completed.session.turns.len(), 1);
    manager.resume_turn(&durable, "small", "one").await.unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oversized_input_is_rejected_without_stranding_an_active_session() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = FileSessionStore::new(dir.path()).with_max_bytes(8192);
    let manager = SessionManager::new(&sessions);
    manager.create("chat", vec![]).await.unwrap();
    assert!(matches!(
        manager.begin_turn("chat", "large", &"x".repeat(5000)).await,
        Err(RuntimeError::StorageLimit(_))
    ));
    let saved = manager.load("chat").await.unwrap();
    assert!(saved.active.is_none());
    assert_eq!(saved.revision, 1);
    manager.begin_turn("chat", "small", "q").await.unwrap();
}

#[test]
fn old_limit_documents_receive_the_default_model_reply_ceiling() {
    let mut value = serde_json::to_value(RunLimits::default()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("max_model_result_bytes");
    let loaded: RunLimits = serde_json::from_value(value).unwrap();
    assert_eq!(loaded.max_model_result_bytes, 64 * 1024);
}

#[test]
fn operational_model_settings_keep_the_answer_namespace_stable() {
    use taskloom::Transport;
    let make = |concurrency| {
        GatewayTransport::new(
            Arc::new(Gateway::new(GatewayConfig {
                models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
                    id: "test".into(),
                    concurrency: Some(concurrency),
                    ..Default::default()
                }]),
                ..Default::default()
            })),
            SchemaEcho(AtomicUsize::new(0)),
        )
    };
    let first = make(1);
    let second = make(8)
        .with_transient_attempts(4)
        .with_attempt_timeout(Some(Duration::from_secs(30)));
    assert_eq!(first.cache_namespace(), second.cache_namespace());
    let alternate_route = GatewayTransport::new(
        Arc::new(Gateway::new(GatewayConfig {
            models: ModelRegistry::new()
                .with_default_fallback_chain(vec!["other".into()])
                .with_overrides(vec![ModelProfileOverride {
                    id: "test".into(),
                    concurrency: Some(1),
                    ..Default::default()
                }]),
            ..Default::default()
        })),
        SchemaEcho(AtomicUsize::new(0)),
    );
    assert_ne!(first.cache_namespace(), alternate_route.cache_namespace());
}

struct ToolModel(AtomicUsize);
impl Model for ToolModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "read".into(),
                        name: "lookup".into(),
                        arguments: serde_json::json!({}),
                    }],
                },
            })
        })
    }
}

#[tokio::test]
async fn tool_capacity_is_reserved_before_dispatching_the_callback() {
    let dir = tempfile::tempdir().unwrap();
    let runs = FileRunStore::new(dir.path()).with_max_bytes(8192);
    let model = ToolModel(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut tools = ToolRegistry::new();
    tools
        .register(FnTool::new(
            ToolDefinition {
                name: "lookup".into(),
                description: "lookup".into(),
                input_schema: serde_json::json!({"type":"object"}),
                effect: ToolEffect::ReadOnly,
                permissions: BTreeSet::new(),
            },
            move |_: serde_json::Value, _: ToolContext| {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(serde_json::json!("x".repeat(5600)))
                }
            },
        ))
        .unwrap();
    let agent = AgentLoop::new(&model, &tools, "test")
        .with_policy(ToolPolicy::default().allow("lookup"))
        .with_limits(RunLimits {
            max_tool_calls: 1,
            max_model_result_bytes: 512,
            max_tool_result_bytes: 6000,
            ..Default::default()
        });
    let durable = DurableAgent::new(&agent, &runs);
    let stopped = durable
        .start("tool-cap", vec![Message::user("lookup")])
        .await
        .unwrap();
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(stopped.report.stop_reason, StopReason::StorageLimit);
    assert_eq!(stopped.report.tool_calls, 0);
    assert_eq!(
        durable.resume("tool-cap").await.unwrap().revision,
        stopped.revision
    );
}

#[tokio::test]
async fn a_small_store_refuses_uncommittable_model_work_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let runs = FileRunStore::new(dir.path()).with_max_bytes(8192);
    let model = SizedModel {
        length: AtomicUsize::new(10000),
        calls: AtomicUsize::new(0),
    };
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test");
    let stopped = DurableAgent::new(&agent, &runs)
        .start("cap", vec![Message::user("question")])
        .await
        .unwrap();
    let saved = runs.inspect("cap").await.unwrap().unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(stopped.report.stop_reason, StopReason::StorageLimit);
    assert_eq!(saved.status, RunStatus::Finished);
    assert!(!matches!(saved.cursor, RunCursor::ModelInFlight { .. }));
}

#[tokio::test]
async fn session_reserves_commit_metadata_and_refuses_a_large_turn_without_a_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = FileSessionStore::new(dir.path().join("sessions")).with_max_bytes(8192);
    let runs = FileRunStore::new(dir.path().join("runs")).with_max_bytes(8192);
    let manager = SessionManager::new(&sessions);
    manager.create("chat", vec![]).await.unwrap();
    let model = SizedModel {
        length: AtomicUsize::new(2),
        calls: AtomicUsize::new(0),
    };
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test").with_limits(RunLimits {
        max_model_result_bytes: 512,
        ..Default::default()
    });
    let durable = DurableAgent::new(&agent, &runs);
    for i in 0..8 {
        manager
            .run_turn(&durable, "chat", &format!("t{i}"), "q")
            .await
            .unwrap();
    }
    let size = serde_json::to_vec(&manager.load("chat").await.unwrap())
        .unwrap()
        .len();
    assert!(size <= 4096, "session size={size}");
    model.length.store(5600, Ordering::SeqCst);
    let large_agent = AgentLoop::new(&model, &tools, "test").with_limits(RunLimits {
        max_model_result_bytes: 6000,
        ..Default::default()
    });
    let large_durable = DurableAgent::new(&large_agent, &runs);
    let binding = manager.begin_turn("chat", "big", "q").await.unwrap();
    let result = manager
        .resume_turn(&large_durable, "chat", "big")
        .await
        .unwrap();
    let saved = runs.inspect(&binding.run_id).await.unwrap().unwrap();
    assert_eq!(saved.report.stop_reason, StopReason::StorageLimit);
    assert_eq!(result.run.status, RunStatus::Finished);
    assert_eq!(model.calls.load(Ordering::SeqCst), 8);
    let closed = manager
        .abandon_turn("chat", "big", &saved, "operator", "capacity exhausted")
        .await
        .unwrap();
    assert!(closed.active.is_none());
    assert_eq!(closed.turns.len(), 9);
    let replay = manager
        .resume_turn(&large_durable, "chat", "big")
        .await
        .unwrap();
    assert_eq!(replay.run.report.stop_reason, StopReason::StorageLimit);
    assert_eq!(model.calls.load(Ordering::SeqCst), 8);
}

async fn serve_html(html: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut block = [0; 4096];
        while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut block).await.unwrap();
            if n == 0 {
                return;
            }
            bytes.extend_from_slice(&block[..n]);
        }
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", html.len());
        stream.write_all(head.as_bytes()).await.unwrap();
        let _ = stream.write_all(html.as_bytes()).await;
    });
    (base, handle)
}

#[test]
fn quoted_attributes_cannot_hide_nesting_or_leave_parser_work_after_return() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let started = Instant::now();
    let fetch_ms = runtime.block_on(async {
        let html = format!(
            "<body>{}ok{}</body>",
            "<div title=\"> </div>\">".repeat(12000),
            "</div>".repeat(12000)
        );
        let (base, server) = serve_html(html).await;
        let web = WebClient::new(WebAccess {
            allowed_origins: BTreeSet::from([base.clone()]),
            allow_private: true,
            max_body_bytes: 1024 * 1024,
            max_text_bytes: 1024,
            max_redirects: 0,
            timeout_ms: 2000,
        })
        .unwrap();
        let started = Instant::now();
        let error = web.fetch(&base, "source", "source").await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            error
                .to_string()
                .contains("HTML nesting exceeds depth limit"),
            "{error}"
        );
        server.await.unwrap();

        elapsed
    });
    drop(runtime);
    assert!(fetch_ms < Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_secs(3));
}
