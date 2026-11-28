#![cfg(all(feature = "app", feature = "database"))]
//! Real loopback HTTP / SQL authorization. Models are explicit fixtures;
//! writes below are actual fsynced local effects, not model-quality evidence.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use taskloom::runtime::{
    app::{multiuser::*, AppConfig, AppRole, AppWorkflowRequest, Application},
    database::*,
    workflow::*,
    *,
};

#[derive(Default)]
struct EffectModel(AtomicUsize);
impl Model for EffectModel {
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
                turn: if request
                    .messages
                    .iter()
                    .any(|m| matches!(m, Message::Tool { .. }))
                {
                    AssistantTurn::text("fixture completed")
                } else {
                    AssistantTurn {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "effect-1".into(),
                            name: "append".into(),
                            arguments: json!({"large":9007199254740993_u64,"decimal":1.0}),
                        }],
                    }
                },
            })
        })
    }
}
async fn app(root: &Path, model: Arc<EffectModel>) -> Arc<Application> {
    let mut tools = ToolRegistry::new();
    let path = root.join("actual-effect.txt");
    tools.register(FnTool::new(ToolDefinition {name:"append".into(),description:"Actual fixture fsynced append".into(),effect:ToolEffect::NonIdempotent,
        permissions:BTreeSet::from(["fixture:write".into()]),input_schema:json!({"type":"object","required":["large","decimal"],"properties":{"large":{"type":"integer"},"decimal":{"type":"number"}},"additionalProperties":false})},move |input:Value,context:ToolContext| {
        let path=path.clone();async move {
            use std::io::Write;
            let mut file=std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            writeln!(file,"{}",serde_json::to_string(&input).unwrap()).unwrap();file.sync_all().unwrap();
            assert_ne!(context.run_id,"unknown","intentional lost result after an actual durable append");
            Ok(json!({"wrote":true}))
        }
    })).unwrap();
    Application::with_model(
        AppConfig {
            state_dir: root.into(),
            knowledge: false,
            roles: vec![AppRole {
                name: "worker".into(),
                system: "Fixture worker".into(),
                output: WorkflowOutput::Text,
                tools: BTreeSet::from(["append".into()]),
            }],
            limits: RunLimits {
                max_cost_micros: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        model,
        tools,
        ToolPolicy::default().allow("append").grant("fixture:write"),
    )
    .await
    .unwrap()
}
async fn token(
    store: &SqlIdentityStore,
    user: &str,
    tenant: &str,
    project: &str,
    role: ProjectRole,
) -> IssuedToken {
    let revision = store.snapshot().await.unwrap().revision;
    store
        .put_user(
            ServiceUser {
                id: user.into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: tenant.into(),
                    project: project.into(),
                    role,
                }],
            },
            revision,
            "test-bootstrap",
        )
        .await
        .unwrap();
    store
        .issue_token(user, Duration::from_secs(60), "test-bootstrap")
        .await
        .unwrap()
}
async fn idle(app: &Application, id: &str) -> RunState {
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.job(id).unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        app.inspect(id).await.unwrap()
    })
    .await
    .unwrap()
}
async fn controls(db: &SqlDatabase, root: &Path) {
    let project = uuid::Uuid::new_v4().to_string();
    let store = db.identities();
    let runner_id = format!("runner-{project}");
    let approver_id = format!("approver-{project}");
    let operator_id = format!("operator-{project}");
    let runner = token(
        &store,
        &runner_id,
        "tenant-a",
        &project,
        ProjectRole::Runner,
    )
    .await;
    let reader = token(
        &store,
        &format!("reader-{project}"),
        "tenant-a",
        &project,
        ProjectRole::Reader,
    )
    .await;
    let approver = token(
        &store,
        &approver_id,
        "tenant-a",
        &project,
        ProjectRole::Approver,
    )
    .await;
    let operator = token(
        &store,
        &operator_id,
        "tenant-a",
        &project,
        ProjectRole::Operator,
    )
    .await;
    let foreign = token(
        &store,
        &format!("foreign-{project}"),
        "tenant-b",
        &project,
        ProjectRole::Operator,
    )
    .await;
    let model = Arc::new(EffectModel::default());
    let a = app(&root.join("a"), model.clone()).await;
    let b = app(&root.join("b"), model.clone()).await;
    let router = router(
        store.clone(),
        vec![
            ProjectApplication {
                tenant: "tenant-a".into(),
                project: project.clone(),
                application: a.clone(),
            },
            ProjectApplication {
                tenant: "tenant-b".into(),
                project: project.clone(),
                application: b.clone(),
            },
        ],
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::new();
    let prefix = format!("{base}/tenants/tenant-a/projects/{project}/api");
    let other = format!("{base}/tenants/tenant-b/projects/{project}/api");
    assert_eq!(
        client
            .get(format!("{prefix}/runs"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{prefix}/runs"))
            .bearer_auth("invalid")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/api/runs"))
            .bearer_auth(operator.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let identity: Value = client
        .get(format!("{prefix}/identity"))
        .bearer_auth(runner.bearer.as_str())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        identity,
        json!({"user":runner_id,"tenant":"tenant-a","project":project,"role":"runner","semantic_memory_enabled":false})
    );
    for bearer in [
        reader.bearer.as_str(),
        approver.bearer.as_str(),
        foreign.bearer.as_str(),
    ] {
        assert_eq!(
            client
                .post(format!("{prefix}/runs"))
                .bearer_auth(bearer)
                .json(&json!({"id":"denied","prompt":"write"}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    for (path, expected) in [
        ("memory", 403),
        ("%6demory", 404),
        ("sessions/x/memory-turns", 403),
        ("not-yet-registered", 404),
    ] {
        assert_eq!(
            client
                .get(format!("{prefix}/{path}"))
                .bearer_auth(operator.bearer.as_str())
                .send()
                .await
                .unwrap()
                .status(),
            expected,
            "{path}"
        );
    }
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        client
            .post(format!("{prefix}/runs"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"id":"same","prompt":"append exact approved values"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let waiting = idle(&a, "same").await;
    assert_eq!(waiting.status, RunStatus::WaitingApproval);
    assert!(b.runs.inspect("same").await.unwrap().is_none());
    let ticket = waiting.pending_approval().unwrap().request_id.clone();
    assert_eq!(
        waiting.pending_approval().unwrap().call.arguments["large"],
        json!(9007199254740993_u64)
    );
    assert!(!root.join("a/actual-effect.txt").exists());
    assert_eq!(
        client
            .post(format!("{prefix}/runs/same/approvals/{ticket}"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"approved":true,"actor":"forged-operator","note":"spoof"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{prefix}/runs/same/approvals/wrong"))
            .bearer_auth(approver.bearer.as_str())
            .json(&json!({"approved":true,"note":"wrong exact binding"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .post(format!("{prefix}/runs/same/approvals/{ticket}"))
            .bearer_auth(approver.bearer.as_str())
            .json(&json!({"approved":true,"actor":"forged-operator","note":"reviewed exact input"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let approved = a.inspect("same").await.unwrap();
    assert_eq!(
        approved.tool_steps[0].approvals[0].decision.actor,
        approver_id
    );
    assert_eq!(
        approved.tool_steps[0].call.arguments,
        waiting.tool_steps[0].call.arguments
    );
    assert!(
        !root.join("a/actual-effect.txt").exists(),
        "approval never dispatches"
    );
    assert_eq!(
        client
            .post(format!("{prefix}/runs/same/resume"))
            .bearer_auth(approver.bearer.as_str())
            .json(&Value::Null)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{prefix}/runs/same/resume"))
            .bearer_auth(runner.bearer.as_str())
            .json(&Value::Null)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        idle(&a, "same").await.report.stop_reason,
        StopReason::Completed
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a/actual-effect.txt")).unwrap(),
        "{\"decimal\":1.0,\"large\":9007199254740993}\n"
    );
    assert_eq!(
        client
            .get(format!("{other}/runs/same"))
            .bearer_auth(operator.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{other}/runs"))
            .bearer_auth(foreign.bearer.as_str())
            .json(&json!({"id":"same","prompt":"other project"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let foreign_wait = idle(&b, "same").await;
    assert_eq!(foreign_wait.status, RunStatus::WaitingApproval);
    assert_eq!(
        client
            .post(format!("{other}/runs/same/cancel"))
            .bearer_auth(foreign.bearer.as_str())
            .json(&json!({"note":"cancel before effect","actor":"spoof"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let cancelled = b.inspect("same").await.unwrap();
    assert_eq!(
        cancelled.tool_steps[0].approvals[0].decision.actor,
        format!("foreign-{project}")
    );
    assert!(!root.join("b/actual-effect.txt").exists());
    // A real effect followed by a lost result cannot be retried by any role.
    assert_eq!(
        client
            .post(format!("{prefix}/runs"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"id":"unknown","prompt":"write once"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let unknown = idle(&a, "unknown").await;
    let ticket = &unknown.pending_approval().unwrap().request_id;
    assert_eq!(
        client
            .post(format!("{prefix}/runs/unknown/approvals/{ticket}"))
            .bearer_auth(approver.bearer.as_str())
            .json(&json!({"approved":true,"note":"exact second append"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{prefix}/runs/unknown/resume"))
            .bearer_auth(runner.bearer.as_str())
            .json(&Value::Null)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let crashed = idle(&a, "unknown").await;
    assert!(matches!(
        crashed.tool_steps[0].progress,
        ToolProgress::InFlight { .. }
    ));
    let model_before_reentry = model.0.load(Ordering::SeqCst);
    assert_eq!(
        client
            .post(format!("{prefix}/runs/unknown/resume"))
            .bearer_auth(runner.bearer.as_str())
            .json(&Value::Null)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(idle(&a, "unknown").await.status, RunStatus::NeedsRecovery);
    assert_eq!(model.0.load(Ordering::SeqCst), model_before_reentry);
    let effects = std::fs::read_to_string(root.join("a/actual-effect.txt")).unwrap();
    assert_eq!(effects.lines().count(), 2);
    assert_eq!(client.post(format!("{prefix}/runs/unknown/reconcile/effect-1")).bearer_auth(approver.bearer.as_str()).json(&json!({"type":"record_result","output":{"wrote":true},"is_error":false,"note":"observed"})).send().await.unwrap().status(),403);
    assert_eq!(client.post(format!("{prefix}/runs/unknown/reconcile/effect-1")).bearer_auth(operator.bearer.as_str()).json(&json!({"type":"record_result","output":{"wrote":true},"is_error":false,"actor":"spoof","note":"operator observed actual append"})).send().await.unwrap().status(),200);
    assert_eq!(
        a.inspect("unknown").await.unwrap().tool_steps[0].recoveries[0]
            .decision
            .actor(),
        operator_id
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a/actual-effect.txt")).unwrap(),
        effects
    );
    // Parent-specific routes must preserve the same actor/authority boundary.
    assert_eq!(
        client
            .post(format!("{prefix}/sessions"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"id":"conversation"}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    assert_eq!(
        client
            .post(format!("{prefix}/sessions/conversation/turns"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"turn_id":"first","prompt":"append under parent authority"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.session_job("conversation").unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let session = a.session("conversation").await.unwrap();
    let child = &session.active.as_ref().unwrap().run_id;
    let waiting = a.inspect(child).await.unwrap();
    let ticket = &waiting.pending_approval().unwrap().request_id;
    assert_eq!(
        client
            .post(format!("{prefix}/sessions/conversation/approvals/{ticket}"))
            .bearer_auth(reader.bearer.as_str())
            .json(&json!({"approved":true,"note":"forbidden"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{prefix}/sessions/conversation/approvals/{ticket}"))
            .bearer_auth(approver.bearer.as_str())
            .json(&json!({"approved":true,"actor":"spoof","note":"parent-bound approval"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        a.inspect(child).await.unwrap().tool_steps[0].approvals[0]
            .decision
            .actor,
        approver_id
    );
    assert_eq!(
        client
            .post(format!("{prefix}/sessions/conversation/cancel"))
            .bearer_auth(operator.bearer.as_str())
            .json(&json!({"actor":"spoof","note":"cancel before dispatch"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{prefix}/sessions/conversation/abandon"))
            .bearer_auth(operator.bearer.as_str())
            .json(&json!({"actor":"spoof","note":"close cancelled turn"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        a.session("conversation").await.unwrap().turns[0]
            .actor
            .as_deref(),
        Some(operator_id.as_str())
    );
    let request = AppWorkflowRequest {
        id: "flow".into(),
        input: json!({"goal":"append"}),
        definition: Some(WorkflowDefinition::new([WorkflowNode::new(
            "worker", "worker",
        )])),
        peer: None,
        limits: WorkflowLimits {
            max_cost_micros: Some(0),
            ..Default::default()
        },
    };
    assert_eq!(
        client
            .post(format!("{prefix}/workflows"))
            .bearer_auth(runner.bearer.as_str())
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.workflow_job("flow").unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let flow = a.workflow("flow").await.unwrap();
    let child = &flow.nodes[0].child.as_ref().unwrap().run_id;
    let waiting = a.inspect(child).await.unwrap();
    let ticket = &waiting.pending_approval().unwrap().request_id;
    let endpoint = format!("{prefix}/workflows/flow/nodes/worker/approvals/{ticket}");
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"approved":true,"note":"forbidden"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(approver.bearer.as_str())
            .json(&json!({"approved":true,"actor":"spoof","note":"approved exact child"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        a.inspect(child).await.unwrap().tool_steps[0].approvals[0]
            .decision
            .actor,
        approver_id
    );
    assert_eq!(
        client
            .post(format!("{prefix}/workflows/flow/cancel"))
            .bearer_auth(operator.bearer.as_str())
            .json(&json!({"actor":"spoof","note":"cancel before effect"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(serde_json::to_string(&a.inspect(child).await.unwrap())
        .unwrap()
        .contains(&operator_id));
    assert_eq!(
        std::fs::read_to_string(root.join("a/actual-effect.txt")).unwrap(),
        effects
    );
    store
        .revoke_token(&runner.id, "security-admin")
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{prefix}/identity"))
            .bearer_auth(runner.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let revision = store.snapshot().await.unwrap().revision;
    store
        .put_user(
            ServiceUser {
                id: operator_id.clone(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "tenant-a".into(),
                    project: project.clone(),
                    role: ProjectRole::Reader,
                }],
            },
            revision,
            "security-admin",
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .post(format!("{prefix}/runs/unknown/resume"))
            .bearer_auth(operator.bearer.as_str())
            .json(&Value::Null)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{prefix}/runs"))
            .bearer_auth(operator.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // Initial registration cannot mount two tenants onto aliased state roots.
    assert!(router_for_overlap(store.clone(), a.clone(), &project).is_err());
    server.abort();
    let _ = server.await;
    a.shutdown(Duration::from_secs(1)).await.unwrap();
    b.shutdown(Duration::from_secs(1)).await.unwrap();
}
fn router_for_overlap(
    store: SqlIdentityStore,
    app: Arc<Application>,
    project: &str,
) -> Result<axum::Router, RuntimeError> {
    router(
        store,
        vec![
            ProjectApplication {
                tenant: "one".into(),
                project: project.into(),
                application: app.clone(),
            },
            ProjectApplication {
                tenant: "two".into(),
                project: project.into(),
                application: app,
            },
        ],
    )
}
#[tokio::test]
async fn sqlite_real_http_role_scope_actor_and_uncertain_effect_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqlDatabase::sqlite(
        dir.path().join("identity.sqlite"),
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    controls(&db, dir.path()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_real_http_role_scope_actor_and_uncertain_effect_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, dir.path()).await;
    db.close().await;
}
