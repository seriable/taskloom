#![cfg(feature = "app")]
//! Real local HTTP/filesystem integration; extraction responses are fixtures.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::{app::*, memory::*, *};

const TOKEN: &str = "memory-test-operator-token";
struct ModelFixture {
    calls: AtomicUsize,
    hang: AtomicBool,
    entered: tokio::sync::Notify,
}
impl ModelFixture {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            hang: AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
        }
    }
}
impl Model for ModelFixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(10).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            let extract = matches!(&request.messages[0],Message::System {content} if content.starts_with("Extract durable user facts"));
            if extract {
                assert!(request.tools.is_empty());
                self.entered.notify_one();
                if self.hang.load(Ordering::SeqCst) {
                    std::future::pending::<()>().await;
                }
            }
            reservation.settle(10);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text(if extract {
                    json!({"memories":[{"kind":"preference","key":"language","value":"English","quote":"I prefer English replies."}]}).to_string()
                } else {
                    "acknowledged".into()
                }),
            })
        })
    }
}
fn config(root: &std::path::Path) -> AppConfig {
    AppConfig {
        state_dir: root.into(),
        knowledge: false,
        max_active_runs: 1,
        limits: RunLimits {
            max_cost_micros: Some(20),
            ..Default::default()
        },
        memory: Some(AppMemory {
            scope: MemoryScope {
                tenant: "acme".into(),
                project: "docs".into(),
                owner: "alice".into(),
            },
            actor: "host-authenticated-operator".into(),
            curate: true,
            recall: Default::default(),
        }),
        ..Default::default()
    }
}
async fn app(config: AppConfig, model: Arc<ModelFixture>) -> Arc<Application> {
    Application::with_model(config, model, ToolRegistry::new(), ToolPolicy::default())
        .await
        .unwrap()
}
async fn idle(app: &Application, id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = app.session_job(id).unwrap();
            if !status.active {
                assert!(status.error.is_none(), "{:?}", status.error);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn origin(app: &Arc<Application>) {
    app.create_session("source").await.unwrap();
    app.start_session_turn("source", "one", "I prefer English replies.")
        .await
        .unwrap();
    idle(app, "source").await;
}
async fn server(app: Arc<Application>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api", listener.local_addr().unwrap());
    (
        url,
        tokio::spawn(async move { serve(listener, app, TOKEN.into()).await.unwrap() }),
    )
}
async fn post(url: &str, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{url}{path}"))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
}
fn request() -> MemoryExtract {
    MemoryExtract {
        expected_revision: 0,
        session_id: "source".into(),
        turn_id: "one".into(),
        request_id: "learn".into(),
    }
}

#[tokio::test]
async fn http_memory_lifecycle_restarts_and_cli_management_needs_no_model_or_tool_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let model = Arc::new(ModelFixture::new());
    let host = app(cfg.clone(), model.clone()).await;
    origin(&host).await;
    let (url, worker) = server(host.clone()).await;
    assert_eq!(
        reqwest::Client::new()
            .get(format!("{url}/memory"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(post(&url,"/memory/extract",json!({"expected_revision":0,"session_id":"source","turn_id":"one","request_id":"learn","scope":{"owner":"bob"}})).await.status(),422);
    let response = post(
        &url,
        "/memory/extract",
        serde_json::to_value(request()).unwrap(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let extracted: Value = response.json().await.unwrap();
    let id = extracted["memory"]["records"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let recall: Value = post(&url, "/memory/recall", json!({"text":"language"}))
        .await
        .json()
        .await
        .unwrap();
    assert!(recall["hits"].as_array().unwrap().is_empty());
    assert_eq!(post(&url,"/memory/review",json!({"expected_revision":1,"id":id,"accept":true,"note":"checked user source","actor":"forged"})).await.status(),422);
    let active: Value = post(
        &url,
        "/memory/review",
        json!({"expected_revision":1,"id":id,"accept":true,"note":"checked user source"}),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(
        active["records"][&id]["decision"]["actor"],
        "host-authenticated-operator"
    );
    host.create_session("new").await.unwrap();
    assert_eq!(
        post(
            &url,
            "/sessions/new/memory-turns",
            json!({"turn_id":"one","prompt":"Write a greeting"})
        )
        .await
        .status(),
        200
    );
    idle(&host, "new").await;
    let session = host.session("new").await.unwrap();
    let Message::User { content } = &session.messages[1] else {
        panic!()
    };
    let frozen: Value = serde_json::from_str(content).unwrap();
    assert_eq!(frozen["memory"]["hits"][0]["record"]["id"], id);
    let child = host
        .inspect(&session.turns[0].binding.run_id)
        .await
        .unwrap();
    assert!(child.config.tools.is_empty());
    assert_eq!(
        child.trace_binding,
        Some(observability::TraceBinding::session("new", "one"))
    );
    // Managed IDs exceed the user-created 64-byte limit. API and cold CLI
    // diagnostics must admit the same bounded read identities as /runs/{id}.
    let diagnostic: Value = reqwest::Client::new()
        .get(format!("{url}/diagnostics/run/{}", child.report.run_id))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let parent = diagnostic_snapshot(&cfg, DiagnosticTarget::Session, "new")
        .await
        .unwrap();
    assert_eq!(diagnostic["snapshot"]["trace_id"], parent.trace_id);
    assert_ne!(
        diagnostic["snapshot"]["selected_span_id"],
        parent.selected_span_id
    );
    worker.abort();
    host.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(host);
    let restarted = app(cfg.clone(), model.clone()).await;
    assert_eq!(
        restarted.recall_memory("language").await.unwrap().hits[0]
            .record
            .id,
        id
    );
    // CLI reads/edits must not initialize even deliberately absent provider/MCP secrets.
    let mut cli_cfg = cfg.clone();
    cli_cfg.provider = AppProvider::Openai {
        base_url: "https://invalid.example/v1".into(),
        key_env: "TASKLOOM_MEMORY_INTENTIONALLY_ABSENT_KEY".into(),
        namespace: "not-contacted".into(),
    };
    cli_cfg.mcp = vec![AppMcpEndpoint {
        endpoint: "https://invalid.example/mcp".into(),
        token_env: Some("TASKLOOM_MEMORY_INTENTIONALLY_ABSENT_MCP".into()),
        namespace: "not-contacted".into(),
        bindings: vec![],
    }];
    let cfg_path = dir.path().join("cli.json");
    std::fs::write(&cfg_path, serde_json::to_vec(&cli_cfg).unwrap()).unwrap();
    let binary = env!("CARGO_BIN_EXE_taskloom");
    let inspected = std::process::Command::new(binary)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "diagnostics",
            "run",
            &child.report.run_id,
        ])
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let inspected: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inspected["snapshot"]["trace_id"], parent.trace_id);
    let cli = std::process::Command::new(binary)
        .args(["--config", cfg_path.to_str().unwrap(), "memory"])
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let export: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(export["revision"], 2);
    let edit = dir.path().join("edit.json");
    std::fs::write(&edit,json!({"expected_revision":2,"id":id,"value":"Chinese","note":"owner changed the preference"}).to_string()).unwrap();
    let competing = dir.path().join("competing-edit.json");
    std::fs::write(
        &competing,
        json!({"expected_revision":2,"id":id,"value":"French","note":"competing operator edit"})
            .to_string(),
    )
    .unwrap();
    let spawn_edit = |path: &std::path::Path| {
        std::process::Command::new(binary)
            .args([
                "--config",
                cfg_path.to_str().unwrap(),
                "memory-edit",
                path.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn_edit(&edit);
    let second = spawn_edit(&competing);
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert_ne!(
        first.status.success(),
        second.status.success(),
        "two CLI processes cannot commit the same revision"
    );
    let failed = if first.status.success() {
        &second
    } else {
        &first
    };
    let rejection = String::from_utf8_lossy(&failed.stderr);
    assert!(
        rejection.contains("memory revision") || rejection.contains("memory is owned"),
        "{rejection}"
    );
    assert_eq!(restarted.memory_snapshot().await.unwrap().revision, 3);
    let forget = dir.path().join("forget.json");
    std::fs::write(
        &forget,
        json!({"expected_revision":3,"kind":"preference","key":"language"}).to_string(),
    )
    .unwrap();
    let erased = std::process::Command::new(binary)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "memory-forget",
            forget.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(erased.status.success());
    assert!(restarted
        .memory_snapshot()
        .await
        .unwrap()
        .records
        .is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn host_readonly_and_disabled_memory_deny_before_any_model_request() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ModelFixture::new());
    let mut cfg = config(dir.path());
    cfg.memory.as_mut().unwrap().curate = false;
    let host = app(cfg.clone(), model.clone()).await;
    assert!(matches!(
        host.extract_memory(request()).await,
        Err(RuntimeError::Permission(_))
    ));
    assert!(matches!(
        host.review_memory(MemoryReview {
            expected_revision: 0,
            id: "invented".into(),
            accept: true,
            replace: true,
            note: "claim admin".into()
        })
        .await,
        Err(RuntimeError::Permission(_))
    ));
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    cfg.memory.as_mut().unwrap().scope.owner = "bob".into();
    let other = app(cfg.clone(), model.clone()).await;
    assert!(other.memory_snapshot().await.unwrap().records.is_empty());
    cfg.memory = None;
    let disabled = app(cfg, model).await;
    assert!(matches!(
        disabled.recall_memory("query").await,
        Err(RuntimeError::Permission(_))
    ));
}

#[tokio::test]
async fn dropped_extraction_releases_admission_and_retains_uncertain_model_spending_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let model = Arc::new(ModelFixture::new());
    let host = app(cfg.clone(), model.clone()).await;
    origin(&host).await;
    model.hang.store(true, Ordering::SeqCst);
    let cloned = host.clone();
    let task = tokio::spawn(async move { cloned.extract_memory(request()).await });
    tokio::time::timeout(Duration::from_secs(5), model.entered.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    host.create_session("other").await.unwrap();
    host.start_session_turn("other", "one", "other question")
        .await
        .unwrap();
    idle(&host, "other").await;
    model.hang.store(false, Ordering::SeqCst);
    let restarted = app(cfg, model.clone()).await;
    let recovered = restarted.extract_memory(request()).await.unwrap();
    assert!(recovered.proposals_recorded);
    assert_eq!(recovered.run.report.budget.committed_micros, 20);
    assert_eq!(recovered.run.report.budget.uncertain_micros, 10);
    assert_eq!(recovered.run.report.budget.uncertain_attempts, 1);
    assert!(recovered
        .run
        .report
        .events
        .iter()
        .any(|e| matches!(e.kind, RunEventKind::RecoveredModel)));
    assert!(recovered.run.config.tools.is_empty());
}
