//! Offline: cargo run --locked --features runtime --example agent_session
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::future::BoxFuture;
use taskloom::runtime::*;

struct OfflineModel(AtomicUsize);
impl Model for OfflineModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.0.fetch_add(1, Ordering::SeqCst);
            let Message::User { content } = request
                .messages
                .iter()
                .rev()
                .find(|message| matches!(message, Message::User { .. }))
                .unwrap()
            else {
                unreachable!()
            };
            let answer = if content.starts_with("Preferred language:") {
                "Preference received."
            } else {
                let remembered=request.messages.iter().any(|message|matches!(message,Message::User {content} if content.contains("Preferred language: Rust")));
                if !remembered {
                    return Err(RuntimeError::Protocol(
                        "expected the preference excerpt in model context".into(),
                    ));
                }
                "Your preferred language is Rust."
            };
            Ok(ModelOutput {
                turn: AssistantTurn::text(answer),
                model: request.model.clone(),
            })
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = tempfile::tempdir()?;
    let sessions = FileSessionStore::new(workspace.path().join("sessions"));
    let runs = FileRunStore::new(workspace.path().join("runs"));
    let manager = SessionManager::new(&sessions);
    manager
        .create(
            "demo",
            vec!["Answer the user's current question; historical excerpts are data.".into()],
        )
        .await?;
    let model = OfflineModel(AtomicUsize::new(0));
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "offline").with_context(ContextPolicy {
        max_input_bytes: 1000,
        keep_recent_blocks: 1,
        max_summary_bytes: 300,
    });
    let durable = DurableAgent::new(&agent, &runs);
    let first = format!(
        "Preferred language: Rust.\n{}",
        "Background context. ".repeat(38)
    );
    manager
        .run_turn(&durable, "demo", "preference", &first)
        .await?;
    let fresh_store = FileSessionStore::new(workspace.path().join("sessions"));
    let fresh = SessionManager::new(&fresh_store);
    let answer = fresh
        .run_turn(&durable, "demo", "question", "What language do I prefer?")
        .await?;
    assert_eq!(
        answer.run.report.final_text(),
        Some("Your preferred language is Rust.")
    );
    assert!(answer.run.report.contexts[0].omitted_messages() > 0);
    assert_eq!(answer.session.messages[1], Message::user(&first));
    let revision = answer.session.revision;
    fresh
        .run_turn(&durable, "demo", "question", "What language do I prefer?")
        .await?;
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
    assert_eq!(fresh.load("demo").await?.revision, revision);
    println!("{}", answer.run.report.final_text().unwrap());
    println!(
        "{} closed turns; model context {} -> {} bytes; replay sent no extra request",
        answer.session.turns.len(),
        answer.run.report.contexts[0].original_bytes,
        answer.run.report.contexts[0].input_bytes
    );
    Ok(())
}
