//! Judge a prompt change against recorded answers, offline:
//! `cargo run -p taskloom --example eval`

use taskloom::eval::{evaluate, load_cases, Candidate, Unchanged};
use taskloom::{
    json_value, Agent, Answer, Environment, FnTransport, PromptTask, Protocol, Recording, Request,
    TaskSpec,
};

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("taskloom-eval-example");
    let _ = std::fs::remove_dir_all(&dir);
    // A stand-in model: it lowercases titles unless told to keep the case.
    let transport = FnTransport(|request: Request<'_>| {
        let title = if request.system_prompt.contains("Keep the case") {
            request.user_prompt.to_string()
        } else {
            request.user_prompt.to_lowercase()
        };
        Ok(Answer::Json(serde_json::json!({ "title": title })))
    });

    // 1. Production runs recorded their requests and answers.
    let recording = Environment::new().with_recording(Recording::Record(dir.clone()));
    for text in ["Rust Pipelines", "typed tasks", "Replay Everything"] {
        let task = PromptTask::new(
            "title",
            TaskSpec::new("model-a", Protocol::Json, 0.0, 50),
            "Title it.",
            text,
            json_value,
        );
        Agent::new(&transport, &recording).run(&task).await.unwrap();
    }

    // 2. Does a new system prompt change any answer?
    let cases = load_cases(&dir);
    let fresh = Environment::new();
    let agent = Agent::new(&transport, &fresh);
    let report = evaluate(
        &agent,
        &cases,
        |case| {
            Some(Candidate {
                system_prompt: "Title it. Keep the case.".into(),
                ..Candidate::from_case(case)
            })
        },
        &Unchanged,
        4,
    )
    .await;
    println!(
        "{} cases: {} unchanged, {} changed",
        cases.len(),
        report.ties,
        report.worse
    );
    for case in report.cases.iter().filter(|case| {
        case.verdict
            .as_ref()
            .is_some_and(|verdict| verdict.score < 0.0)
    }) {
        println!("  changed: {:?}", case.candidate);
    }
    let _ = std::fs::remove_dir_all(dir);
}
