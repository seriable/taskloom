#![cfg(all(feature = "app", feature = "database"))]
//! Actual SQLite/PostgreSQL and HTTP/storage controls; model/embedding choices
//! here are explicit fixtures, not native quality measurements.
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
    gateway::ModelPrice,
    runtime::{app::*, database::*, memory::*, rag::*, skills::*, workflow::*, *},
};

#[derive(Default)]
struct Fixture(AtomicUsize);
impl Model for Fixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(5).await?.settle(3);
            self.0.fetch_add(1, Ordering::SeqCst);
            let extract = matches!(&request.messages[0],Message::System{content} if content.starts_with("Extract durable user facts"));
            let wrote = request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }));
            let wants_write = request
                .messages
                .iter()
                .any(|m| matches!(m,Message::User{content} if content.contains("needs_write")));
            let wants_research = request
                .messages
                .iter()
                .any(|m| matches!(m,Message::User{content} if content.contains("needs_research")));
            let turn = if extract {
                AssistantTurn::text(json!({"memories":[{"kind":"preference","key":"language","value":"English","quote":"I prefer English replies."}]}).to_string())
            } else if wants_research && !wrote {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "search-1".into(),
                        name: "knowledge_search".into(),
                        arguments: json!({"query":"unrelated terms"}),
                    }],
                }
            } else if wants_write && !wrote {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "append-1".into(),
                        name: "append".into(),
                        arguments: json!({"large":9007199254740993_u64,"decimal":1.0}),
                    }],
                }
            } else {
                AssistantTurn::text("fixture complete")
            };
            Ok(ModelOutput {
                model: request.model.clone(),
                turn,
            })
        })
    }
}
fn skill() -> SkillPackage {
    use sha2::{Digest, Sha256};
    let text = "Return a brief answer; no tools or additional authority.";
    serde_json::from_value(json!({"manifest":{"format":1,"name":"sql-answer","version":"1.0.0","description":"SQL inert fixture",
        "files":{"role.md":hex::encode(Sha256::digest(text))},"roles":[{"name":"answer","instructions":"role.md","tools":[],"output":{"type":"text"}}],
        "templates":[{"id":"answer","title":"Answer","description":"SQL template fixture","input_schema":{"type":"object"},"workflow":{"type":"dag","definition":WorkflowDefinition::new([WorkflowNode::new("answer","answer")])}}]},"files":{"role.md":text}})).unwrap()
}
fn config(root: &Path, url: &str, project: &str) -> AppConfig {
    let env = format!("TASKLOOM_APP_SQL_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&env, url);
    AppConfig {
        state_dir: root.into(),
        database: Some(AppDatabase {
            url_env: env,
            tenant: "app-sql".into(),
            project: project.into(),
            max_connections: 4,
            max_record_bytes: 64 * 1024 * 1024,
            lease_ms: 10_000,
        }),
        provider: AppProvider::Openai {
            base_url: "http://127.0.0.1:1/v1".into(),
            key_env: "TASKLOOM_UNRESOLVED_GENERATION_FOR_COLD_SQL_READ".into(),
            namespace: "fixture-only-no-dispatch".into(),
        },
        model: "fixture".into(),
        prices: vec![ModelPrice {
            prefix: "fixture".into(),
            input_per_million: 0.0,
            output_per_million: 0.0,
        }],
        limits: RunLimits {
            max_cost_micros: Some(100),
            ..Default::default()
        },
        roles: vec![AppRole {
            name: "worker".into(),
            system: "Fixture role".into(),
            output: WorkflowOutput::Text,
            tools: BTreeSet::new(),
        }],
        memory: Some(AppMemory {
            scope: MemoryScope {
                tenant: "app-sql".into(),
                project: project.into(),
                owner: "alice".into(),
            },
            actor: "host-alice".into(),
            curate: true,
            recall: Default::default(),
        }),
        ..Default::default()
    }
}
async fn open(config: AppConfig, model: Arc<Fixture>, effect: &Path) -> Arc<Application> {
    let path = effect.to_path_buf();
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {name:"append".into(),description:"SQL actual once-only effect fixture".into(),effect:ToolEffect::NonIdempotent,
        permissions:BTreeSet::from(["write".into()]),input_schema:json!({"type":"object","properties":{"large":{"type":"integer"},"decimal":{"type":"number"}},"required":["large","decimal"],"additionalProperties":false})},move |value:Value,_:ToolContext| {
        let path=path.clone();async move {use std::io::Write;
            let mut file=std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();writeln!(file,"{value}").unwrap();file.sync_all().unwrap();
            panic!("fixture response lost after real fsynced write");
            #[allow(unreachable_code)] Ok(Value::Null)
        }
    })).unwrap();
    Application::with_model(
        config,
        model,
        tools,
        ToolPolicy::default().allow("append").grant("write"),
    )
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
async fn parent_idle(app: &Application, id: &str, workflow: bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while if workflow {
            app.workflow_job(id)
        } else {
            app.session_job(id)
        }
        .unwrap()
        .active
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn controls(db: &SqlDatabase, url: &str, root: &Path) {
    let project = uuid::Uuid::new_v4().to_string();
    let cfg = config(root, url, &project);
    let model = Arc::new(Fixture::default());
    let effect = root.join("actual-effect.txt");
    let identity = db.identity().await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(db.identity().await.unwrap(), identity);
    let preview = preview_configuration(cfg.clone()).unwrap();
    assert!(preview
        .credential_names
        .contains(&cfg.database.as_ref().unwrap().url_env));
    assert!(!preview.services_contacted);
    let app = open(cfg.clone(), model.clone(), &effect).await;
    assert!(
        app.runs.file_root().is_none()
            && app.corpus.file_root().is_none()
            && app.stores.skills.file_root().is_none()
    );
    ingest(
        app.corpus.as_ref(),
        Document {
            id: "source".into(),
            title: "Zephyr policy".into(),
            source: "fixture://sql".into(),
            text: "Unicode 原文 persists exactly.".into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        Some(0),
    )
    .await
    .unwrap();
    app.start("plain", "brief task").await.unwrap();
    let plain = idle(&app, "plain").await;
    assert_eq!(plain.report.stop_reason, StopReason::Completed);
    assert!(app.list().await.unwrap().iter().any(|r| r.id == "plain"));
    assert!(FileRunStore::new(root.join("runs"))
        .inspect("plain")
        .await
        .unwrap()
        .is_none());
    app.start_workflow(AppWorkflowRequest {
        id: "flow".into(),
        input: Value::Null,
        definition: Some(WorkflowDefinition::new([
            WorkflowNode::new("first", "worker"),
            WorkflowNode::new("second", "worker").after(["first"]),
        ])),
        peer: None,
        limits: WorkflowLimits {
            max_cost_micros: Some(300),
            ..Default::default()
        },
    })
    .await
    .unwrap();
    parent_idle(&app, "flow", true).await;
    let flow = app.workflow("flow").await.unwrap();
    assert_eq!(flow.status, WorkflowStatus::Completed);
    assert_eq!(app.list_workflows().await.unwrap()[0]["id"], "flow");
    app.create_session("conversation").await.unwrap();
    app.start_session_turn("conversation", "source", "I prefer English replies.")
        .await
        .unwrap();
    parent_idle(&app, "conversation", false).await;
    let source = app.session("conversation").await.unwrap();
    assert_eq!(source.turns.len(), 1);
    assert_eq!(app.list_sessions().await.unwrap()[0]["turns"], 1);
    let extracted = app
        .extract_memory(MemoryExtract {
            expected_revision: 0,
            session_id: "conversation".into(),
            turn_id: "source".into(),
            request_id: "learn".into(),
        })
        .await
        .unwrap();
    assert!(extracted.proposals_recorded);
    let id = extracted.memory.records.keys().next().unwrap().clone();
    app.review_memory(MemoryReview {
        expected_revision: 1,
        id,
        accept: true,
        replace: false,
        note: "host reviewed source".into(),
    })
    .await
    .unwrap();
    let auth = cfg.memory.as_ref().unwrap().access().unwrap();
    assert_eq!(
        MemoryManager::new(&app.memory)
            .recall(&auth, "language", &Default::default())
            .await
            .unwrap()
            .hits[0]
            .record
            .value,
        "English"
    );
    let pkg = skill();
    let pin = pkg.reference().unwrap();
    app.stores.skills.install(pkg, 0).await.unwrap();
    let mut bad = skill();
    bad.files.insert("role.md".into(), "tampered".into());
    assert!(app.stores.skills.install(bad, 1).await.is_err());
    app.start("write", "needs_write").await.unwrap();
    let waiting = idle(&app, "write").await;
    assert_eq!(waiting.status, RunStatus::WaitingApproval);
    let ticket = waiting.pending_approval().unwrap().request_id.clone();
    assert!(!effect.exists());
    let trace = app
        .diagnostic_snapshot(DiagnosticTarget::Workflow, "flow")
        .await
        .unwrap();
    let child = &flow.nodes[0].child.as_ref().unwrap().run_id;
    let child_trace = app
        .diagnostic_snapshot(DiagnosticTarget::Run, child)
        .await
        .unwrap();
    assert_eq!(trace.trace_id, child_trace.trace_id);
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(app);
    let cold = AppStores::open(&cfg).await.unwrap();
    assert_eq!(
        cold.runs.inspect("plain").await.unwrap().unwrap().revision,
        plain.revision
    );
    assert_eq!(
        cold.sessions.load("conversation").await.unwrap().unwrap(),
        source
    );
    assert_eq!(cold.corpus.snapshot().await.unwrap().revision, 1);
    assert_eq!(
        cold.skills
            .inspect(&pin)
            .await
            .unwrap()
            .reference()
            .unwrap(),
        pin
    );
    assert_eq!(
        diagnostic_snapshot(&cfg, DiagnosticTarget::Workflow, "flow")
            .await
            .unwrap()
            .trace_id,
        trace.trace_id
    );
    cold.close().await;
    let mut loaded = cfg.clone();
    loaded.skills = vec![AppSkill {
        package: pin.clone(),
        mount: "sql".into(),
        grant: SkillGrant {
            tools: BTreeSet::new(),
            permissions: BTreeSet::new(),
            credentials: BTreeSet::new(),
        },
        mode: SkillActivationMode::Active,
    }];
    let app = open(loaded.clone(), model.clone(), &effect).await;
    assert_eq!(
        app.skill_templates()["templates"].as_array().unwrap().len(),
        1
    );
    app.start_skill_template(
        "sql",
        "answer",
        AppTemplateRequest {
            id: "template".into(),
            input: json!({}),
            limits: WorkflowLimits {
                max_cost_micros: Some(300),
                ..Default::default()
            },
        },
    )
    .await
    .unwrap();
    parent_idle(&app, "template", true).await;
    assert_eq!(
        app.workflow("template").await.unwrap().status,
        WorkflowStatus::Completed
    );
    assert!(app
        .decide(
            "write",
            "wrong",
            ApprovalDecision {
                approved: true,
                actor: "operator".into(),
                note: "wrong".into()
            }
        )
        .await
        .is_err());
    app.decide(
        "write",
        &ticket,
        ApprovalDecision {
            approved: true,
            actor: "operator".into(),
            note: "exact original values".into(),
        },
    )
    .await
    .unwrap();
    app.resume("write").await.unwrap();
    let interrupted = idle(&app, "write").await;
    assert!(matches!(
        interrupted.tool_steps[0].progress,
        ToolProgress::InFlight { .. }
    ));
    assert_eq!(std::fs::read_to_string(&effect).unwrap().lines().count(), 1);
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(app);
    let app = open(loaded.clone(), model.clone(), &effect).await;
    let calls = model.0.load(Ordering::SeqCst);
    app.resume("write").await.unwrap();
    assert_eq!(idle(&app, "write").await.status, RunStatus::NeedsRecovery);
    assert_eq!(model.0.load(Ordering::SeqCst), calls);
    app.reconcile(
        "write",
        "append-1",
        RecoveryDecision::RecordResult {
            output: json!({"observed":true}),
            is_error: false,
            actor: "operator".into(),
            note: "observed actual effect".into(),
        },
    )
    .await
    .unwrap();
    app.resume("write").await.unwrap();
    let final_write = idle(&app, "write").await;
    assert!(
        app.job("write").unwrap().error.is_none(),
        "{:?}",
        app.job("write").unwrap()
    );
    assert_eq!(final_write.report.stop_reason, StopReason::Completed);
    assert_eq!(std::fs::read_to_string(&effect).unwrap().lines().count(), 1);
    app.stores.skills.retire(&pin, 1).await.unwrap();
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(app);
    assert!(Application::with_model(
        loaded.clone(),
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default()
    )
    .await
    .is_err());
    loaded.skills[0].mode = SkillActivationMode::RecoveryOnly;
    let recovered = open(loaded.clone(), model.clone(), &effect).await;
    assert!(recovered.skill_templates()["templates"]
        .as_array()
        .unwrap()
        .is_empty());
    let revision = recovered.workflow("template").await.unwrap().revision;
    recovered.resume_workflow("template").await.unwrap();
    parent_idle(&recovered, "template", true).await;
    assert_eq!(
        recovered.workflow("template").await.unwrap().revision,
        revision
    );
    let mut foreign = cfg.clone();
    foreign.database.as_mut().unwrap().project = "foreign".into();
    assert!(
        AppStores::open(&foreign).await.is_err(),
        "memory scope mismatch must be rejected"
    );
    foreign.memory.as_mut().unwrap().scope.project = "foreign".into();
    let foreign = AppStores::open(&foreign).await.unwrap();
    assert!(foreign.runs.list().await.unwrap().is_empty());
    assert!(foreign
        .corpus
        .snapshot()
        .await
        .unwrap()
        .documents
        .is_empty());
    assert!(foreign.skills.catalog().await.unwrap().entries.is_empty());
    foreign.close().await;
    // A declared global telemetry namespace cannot merge two SQL projects.
    let mut scoped = cfg.clone();
    scoped.observability = Some(AppObservability {
        namespace: Some("shared-operator-label".into()),
        otlp: None,
    });
    let scoped_trace = diagnostic_snapshot(&scoped, DiagnosticTarget::Run, "plain")
        .await
        .unwrap();
    let mut different = scoped.clone();
    different.database.as_mut().unwrap().project = format!("foreign-trace-{project}");
    different.memory = None;
    let isolated = open(different.clone(), model.clone(), &effect).await;
    isolated.start("plain", "different project").await.unwrap();
    idle(&isolated, "plain").await;
    let foreign_trace = isolated
        .diagnostic_snapshot(DiagnosticTarget::Run, "plain")
        .await
        .unwrap();
    assert_ne!(scoped_trace.trace_id, foreign_trace.trace_id);
    isolated.shutdown(Duration::from_secs(1)).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}/api", listener.local_addr().unwrap());
    let host = recovered.clone();
    let server = tokio::spawn(async move {
        serve(listener, host, "sql-api-test-token".into())
            .await
            .unwrap();
    });
    let client = reqwest::Client::new();
    let list: Value = client
        .get(format!("{api}/runs"))
        .bearer_auth("sql-api-test-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == "plain"));
    let diag: Value = client
        .get(format!("{api}/diagnostics/workflow/flow"))
        .bearer_auth("sql-api-test-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diag["snapshot"]["trace_id"], trace.trace_id);
    server.abort();
    let _ = server.await;
    recovered.shutdown(Duration::from_secs(1)).await.unwrap();
    assert!(
        !root.join("runs").exists()
            && !root.join("sessions").exists()
            && !root.join("knowledge").exists()
            && !root.join("skills").exists()
    );
}
#[tokio::test]
async fn sqlite_application_backend_cold_inspection_skills_and_unknown_effect_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.sqlite");
    let url = SqlDatabase::sqlite_url(&path).unwrap();
    let cfg = config(dir.path(), &url, "before-migration");
    assert!(AppStores::open(&cfg).await.is_err());
    let db = SqlDatabase::sqlite(&path, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_application_backend_cold_inspection_skills_and_unknown_effect_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
