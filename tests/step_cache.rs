//! The step cache: a validated answer is served again only to a task that
//! opted in, and only for the same request.

use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::future::BoxFuture;
use taskloom::{
    json_value, Agent, Answer, Environment, PromptTask, Protocol, Recording, Rejection, Request,
    TaskError, TaskSpec, Transport,
};

/// Answers `{"n": <call number>}` and counts its calls.
#[derive(Default)]
struct Counting(AtomicUsize);

impl Transport for Counting {
    fn send<'a>(&'a self, _: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
        let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(Answer::Json(serde_json::json!({ "n": call }))) })
    }
}

#[tokio::test]
async fn sampling_and_output_limits_isolate_cached_and_recorded_answers() {
    let dir = std::env::temp_dir().join(format!("taskloom-request-options-{}", std::process::id()));
    let cache_dir = dir.join("cache");
    let record_dir = dir.join("record");
    let environment = Environment::new()
        .with_step_cache_dir(cache_dir)
        .with_recording(Recording::Record(record_dir.clone()));
    let transport = Counting::default();
    let agent = Agent::new(&transport, &environment);
    let configured = |temperature, max_output_tokens| {
        PromptTask::new(
            "sampling",
            TaskSpec::new(
                "cache-model",
                Protocol::Json,
                temperature,
                max_output_tokens,
            )
            .step_cache(true),
            "system",
            "same prompt",
            json_value,
        )
    };
    let settings = [(0.0, 100), (0.7, 100), (0.0, 200), (0.0, 0)];
    for (index, (temperature, max_output_tokens)) in settings.into_iter().enumerate() {
        let task = configured(temperature, max_output_tokens);
        let fresh = agent.run_traced(&task).await.unwrap();
        assert!(!fresh.reused);
        assert_eq!(fresh.output["n"], index + 1);
        let cached = agent.run_traced(&task).await.unwrap();
        assert!(cached.reused);
        assert_eq!(cached.output, fresh.output);
    }
    let calls = transport.0.load(Ordering::SeqCst);
    let replay = Environment::new().with_recording(Recording::Replay(record_dir));
    let agent = Agent::new(&transport, &replay);
    for (index, (temperature, max_output_tokens)) in settings.into_iter().enumerate() {
        let outcome = agent
            .run_traced(&configured(temperature, max_output_tokens))
            .await
            .unwrap();
        assert!(outcome.reused);
        assert_eq!(outcome.output["n"], index + 1);
    }
    assert!(matches!(
        agent.run(&configured(0.9, 100)).await,
        Err(TaskError::NotRecorded(_))
    ));
    assert_eq!(transport.0.load(Ordering::SeqCst), calls);
    std::fs::remove_dir_all(dir).unwrap();
}

type Decode = fn(Answer) -> Result<serde_json::Value, Rejection>;

fn task(step_cache: bool, prompt: &str) -> PromptTask<'_, serde_json::Value, Decode> {
    PromptTask::new(
        "test_step_cache",
        TaskSpec::new("cache-model", Protocol::Json, 0.0, 100).step_cache(step_cache),
        "system",
        prompt,
        json_value as Decode,
    )
}

#[tokio::test]
async fn a_cached_answer_is_served_only_to_a_task_that_opted_in() {
    let dir = std::env::temp_dir().join(format!("taskloom-step-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let environment = Environment::new().with_step_cache_dir(dir.clone());
    let transport = Counting::default();
    let agent = Agent::new(&transport, &environment);

    let first = agent.run_traced(&task(true, "same request")).await.unwrap();
    assert!(!first.reused);
    let again = agent.run_traced(&task(true, "same request")).await.unwrap();
    assert!(again.reused, "served from the step cache");
    assert_eq!(again.output, first.output);
    assert_eq!(transport.0.load(Ordering::SeqCst), 1);

    // Not opted in: the cache is not read.
    let fresh = agent
        .run_traced(&task(false, "same request"))
        .await
        .unwrap();
    assert!(!fresh.reused);
    // Another request is not served the cached answer.
    let other = agent
        .run_traced(&task(true, "another request"))
        .await
        .unwrap();
    assert!(!other.reused);
    assert_eq!(transport.0.load(Ordering::SeqCst), 3);

    // Entries younger than the maximum age survive pruning.
    environment.prune_step_cache();
    assert!(
        agent
            .run_traced(&task(true, "same request"))
            .await
            .unwrap()
            .reused
    );
    let _ = std::fs::remove_dir_all(dir);
}
