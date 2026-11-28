#![cfg(all(feature = "app", feature = "database"))]
//! Labelled models with real SQL/HTTP and fsynced effects; no quality scores.
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
use taskloom::{
    runtime::{
        app::{queue::*, *},
        database::*,
        workflow::*,
        *,
    },
    CancelToken,
};

struct Fixture {
    calls: AtomicUsize,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}
impl Model for Fixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reserve = budget.reserve_before_dispatch(0).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(barrier) = &self.barrier {
                tokio::time::timeout(Duration::from_secs(10), barrier.wait())
                    .await
                    .expect("workers must overlap");
            }
            reserve.settle(0);
            let wrote = request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }));
            let wants = request
                .messages
                .iter()
                .any(|m| matches!(m,Message::User{content} if content.contains("write")));
            let turn = if wants && !wrote && !request.tools.is_empty() {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "effect-1".into(),
                        name: "append".into(),
                        arguments: json!({"n":7}),
                    }],
                }
            } else {
                AssistantTurn::text("fixture completed")
            };
            Ok(ModelOutput {
                model: request.model.clone(),
                turn,
            })
        })
    }
}
fn fixture() -> Arc<Fixture> {
    Arc::new(Fixture {
        calls: AtomicUsize::new(0),
        barrier: None,
    })
}
async fn application(
    root: &Path,
    url: &str,
    project: &str,
    model: Arc<Fixture>,
) -> Arc<Application> {
    let name = format!("TASKLOOM_QUEUE_TEST_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&name, url);
    let path = root.join("effects.txt");
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition{name:"append".into(),description:"fsynced fixture append with lost-result probe".into(),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["fixture:write".into()]),input_schema:json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false})},move|value:Value,context:ToolContext|{let path=path.clone();async move{use std::io::Write;let mut file=std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();writeln!(file,"{value}").unwrap();file.sync_all().unwrap();assert_ne!(context.run_id,"unknown","lost result after actual append");Ok(json!({"wrote":true}))}})).unwrap();
    Application::with_model(
        AppConfig {
            state_dir: root.into(),
            database: Some(AppDatabase {
                url_env: name,
                tenant: "queue-test".into(),
                project: project.into(),
                max_connections: 4,
                max_record_bytes: 64 * 1024 * 1024,
                lease_ms: 10000,
            }),
            knowledge: false,
            roles: vec![AppRole {
                name: "writer".into(),
                system: "fixture role".into(),
                output: WorkflowOutput::Text,
                tools: BTreeSet::new(),
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
async fn work(app: &Application) -> QueueJob {
    app.work_queue_once("fixture-worker", &CancelToken::new())
        .await
        .unwrap()
        .unwrap()
}
async fn resume(app: &Application, job: &QueueJob) {
    app.resume_queue_job(
        &job.id,
        QueueResume {
            expected_revision: job.revision,
            actor: "runner".into(),
            note: "explicit continuation after review".into(),
        },
    )
    .await
    .unwrap();
}
async fn approve(app: &Application, id: &str) {
    let state = app.inspect(id).await.unwrap();
    let request = state.pending_approval().unwrap();
    app.decide(
        id,
        &request.request_id,
        ApprovalDecision {
            approved: true,
            actor: "approver".into(),
            note: "exact n=7".into(),
        },
    )
    .await
    .unwrap();
}
async fn lifecycle(db: &SqlDatabase, url: &str, root: &Path) {
    let project = uuid::Uuid::new_v4().to_string();
    let model = fixture();
    let app = application(root, url, &project, model.clone()).await;
    app.configure_queue(
        Some(QueueLimits {
            max_open_jobs: 16,
            max_running: 2,
        }),
        0,
        "operator",
        "lifecycle",
    )
    .await
    .unwrap();
    app.enqueue_task(
        QueuedTask::Run {
            id: "approved".into(),
            prompt: "write".into(),
        },
        "runner",
    )
    .await
    .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let waiting = work(&app).await;
    assert_eq!(waiting.state, QueueState::Waiting);
    assert!(!root.join("effects.txt").exists());
    approve(&app, "approved").await;
    assert!(!root.join("effects.txt").exists());
    resume(&app, &waiting).await;
    assert_eq!(work(&app).await.state, QueueState::Completed);
    assert_eq!(
        std::fs::read_to_string(root.join("effects.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(app
        .work_queue_once("idle", &CancelToken::new())
        .await
        .unwrap()
        .is_none());
    app.enqueue_task(
        QueuedTask::Run {
            id: "unknown".into(),
            prompt: "write".into(),
        },
        "runner",
    )
    .await
    .unwrap();
    let waiting = work(&app).await;
    approve(&app, "unknown").await;
    resume(&app, &waiting).await;
    let failed = work(&app).await;
    assert_eq!(failed.state, QueueState::Failed);
    assert_eq!(
        std::fs::read_to_string(root.join("effects.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    resume(&app, &failed).await;
    let recovered = work(&app).await;
    assert_eq!(recovered.state, QueueState::Waiting);
    assert_eq!(
        app.inspect("unknown").await.unwrap().status,
        RunStatus::NeedsRecovery
    );
    assert_eq!(
        std::fs::read_to_string(root.join("effects.txt"))
            .unwrap()
            .lines()
            .count(),
        2,
        "uncertain write must not repeat"
    );
    app.reconcile(
        "unknown",
        "effect-1",
        RecoveryDecision::RecordResult {
            output: json!({"wrote":true}),
            is_error: false,
            actor: "operator".into(),
            note: "observed two fsynced lines".into(),
        },
    )
    .await
    .unwrap();
    resume(&app, &recovered).await;
    assert_eq!(work(&app).await.state, QueueState::Completed);
    assert_eq!(
        std::fs::read_to_string(root.join("effects.txt"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let request = AppWorkflowRequest {
        id: "flow".into(),
        input: json!({"goal":"fixture"}),
        definition: Some(WorkflowDefinition::new([
            WorkflowNode::new("first", "writer"),
            WorkflowNode::new("second", "writer").after(["first"]),
        ])),
        peer: None,
        limits: WorkflowLimits {
            max_cost_micros: Some(0),
            ..Default::default()
        },
    };
    let preview = app.preview_workflow(request.clone()).unwrap();
    app.enqueue_task(
        QueuedTask::Workflow {
            launch: Box::new(AppWorkflowLaunch {
                request,
                preview_key: preview.preview_key,
            }),
        },
        "runner",
    )
    .await
    .unwrap();
    assert_eq!(work(&app).await.state, QueueState::Completed);
    assert_eq!(
        app.workflow("flow").await.unwrap().status,
        WorkflowStatus::Completed
    );
    app.create_session("conversation").await.unwrap();
    for turn in ["first", "second"] {
        app.enqueue_task(
            QueuedTask::SessionTurn {
                id: "conversation".into(),
                turn: turn.into(),
                prompt: "brief fixture".into(),
            },
            "runner",
        )
        .await
        .unwrap();
        assert_eq!(work(&app).await.state, QueueState::Completed);
    }
    assert_eq!(app.session("conversation").await.unwrap().turns.len(), 2);
    let queued = app
        .enqueue_task(
            QueuedTask::Run {
                id: "cancel-before-start".into(),
                prompt: "brief fixture".into(),
            },
            "runner",
        )
        .await
        .unwrap();
    app.signal_queue_job(
        &queued.id,
        QueueSignal {
            expected_revision: queued.revision,
            action: QueueSignalAction::Cancel,
            actor: "operator".into(),
            note: "cancel before dispatch".into(),
        },
    )
    .await
    .unwrap();
    let before = model.calls.load(Ordering::SeqCst);
    assert_eq!(work(&app).await.state, QueueState::Stopped);
    assert_eq!(model.calls.load(Ordering::SeqCst), before);
    assert_eq!(
        app.inspect("cancel-before-start")
            .await
            .unwrap()
            .report
            .stop_reason,
        StopReason::Cancelled
    );
    let quota = db
        .generation_quota(DatabaseNamespace::new("queue-test", &project).unwrap())
        .snapshot()
        .await
        .unwrap();
    assert!(quota.usage.is_none());
    let before = model.calls.load(Ordering::SeqCst);
    app.queue_store()
        .unwrap()
        .enqueue(
            "invalid-host-payload",
            "run:invalid",
            json!({"unsupported_host_payload":true}),
            &app.queue_binding().unwrap(),
            "trusted-sdk-probe",
        )
        .await
        .unwrap();
    let invalid = work(&app).await;
    assert_eq!(
        invalid.state,
        QueueState::Failed,
        "unsupported trusted SDK payload is durably failed, never automatically reclaimed"
    );
    assert!(app
        .work_queue_once("idle-after-invalid", &CancelToken::new())
        .await
        .unwrap()
        .is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), before);
    let archived = app
        .signal_queue_job(
            &invalid.id,
            QueueSignal {
                expected_revision: invalid.revision,
                action: QueueSignalAction::Abandon,
                actor: "operator".into(),
                note: "retire invalid host-only input while retaining original audit".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(archived.state, QueueState::Abandoned);
    assert_eq!(archived.payload, invalid.payload);
    app.shutdown(Duration::from_secs(2)).await.unwrap();
    http_controls(db, url, root).await;
}

async fn issue(
    store: &SqlIdentityStore,
    user: &str,
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
                    tenant: "queue-test".into(),
                    project: project.into(),
                    role,
                }],
            },
            revision,
            "identity-admin",
        )
        .await
        .unwrap();
    store
        .issue_token(user, Duration::from_secs(300), "identity-admin")
        .await
        .unwrap()
}
async fn http_controls(db: &SqlDatabase, url: &str, root: &Path) {
    use taskloom::runtime::app::multiuser::{self, ProjectApplication};
    let one = root.join("http-one");
    let two = root.join("http-two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    let foreign = uuid::Uuid::new_v4().to_string();
    let model = fixture();
    let app = application(&one, url, &project, model.clone()).await;
    let other = application(&two, url, &foreign, fixture()).await;
    for app in [&app, &other] {
        app.configure_queue(
            Some(QueueLimits {
                max_open_jobs: 1,
                max_running: 1,
            }),
            0,
            "operator",
            "scoped HTTP queue",
        )
        .await
        .unwrap();
    }
    other
        .enqueue_task(
            QueuedTask::Run {
                id: "hidden".into(),
                prompt: "foreign fixture".into(),
            },
            "foreign-user",
        )
        .await
        .unwrap();
    let identities = db.identities();
    let prefix = uuid::Uuid::new_v4().simple().to_string();
    let runner_id = format!("runner-{prefix}");
    let operator_id = format!("operator-{prefix}");
    let runner = issue(&identities, &runner_id, &project, ProjectRole::Runner).await;
    let reader = issue(
        &identities,
        &format!("reader-{prefix}"),
        &project,
        ProjectRole::Reader,
    )
    .await;
    let approver = issue(
        &identities,
        &format!("approver-{prefix}"),
        &project,
        ProjectRole::Approver,
    )
    .await;
    let operator = issue(&identities, &operator_id, &project, ProjectRole::Operator).await;
    let router = multiuser::router(
        identities.clone(),
        vec![
            ProjectApplication {
                tenant: "queue-test".into(),
                project: project.clone(),
                application: app.clone(),
            },
            ProjectApplication {
                tenant: "queue-test".into(),
                project: foreign.clone(),
                application: other.clone(),
            },
        ],
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/tenants/queue-test/projects/{project}/api",
        listener.local_addr().unwrap()
    );
    let foreign_base = base.replace(&project, &foreign);
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let task = json!({"task":{"type":"run","id":"remote","prompt":"brief fixture"},"actor":"spoofed-admin"});
    assert_eq!(
        client
            .post(format!("{base}/queue/jobs"))
            .bearer_auth(reader.bearer.as_str())
            .json(&task)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(format!("{base}/queue/jobs"))
        .bearer_auth(runner.bearer.as_str())
        .json(&task)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let mut job: QueueJob =
        serde_json::from_value(response.json::<Value>().await.unwrap()["job"].clone()).unwrap();
    assert_eq!(job.submitted_by, runner_id);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        client
            .post(format!("{base}/runs"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"id":"bypass","prompt":"fixture"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .get(format!("{foreign_base}/queue/jobs/run-hidden"))
            .bearer_auth(runner.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let mut another = task.clone();
    another["task"]["id"] = json!("over-cap");
    assert_eq!(
        client
            .post(format!("{base}/queue/jobs"))
            .bearer_auth(runner.bearer.as_str())
            .json(&another)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let pause = json!({"expected_revision":job.revision,"action":"pause","actor":"spoofed-admin","note":"operator pause"});
    assert_eq!(
        client
            .post(format!("{base}/queue/jobs/{}/control", job.id))
            .bearer_auth(runner.bearer.as_str())
            .json(&pause)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(format!("{base}/queue/jobs/{}/control", job.id))
        .bearer_auth(operator.bearer.as_str())
        .json(&pause)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    job = serde_json::from_value(response.json::<Value>().await.unwrap()["job"].clone()).unwrap();
    assert!(matches!(&job.control,Some(QueueControl::Pause{actor,..}) if actor==&operator_id));
    let resume =
        json!({"expected_revision":job.revision,"actor":"spoofed-admin","note":"explicit resume"});
    assert_eq!(
        client
            .post(format!("{base}/queue/jobs/{}/resume", job.id))
            .bearer_auth(approver.bearer.as_str())
            .json(&resume)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(format!("{base}/queue/jobs/{}/resume", job.id))
        .bearer_auth(runner.bearer.as_str())
        .json(&resume)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    job = serde_json::from_value(response.json::<Value>().await.unwrap()["job"].clone()).unwrap();
    assert_eq!(job.events.last().unwrap().actor, runner_id);
    let response=client.post(format!("{base}/queue/jobs/{}/control",job.id)).bearer_auth(operator.bearer.as_str()).json(&json!({"expected_revision":job.revision,"action":"cancel","actor":"spoofed-admin","note":"stop before dispatch"})).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(work(&app).await.state, QueueState::Stopped);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let retire = app
        .enqueue_task(
            QueuedTask::Run {
                id: "retire-without-worker".into(),
                prompt: "brief fixture".into(),
            },
            &runner_id,
        )
        .await
        .unwrap();
    let signal = json!({"expected_revision":retire.revision,"action":"abandon","actor":"spoofed-admin","note":"retire scheduling without a model or worker call"});
    assert_eq!(
        client
            .post(format!("{base}/queue/jobs/{}/control", retire.id))
            .bearer_auth(runner.bearer.as_str())
            .json(&signal)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(format!("{base}/queue/jobs/{}/control", retire.id))
        .bearer_auth(operator.bearer.as_str())
        .json(&signal)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let archived: QueueJob =
        serde_json::from_value(response.json::<Value>().await.unwrap()["job"].clone()).unwrap();
    assert_eq!(archived.state, QueueState::Abandoned);
    assert_eq!(archived.events.last().unwrap().actor, operator_id);
    app.configure_queue(
        None,
        1,
        "operator",
        "disabled queue does not reopen direct execution",
    )
    .await
    .unwrap();
    assert_eq!(
        client
            .post(format!("{base}/runs"))
            .bearer_auth(runner.bearer.as_str())
            .json(&json!({"id":"disabled-bypass","prompt":"fixture"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    identities
        .revoke_token(&runner.id, "identity-admin")
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/queue"))
            .bearer_auth(runner.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
    app.shutdown(Duration::from_secs(2)).await.unwrap();
    other.shutdown(Duration::from_secs(2)).await.unwrap();
}
#[tokio::test]
async fn sqlite_application_queue_approvals_unknown_effects_workflows_sessions_cancel() {
    let root = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(root.path().join("app.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    lifecycle(&db, &url, root.path()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately"]
async fn postgres_application_queue_approvals_unknown_effects_workflows_sessions_cancel() {
    let root = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    lifecycle(&db, &url, root.path()).await;
    db.close().await;
}
#[tokio::test]
async fn sqlite_two_application_pools_overlap_generations() {
    let root = tempfile::tempdir().unwrap();
    let one = root.path().join("one");
    let two = root.path().join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let url = SqlDatabase::sqlite_url(root.path().join("parallel.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let model = Arc::new(Fixture {
        calls: AtomicUsize::new(0),
        barrier: Some(Arc::new(tokio::sync::Barrier::new(2))),
    });
    let a = application(&one, &url, "parallel", model.clone()).await;
    let b = application(&two, &url, "parallel", model.clone()).await;
    assert_eq!(a.queue_binding().unwrap(), b.queue_binding().unwrap());
    a.configure_queue(
        Some(QueueLimits {
            max_open_jobs: 2,
            max_running: 2,
        }),
        0,
        "operator",
        "parallel cap",
    )
    .await
    .unwrap();
    for id in ["a", "b"] {
        a.enqueue_task(
            QueuedTask::Run {
                id: id.into(),
                prompt: "brief fixture".into(),
            },
            "runner",
        )
        .await
        .unwrap();
    }
    let (x, y) = tokio::join!(work(&a), work(&b));
    assert_eq!(
        (x.state, y.state),
        (QueueState::Completed, QueueState::Completed)
    );
    assert_ne!(x.id, y.id);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    a.shutdown(Duration::from_secs(2)).await.unwrap();
    b.shutdown(Duration::from_secs(2)).await.unwrap();
    db.close().await;
}
