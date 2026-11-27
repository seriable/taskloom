//! Release regressions for vector spaces, store isolation, and time limits.
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use taskloom::gateway::{
    AttemptOutput, ModelProfileOverride, ModelRegistry, TaskEntry, TaskRegistry, TokenEstimator,
};
use taskloom::{
    json_value, Agent, Answer, ChatRequest, ChatResponse, EmbeddingRequest, EmbeddingResponse,
    Environment, FnTransport, Gateway, GatewayConfig, GatewayTransport, ModelError, PromptTask,
    Protocol, Provider, Recording, Request, RequestKey, Streaming, TaskError, TaskSpec,
};

struct Endpoint {
    id: &'static str,
    calls: AtomicUsize,
    fail: AtomicBool,
}
impl Endpoint {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
        }
    }
}
impl Provider for Endpoint {
    fn id(&self) -> &str {
        self.id
    }
    fn complete<'a>(
        &'a self,
        _: &'a ChatRequest,
        _: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(ChatResponse {
                content: serde_json::json!({"endpoint":self.id}).to_string(),
                ..Default::default()
            })
        })
    }
    fn embed<'a>(
        &'a self,
        request: &'a EmbeddingRequest,
    ) -> BoxFuture<'a, Result<EmbeddingResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if self.fail.load(Ordering::SeqCst) && request.model == "primary" {
                return Err(ModelError::transient(Some(503), "down"));
            }
            Ok(EmbeddingResponse {
                vectors: request
                    .inputs
                    .iter()
                    .map(|s| vec![s.len() as f32, 1.0])
                    .collect(),
                usage: None,
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
        ..Default::default()
    }))
}

