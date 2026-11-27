#![cfg(feature = "app")]
//! HTTP/controller fixtures, separate from the actual native/MCP campaign.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use taskloom::runtime::workflow::*;
use taskloom::runtime::{app::*, skills::*, *};

fn package() -> SkillPackage {
    let text =
        "Call write once, then return only JSON {\"done\":true}. Host approval cannot be waived.";
    use sha2::{Digest, Sha256};
    serde_json::from_value(json!({"manifest":{"format":1,"name":"app-fixture","version":"1.0.0","description":"Controller fixture","files":{"role.md":hex::encode(Sha256::digest(text))},"tools":{"write":{"effect":"non_idempotent","permissions":["notes:write"]}},"roles":[{"name":"worker","instructions":"role.md","tools":["write"],"output":{"type":"validated","policy":{"type":"json_schema","schema":{"type":"object","properties":{"done":{"const":true}},"required":["done"],"additionalProperties":false},"max_repairs":0}}}],"templates":[{"id":"write-once","title":"Write once","description":"Explicit effect fixture","input_schema":{"type":"object","properties":{"goal":{"type":"string"}},"required":["goal"],"additionalProperties":false},"workflow":{"type":"dag","definition":WorkflowDefinition::new(vec![WorkflowNode::new("result","worker")])}}]},"files":{"role.md":text}})).unwrap()
}
struct FixtureModel(AtomicUsize);
impl Model for FixtureModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(10).await?.settle(10);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: if request
                    .messages
                    .iter()
                    .any(|m| matches!(m, Message::Tool { .. }))
                {
                    AssistantTurn::text("{\"done\":true}")
                } else {
                    AssistantTurn {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "one-write".into(),
                            name: "write".into(),
                            arguments: json!({"n":7}),
                        }],
                    }
                },
            })
        })
    }
}
fn config(root: &std::path::Path, pin: SkillRef, mode: SkillActivationMode) -> AppConfig {
    AppConfig {
        state_dir: root.into(),
        knowledge: false,
        limits: RunLimits {
            max_cost_micros: Some(100),
            ..Default::default()
        },
        final_output: Some(FinalOutputPolicy::JsonSchema {
            schema: json!({"type":"object","required":["top_level_only"]}),
            max_repairs: 0,
        }),
        skills: vec![AppSkill {
            package: pin,
            mount: "demo".into(),
            mode,
            grant: SkillGrant {
                tools: BTreeSet::from(["write".into()]),
                permissions: BTreeSet::from(["notes:write".into()]),
                credentials: BTreeSet::new(),
            },
        }],
        ..Default::default()
    }
}
async fn app(
    config: AppConfig,
    model: Arc<FixtureModel>,
    writes: Arc<AtomicUsize>,
) -> Arc<Application> {
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {name:"write".into(),description:"fixture write".into(),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["notes:write".into()]),input_schema:json!({"type":"object","properties":{"n":{"const":7}},"required":["n"],"additionalProperties":false})},move |_,_|{let writes=writes.clone();async move {writes.fetch_add(1,Ordering::SeqCst);Ok(json!({"done":true}))}})).unwrap();
    Application::with_model(
        config,
        model,
        tools,
        ToolPolicy::default().allow("write").grant("notes:write"),
    )
    .await
    .unwrap()
}
async fn idle(app: &Application, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = app.workflow_job(id).unwrap();
            if !job.active {
                assert!(job.error.is_none(), "{:?}", job.error);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn template(id: &str) -> AppTemplateRequest {
    AppTemplateRequest {
        id: id.into(),
        input: json!({"goal":"write n=7"}),
        limits: WorkflowLimits {
            max_cost_micros: Some(100),
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn packaged_validated_role_preserves_approval_contract_across_restart_and_explicit_recovery_mode(
) {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSkillStore::new(dir.path().join("skills"));
    let package = package();
    let pin = package.reference().unwrap();
    store.install(package, 0).await.unwrap();
    let model = Arc::new(FixtureModel(AtomicUsize::new(0)));
    let writes = Arc::new(AtomicUsize::new(0));
    let cfg = config(dir.path(), pin.clone(), SkillActivationMode::Active);
    let host = app(cfg.clone(), model.clone(), writes.clone()).await;
    assert_eq!(host.registered_roles().count(), 1);
    assert_eq!(host.skill_templates()["templates"][0]["id"], "write-once");
    host.start_skill_template("demo", "write-once", template("workflow"))
        .await
        .unwrap();
    idle(&host, "workflow").await;
    let waiting = host.workflow("workflow").await.unwrap();
    assert_eq!(waiting.status, WorkflowStatus::Waiting);
    let child = host
        .inspect(
            &waiting
                .node("result")
                .unwrap()
                .child
                .as_ref()
                .unwrap()
                .run_id,
        )
        .await
        .unwrap();
    let ticket = child.pending_approval().unwrap().request_id.clone();
    assert_eq!(child.config.skill_binding.as_ref().unwrap().package, pin);
    assert!(child.config.output_policy.is_some());
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    store.retire(&pin, 1).await.unwrap();
    assert!(Application::with_model(
        cfg.clone(),
        model.clone(),
        ToolRegistry::new(),
        ToolPolicy::default()
    )
    .await
    .is_err());
    let mut recovery = cfg;
    recovery.skills[0].mode = SkillActivationMode::RecoveryOnly;
    let restored = app(recovery, model.clone(), writes.clone()).await;
    assert!(restored.skill_templates()["templates"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(matches!(
        restored
            .start_skill_template("demo", "write-once", template("new-template"))
            .await,
        Err(RuntimeError::Permission(_))
    ));
    let raw = AppWorkflowRequest {
        id: "new-raw".into(),
        input: Value::Null,
        definition: Some(WorkflowDefinition::new(vec![WorkflowNode::new(
            "result",
            "demo-worker",
        )])),
        peer: None,
        limits: Default::default(),
    };
    assert!(matches!(
        restored.start_workflow(raw).await,
        Err(RuntimeError::Permission(_))
    ));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
    restored
        .decide_workflow(
            "workflow",
            "result",
            &ticket,
            ApprovalDecision {
                approved: true,
                actor: "operator".into(),
                note: "checked n=7".into(),
            },
        )
        .await
        .unwrap();
    restored.resume_workflow("workflow").await.unwrap();
    idle(&restored, "workflow").await;
    let done = restored.workflow("workflow").await.unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!(done.output("result"), Some(&json!({"done":true})));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    restored.resume_workflow("workflow").await.unwrap();
    idle(&restored, "workflow").await;
    assert_eq!(
        restored.workflow("workflow").await.unwrap().revision,
        done.revision
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn template_api_cannot_submit_its_own_grants_roles_or_invalid_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let p = package();
    let pin = p.reference().unwrap();
    FileSkillStore::new(dir.path().join("skills"))
        .install(p, 0)
        .await
        .unwrap();
    let model = Arc::new(FixtureModel(AtomicUsize::new(0)));
    let host = app(
        config(dir.path(), pin, SkillActivationMode::Active),
        model.clone(),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api", listener.local_addr().unwrap());
    let cloned = host.clone();
    let service = tokio::spawn(async move {
        serve(listener, cloned, "template-operator-token".into())
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{url}/templates"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .get(format!("{url}/templates"))
        .bearer_auth("template-operator-token")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    for body in [
        json!({"id":"untrusted","input":{"goal":"write"},"grant":{"permissions":["all"]}}),
        json!({"id":"untrusted","input":{"goal":"write"},"role":"new-role"}),
    ] {
        assert_eq!(
            client
                .post(format!("{url}/templates/demo/write-once/runs"))
                .bearer_auth("template-operator-token")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    let response = client
        .post(format!("{url}/templates/demo/write-once/runs"))
        .bearer_auth("template-operator-token")
        .json(&json!({"id":"invalid","input":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    assert!(host.workflow("invalid").await.is_err());
    for body in [
        json!({"id":"invalid-preview","input":{}}),
        json!({"id":"invalid-preview","input":{"goal":"write"},"grant":{"permissions":["all"]}}),
    ] {
        assert!(client
            .post(format!("{url}/templates/demo/write-once/preview"))
            .bearer_auth("template-operator-token")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
            .is_client_error());
    }
    let preview: Value = client
        .post(format!("{url}/templates/demo/write-once/preview"))
        .bearer_auth("template-operator-token")
        .json(&template("template-preview"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["dispatch_performed"], false);
    assert_eq!(
        preview["request"]["definition"]["nodes"][0]["task"],
        "demo-worker"
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    assert!(host.workflow("template-preview").await.is_err());
    let response = client
        .post(format!("{url}/workflow-design/launch"))
        .bearer_auth("template-operator-token")
        .json(&json!({"request":preview["request"],"preview_key":preview["preview_key"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    idle(&host, "template-preview").await;
    let state = host.workflow("template-preview").await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Waiting);
    let child = host
        .inspect(&state.node("result").unwrap().child.as_ref().unwrap().run_id)
        .await
        .unwrap();
    assert_eq!(
        child.pending_approval().unwrap().call.arguments,
        json!({"n":7})
    );
    service.abort();
}

#[tokio::test]
async fn a_package_upgrade_cannot_change_the_pending_approved_operation() {
    let dir = tempfile::tempdir().unwrap();
    let p = package();
    let pin = p.reference().unwrap();
    let store = FileSkillStore::new(dir.path().join("skills"));
    store.install(p.clone(), 0).await.unwrap();
    let model = Arc::new(FixtureModel(AtomicUsize::new(0)));
    let writes = Arc::new(AtomicUsize::new(0));
    let host = app(
        config(dir.path(), pin, SkillActivationMode::Active),
        model.clone(),
        writes.clone(),
    )
    .await;
    host.start_skill_template("demo", "write-once", template("old"))
        .await
        .unwrap();
    idle(&host, "old").await;
    let state = host.workflow("old").await.unwrap();
    let child = host
        .inspect(&state.node("result").unwrap().child.as_ref().unwrap().run_id)
        .await
        .unwrap();
    let mut newer = p;
    newer.manifest.version = "1.1.0".into();
    let newpin = newer.reference().unwrap();
    store.install(newer, 1).await.unwrap();
    let changed = app(
        config(dir.path(), newpin, SkillActivationMode::Active),
        model.clone(),
        writes.clone(),
    )
    .await;
    assert!(matches!(
        changed
            .decide_workflow(
                "old",
                "result",
                &child.pending_approval().unwrap().request_id,
                ApprovalDecision {
                    approved: true,
                    actor: "operator".into(),
                    note: "must refuse changed package".into()
                }
            )
            .await,
        Err(RuntimeError::Configuration(_))
    ));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
}
