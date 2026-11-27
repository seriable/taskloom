//! Offline: cargo run --locked --features runtime --example agent_peer
//! Four durable model roles, real file effects, approvals and one rewrite.
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::workflow::*;
use taskloom::runtime::*;

struct OfflineRoles([AtomicUsize; 4]);
impl Model for OfflineRoles {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let role = match &request.messages[0] {
                Message::System { content } => content.as_str(),
                _ => return Err(RuntimeError::Protocol("missing role".into())),
            };
            let input = match &request.messages[1] {
                Message::User { content } => serde_json::from_str::<NodeContext>(content)
                    .map_err(|e| RuntimeError::Protocol(e.to_string()))?,
                _ => return Err(RuntimeError::Protocol("missing handoff".into())),
            };
            let (index, turn) = match role {
                "planner" => (
                    0,
                    AssistantTurn::text(
                        json!({"tasks":[
                            {"id":"sources","instructions":"save source notes"},
                            {"id":"findings","instructions":"save findings"}
                        ]})
                        .to_string(),
                    ),
                ),
                "executor" => {
                    let result = request.messages.iter().find_map(|m| match m {
                        Message::Tool { result } => Some(result),
                        _ => None,
                    });
                    let turn = if let Some(result) = result {
                        if result.is_error {
                            return Err(RuntimeError::Protocol("file effect failed".into()));
                        }
                        AssistantTurn::text(result.output.to_string())
                    } else {
                        AssistantTurn {
                            content: String::new(),
                            tool_calls: vec![ToolCall {
                                id: "save-note".into(),
                                name: "save_note".into(),
                                arguments: json!({"note":input.dependencies["task"]["instructions"]}),
                            }],
                        }
                    };
                    (1, turn)
                }
                "writer" => {
                    let revision = input.params["revision"].as_u64().unwrap();
                    assert!(input.dependencies.contains_key("worker-0"));
                    assert!(input.dependencies.contains_key("worker-1"));
                    if revision > 0 {
                        assert!(input.dependencies.contains_key("check-review-0"));
                    }
                    (
                        2,
                        AssistantTurn::text(if revision == 0 {
                            "Draft: two notes saved."
                        } else {
                            "Revised report: source notes and findings saved; evidence checked."
                        }),
                    )
                }
                "reviewer" => {
                    let approved = input.params["revision"] == 1;
                    (3,AssistantTurn::text(json!({"approved":approved,"feedback":if approved {"evidence checked"}else{"name the evidence in the report"}}).to_string()))
                }
                _ => return Err(RuntimeError::Protocol("unknown role".into())),
            };
            self.0[index].fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = tempfile::tempdir()?;
    let runs = FileRunStore::new(workspace.path().join("runs"));
    let workflows = FileWorkflowStore::new(workspace.path().join("workflows"));
    let effects = Arc::new(AtomicUsize::new(0));
    let files = workspace.path().join("notes");
    std::fs::create_dir(&files)?;
    let mut tools = ToolRegistry::new();
    let counter = Arc::clone(&effects);
    tools.register(FnTool::new(ToolDefinition {
        name:"save_note".into(),description:"Create a note file once".into(),
        effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["notes:write".into()]),
        input_schema:json!({"type":"object","properties":{"note":{"type":"string"}},"required":["note"],"additionalProperties":false}),
    },move |args:Value,context:ToolContext| {
        let path=files.join(format!("{}.txt",context.run_id));
        let counter=Arc::clone(&counter);
        async move {
            let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e|ToolError(e.to_string()))?;
            file.write_all(args["note"].as_str().unwrap().as_bytes()).map_err(|e|ToolError(e.to_string()))?;
            file.sync_all().map_err(|e|ToolError(e.to_string()))?;
            counter.fetch_add(1,Ordering::SeqCst);
            Ok(json!({"saved":true,"note":args["note"]}))
        }
    }))?;
    let model = OfflineRoles(std::array::from_fn(|_| AtomicUsize::new(0)));
    let read_tools = ToolRegistry::new();
    let limits = RunLimits {
        max_cost_micros: Some(0),
        ..Default::default()
    };
    let reader = AgentLoop::new(&model, &read_tools, "offline").with_limits(limits.clone());
    let executor = AgentLoop::new(&model, &tools, "offline")
        .with_limits(limits)
        .with_policy(
            ToolPolicy::default()
                .allow("save_note")
                .grant("notes:write"),
        );
    let mut tasks = WorkflowTasks::new();
    for (name, agent, output) in [
        (
            "planner",
            &reader,
            WorkflowOutput::Json {
                schema: PeerPlan::schema(4),
            },
        ),
        ("executor", &executor, WorkflowOutput::Text),
        ("writer", &reader, WorkflowOutput::Text),
        (
            "reviewer",
            &reader,
            WorkflowOutput::Json {
                schema: PeerReview::schema(),
            },
        ),
    ] {
        tasks.register(AgentWorkflowTask::new(name, agent, &runs, name, output)?)?;
    }
    let graph = PeerSpec::default().install(&mut tasks)?;
    let runner = WorkflowRunner::new(&tasks, &workflows).with_limits(WorkflowLimits {
        max_cost_micros: Some(0),
        ..Default::default()
    });
    let waiting = runner
        .start(
            "report",
            graph,
            json!({"goal":"write an evidence-based report"}),
        )
        .await?;
    assert_eq!(waiting.status, WorkflowStatus::Waiting);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    // In a real application the operator reviews each exact ticket in the UI.
    // This offline demonstration explicitly approves its two known local files.
    for node in &waiting.nodes {
        if let Some(child) = &node.child {
            let state = runs.inspect(&child.run_id).await?.unwrap();
            if let Some(ticket) = state.pending_approval() {
                assert_eq!(ticket.call.name, "save_note");
                DurableAgent::new(&executor, &runs)
                    .decide(
                        &child.run_id,
                        &ticket.request_id,
                        ApprovalDecision {
                            approved: true,
                            actor: "demo-operator".into(),
                            note: "reviewed local note creation".into(),
                        },
                    )
                    .await?;
            }
        }
    }
    let done = runner.resume("report").await?;
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!(done.output("result").unwrap()["revision"], 1);
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert_eq!(
        model
            .0
            .iter()
            .map(|n| n.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        vec![1, 4, 2, 2]
    );
    assert_eq!(runner.resume("report").await?.revision, done.revision);
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    println!(
        "{}",
        serde_json::to_string_pretty(done.output("result").unwrap())?
    );
    println!("2 approved file effects; 1 rewrite; completed replay added no model/tool calls");
    Ok(())
}
