//! Record answers once, then re-run the same code offline from them:
//! `cargo run -p taskloom --example record_replay`.

use std::sync::atomic::{AtomicUsize, Ordering};

use taskloom::{
    json_value, Agent, Answer, Environment, FnTransport, PromptTask, Protocol, Recording, TaskSpec,
};

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("taskloom-record-replay-example");
    let _ = std::fs::remove_dir_all(&dir);
    let calls = AtomicUsize::new(0);
    let transport = FnTransport(|_request: taskloom::Request<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Answer::Json(serde_json::json!({ "sentiment": "positive" })))
    });
    let task = PromptTask::new(
        "sentiment",
        TaskSpec::new("model-a", Protocol::Json, 0.0, 50),
        "Classify the sentiment.",
        "What a lovely day!",
        json_value,
    );

    // 1. Record: answers come from the provider and are written to `dir`.
    let recording = Environment::new().with_recording(Recording::Record(dir.clone()));
    let first = Agent::new(&transport, &recording).run(&task).await.unwrap();

    // 2. Replay: nothing is sent; a request without a recording fails.
    let replay = Environment::new().with_recording(Recording::Replay(dir.clone()));
    let again = Agent::new(&transport, &replay).run(&task).await.unwrap();

    assert_eq!(first, again);
    println!(
        "{again} — provider asked {} time(s) for two runs",
        calls.load(Ordering::SeqCst)
    );
    let _ = std::fs::remove_dir_all(dir);
}
