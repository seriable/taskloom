//! Offline approval/restart demo:
//! cargo run --locked --features runtime --example agent_resume
//!
//! Across separate processes, pass `start|approve|resume|show <workspace>`.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::*;

struct OfflineModel;
impl Model for OfflineModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(0).await?;
            let turn = if request
                .messages
                .iter()
                .any(|message| matches!(message, Message::Tool { .. }))
            {
                AssistantTurn::text("The approved note was saved.")
            } else {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "save-note-1".into(),
                        name: "save_note".into(),
                        arguments: json!({"text":"A durable agent wrote this after approval."}),
                    }],
                }
            };
            reservation.settle(0);
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}

fn registry(root: &Path) -> Result<ToolRegistry, RuntimeError> {
    let note = root.join("note.txt");
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {
        name:"save_note".into(),description:"Create note.txt once inside this example workspace.".into(),
        input_schema:json!({"type":"object","properties":{"text":{"type":"string","maxLength":1000}},"required":["text"],"additionalProperties":false}),
        effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["notes:write".into()]),
    },move |arguments:Value,_:ToolContext| {
        let note = note.clone();
        async move {
            // Blocking file work is off the Tokio worker. create_new refuses
            // to overwrite a preexisting note, including on an unsafe retry.
            tokio::task::spawn_blocking(move || {
                let mut file = std::fs::OpenOptions::new().create_new(true).write(true).open(&note).map_err(|e|ToolError(e.to_string()))?;
                file.write_all(arguments["text"].as_str().unwrap().as_bytes()).and_then(|()|file.sync_all()).map_err(|e|ToolError(e.to_string()))?;
                Ok(json!({"path":note}))
            }).await.map_err(|e|ToolError(e.to_string()))?
        }
    }))?;
    Ok(tools)
}

async fn operation(action: &str, root: &Path) -> Result<RunState, Box<dyn std::error::Error>> {
    let store = FileRunStore::new(root.join("runs"));
    let tools = registry(root)?;
    let agent = AgentLoop::new(&OfflineModel, &tools, "offline").with_policy(
        ToolPolicy::default()
            .allow("save_note")
            .grant("notes:write"),
    );
    let durable = DurableAgent::new(&agent, &store);
    let state = match action {
        "start" => {
            durable
                .start(
                    "approved-note",
                    vec![Message::user(
                        "Save a note after I approve the exact write.",
                    )],
                )
                .await?
        }
        "approve" => {
            let state = store
                .inspect("approved-note")
                .await?
                .ok_or("start the run first")?;
            let ticket = state
                .pending_approval()
                .ok_or("run has no pending approval")?;
            durable
                .decide(
                    "approved-note",
                    &ticket.request_id,
                    ApprovalDecision {
                        approved: true,
                        actor: "example-user".into(),
                        note: "reviewed the note text and destination".into(),
                    },
                )
                .await?
        }
        "resume" => durable.resume("approved-note").await?,
        "show" => store
            .inspect("approved-note")
            .await?
            .ok_or("start the run first")?,
        _ => return Err("action must be start, approve, resume or show".into()),
    };
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(state)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() == 1 {
        let workspace = tempfile::tempdir()?;
        let waiting = operation("start", workspace.path()).await?;
        assert_eq!(waiting.status, RunStatus::WaitingApproval);
        assert!(!workspace.path().join("note.txt").exists());
        operation("approve", workspace.path()).await?;
        let completed = operation("resume", workspace.path()).await?;
        assert_eq!(
            completed.report.final_text(),
            Some("The approved note was saved.")
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("note.txt"))?,
            "A durable agent wrote this after approval."
        );
        assert_eq!(completed.report.tool_calls, 1);
    } else if args.len() == 3 {
        operation(&args[1], Path::new(&args[2])).await?;
    } else {
        return Err("usage: agent_resume [start|approve|resume|show <workspace>]".into());
    }
    Ok(())
}
