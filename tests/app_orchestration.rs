#![cfg(feature = "app")]
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::app::*;
use taskloom::runtime::workflow::*;
use taskloom::runtime::*;

const TOKEN: &str = "collaboration-test-token";
struct RoleModel;
impl Model for RoleModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let system = match &request.messages[0] {
                Message::System { content } => content.as_str(),
                _ => "",
            };
            let input: Value = request
                .messages
                .iter()
                .rev()
                .find_map(|m| match m {
                    Message::User { content } => serde_json::from_str(content).ok(),
                    _ => None,
                })
                .unwrap_or(Value::Null);
            let turn = match system {
                "planner" => AssistantTurn::text(
                    r#"{"tasks":[{"id":"one","instructions":"write exactly once"}]}"#,
                ),
                "writer" => {
                    let revision = input["params"]["revision"].as_u64().unwrap();
                    assert_eq!(input["dependencies"]["worker-0"], "effect complete");
                    if revision == 1 {
                        assert_eq!(
                            input["dependencies"]["check-review-0"]["feedback"],
                            "revise once"
                        );
                    }
                    AssistantTurn::text(format!("draft {revision}"))
                }
                "reviewer" => {
                    let good = input["dependencies"]["draft"] == "draft 1";
                    AssistantTurn::text(json!({"approved":good,"feedback":if good {"accepted"} else {"revise once"}}).to_string())
                }
                _ if request
                    .messages
                    .iter()
                    .any(|m| matches!(m, Message::Tool { .. })) =>
                {
                    AssistantTurn::text("effect complete")
                }
                _ => AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "write".into(),
                        name: "effect".into(),
                        arguments: json!({"n":7}),
                    }],
                },
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
fn role(name: &str, output: WorkflowOutput, effect: bool) -> AppRole {
    AppRole {
        name: name.into(),
        system: name.into(),
        output,
        tools: if effect {
            BTreeSet::from(["effect".into()])
        } else {
            BTreeSet::new()
        },
    }
}
fn config(root: &std::path::Path) -> AppConfig {
    AppConfig {
        state_dir: root.into(),
        knowledge: false,
        roles: vec![
            role("executor", WorkflowOutput::Text, true),
            role(
                "planner",
                WorkflowOutput::Json {
                    schema: PeerPlan::schema(2),
                },
                false,
            ),
            role("writer", WorkflowOutput::Text, false),
            role(
                "reviewer",
                WorkflowOutput::Json {
                    schema: PeerReview::schema(),
                },
                false,
            ),
        ],
        ..Default::default()
    }
}
async fn app(config: AppConfig, calls: Arc<AtomicUsize>, panic: bool) -> Arc<Application> {
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {name:"effect".into(),description:"counter v1".into(),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["write".into()]),input_schema:json!({"type":"object","properties":{"n":{"const":7}},"required":["n"],"additionalProperties":false})},move |_:Value,_:ToolContext| {let calls=calls.clone(); async move {calls.fetch_add(1,Ordering::SeqCst);assert!(!panic,"interrupt after effect");Ok(json!(7))}})).unwrap();
    Application::with_model(
        config,
        Arc::new(RoleModel),
        tools,
        ToolPolicy::default().allow("effect").grant("write"),
    )
    .await
    .unwrap()
}
async fn idle(app: &Application, id: &str, workflow: bool, allow_error: bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = if workflow {
                app.workflow_job(id)
            } else {
                app.session_job(id)
            }
            .unwrap();
            if !status.active {
                if !allow_error {
                    assert!(status.error.is_none(), "{:?}", status.error);
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
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
fn dag(id: &str) -> AppWorkflowRequest {
    AppWorkflowRequest {
        id: id.into(),
        input: json!("write"),
        definition: Some(WorkflowDefinition::new([WorkflowNode::new(
            "execute", "executor",
        )])),
        peer: None,
        limits: WorkflowLimits::default(),
    }
}
fn approve() -> ApprovalDecision {
    ApprovalDecision {
        approved: true,
        actor: "test-operator".into(),
        note: "verified n=7".into(),
    }
}

#[tokio::test]
async fn workflow_api_restart_uses_original_role_and_blocks_generic_child_execution() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = config(dir.path());
    let host = app(cfg.clone(), calls.clone(), false).await;
    let (url, client, server) = server(host.clone()).await;
    assert_eq!(
        client
            .get(format!("{url}/api/workflows"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/workflows"),
            serde_json::to_value(dag("same-id")).unwrap()
        )
        .await
        .status(),
        202
    );
    idle(&host, "same-id", true, false).await;
    let state = host.workflow("same-id").await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Waiting);
    let run = state.nodes[0].child.as_ref().unwrap().run_id.clone();
    assert_eq!(run.len(), 73);
    let child = host.inspect(&run).await.unwrap();
    assert_eq!(child.status, RunStatus::WaitingApproval);
    assert_eq!(
        client
            .get(format!("{url}/api/runs/{run}"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/runs/{run}/resume"),
            Value::Null
        )
        .await
        .status(),
        400
    );
    assert!(host
        .decide(
            &run,
            &child.pending_approval().unwrap().request_id,
            approve()
        )
        .await
        .is_err());
    assert_eq!(host.list_workflows().await.unwrap().len(), 1);
    let events: Value = client
        .get(format!("{url}/api/workflows/same-id/events?after=0"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!events["events"].as_array().unwrap().is_empty());
    let next: Value = client
        .get(format!(
            "{url}/api/workflows/same-id/events?after={}",
            events["next"]
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(next["events"], json!([]));
    server.abort();
    drop(host);
    let mut changed = cfg.clone();
    changed.roles[0].system = "changed executor".into();
    let host = app(changed, calls.clone(), false).await;
    assert!(host
        .decide_workflow(
            "same-id",
            "execute",
            &child.pending_approval().unwrap().request_id,
            approve()
        )
        .await
        .is_err());
    drop(host);
    let host = app(cfg, calls.clone(), false).await;
    host.decide_workflow(
        "same-id",
        "execute",
        &child.pending_approval().unwrap().request_id,
        approve(),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    host.resume_workflow("same-id").await.unwrap();
    idle(&host, "same-id", true, false).await;
    let done = host.workflow("same-id").await.unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    host.resume_workflow("same-id").await.unwrap();
    idle(&host, "same-id", true, false).await;
    assert_eq!(
        host.workflow("same-id").await.unwrap().revision,
        done.revision
    );
}

#[tokio::test]
async fn peer_review_revision_preserves_handoffs_and_never_replays_executor() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = app(config(dir.path()), calls.clone(), false).await;
    host.start_workflow(AppWorkflowRequest {
        id: "peer".into(),
        input: json!("write"),
        definition: None,
        peer: Some(PeerSpec {
            max_tasks: 2,
            max_revisions: 1,
            ..Default::default()
        }),
        limits: WorkflowLimits::default(),
    })
    .await
    .unwrap();
    idle(&host, "peer", true, false).await;
    let state = host.workflow("peer").await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Waiting);
    let child = host
        .inspect(
            &state
                .nodes
                .iter()
                .find(|n| n.id == "worker-0")
                .unwrap()
                .child
                .as_ref()
                .unwrap()
                .run_id,
        )
        .await
        .unwrap();
    host.decide_workflow(
        "peer",
        "worker-0",
        &child.pending_approval().unwrap().request_id,
        approve(),
    )
    .await
    .unwrap();
    host.resume_workflow("peer").await.unwrap();
    idle(&host, "peer", true, false).await;
    let done = host.workflow("peer").await.unwrap();
    assert_eq!(
        done.status,
        WorkflowStatus::Completed,
        "{:?}",
        done.nodes
            .iter()
            .filter_map(|n| n.detail.as_ref())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        done.nodes
            .iter()
            .find(|n| n.id == "result")
            .unwrap()
            .output
            .as_ref()
            .unwrap()["revision"],
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(done.budget().unwrap().committed_micros, 0);
}

#[tokio::test]
async fn peer_rejects_effectful_reviewer_before_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.roles[3].tools.insert("effect".into());
    let host = app(cfg, Arc::new(AtomicUsize::new(0)), false).await;
    assert!(host
        .start_workflow(AppWorkflowRequest {
            id: "bad".into(),
            input: Value::Null,
            definition: None,
            peer: Some(PeerSpec::default()),
            limits: WorkflowLimits::default()
        })
        .await
        .is_err());
    assert!(host.workflow("bad").await.is_err());
}

#[tokio::test]
async fn sessions_keep_active_binding_over_restart_and_commit_followup_history() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = config(dir.path());
    let host = app(cfg.clone(), calls.clone(), false).await;
    let (url, client, server) = server(host.clone()).await;
    assert_eq!(
        post(
            &client,
            &format!("{url}/api/sessions"),
            json!({"id":"chat"})
        )
        .await
        .status(),
        201
    );
    let response = post(
        &client,
        &format!("{url}/api/sessions/chat/turns"),
        json!({"turn_id":"first","prompt":"write"}),
    )
    .await;
    assert_eq!(response.status(), 202);
    let run = response.json::<Value>().await.unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    idle(&host, "chat", false, false).await;
    assert_eq!(
        host.session("chat")
            .await
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .run_id,
        run
    );
    assert!(host
        .start_session_turn("chat", "second", "competing")
        .await
        .is_err());
    server.abort();
    drop(host);
    let host = app(cfg, calls.clone(), false).await;
    let child = host.inspect(&run).await.unwrap();
    host.decide_session(
        "chat",
        &child.pending_approval().unwrap().request_id,
        approve(),
    )
    .await
    .unwrap();
    host.resume_session("chat").await.unwrap();
    idle(&host, "chat", false, false).await;
    assert!(host.session("chat").await.unwrap().active.is_none());
    host.start_session_turn("chat", "second", "continue")
        .await
        .unwrap();
    idle(&host, "chat", false, false).await;
    let chat = host.session("chat").await.unwrap();
    assert_eq!(chat.turns.len(), 2);
    assert!(chat.messages.len() > 5);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(host.list_sessions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancelled_session_requires_acknowledged_abandon_before_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = app(config(dir.path()), calls.clone(), false).await;
    host.create_session("chat").await.unwrap();
    host.start_session_turn("chat", "first", "write")
        .await
        .unwrap();
    idle(&host, "chat", false, false).await;
    host.cancel_session("chat", "operator", "stop")
        .await
        .unwrap();
    assert!(host
        .start_session_turn("chat", "second", "continue")
        .await
        .is_err());
    host.abandon_session("chat", "operator", "write was never dispatched")
        .await
        .unwrap();
    let chat = host.session("chat").await.unwrap();
    assert_eq!(chat.turns[0].outcome, StopReason::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    host.start_session_turn("chat", "second", "continue")
        .await
        .unwrap();
    idle(&host, "chat", false, false).await;
    assert_eq!(host.session("chat").await.unwrap().turns.len(), 2);
}

#[tokio::test]
async fn workflow_uncertain_write_reconciles_via_role_after_restart_without_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = config(dir.path());
    let host = app(cfg.clone(), calls.clone(), true).await;
    host.start_workflow(dag("recovery")).await.unwrap();
    idle(&host, "recovery", true, false).await;
    let state = host.workflow("recovery").await.unwrap();
    let run = state.nodes[0].child.as_ref().unwrap().run_id.clone();
    let child = host.inspect(&run).await.unwrap();
    host.decide_workflow(
        "recovery",
        "execute",
        &child.pending_approval().unwrap().request_id,
        approve(),
    )
    .await
    .unwrap();
    host.resume_workflow("recovery").await.unwrap();
    idle(&host, "recovery", true, true).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(host);
    let host = app(cfg, calls.clone(), false).await;
    host.resume_workflow("recovery").await.unwrap();
    idle(&host, "recovery", true, false).await;
    assert_eq!(
        host.inspect(&run).await.unwrap().status,
        RunStatus::NeedsRecovery
    );
    host.reconcile_workflow(
        "recovery",
        "execute",
        "write",
        RecoveryDecision::RecordResult {
            output: json!(7),
            is_error: false,
            actor: "operator".into(),
            note: "verified stored effect".into(),
        },
    )
    .await
    .unwrap();
    host.resume_workflow("recovery").await.unwrap();
    idle(&host, "recovery", true, false).await;
    assert_eq!(
        host.workflow("recovery").await.unwrap().status,
        WorkflowStatus::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct HeldModel(Arc<tokio::sync::Notify>);
impl Model for HeldModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.0.notified().await;
            Ok(ModelOutput {
                turn: AssistantTurn::text("done"),
                model: request.model.clone(),
            })
        })
    }
}
#[tokio::test]
async fn global_admission_spans_all_parent_types_without_id_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    let mut cfg = config(dir.path());
    cfg.roles = vec![role("executor", WorkflowOutput::Text, false)];
    cfg.max_active_runs = 2;
    let host = Application::with_model(
        cfg,
        Arc::new(HeldModel(release.clone())),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    host.start("shared", "hold").await.unwrap();
    host.start_workflow(dag("shared")).await.unwrap();
    assert!(host.job("shared").unwrap().active);
    assert!(host.workflow_job("shared").unwrap().active);
    host.create_session("shared").await.unwrap();
    assert!(host
        .start_session_turn("shared", "one", "hold")
        .await
        .is_err());
    assert!(host.session("shared").await.unwrap().active.is_none());
    assert!(host.pause_session("shared").is_err());
    tokio::time::timeout(Duration::from_secs(10), async {
        while host.job("shared").unwrap().active || host.workflow_job("shared").unwrap().active {
            release.notify_waiters();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        host.workflow("shared").await.unwrap().status,
        WorkflowStatus::Completed
    );
}

#[tokio::test]
async fn cancelling_waiting_workflow_revokes_approval_and_keeps_child_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let host = app(config(dir.path()), calls.clone(), false).await;
    host.start_workflow(dag("cancel")).await.unwrap();
    idle(&host, "cancel", true, false).await;
    let state = host.workflow("cancel").await.unwrap();
    let run = state.nodes[0].child.as_ref().unwrap().run_id.clone();
    let ticket = host
        .inspect(&run)
        .await
        .unwrap()
        .pending_approval()
        .unwrap()
        .request_id
        .clone();
    host.cancel_workflow("cancel", "operator", "stop pending effect")
        .await
        .unwrap();
    assert_eq!(
        host.workflow("cancel").await.unwrap().status,
        WorkflowStatus::Cancelled
    );
    assert_eq!(
        host.inspect(&run).await.unwrap().report.stop_reason,
        StopReason::Cancelled
    );
    assert!(host
        .decide_workflow("cancel", "execute", &ticket, approve())
        .await
        .is_err());
    host.resume_workflow("cancel").await.unwrap();
    idle(&host, "cancel", true, false).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