#[tokio::test]
async fn partial_embedding_cache_never_mixes_fallback_models() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::new().with_step_cache_dir(dir.path().into());
    let provider = Arc::new(Endpoint::new("https://embedding.test"));
    let transport = GatewayTransport::new(gateway(), provider.clone()).with_transient_attempts(1);
    let agent = Agent::new(&transport, &env);
    let first = agent
        .embed("vectors", "primary", &["cached".into()], true)
        .await
        .unwrap();
    provider.fail.store(true, Ordering::SeqCst);
    let inputs = ["cached".into(), "new".into()];
    assert!(matches!(
        agent.embed("vectors", "primary", &inputs, true).await,
        Err(TaskError::Provider(ModelError::Transient { .. }))
    ));
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "must not call backup"
    );
    provider.fail.store(false, Ordering::SeqCst);
    let result = agent
        .embed("vectors", "primary", &inputs, true)
        .await
        .unwrap();
    assert_eq!(result.reused, 1);
    assert_eq!(result.model, "primary");
    assert_eq!(result.vectors[0], first.vectors[0]);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn shared_stores_isolate_endpoints_and_tenants() {
    let dir = tempfile::tempdir().unwrap();
    let recording = dir.path().join("record");
    let env = Environment::new()
        .with_step_cache_dir(dir.path().join("cache"))
        .with_recording(Recording::Record(recording.clone()));
    let a = Arc::new(Endpoint::new("https://a.test"));
    let b = Arc::new(Endpoint::new("https://b.test"));
    let first = Arc::new(GatewayTransport::new(gateway(), a.clone()));
    let second = Arc::new(GatewayTransport::new(gateway(), b.clone()));
    let tenant =
        Arc::new(GatewayTransport::new(gateway(), a.clone()).with_cache_namespace("tenant-b"));
    let task = PromptTask::new(
        "same",
        TaskSpec::new("primary", Protocol::Json, 0.0, 100).step_cache(true),
        "system",
        "user",
        json_value,
    );
    for transport in [&first, &second, &tenant] {
        let agent = Agent::new(&transport, &env);
        assert!(!agent.run_traced(&task).await.unwrap().reused);
        assert!(agent.run_traced(&task).await.unwrap().reused);
    }
    let replay = Environment::new().with_recording(Recording::Replay(recording));
    for (transport, endpoint) in [
        (&first, "https://a.test"),
        (&second, "https://b.test"),
        (&tenant, "https://a.test"),
    ] {
        let result = Agent::new(&transport, &replay)
            .run_traced(&task)
            .await
            .unwrap();
        assert!(result.reused);
        assert_eq!(result.output["endpoint"], endpoint);
    }
    assert_eq!(a.calls.load(Ordering::SeqCst), 2);
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_recorded_embeddings_never_reach_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let key = RequestKey::for_payload("vectors", "model", "embedding", "text");
    Environment::write_answer(dir.path(), &key, &Answer::Json(serde_json::json!([null]))).await;
    let env = Environment::new().with_recording(Recording::Replay(dir.path().into()));
    let transport = FnTransport(|_: Request<'_>| panic!("replay must not send"));
    assert!(matches!(
        Agent::new(&transport, &env)
            .embed("vectors", "model", &["text".into()], false)
            .await,
        Err(TaskError::Rejected(_))
    ));
}

#[tokio::test]
async fn recording_failure_does_not_spend_another_model_call() {
    let blocker = tempfile::NamedTempFile::new().unwrap();
    let env = Environment::new().with_recording(Recording::Record(blocker.path().into()));
    let calls = AtomicUsize::new(0);
    let transport = FnTransport(|_: Request<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Answer::Json(serde_json::json!({"ok":true})))
    });
    let task = PromptTask::new(
        "record",
        TaskSpec::new("a", Protocol::Json, 0.0, 100).then("b"),
        "system",
        "user",
        json_value,
    );
    assert!(matches!(
        Agent::new(&transport, &env).run(&task).await,
        Err(TaskError::Storage { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn custom_transport_deadlines_are_terminal() {
    let calls = AtomicUsize::new(0);
    let transport = FnTransport(|_: Request<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(TaskError::Provider(ModelError::Deadline {
            limit: Duration::from_secs(1),
            detail: "expired".into(),
        }))
    });
    let env = Environment::new();
    let task = PromptTask::new(
        "deadline",
        TaskSpec::new("a", Protocol::ResilientJson, 0.0, 100).then("b"),
        "system",
        "user",
        json_value,
    );
    assert!(matches!(
        Agent::new(&transport, &env).run(&task).await,
        Err(TaskError::Provider(ModelError::Deadline { .. }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dot_labels_stay_inside_the_store_directory() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("store");
    let key = RequestKey::for_payload("..", "model", "embedding", "text");
    Environment::write_answer(&store, &key, &Answer::Json(serde_json::json!([1.0]))).await;
    assert_eq!(
        std::fs::read_dir(store.join("_"))
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "json"))
            .count(),
        1
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

struct Slow(AtomicUsize);
impl Provider for Slow {
    fn id(&self) -> &str {
        "https://slow.test"
    }
    fn complete<'a>(
        &'a self,
        _: &'a ChatRequest,
        _: Option<Streaming<'a>>,
    ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}
#[tokio::test(start_paused = true)]
async fn registry_timeout_and_task_deadline_use_the_shorter_limit() {
    for explicit in [
        None,
        Some(Duration::from_millis(30)),
        Some(Duration::from_secs(2)),
    ] {
        let mut entry = TaskEntry::bounded_text("slow", 100, 0.0);
        entry.timeout_seconds = 1;
        let gateway = Arc::new(Gateway::new(GatewayConfig {
            tasks: TaskRegistry::new([entry]),
            ..Default::default()
        }));
        let provider = Arc::new(Slow(AtomicUsize::new(0)));
        let transport = Arc::new(GatewayTransport::new(gateway, provider.clone()));
        let reference = &transport;
        let env = Environment::new();
        let mut spec = TaskSpec::new("a", Protocol::ResilientJson, 0.0, 100)
            .then("b")
            .repair(2);
        spec.deadline = explicit;
        let task = PromptTask::new("slow", spec, "system", "user", json_value);
        let expected = explicit
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(1));
        let started = tokio::time::Instant::now();
        assert!(
            matches!(Agent::new(&reference, &env).run(&task).await, Err(TaskError::Deadline(limit)) if limit == expected)
        );
        assert_eq!(started.elapsed(), expected);
        assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    }
}

/// Holds the only permit of a provider with a request that never answers.
async fn occupy_single_permit(
    gateway: &Arc<Gateway>,
) -> (taskloom::gateway::CallContext, tokio::task::JoinHandle<()>) {
    gateway.install_provider_control(
        "https://queued.test",
        1,
        taskloom::gateway::CircuitBreaker::new(5, Duration::from_secs(30)),
    );
    let ctx = gateway.call_context(
        "queue",
        "https://queued.test",
        "model",
        "system",
        "user",
        0,
        100,
        TokenEstimator::default(),
    );
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let first_gateway = gateway.clone();
    let first_context = ctx.clone();
    let first = tokio::spawn(async move {
        let mut ready = Some(ready);
        let _ = first_gateway
            .execute(&first_context, |_| {
                if let Some(ready) = ready.take() {
                    ready.send(()).unwrap();
                }
                std::future::pending::<Result<AttemptOutput<()>, ModelError>>()
            })
            .await;
    });
    waiting.await.unwrap();
    (ctx, first)
}

#[tokio::test(start_paused = true)]
async fn a_queued_request_waits_for_its_permit_and_the_attempt_timeout_starts_after() {
    let gateway = gateway();
    let (ctx, first) = occupy_single_permit(&gateway).await;
    let mut queued = ctx;
    // Far shorter than the queueing: the attempt timeout must not cover it.
    queued.attempt_timeout = Some(Duration::from_millis(20));
    queued.transient_attempts = 1;
    let sent = AtomicUsize::new(0);
    let call = gateway.execute(&queued, |_| {
        sent.fetch_add(1, Ordering::SeqCst);
        async { Ok(AttemptOutput::new((), None)) }
    });
    tokio::pin!(call);
    assert!(
        tokio::time::timeout(Duration::from_secs(60), &mut call)
            .await
            .is_err(),
        "a queued request keeps waiting for its permit"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    first.abort();
    let _ = first.await;
    assert!(call.await.is_ok());
    assert_eq!(sent.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_whole_call_timeout_covers_the_permit_wait() {
    let gateway = gateway();
    let (ctx, first) = occupy_single_permit(&gateway).await;
    let mut queued = ctx;
    queued.task.timeout = Some(Duration::from_millis(20));
    queued.transient_attempts = 1;
    let sent = AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let result = gateway
        .execute(&queued, |_| {
            sent.fetch_add(1, Ordering::SeqCst);
            async { Ok(AttemptOutput::new((), None)) }
        })
        .await;
    assert!(
        matches!(result, Err(ModelError::Deadline { limit, .. }) if limit == Duration::from_millis(20))
    );
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    assert_eq!(started.elapsed(), Duration::from_millis(20));
    first.abort();
    let _ = first.await;
}

#[tokio::test(start_paused = true)]
async fn whole_call_timeout_covers_retry_backoff() {
    let gateway = gateway();
    let mut ctx = gateway.call_context(
        "backoff",
        "https://backoff.test",
        "primary",
        "system",
        "user",
        0,
        100,
        TokenEstimator::default(),
    );
    ctx.task.timeout = Some(Duration::from_millis(50));
    ctx.transient_attempts = 100;
    let calls = AtomicUsize::new(0);
    let started = tokio::time::Instant::now();
    let result = gateway
        .execute(&ctx, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<AttemptOutput<()>, _>(ModelError::transient(Some(429), "rate limited")) }
        })
        .await;
    assert!(
        matches!(result, Err(ModelError::Deadline { limit, .. }) if limit == Duration::from_millis(50))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(started.elapsed(), Duration::from_millis(50));
}

// Invoked in separate processes by the test below. A normal test run is a no-op.
#[tokio::test]
async fn store_writer_child() {
    let Ok(directory) = std::env::var("TASKLOOM_ATOMIC_TEST_DIR") else {
        return;
    };
    let writer = std::env::var("TASKLOOM_ATOMIC_TEST_WRITER").unwrap();
    let env = Environment::new().with_recording(Recording::Record(directory.into()));
    let iteration = AtomicUsize::new(0);
    let transport = FnTransport(|_: Request<'_>| {
        Ok(Answer::Json(serde_json::json!({
            "writer": writer, "iteration": iteration.fetch_add(1, Ordering::SeqCst), "padding": "x".repeat(32_768)
        })))
    });
    let task = PromptTask::new(
        "shared",
        TaskSpec::new("model", Protocol::Json, 0.0, 100),
        "system",
        "user",
        json_value,
    );
    for _ in 0..24 {
        Agent::new(&transport, &env).run(&task).await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_process_writers_only_expose_complete_records() {
    let directory = tempfile::tempdir().unwrap();
    let mut children = Vec::new();
    for writer in ["a", "b"] {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "store_writer_child", "--nocapture"])
            .env("TASKLOOM_ATOMIC_TEST_DIR", directory.path())
            .env("TASKLOOM_ATOMIC_TEST_WRITER", writer)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        children.push(tokio::task::spawn_blocking(move || {
            child.wait_with_output().unwrap()
        }));
    }
    let folder = directory.path().join("shared");
    let mut reads = 0;
    loop {
        if let Ok(files) = std::fs::read_dir(&folder) {
            for file in files.flatten().filter(|file| {
                file.path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            }) {
                let bytes = std::fs::read(file.path()).unwrap();
                let record: serde_json::Value = serde_json::from_slice(&bytes)
                    .expect("an atomic reader must never see partial JSON");
                assert_eq!(record["json"]["padding"].as_str().unwrap().len(), 32_768);
                assert_eq!(record["served_model"], "model");
                reads += 1;
            }
        }
        if children.iter().all(|child| child.is_finished()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    for child in children {
        let output = child.await.unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(reads > 0);
    assert_eq!(
        std::fs::read_dir(folder)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != ".taskloom-store.lock")
            .count(),
        1,
        "temporary files must be cleaned up"
    );
}
