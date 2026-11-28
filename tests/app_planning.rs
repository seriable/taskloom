#![cfg(feature = "app")]
//! Admission/configuration controls use fixtures; no native effectiveness claim.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use taskloom::gateway::ModelPrice;
use taskloom::runtime::{app::*, mcp::McpToolBinding, workflow::*, *};

const TOKEN: &str = "design-preview-test-operator-token";
#[derive(Default)]
struct ObservedModel {
    calls: AtomicUsize,
    inputs: Mutex<Vec<ModelRequest>>,
}
impl Model for ObservedModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inputs.lock().unwrap().push(request.clone());
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text("completed by fixture"),
            })
        })
    }
}
fn config(root: &std::path::Path) -> AppConfig {
    AppConfig {
        state_dir: root.into(),
        knowledge: false,
        limits: RunLimits {
            max_cost_micros: Some(0),
            ..Default::default()
        },
        roles: vec![AppRole {
            name: "worker".into(),
            system: "original host role".into(),
            tools: BTreeSet::new(),
            output: WorkflowOutput::Text,
        }],
        ..Default::default()
    }
}
fn request() -> AppWorkflowRequest {
    let params: Value = serde_json::from_str(
        r#"{"decimal":1.0,"large":9007199254740993,"text":"<script>never execute</script>"}"#,
    )
    .unwrap();
    AppWorkflowRequest {
        id: "preview".into(),
        input: json!({"goal":"exact input"}),
        peer: None,
        definition: Some(WorkflowDefinition::new([WorkflowNode::new(
            "work", "worker",
        )
        .with_params(params)])),
        limits: WorkflowLimits {
            max_cost_micros: Some(0),
            ..Default::default()
        },
    }
}
async fn server(app: Arc<Application>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api", listener.local_addr().unwrap());
    (
        url,
        tokio::spawn(async move { serve(listener, app, TOKEN.into()).await.unwrap() }),
    )
}
async fn idle(app: &Application, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.workflow_job(id).unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(app.workflow_job(id).unwrap().error.is_none());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn preview_is_pure_and_binds_exact_input_host_contracts_limits_and_launch() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let model = Arc::new(ObservedModel::default());
    let app = Application::with_model(
        cfg.clone(),
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    let input = request();
    let preview = app.preview_workflow(input.clone()).unwrap();
    assert_eq!(preview.plan.revision, 0);
    assert!(preview.plan.events.is_empty());
    assert_eq!(preview.plan.dispatches, 0);
    assert!(!preview.dispatch_performed);
    assert!(!preview.identity_reserved);
    assert!(app.workflows.inspect("preview").await.unwrap().is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        preview.preview_key,
        app.preview_workflow(input.clone()).unwrap().preview_key
    );
    let mut changed = input.clone();
    changed.input = json!("different input");
    assert_ne!(
        preview.preview_key,
        app.preview_workflow(changed.clone()).unwrap().preview_key
    );
    assert!(matches!(
        app.launch_workflow(AppWorkflowLaunch {
            request: changed,
            preview_key: preview.preview_key.clone()
        })
        .await,
        Err(RuntimeError::Conflict(_))
    ));
    let mut changed = input.clone();
    changed.limits.max_dispatches -= 1;
    assert_ne!(
        preview.preview_key,
        app.preview_workflow(changed).unwrap().preview_key
    );
    let mut changed_cfg = cfg;
    changed_cfg.roles[0].system = "revised host contract".into();
    let changed_host = Application::with_model(
        changed_cfg,
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    assert!(matches!(
        changed_host
            .launch_workflow(AppWorkflowLaunch {
                request: input.clone(),
                preview_key: preview.preview_key.clone()
            })
            .await,
        Err(RuntimeError::Conflict(_))
    ));
    assert!(app.workflows.inspect("preview").await.unwrap().is_none());
    let (url, worker) = server(app.clone()).await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(format!("{url}/workflow-design/preview"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let echoed: AppWorkflowRequest =
        serde_json::from_value(serde_json::to_value(&preview.request).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&echoed).unwrap(),
        serde_json::to_value(&input).unwrap()
    );
    let response = client
        .post(format!("{url}/workflow-design/launch"))
        .bearer_auth(TOKEN)
        .json(&AppWorkflowLaunch {
            request: echoed,
            preview_key: preview.preview_key.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    idle(&app, "preview").await;
    let state = app.workflow("preview").await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        serde_json::to_value(&state.definition).unwrap(),
        serde_json::to_value(&preview.plan.definition).unwrap()
    );
    assert_eq!(state.input, preview.plan.input);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    {
        let requests = model.inputs.lock().unwrap();
        let Message::User { content } = requests[0].messages.last().unwrap() else {
            panic!()
        };
        assert!(content.contains("9007199254740993"));
        assert!(content.contains("\"decimal\":1.0"));
    }
    // Design routes must not shadow a valid workflow named "preview".
    assert_eq!(
        client
            .get(format!("{url}/workflows/preview"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{url}/workflow-design/launch"))
            .bearer_auth(TOKEN)
            .json(&AppWorkflowLaunch {
                request: input,
                preview_key: preview.preview_key
            })
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    worker.abort();
    app.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn graph_semantics_and_peer_authority_are_checked_without_store_or_callback() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ObservedModel::default());
    let app = Application::with_model(
        config(dir.path()),
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    let mut input = request();
    input.definition = Some(WorkflowDefinition::new([
        WorkflowNode::new("a", "worker").after(["b"]),
        WorkflowNode::new("b", "worker").after(["a"]),
    ]));
    assert!(app.preview_workflow(input.clone()).is_err());
    input.definition = Some(WorkflowDefinition::new([
        WorkflowNode::new("a", "worker").when(Condition::Exists {
            node: "b".into(),
            pointer: "".into(),
        }),
        WorkflowNode::new("b", "worker"),
    ]));
    assert!(app.preview_workflow(input.clone()).is_err());
    input.definition = Some(WorkflowDefinition::new([WorkflowNode::new(
        "a",
        "not-a-host-role",
    )]));
    assert!(app.preview_workflow(input.clone()).is_err());
    input.definition = Some(WorkflowDefinition::new([
        WorkflowNode::new("a", "worker").select("forged", "missing", "")
    ]));
    assert!(app.preview_workflow(input.clone()).is_err());
    input.definition = None;
    input.peer = Some(PeerSpec::default());
    assert!(app.preview_workflow(input.clone()).is_err());
    input = request();
    input.limits.max_parallel = 33;
    assert!(app.preview_workflow(input).is_err());
    assert!(app.workflows.inspect("preview").await.unwrap().is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn proposed_configuration_never_resolves_credentials_connects_or_changes_live_grants() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ObservedModel::default());
    let app = Application::with_model(
        config(dir.path()),
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    source.set_nonblocking(true).unwrap();
    let base = format!("http://{}", source.local_addr().unwrap());
    let proposed = AppConfig {
        state_dir: dir.path().join("not-created"),
        provider: AppProvider::Openai {
            base_url: format!("{base}/v1"),
            key_env: "TASKLOOM_PROPOSED_CONFIG_UNRESOLVED".into(),
            namespace: "new-account".into(),
        },
        model: "local-model".into(),
        prices: vec![ModelPrice::new("local-model", 0.5, 2.0)],
        limits: RunLimits {
            max_cost_micros: Some(100),
            ..Default::default()
        },
        knowledge: false,
        mcp: vec![AppMcpEndpoint {
            endpoint: format!("{base}/mcp"),
            token_env: Some("TASKLOOM_PROPOSED_MCP_UNRESOLVED".into()),
            namespace: "new-service".into(),
            bindings: vec![McpToolBinding {
                name: "evidence".into(),
                remote_name: "search".into(),
                effect: ToolEffect::ReadOnly,
                permissions: BTreeSet::from(["docs:read".into()]),
                description: None,
            }],
        }],
        roles: vec![AppRole {
            name: "researcher".into(),
            system: "host grants stay separate from source instructions".into(),
            tools: BTreeSet::from(["evidence".into()]),
            output: WorkflowOutput::Validated {
                policy: FinalOutputPolicy::CitedReport {
                    retrieval_tools: BTreeSet::from(["evidence".into()]),
                    min_sources: 1,
                    required_documents: BTreeSet::new(),
                    max_repairs: 1,
                    answer_schema: Some(
                        json!({"type":"object","required":["text","citations"],"properties":{"text":{"type":"string","minLength":1}}}),
                    ),
                },
            },
        }],
        ..Default::default()
    };
    let checked = preview_configuration(proposed.clone()).unwrap();
    assert!(!checked.services_contacted);
    assert!(!checked.configuration_applied);
    assert_eq!(checked.credential_names.len(), 2);
    assert_eq!(checked.declared_tools.len(), 1);
    assert!(!proposed.state_dir.exists());
    let (url, worker) = server(app.clone()).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/setup/validate"))
        .bearer_auth(TOKEN)
        .json(&proposed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let view: Value = response.json().await.unwrap();
    assert_eq!(view["services_contacted"], false);
    assert_eq!(
        view["configuration"]["roles"][0]["output"]["policy"]["answer_schema"]["required"],
        json!(["text", "citations"])
    );
    let mut selected = proposed.clone();
    selected.roles[0].output = WorkflowOutput::Validated {
        policy: FinalOutputPolicy::SelectedSources {
            retrieval_tools: BTreeSet::from(["evidence".into()]),
            min_sources: 1,
            required_documents: BTreeSet::new(),
            max_repairs: 1,
            answer_schema: Some(json!({"type":"object","required":["text","sources"]})),
            bibliography: false,
        },
    };
    let selected_view = client
        .post(format!("{url}/setup/validate"))
        .bearer_auth(TOKEN)
        .json(&selected)
        .send()
        .await
        .unwrap();
    assert_eq!(selected_view.status(), 200);
    let selected_view: Value = selected_view.json().await.unwrap();
    assert_eq!(
        selected_view["configuration"]["roles"][0]["output"]["policy"]["type"],
        "selected_sources"
    );
    assert_eq!(selected_view["services_contacted"], false);
    let active: Value = client
        .get(format!("{url}/setup"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(active["active_tools"].as_array().unwrap().is_empty());
    assert_eq!(active["active_model"], "offline-demo");
    assert!(matches!(source.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let mut bad = proposed.clone();
    bad.limits.max_cost_micros = None;
    assert!(preview_configuration(bad).is_err());
    let mut bad = proposed.clone();
    bad.prices.clear();
    assert!(preview_configuration(bad).is_err());
    let mut bad = proposed.clone();
    bad.mcp[0].bindings[0].effect = ToolEffect::NonIdempotent;
    assert!(preview_configuration(bad).is_err());
    let mut bad = proposed.clone();
    bad.mcp[0].token_env = Some("sk-secret-value-not-a-variable".into());
    assert!(preview_configuration(bad).is_err());
    let mut bad = proposed.clone();
    bad.mcp[0].endpoint.push_str("?token=inline-value");
    assert!(preview_configuration(bad).is_err());
    let mut bad = proposed.clone();
    if let WorkflowOutput::Validated {
        policy: FinalOutputPolicy::CitedReport { answer_schema, .. },
    } = &mut bad.roles[0].output
    {
        *answer_schema = Some(json!({"type":"not-a-json-type"}));
    } else {
        panic!("fixture must retain its citation contract");
    }
    assert!(preview_configuration(bad).is_err());
    let mut bad = proposed.clone();
    bad.roles[0].output = WorkflowOutput::Json {
        schema: json!({"type":"not-a-json-type"}),
    };
    assert!(preview_configuration(bad).is_err());
    let file = dir.path().join("proposed.json");
    std::fs::write(&file, serde_json::to_vec(&proposed).unwrap()).unwrap();
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_taskloom"))
        .args([
            "--config",
            "deliberately-nonexistent-main.json",
            "config-check",
            file.to_str().unwrap(),
        ])
        .env(
            "TASKLOOM_PROPOSED_CONFIG_UNRESOLVED",
            "secret-value-must-not-be-resolved-or-serialized",
        )
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(!String::from_utf8_lossy(&cli.stdout)
        .contains("secret-value-must-not-be-resolved-or-serialized"));
    assert!(matches!(source.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
    assert!(!proposed.state_dir.exists());
    worker.abort();
    app.shutdown(Duration::from_secs(1)).await.unwrap();
}
