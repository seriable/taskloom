//! A typed task on an offline transport: run with
//! `cargo run -p taskloom --example basic`.

use std::borrow::Cow;

use serde::Deserialize;
use taskloom::{
    Agent, AgentTask, Answer, Environment, FnTransport, Protocol, Rejection, Request, TaskError,
    TaskSpec,
};

/// Titles a paragraph.
struct Title<'a> {
    paragraph: &'a str,
}

#[derive(Debug, Deserialize)]
struct TitleAnswer {
    title: String,
}

impl AgentTask for Title<'_> {
    type Output = String;

    fn label(&self) -> &str {
        "title"
    }

    fn spec(&self) -> TaskSpec {
        // A cheap model first, a stronger one if its answer is refused.
        TaskSpec::new("small-model", Protocol::ResilientJson, 0.2, 200).then("large-model")
    }

    fn system_prompt(&self) -> Cow<'_, str> {
        Cow::Borrowed(r#"Title the paragraph. Answer {"title": "..."}."#)
    }

    fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError> {
        if self.paragraph.trim().is_empty() {
            return Err(TaskError::Misconfigured("nothing to title".into()));
        }
        Ok(Cow::Borrowed(self.paragraph))
    }

    fn decode(&self, answer: Answer) -> Result<String, Rejection> {
        let answer: TitleAnswer = taskloom::decode_json(answer)?;
        let title = answer.title.trim();
        // Validation is part of decoding: a refused answer moves on to the
        // next model.
        if title.is_empty() || title.chars().count() > 60 {
            return Err(Rejection::new("the title must be 1 to 60 characters"));
        }
        Ok(title.to_string())
    }
}

#[tokio::main]
async fn main() {
    // The small model rambles; the large one answers well.
    let transport = FnTransport(|request: Request<'_>| {
        let title = if request.model == "small-model" {
            "A".repeat(80)
        } else {
            "Rust and reliable LLM pipelines".to_string()
        };
        Ok(Answer::Json(serde_json::json!({ "title": title })))
    });
    let environment = Environment::new();
    let agent = Agent::new(&transport, &environment);
    let outcome = agent
        .run_traced(&Title {
            paragraph: "taskloom runs typed model tasks with fallbacks and replay.",
        })
        .await
        .expect("a title");
    println!(
        "{} (by {}, after {} model(s))",
        outcome.output, outcome.model, outcome.models_tried
    );
}
