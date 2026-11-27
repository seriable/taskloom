#![cfg(feature = "runtime")]
//! Control/protocol fixtures. Real Collector/native-model acceptance is separate.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;
use taskloom::runtime::{observability::*, rag::*, workflow::*, *};

struct FixtureModel {
    reads: bool,
}
impl Model for FixtureModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let latest = request
                .messages
                .iter()
                .rposition(|m| matches!(m, Message::User { .. }))
                .unwrap();
            let read = request.messages[latest + 1..]
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }));
            let turn = if self.reads && !read {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: format!("PRIVATE_CALL_{}", request.messages.len()),
                        name: "private_lookup".into(),
                        arguments: json!({"query":"approval"}),
                    }],
                }
            } else {
                AssistantTurn::text("PRIVATE_ANSWER")
            };
            Ok(ModelOutput {
                model: request.model.clone(),
                turn,
            })
        })
    }
}
async fn registry(root: &std::path::Path) -> ToolRegistry {
    let corpus = Arc::new(FileCorpusStore::new(root.join("corpus")));
    ingest(
        corpus.as_ref(),
        Document {
            id: "PRIVATE_DOCUMENT".into(),
            title: "PRIVATE_TITLE".into(),
            source: "PRIVATE_SOURCE_URI".into(),
            text: "PRIVATE_QUOTE approval must bind the exact arguments.".into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        None,
    )
    .await
    .unwrap();
    let mut tools = ToolRegistry::new();
    tools
        .register(RagTool::new(
            "private_lookup",
            corpus,
            Arc::new(LexicalRetriever),
        ))
        .unwrap();
    tools
}
fn agent<'a>(model: &'a dyn Model, tools: &'a ToolRegistry) -> AgentLoop<'a> {
    AgentLoop::new(model, tools, "PRIVATE_MODEL").with_policy(
        ToolPolicy::default()
            .allow("private_lookup")
            .grant("knowledge:read"),
    )
}
async fn finished() -> RunState {
    let dir = tempfile::tempdir().unwrap();
    let tools = registry(dir.path()).await;
    let model = FixtureModel { reads: true };
    let agent = agent(&model, &tools);
    DurableAgent::new(&agent, &FileRunStore::new(dir.path().join("runs")))
        .start("PRIVATE_RUN", vec![Message::user("PRIVATE_PROMPT")])
        .await
        .unwrap()
}
#[tokio::test]
async fn run_projection_has_exact_parentage_private_allowlist_and_no_billing_conflation() {
    let state = finished().await;
    let snapshot = TraceSnapshot::run("test-instance", &state).unwrap();
    assert_eq!(snapshot.metrics.model_decisions_started, 2);
    assert_eq!(snapshot.metrics.tool_attempts, 1);
    assert_eq!(snapshot.metrics.retrievals, 1);
    assert!(!snapshot.metrics.generation_usage_reported);
    assert!(!snapshot
        .otlp_metrics("test")
        .unwrap()
        .to_string()
        .contains("taskloom.generation.input_tokens"));
    assert_eq!(
        snapshot.retrievals[0].hits[0].document_id,
        "PRIVATE_DOCUMENT"
    );
    assert_eq!(snapshot.metrics.embedding_attributed_cost_micros, None);
    assert_eq!(snapshot.metrics.tool_services_attributed_cost_micros, None);
    let run = snapshot
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Run)
        .unwrap();
    let tool = snapshot
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Tool)
        .unwrap();
    let retrieval = snapshot
        .spans
        .iter()
        .find(|s| s.kind == SpanKind::Retrieval)
        .unwrap();
    assert_eq!(tool.parent_span_id.as_deref(), Some(run.span_id.as_str()));
    assert_eq!(
        retrieval.parent_span_id.as_deref(),
        Some(tool.span_id.as_str())
    );
    let payload = snapshot.otlp_traces("taskloom-test").unwrap();
    assert_eq!(payload.exported_spans, 5);
    assert_eq!(payload.omitted_without_clock, 0);
    let wire = serde_json::to_string(&payload.body).unwrap();
    for secret in [
        "PRIVATE_RUN",
        "PRIVATE_MODEL",
        "PRIVATE_CALL",
        "PRIVATE_PROMPT",
        "PRIVATE_ANSWER",
        "PRIVATE_DOCUMENT",
        "PRIVATE_TITLE",
        "PRIVATE_SOURCE_URI",
        "PRIVATE_QUOTE",
        "private_lookup",
    ] {
        assert!(!wire.contains(secret), "{secret}");
    }
    let restored: RunState = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    let replay = TraceSnapshot::run("test-instance", &restored).unwrap();
    assert_eq!(snapshot.trace_id, replay.trace_id);
    assert_eq!(snapshot.source_revision_key, replay.source_revision_key);
    assert_eq!(
        serde_json::to_value(&snapshot.spans).unwrap(),
        serde_json::to_value(&replay.spans).unwrap()
    );
    assert_ne!(
        snapshot.trace_id,
        TraceSnapshot::run("another-instance", &restored)
            .unwrap()
            .trace_id
    );
    let mut modified = snapshot;
    modified.spans[0]
        .attributes
        .insert("prompt".into(), json!("PRIVATE_PROMPT"));
    assert!(modified.otlp_traces("taskloom-test").is_err());
}
#[tokio::test]
async fn old_missing_clocks_and_backwards_clocks_never_become_fabricated_wall_times() {
    let state = finished().await;
    let mut old = serde_json::to_value(state).unwrap();
    for event in old["report"]["events"].as_array_mut().unwrap() {
        event.as_object_mut().unwrap().remove("unix_ms");
    }
    let old: RunState = serde_json::from_value(old).unwrap();
    old.validate().unwrap();
    let snapshot = TraceSnapshot::run("test", &old).unwrap();
    let wire = snapshot.otlp_traces("test").unwrap();
    assert_eq!(wire.exported_spans, 0);
    assert_eq!(wire.omitted_without_clock, snapshot.spans.len());
    assert_eq!(wire.omitted_open, 0);
    let roundtrip = serde_json::to_value(&old).unwrap();
    assert!(roundtrip["report"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e.get("unix_ms").is_none()));
    let mut snapshot = TraceSnapshot::run("test", &finished().await).unwrap();
    snapshot.spans[0].end_unix_ms = Some(snapshot.spans[0].start_unix_ms.unwrap() - 1);
    assert_eq!(
        snapshot.otlp_traces("test").unwrap().omitted_invalid_clock,
        1
    );
    let gauge = snapshot.otlp_metrics("test").unwrap().to_string();
    assert!(!gauge.contains("embedding.cost"));
    assert!(!gauge.contains("tool_services.cost"));
}
#[tokio::test]
async fn workflow_and_session_join_only_their_frozen_children_and_count_usage_once() {
    let dir = tempfile::tempdir().unwrap();
    let tools = registry(dir.path()).await;
    let model = FixtureModel { reads: true };
    let agent = agent(&model, &tools);
    let runs = FileRunStore::new(dir.path().join("runs"));
    let workflows = FileWorkflowStore::new(dir.path().join("workflows"));
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(
            AgentWorkflowTask::new(
                "analyst",
                &agent,
                &runs,
                "PRIVATE_SYSTEM",
                WorkflowOutput::Text,
            )
            .unwrap(),
        )
        .unwrap();
    let runner = WorkflowRunner::new(&tasks, &workflows).with_limits(WorkflowLimits {
        max_parallel: 2,
        ..Default::default()
    });
    let state = runner
        .start(
            "PRIVATE_WORKFLOW",
            WorkflowDefinition::new(vec![
                WorkflowNode::new("left", "analyst"),
                WorkflowNode::new("right", "analyst"),
            ]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    let mut children = Vec::new();
    for n in &state.nodes {
        children.push(
            runs.inspect(workflow_run_id(&state, n).unwrap())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let snapshot = TraceSnapshot::workflow("test", &state, &children).unwrap();
    for child in &children {
        assert!(child.trace_binding.is_some());
        let selected = TraceSnapshot::run("test", child).unwrap();
        assert_eq!(selected.trace_id, snapshot.trace_id);
        assert_eq!(selected.root_span_id, snapshot.root_span_id);
        assert_ne!(selected.selected_span_id, snapshot.selected_span_id);
        assert_eq!(selected.otlp_traces("test").unwrap().omitted_context, 2);
        for span in selected.spans.iter().filter(|s| !s.context_only) {
            let full = snapshot
                .spans
                .iter()
                .find(|s| s.span_id == span.span_id)
                .unwrap();
            assert_eq!(
                serde_json::to_value(span).unwrap(),
                serde_json::to_value(full).unwrap()
            );
        }
    }
    assert_ne!(
        TraceSnapshot::run("test", &children[0])
            .unwrap()
            .source_revision_key,
        TraceSnapshot::run("test", &children[1])
            .unwrap()
            .source_revision_key
    );
    assert_eq!(snapshot.metrics.model_decisions_started, 4);
    assert_eq!(snapshot.metrics.retrievals, 2);
    assert_eq!(
        snapshot
            .spans
            .iter()
            .filter(|s| s.kind == SpanKind::Run)
            .count(),
        2
    );
    assert_eq!(snapshot.missing_children, 0);
    let missing = TraceSnapshot::workflow("test", &state, &[]).unwrap();
    assert!(!missing.metrics.saved_run_observations_complete);
    let metrics = missing.otlp_metrics("test").unwrap();
    assert!(metrics["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| !m["name"]
            .as_str()
            .unwrap()
            .starts_with("taskloom.generation.")));
    let key = snapshot.source_revision_key.clone();
    children.reverse();
    assert_eq!(
        key,
        TraceSnapshot::workflow("test", &state, &children)
            .unwrap()
            .source_revision_key
    );
    assert_eq!(
        TraceSnapshot::workflow("test", &state, &[])
            .unwrap()
            .missing_children,
        2
    );
    children.push(finished().await);
    assert!(TraceSnapshot::workflow("test", &state, &children).is_err());
    children.pop();
    children[0].report.messages[1] = Message::user("wrong workflow context");
    assert!(TraceSnapshot::workflow("test", &state, &children).is_err());
    let sessions = FileSessionStore::new(dir.path().join("sessions"));
    let manager = SessionManager::new(&sessions);
    manager.create("PRIVATE_SESSION", vec![]).await.unwrap();
    let durable = DurableAgent::new(&agent, &runs);
    let a = manager
        .run_turn(&durable, "PRIVATE_SESSION", "one", "PRIVATE_FIRST")
        .await
        .unwrap();
    let b = manager
        .run_turn(&durable, "PRIVATE_SESSION", "two", "PRIVATE_SECOND")
        .await
        .unwrap();
    let snapshot =
        TraceSnapshot::session("test", &b.session, &[a.run.clone(), b.run.clone()]).unwrap();
    for run in [&a.run, &b.run] {
        assert!(run.trace_binding.is_some());
        let selected = TraceSnapshot::run("test", run).unwrap();
        assert_eq!(selected.trace_id, snapshot.trace_id);
        assert_eq!(selected.root_span_id, snapshot.root_span_id);
        for reference in selected.spans.iter().filter(|s| s.context_only) {
            assert_eq!(reference.outcome, SpanOutcome::Unknown);
            assert_eq!(reference.start_unix_ms, None);
            assert_eq!(reference.end_unix_ms, None);
        }
        for span in selected.spans.iter().filter(|s| !s.context_only) {
            assert_eq!(
                serde_json::to_value(span).unwrap(),
                serde_json::to_value(
                    snapshot
                        .spans
                        .iter()
                        .find(|s| s.span_id == span.span_id)
                        .unwrap()
                )
                .unwrap()
            );
        }
    }
    assert_eq!(snapshot.metrics.model_decisions_started, 4);
    assert_eq!(
        snapshot
            .spans
            .iter()
            .filter(|s| s.kind == SpanKind::Turn)
            .count(),
        2
    );
    assert_eq!(snapshot.otlp_traces("test").unwrap().omitted_open, 1); // session remains open
    let mut changed = a.run;
    changed.report.events[0].unix_ms = Some(1);
    assert!(TraceSnapshot::session("test", &b.session, &[changed, b.run]).is_err());
}

#[tokio::test]
async fn trace_binding_is_host_owned_immutable_and_checked_before_any_creation_or_resume() {
    let dir = tempfile::tempdir().unwrap();
    let model = FixtureModel { reads: false };
    let tools = ToolRegistry::new();
    let agent = agent(&model, &tools);
    let runs = FileRunStore::new(dir.path().join("runs"));
    let sessions = FileSessionStore::new(dir.path().join("sessions"));
    let manager = SessionManager::new(&sessions);
    manager.create("frozen-parent", vec![]).await.unwrap();
    let pause = taskloom::CancelToken::new();
    pause.cancel();
    let durable = DurableAgent::new(&agent, &runs).with_pause(&pause);
    let paused = manager
        .run_turn(&durable, "frozen-parent", "one", "no model before pause")
        .await
        .unwrap();
    assert_eq!(paused.run.status, RunStatus::Paused);
    assert_eq!(paused.run.report.steps, 0);
    assert_eq!(
        paused.run.trace_binding,
        Some(TraceBinding::session("frozen-parent", "one"))
    );
    let wrong = DurableAgent::new(&agent, &runs)
        .with_trace_binding(TraceBinding::session("wrong-parent", "one"))
        .unwrap();
    assert!(matches!(
        wrong.resume(&paused.run.report.run_id).await,
        Err(RuntimeError::Configuration(_))
    ));
    let lease = runs.acquire(&paused.run.report.run_id).await.unwrap();
    let mut detached = paused.run.clone();
    detached.revision += 1;
    detached.trace_binding = None;
    assert!(lease.save(&detached).await.is_err());
    drop(lease);
    assert_eq!(
        runs.inspect(&paused.run.report.run_id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        paused.run.revision
    );
    let invalid = DurableAgent::new(&agent, &runs)
        .with_trace_binding(TraceBinding::session("parent", "turn"))
        .unwrap();
    assert!(invalid
        .create("invalid-child-identity", vec![Message::user("data")])
        .await
        .is_err());
    // An invalid binding left neither checkpoint nor orphaned reservation.
    let created = DurableAgent::new(&agent, &runs)
        .create("invalid-child-identity", vec![Message::user("data")])
        .await
        .unwrap();
    assert!(created.trace_binding.is_none());
    let fake = DurableAgent::new(&agent, &runs)
        .create(
            "user-message-only",
            vec![Message::user(
                r#"{"trace_binding":{"type":"session","session_id":"fake","turn_id":"fake"}}"#,
            )],
        )
        .await
        .unwrap();
    assert!(fake.trace_binding.is_none());
    let standalone = DurableAgent::new(&agent, &runs);
    let cancelled = standalone
        .cancel(&paused.run.report.run_id, "operator", "no dispatch")
        .await
        .unwrap();
    assert_eq!(cancelled.trace_binding, paused.run.trace_binding);
    assert_eq!(cancelled.report.steps, 0);
}

#[tokio::test]
async fn old_unbound_managed_runs_resume_without_backfill_or_changed_input_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let model = FixtureModel { reads: false };
    let tools = ToolRegistry::new();
    let agent = agent(&model, &tools);
    let runs = FileRunStore::new(dir.path().join("runs"));
    let sessions = FileSessionStore::new(dir.path().join("sessions"));
    let manager = SessionManager::new(&sessions);
    manager.create("legacy-session", vec![]).await.unwrap();
    let prepared = manager
        .begin_turn("legacy-session", "one", "old input")
        .await
        .unwrap();
    let durable = DurableAgent::new(&agent, &runs);
    let old = durable
        .create(&prepared.run_id, prepared.messages)
        .await
        .unwrap();
    assert!(serde_json::to_value(&old)
        .unwrap()
        .get("trace_binding")
        .is_none());
    let done = manager
        .resume_turn(&durable, "legacy-session", "one")
        .await
        .unwrap();
    assert_eq!(done.run.report.stop_reason, StopReason::Completed);
    assert!(done.run.trace_binding.is_none());
    assert!(serde_json::to_value(&done.run)
        .unwrap()
        .get("trace_binding")
        .is_none());
    TraceSnapshot::session("legacy", &done.session, std::slice::from_ref(&done.run)).unwrap();
    let lease = runs.acquire(&done.run.report.run_id).await.unwrap();
    let mut backfilled = done.run.clone();
    backfilled.revision += 1;
    backfilled.trace_binding = Some(TraceBinding::session("legacy-session", "one"));
    assert!(lease.save(&backfilled).await.is_err());
}

#[cfg(feature = "app")]
mod network {
    use super::*;
    use axum::{
        extract::{Json, State},
        http::{StatusCode, Uri},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    struct Receiver {
        mode: u8,
        calls: AtomicUsize,
        paths: Mutex<Vec<String>>,
        entered: tokio::sync::Notify,
    }
    async fn receive(
        State(state): State<Arc<Receiver>>,
        uri: Uri,
        Json(body): Json<Value>,
    ) -> Response {
        state.calls.fetch_add(1, Ordering::SeqCst);
        state.paths.lock().unwrap().push(uri.path().into());
        assert!(body.get("resourceSpans").is_some() || body.get("resourceMetrics").is_some());
        state.entered.notify_one();
        match state.mode {
            1 => Json(if uri.path().ends_with("traces") {
                json!({"partialSuccess":{"rejectedSpans":"1"}})
            } else {
                json!({"partialSuccess":{"rejectedDataPoints":"2"}})
            })
            .into_response(),
            2 => (StatusCode::TEMPORARY_REDIRECT, [("location", "/trap")]).into_response(),
            3 => {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                Json(json!({})).into_response()
            }
            _ => Json(json!({})).into_response(),
        }
    }
    async fn receiver(mode: u8) -> (String, Arc<Receiver>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Receiver {
            mode,
            calls: AtomicUsize::new(0),
            paths: Mutex::new(vec![]),
            entered: tokio::sync::Notify::new(),
        });
        let router = Router::new()
            .route("/v1/traces", post(receive))
            .route("/v1/metrics", post(receive))
            .route("/trap", post(receive))
            .with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, state, server)
    }
    #[tokio::test]
    async fn explicit_export_acceptance_partial_failure_redirects_and_shutdown_are_observed() {
        let snapshot = TraceSnapshot::run("test", &finished().await).unwrap();
        for mode in [0, 1, 2] {
            let (endpoint, state, server) = receiver(mode).await;
            let exporter = OtlpHttpExporter::new(OtlpHttpConfig {
                endpoint,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(state.calls.load(Ordering::SeqCst), 0); // constructor never sends
            let receipt = exporter.export(&snapshot).await.unwrap();
            match mode {
                0 => {
                    assert_eq!(receipt.traces, ExportAttempt::Accepted);
                    assert_eq!(receipt.metrics, ExportAttempt::Accepted);
                }
                1 => {
                    assert_eq!(receipt.traces, ExportAttempt::Partial { rejected_items: 1 });
                    assert_eq!(
                        receipt.metrics,
                        ExportAttempt::Partial { rejected_items: 2 }
                    );
                }
                _ => {
                    assert_eq!(receipt.traces, ExportAttempt::HttpError { code: 307 });
                    assert_eq!(receipt.metrics, ExportAttempt::HttpError { code: 307 });
                }
            }
            assert_eq!(*state.paths.lock().unwrap(), ["/v1/traces", "/v1/metrics"]);
            exporter.shutdown().await.unwrap();
            exporter.shutdown().await.unwrap();
            assert!(exporter.export(&snapshot).await.is_err());
            assert_eq!(state.calls.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }
    #[tokio::test]
    async fn concurrency_payload_and_shutdown_limits_do_not_queue_or_retry_network_work() {
        let snapshot = Arc::new(TraceSnapshot::run("test", &finished().await).unwrap());
        let (endpoint, state, server) = receiver(3).await;
        let exporter = Arc::new(
            OtlpHttpExporter::new(OtlpHttpConfig {
                endpoint: endpoint.clone(),
                max_in_flight: 1,
                timeout_ms: 100,
                ..Default::default()
            })
            .unwrap(),
        );
        let task = tokio::spawn({
            let e = exporter.clone();
            let s = snapshot.clone();
            async move { e.export(&s).await.unwrap() }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), state.entered.notified())
            .await
            .unwrap();
        assert!(matches!(
            exporter.export(&snapshot).await,
            Err(RuntimeError::Conflict(_))
        ));
        let _shutdown = exporter.shutdown().await;
        assert!(exporter.export(&snapshot).await.is_err());
        let receipt = task.await.unwrap();
        assert!(matches!(receipt.traces, ExportAttempt::Unknown { .. }));
        assert!(matches!(receipt.metrics, ExportAttempt::Unknown { .. }));
        assert_eq!(state.calls.load(Ordering::SeqCst), 2);
        let small = OtlpHttpExporter::new(OtlpHttpConfig {
            endpoint,
            max_payload_bytes: 1024,
            ..Default::default()
        })
        .unwrap();
        assert!(small.export(&snapshot).await.is_err());
        assert_eq!(state.calls.load(Ordering::SeqCst), 2);
        server.abort();
        for endpoint in [
            "http://example.com",
            "https://user:pass@example.com",
            "https://example.com/?secret=x",
            "https://example.com/#fragment",
        ] {
            assert!(OtlpHttpConfig {
                endpoint: endpoint.into(),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
    }

    #[tokio::test]
    async fn api_export_is_explicit_revision_bound_and_cli_inspection_needs_no_service_credentials()
    {
        use taskloom::runtime::app::*;
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, receiver_state, receiver_server) = receiver(0).await;
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            knowledge: false,
            observability: Some(AppObservability {
                namespace: Some("test-api".into()),
                otlp: Some(OtlpHttpConfig {
                    endpoint,
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let app = Application::with_model(
            config.clone(),
            Arc::new(FixtureModel { reads: false }),
            ToolRegistry::new(),
            ToolPolicy::default(),
        )
        .await
        .unwrap();
        app.start("private-api-run", "PRIVATE_API_PROMPT")
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while app.job("private-api-run").unwrap().active {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/diagnostics/run/private-api-run",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn({
            let app = app.clone();
            async move {
                serve(listener, app, "diagnostic-test-token-1234".into())
                    .await
                    .unwrap()
            }
        });
        let client = reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        let view: Value = client
            .get(&url)
            .bearer_auth("diagnostic-test-token-1234")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(receiver_state.calls.load(Ordering::SeqCst), 0);
        assert_eq!(view["export_configured"], true);
        let stale = client
            .post(format!("{url}/export"))
            .bearer_auth("diagnostic-test-token-1234")
            .json(&json!({"source_revision_key":"stale","export_binding":view["export_binding"]}))
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), 409);
        assert_eq!(receiver_state.calls.load(Ordering::SeqCst), 0);
        let changed_destination=client.post(format!("{url}/export")).bearer_auth("diagnostic-test-token-1234").json(&json!({"source_revision_key":view["snapshot"]["source_revision_key"],"export_binding":"stale-destination"})).send().await.unwrap();
        assert_eq!(changed_destination.status(), 409);
        assert_eq!(receiver_state.calls.load(Ordering::SeqCst), 0);
        let receipt: Value = client
            .post(format!("{url}/export"))
            .bearer_auth("diagnostic-test-token-1234")
            .json(&json!({"source_revision_key":view["snapshot"]["source_revision_key"],"export_binding":view["export_binding"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(receipt["traces"]["status"], "accepted");
        assert_eq!(receipt["metrics"]["status"], "accepted");
        assert_eq!(receiver_state.calls.load(Ordering::SeqCst), 2);
        app.shutdown(std::time::Duration::from_secs(1))
            .await
            .unwrap();
        server.abort();
        receiver_server.abort();
        let mut offline_inspection = config;
        offline_inspection.provider = AppProvider::Openai {
            base_url: "http://127.0.0.1:1/v1".into(),
            key_env: "TASKLOOM_OBSERVABILITY_UNSET_TEST_MODEL_KEY".into(),
            namespace: "test-no-service".into(),
        };
        offline_inspection
            .observability
            .as_mut()
            .unwrap()
            .otlp
            .as_mut()
            .unwrap()
            .bearer_env = Some("TASKLOOM_OBSERVABILITY_UNSET_TEST_OTLP_KEY".into());
        let path = dir.path().join("inspect.json");
        std::fs::write(&path, serde_json::to_vec(&offline_inspection).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_taskloom"))
            .args([
                "--config",
                path.to_str().unwrap(),
                "diagnostics",
                "run",
                "private-api-run",
            ])
            .env_remove("TASKLOOM_OBSERVABILITY_UNSET_TEST_MODEL_KEY")
            .env_remove("TASKLOOM_OBSERVABILITY_UNSET_TEST_OTLP_KEY")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let inspected: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            inspected["snapshot"]["trace_id"],
            view["snapshot"]["trace_id"]
        );
        assert_eq!(
            inspected["snapshot"]["source_revision_key"],
            view["snapshot"]["source_revision_key"]
        );
    }
}
