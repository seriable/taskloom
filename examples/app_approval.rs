//! Offline operator UI with an explicitly approved real file effect.
//! TASKLOOM_API_TOKEN=... cargo run --features app --example app_approval -- STATE_DIR
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::app::{router, AppConfig, AppRole, Application};
use taskloom::runtime::workflow::{PeerPlan, PeerReview, WorkflowOutput};
use taskloom::runtime::*;

struct ApprovalDemo;
impl Model for ApprovalDemo {
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
            // Explicit fixtures for checking handoffs and reviewer rework in
            // the console. These branches do not claim model reasoning quality.
            let fixture = match system {
                "Offline fixture: planner" => Some(AssistantTurn::text(
                    r#"{"tasks":[{"id":"note","instructions":"Create one reviewed local fixture note."}]}"#,
                )),
                "Offline fixture: writer" | "Offline fixture: reviewer" => {
                    let input = request
                        .messages
                        .iter()
                        .rev()
                        .find_map(|m| match m {
                            Message::User { content } => {
                                serde_json::from_str::<Value>(content).ok()
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            RuntimeError::Protocol("fixture expected a node context".into())
                        })?;
                    Some(if system.ends_with("writer") {
                        AssistantTurn::text(format!(
                            "Offline fixture draft {}: the executor saved the approved local note.",
                            input["params"]["revision"]
                        ))
                    } else {
                        let approved = input["dependencies"]["draft"]
                            .as_str()
                            .is_some_and(|draft| draft.contains("draft 1:"));
                        AssistantTurn::text(json!({"approved":approved,"feedback":if approved {"Fixture accepted."} else {"Fixture requests one rewrite; do not repeat the executor."}}).to_string())
                    })
                }
                _ => None,
            };
            if let Some(turn) = fixture {
                return Ok(ModelOutput {
                    turn,
                    model: request.model.clone(),
                });
            }
            let result = request.messages.iter().find_map(|message| match message {
                Message::Tool { result } => Some(result),
                _ => None,
            });
            let turn = if let Some(result) = result {
                if result.is_error {
                    return Err(RuntimeError::Protocol(
                        "note creation failed; inspect the tool result".into(),
                    ));
                }
                AssistantTurn::text("Offline approval demo: the reviewed note was created once.")
            } else {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "save-note".into(),
                        name: "save_note".into(),
                        arguments: json!({"note":"This local note was reviewed and approved in the taskloom console."}),
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
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("supply an explicit demo state directory")?,
    );
    let address = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "127.0.0.1:8081".into())
        .parse::<std::net::SocketAddr>()?;
    if !address.ip().is_loopback() {
        return Err("demo binds loopback only".into());
    }
    let notes = root.join("notes");
    std::fs::create_dir_all(&notes)?;
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {
        name:"save_note".into(),description:"Create one reviewed note in the explicit demo state directory".into(),
        effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["notes:write".into()]),
        input_schema:json!({"type":"object","properties":{"note":{"type":"string","maxLength":1000}},"required":["note"],"additionalProperties":false}),
    },move |arguments:Value,context:ToolContext| {
        let path=notes.join(format!("{}.txt",context.run_id));
        async move {
            let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e|ToolError(e.to_string()))?;
            file.write_all(arguments["note"].as_str().unwrap().as_bytes()).map_err(|e|ToolError(e.to_string()))?;
            file.sync_all().map_err(|e|ToolError(e.to_string()))?;
            Ok(json!({"created":true}))
        }
    }))?;
    let app = Application::with_model(
        AppConfig {
            state_dir: root,
            knowledge: false,
            model: "offline-approval-demo".into(),
            roles: vec![
                AppRole {
                    name: "planner".into(),
                    system: "Offline fixture: planner".into(),
                    output: WorkflowOutput::Json {
                        schema: PeerPlan::schema(2),
                    },
                    tools: BTreeSet::new(),
                },
                AppRole {
                    name: "executor".into(),
                    system: "Offline fixture: executor".into(),
                    output: WorkflowOutput::Text,
                    tools: BTreeSet::from(["save_note".into()]),
                },
                AppRole {
                    name: "writer".into(),
                    system: "Offline fixture: writer".into(),
                    output: WorkflowOutput::Text,
                    tools: BTreeSet::new(),
                },
                AppRole {
                    name: "reviewer".into(),
                    system: "Offline fixture: reviewer".into(),
                    output: WorkflowOutput::Json {
                        schema: PeerReview::schema(),
                    },
                    tools: BTreeSet::new(),
                },
            ],
            limits: RunLimits {
                max_cost_micros: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        Arc::new(ApprovalDemo),
        tools,
        ToolPolicy::default()
            .allow("save_note")
            .grant("notes:write"),
    )
    .await?;
    let token = std::env::var("TASKLOOM_API_TOKEN")?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!(
        "Offline approval fixture: http://{}; create a run to review a local note write",
        listener.local_addr()?
    );
    let routes = router(Arc::clone(&app), token)?;
    axum::serve(listener, routes)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = app.shutdown(Duration::from_secs(5)).await;
        })
        .await?;
    Ok(())
}
