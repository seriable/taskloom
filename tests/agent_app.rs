#![cfg(feature = "app")]
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::app::*;
use taskloom::runtime::*;

const TOKEN: &str = "local-test-token-123456789";
async fn server(app: Arc<Application>) -> (String, reqwest::Client, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { serve(listener, app, TOKEN.into()).await.unwrap() });
    (url, reqwest::Client::new(), handle)
}
async fn post(client: &reqwest::Client, url: &str, body: Value) -> reqwest::Response {
    client
        .post(url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap()
}
async fn get(client: &reqwest::Client, url: &str) -> Value {
    let response = client.get(url).bearer_auth(TOKEN).send().await.unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    response.json().await.unwrap()
}
async fn idle(app: &Application, id: &str) -> RunState {
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.job(id).unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            app.job(id).unwrap().error.is_none(),
            "{:?}",
            app.job(id).unwrap()
        );
        app.inspect(id).await.unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn api_creates_persisted_runs_serves_events_and_verifies_archived_knowledge() {
    let dir = tempfile::tempdir().unwrap();
    let config = AppConfig {
        state_dir: dir.path().into(),
        ..Default::default()
    };
    let app = Application::open(config.clone()).await.unwrap();
    let (url, client, server) = server(Arc::clone(&app)).await;
    let response = client.get(format!("{url}/api/runs")).send().await.unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let page = client.get(&url).send().await.unwrap();
    assert!(page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("frame-ancestors 'none'"));
    assert!(page.text().await.unwrap().contains("Approval required"));
    assert_eq!(post(&client,&format!("{url}/api/knowledge"),json!({"document":{"id":"guide","title":"Guide","source":"test:guide","text":"Approval records the exact tool input.","metadata":{}},"expected_revision":0})).await.status(),200);
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/runs"),
            json!({"id":"research","prompt":"approval"})
        )
        .await
        .status(),
        202
    );
    let state = idle(&app, "research").await;
    assert_eq!(state.report.stop_reason, StopReason::Completed);
    let inspected = get(&client, &format!("{url}/api/runs/research")).await;
    assert_eq!(inspected["job"]["active"], false);
    let events = get(&client, &format!("{url}/api/runs/research/events?after=0")).await;
    assert!(!events["events"].as_array().unwrap().is_empty());
    let done = get(
        &client,
        &format!("{url}/api/runs/research/events?after={}", events["next"]),
    )
    .await;
    assert!(done["events"].as_array().unwrap().is_empty());
    let report = get(&client, &format!("{url}/api/runs/research/report")).await;
    assert_eq!(report["sources"][0]["document_id"], "guide");
    assert_eq!(
        client
            .delete(format!("{url}/api/knowledge/guide?expected_revision=1"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        get(&client, &format!("{url}/api/runs/research/report")).await,
        report
    );
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/runs"),
            json!({"id":"research","prompt":"changed"})
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        app.inspect("research").await.unwrap().revision,
        state.revision
    );
    server.abort();
    drop(app);
    let restarted = Application::open(config).await.unwrap();
    assert_eq!(restarted.list().await.unwrap().len(), 1);
    restarted.resume("research").await.unwrap();
    assert_eq!(idle(&restarted, "research").await.revision, state.revision);
}

struct EffectModel;
impl Model for EffectModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let turn = if request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }))
            {
                AssistantTurn::text("done")
            } else {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "effect-1".into(),
                        name: "effect".into(),
                        arguments: json!({"n":7}),
                    }],
                }
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
async fn effect_app(
    root: &std::path::Path,
    calls: Arc<AtomicUsize>,
    panic: bool,
) -> Arc<Application> {
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {name:"effect".into(),description:"effect v1".into(),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["write".into()]),input_schema:json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"]})},move |_:Value,_:ToolContext| {let calls=Arc::clone(&calls);async move {calls.fetch_add(1,Ordering::SeqCst);assert!(!panic,"injected tool panic");Ok(json!(7))}})).unwrap();
    Application::with_model(
        AppConfig {
            state_dir: root.into(),
            knowledge: false,
            ..Default::default()
        },
        Arc::new(EffectModel),
        tools,
        ToolPolicy::default().allow("effect").grant("write"),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn api_approval_binds_exact_ticket_and_decision_does_not_execute() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let app = effect_app(dir.path(), Arc::clone(&calls), false).await;
    let (url, client, server) = server(Arc::clone(&app)).await;
    post(
        &client,
        &format!("{url}/api/runs"),
        json!({"id":"approval","prompt":"write"}),
    )
    .await;
    let waiting = idle(&app, "approval").await;
    assert_eq!(waiting.status, RunStatus::WaitingApproval);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let body = json!({"approved":true,"actor":"operator","note":"reviewed exact n=7"});
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/runs/approval/approvals/wrong"),
            body.clone()
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        post(
            &client,
            &format!(
                "{url}/api/runs/approval/approvals/{}",
                waiting.pending_approval().unwrap().request_id
            ),
            body
        )
        .await
        .status(),
        200
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/runs/approval/resume"),
            Value::Null
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        idle(&app, "approval").await.report.stop_reason,
        StopReason::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn cancellation_while_waiting_persists_rejection_and_never_dispatches_tool() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let app = effect_app(dir.path(), Arc::clone(&calls), false).await;
    app.start("cancel", "write").await.unwrap();
    let waiting = idle(&app, "cancel").await;
    let ticket = waiting.pending_approval().unwrap().request_id.clone();
    app.cancel("cancel", "operator", "cancelled pending write")
        .await
        .unwrap();
    let cancelled = app.inspect("cancel").await.unwrap();
    assert_eq!(cancelled.status, RunStatus::Finished);
    assert_eq!(cancelled.report.stop_reason, StopReason::Cancelled);
    assert!(cancelled.pending_approval().is_none());
    assert!(!cancelled.tool_steps[0].approvals[0].decision.approved);
    assert!(app
        .decide(
            "cancel",
            &ticket,
            ApprovalDecision {
                approved: true,
                actor: "operator".into(),
                note: String::new()
            }
        )
        .await
        .is_err());
    app.resume("cancel").await.unwrap();
    idle(&app, "cancel").await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn worker_panic_releases_controller_but_retains_uncertain_effect_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let app = effect_app(dir.path(), Arc::clone(&calls), true).await;
    app.start("panic", "write").await.unwrap();
    let waiting = idle(&app, "panic").await;
    app.decide(
        "panic",
        &waiting.pending_approval().unwrap().request_id,
        ApprovalDecision {
            approved: true,
            actor: "operator".into(),
            note: String::new(),
        },
    )
    .await
    .unwrap();
    app.resume("panic").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.job("panic").unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.job("panic").unwrap().error.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    app.resume("panic").await.unwrap();
    let recovery = idle(&app, "panic").await;
    assert_eq!(recovery.status, RunStatus::NeedsRecovery);
    app.cancel("panic", "operator", "stop after panic")
        .await
        .unwrap();
    app.reconcile(
        "panic",
        "effect-1",
        RecoveryDecision::RecordResult {
            output: json!(7),
            is_error: false,
            actor: "operator".into(),
            note: "observed result".into(),
        },
    )
    .await
    .unwrap();
    let final_state = app.inspect("panic").await.unwrap();
    assert_eq!(final_state.report.stop_reason, StopReason::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct SlowModel {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Model for SlowModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(3).await?;
            self.started.notify_one();
            self.release.notified().await;
            reservation.settle(1);
            Ok(ModelOutput {
                turn: AssistantTurn::text("released"),
                model: request.model.clone(),
            })
        })
    }
}
#[tokio::test]
async fn active_cancellation_preserves_uncertain_model_spending_and_capacity_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(SlowModel {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let app = Application::with_model(
        AppConfig {
            state_dir: dir.path().into(),
            knowledge: false,
            max_active_runs: 1,
            limits: RunLimits {
                max_cost_micros: Some(10),
                ..Default::default()
            },
            ..Default::default()
        },
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    app.start("slow", "wait").await.unwrap();
    model.started.notified().await;
    assert!(app.start("extra", "wait").await.is_err());
    assert!(app.resume("slow").await.is_err());
    app.cancel("slow", "operator", "stop active request")
        .await
        .unwrap();
    let state = idle(&app, "slow").await;
    assert_eq!(state.report.stop_reason, StopReason::Cancelled);
    assert_eq!(
        state.report.detail.as_deref(),
        Some("cancelled by operator: stop active request")
    );
    assert_eq!(state.report.budget.uncertain_micros, 3);
    assert!(app.runs.inspect("extra").await.unwrap().is_none());
}

#[test]
fn cli_offline_install_flow_and_read_only_commands_do_not_need_provider_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_taskloom");
    let run = |args: &[&str]| {
        let result = std::process::Command::new(bin)
            .current_dir(dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result
    };
    run(&["init", "taskloom.json"]);
    std::fs::write(dir.path().join("guide.md"), "Approval records exact input.").unwrap();
    run(&["import", "guide", "Guide", "guide.md"]);
    run(&["search", "approval"]);
    run(&["run", "report", "approval"]);
    run(&["report", "report"]);
    let config_path = dir.path().join("taskloom.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["provider"] = json!({"type":"openai","base_url":"http://127.0.0.1:1/v1","key_env":"TASKLOOM_TEST_MISSING_KEY_349573","namespace":"test"});
    config["mcp"] = json!([{"endpoint":"http://127.0.0.1:1/mcp","token_env":null,"namespace":"test","bindings":[]}]);
    std::fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    for args in [
        &["inspect", "report"][..],
        &["events", "report"],
        &["report", "report"],
        &["list"],
    ] {
        run(args);
    }
    assert!(!std::process::Command::new(bin)
        .current_dir(dir.path())
        .args(["init", "taskloom.json"])
        .output()
        .unwrap()
        .status
        .success());
}

struct PausableModel {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Model for PausableModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let turn = if request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }))
            {
                AssistantTurn::text("done")
            } else {
                self.started.notify_one();
                self.release.notified().await;
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "read".into(),
                        name: "read".into(),
                        arguments: json!({}),
                    }],
                }
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
#[tokio::test]
async fn graceful_application_pause_finishes_model_then_stops_before_tool_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(PausableModel {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let count = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&count);
    let mut tools = ToolRegistry::new();
    tools
        .register(FnTool::new(
            ToolDefinition {
                name: "read".into(),
                description: "read v1".into(),
                effect: ToolEffect::ReadOnly,
                permissions: BTreeSet::new(),
                input_schema: json!({"type":"object"}),
            },
            move |_: Value, _: ToolContext| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(json!(1))
                }
            },
        ))
        .unwrap();
    let app = Application::with_model(
        AppConfig {
            state_dir: dir.path().into(),
            knowledge: false,
            ..Default::default()
        },
        model.clone(),
        tools,
        ToolPolicy::default().allow("read"),
    )
    .await
    .unwrap();
    app.start("pause", "read").await.unwrap();
    model.started.notified().await;
    app.pause("pause").unwrap();
    model.release.notify_one();
    let paused = idle(&app, "pause").await;
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    app.resume("pause").await.unwrap();
    let done = idle(&app, "pause").await;
    assert_eq!(done.report.stop_reason, StopReason::Completed);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(done.report.steps, 2);
}
