//! Evaluating a change (a prompt, a model) against recorded answers.
//!
//! A recording ([`crate::Recording::Record`]) keeps every answer with the
//! request it answers. [`load_cases`] reads them back as cases; [`evaluate`]
//! runs a variant of each request (another system prompt, user prompt or
//! model — whatever the variant function returns) and asks a [`Judge`]
//! whether the new answer is better, worse or the same as the recorded one.
//!
//! - [`Unchanged`]: the new answer must equal the recorded one (a
//!   regression check for refactors that should not change outputs).
//! - [`ModelJudge`]: a model compares the two answers against criteria, in
//!   both orders across cases, to offset position bias.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::environment::RecordedRequest;
use crate::task::{Answer, Protocol, TaskSpec};
use crate::{Agent, AgentTask, Rejection, TaskError};

/// One recorded request and its answer.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedCase {
    pub file: PathBuf,
    pub request: RecordedRequest,
    pub answer: Answer,
}

/// Reads every recording with its request under `dir` (one folder per
/// label), sorted by label and file.
pub fn load_cases(dir: &Path) -> Vec<RecordedCase> {
    let mut cases = Vec::new();
    let Ok(folders) = std::fs::read_dir(dir) else {
        return cases;
    };
    for folder in folders.flatten() {
        let Ok(files) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(record) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            else {
                continue;
            };
            let Some(request) = record.get("request").and_then(|request| {
                serde_json::from_value::<RecordedRequest>(request.clone()).ok()
            }) else {
                continue;
            };
            if let Some(answer) = Answer::from_record(&record) {
                cases.push(RecordedCase {
                    file: path,
                    request,
                    answer,
                });
            }
        }
    }
    cases.sort_by(|a, b| (&a.request.label, &a.file).cmp(&(&b.request.label, &b.file)));
    cases
}

/// What a variant sends instead of the recorded request.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
}

impl Candidate {
    /// The recorded request unchanged (change only what you vary).
    pub fn from_case(case: &RecordedCase) -> Self {
        Self {
            model: case.request.model.clone(),
            system_prompt: case.request.system_prompt.clone(),
            user_prompt: case.request.user_prompt.clone(),
        }
    }
}

/// A judge's reading of a candidate answer against the recorded one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// From -1 (the candidate is worse) through 0 (as good) to 1 (better).
    pub score: f64,
    pub note: String,
}

/// Compares a candidate answer with the recorded one.
pub trait Judge: Send + Sync {
    fn judge<'a>(
        &'a self,
        case: &'a RecordedCase,
        baseline: &'a Answer,
        candidate: &'a Answer,
    ) -> BoxFuture<'a, Result<Verdict, TaskError>>;
}

/// The candidate must answer exactly as recorded: equal is 0, different -1.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unchanged;

impl Judge for Unchanged {
    fn judge<'a>(
        &'a self,
        _case: &'a RecordedCase,
        baseline: &'a Answer,
        candidate: &'a Answer,
    ) -> BoxFuture<'a, Result<Verdict, TaskError>> {
        let same = baseline == candidate;
        Box::pin(async move {
            Ok(Verdict {
                score: if same { 0.0 } else { -1.0 },
                note: if same { "unchanged" } else { "changed" }.to_string(),
            })
        })
    }
}

/// A model compares the two answers against `criteria`.
pub struct ModelJudge<'a> {
    agent: Agent<'a>,
    model: String,
    criteria: String,
}

impl<'a> ModelJudge<'a> {
    pub fn new(agent: Agent<'a>, model: impl Into<String>, criteria: impl Into<String>) -> Self {
        Self {
            agent,
            model: model.into(),
            criteria: criteria.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Preference {
    winner: String,
    #[serde(default)]
    reason: String,
}

const JUDGE_SYSTEM: &str = "You compare two answers to the same request. Judge only by the criteria given. Answer with one JSON object: {\"winner\": \"A\" | \"B\" | \"tie\", \"reason\": \"one sentence\"}.";

fn clipped(text: &str, limit: usize) -> Cow<'_, str> {
    if text.chars().count() <= limit {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(format!(
            "{} …",
            text.chars().take(limit).collect::<String>()
        ))
    }
}

fn answer_text(answer: &Answer) -> String {
    match answer {
        Answer::Json(value) => value.to_string(),
        Answer::Text(text) => text.clone(),
    }
}

impl Judge for ModelJudge<'_> {
    fn judge<'b>(
        &'b self,
        case: &'b RecordedCase,
        baseline: &'b Answer,
        candidate: &'b Answer,
    ) -> BoxFuture<'b, Result<Verdict, TaskError>> {
        Box::pin(async move {
            // Alternate the order across cases against position bias.
            let candidate_first = case.file.to_string_lossy().len() % 2 == 1;
            let (a, b) = if candidate_first {
                (candidate, baseline)
            } else {
                (baseline, candidate)
            };
            let user = format!(
                "Criteria:\n{}\n\nThe request's instructions:\n{}\n\nThe request:\n{}\n\nAnswer A:\n{}\n\nAnswer B:\n{}",
                self.criteria,
                clipped(&case.request.system_prompt, 4_000),
                clipped(&case.request.user_prompt, 8_000),
                clipped(&answer_text(a), 8_000),
                clipped(&answer_text(b), 8_000),
            );
            let task = crate::PromptTask::new(
                "taskloom_eval_judge",
                TaskSpec::new(self.model.as_str(), Protocol::ResilientJson, 0.0, 400),
                JUDGE_SYSTEM,
                user,
                |answer: Answer| crate::decode_json::<Preference>(answer),
            );
            let preference = self.agent.run(&task).await?;
            let winner = preference.winner.trim().to_ascii_uppercase();
            let score = match (winner.as_str(), candidate_first) {
                ("A", true) | ("B", false) => 1.0,
                ("A", false) | ("B", true) => -1.0,
                _ => 0.0,
            };
            Ok(Verdict {
                score,
                note: preference.reason,
            })
        })
    }
}

/// The outcome of one case.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    pub label: String,
    pub file: PathBuf,
    /// `None` when the variant skipped the case or it failed.
    pub verdict: Option<Verdict>,
    pub candidate: Option<Answer>,
    pub error: Option<String>,
}

/// The outcome of an evaluation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    pub cases: Vec<CaseResult>,
    pub better: usize,
    pub worse: usize,
    pub ties: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Mean score of the judged cases (0 when none).
    pub mean_score: f64,
}

/// Runs the candidate of a case as a task.
struct CandidateTask<'c> {
    case: &'c RecordedCase,
    candidate: Candidate,
}

impl AgentTask for CandidateTask<'_> {
    type Output = Answer;

    fn label(&self) -> &str {
        &self.case.request.label
    }

    fn spec(&self) -> TaskSpec {
        let protocol = if self.case.request.format == "text" {
            Protocol::Text
        } else {
            Protocol::Json
        };
        let spec = TaskSpec::new(
            self.candidate.model.as_str(),
            protocol,
            self.case.request.temperature,
            self.case.request.max_output_tokens,
        );
        match &self.case.request.schema {
            Some(schema) => spec.schema(schema.clone()),
            None => spec,
        }
    }

    fn system_prompt(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.candidate.system_prompt)
    }

    fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError> {
        Ok(Cow::Borrowed(&self.candidate.user_prompt))
    }

    fn images(&self) -> &[PathBuf] {
        &self.case.request.images
    }

    fn decode(&self, answer: Answer) -> Result<Answer, Rejection> {
        Ok(answer)
    }
}

/// Runs `variant` of each case on `agent` (at most `concurrency` at once)
/// and judges the answers against the recorded ones. A case the variant
/// returns `None` for is skipped.
pub async fn evaluate(
    agent: &Agent<'_>,
    cases: &[RecordedCase],
    variant: impl Fn(&RecordedCase) -> Option<Candidate>,
    judge: &dyn Judge,
    concurrency: usize,
) -> Report {
    let jobs = cases.iter().map(|case| (case, variant(case)));
    let results = futures_util::stream::iter(jobs)
        .map(|(case, candidate)| async move {
            let mut result = CaseResult {
                label: case.request.label.clone(),
                file: case.file.clone(),
                verdict: None,
                candidate: None,
                error: None,
            };
            let Some(candidate) = candidate else {
                return result;
            };
            let answer = match agent.run(&CandidateTask { case, candidate }).await {
                Ok(answer) => answer,
                Err(error) => {
                    result.error = Some(error.to_string());
                    return result;
                }
            };
            match judge.judge(case, &case.answer, &answer).await {
                Ok(verdict) => result.verdict = Some(verdict),
                Err(error) => result.error = Some(format!("judge: {error}")),
            }
            result.candidate = Some(answer);
            result
        })
        .buffered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await;
    let mut report = Report::default();
    let mut total = 0.0;
    for result in &results {
        match (&result.verdict, &result.error) {
            (Some(verdict), _) => {
                total += verdict.score;
                if verdict.score > 0.0 {
                    report.better += 1;
                } else if verdict.score < 0.0 {
                    report.worse += 1;
                } else {
                    report.ties += 1;
                }
            }
            (None, Some(_)) => report.failed += 1,
            (None, None) => report.skipped += 1,
        }
    }
    let judged = report.better + report.worse + report.ties;
    report.mean_score = if judged == 0 {
        0.0
    } else {
        total / judged as f64
    };
    report.cases = results;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Environment, FnTransport, PromptTask, Recording, Request};

    #[tokio::test]
    async fn a_prompt_change_is_judged_against_the_recording() {
        let dir = std::env::temp_dir().join(format!("taskloom-eval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The "provider" titles in upper case when the system prompt says so.
        let transport = FnTransport(|request: Request<'_>| {
            let title = if request.system_prompt.contains("SHOUT") {
                request.user_prompt.to_uppercase()
            } else {
                request.user_prompt.to_string()
            };
            Ok(Answer::Json(serde_json::json!({ "title": title })))
        });
        let recording = Environment::new().with_recording(Recording::Record(dir.clone()));
        for text in ["one", "two"] {
            let task = PromptTask::new(
                "title",
                TaskSpec::new("m", Protocol::Json, 0.0, 50),
                "Title it.",
                text,
                crate::json_value,
            );
            Agent::new(&transport, &recording).run(&task).await.unwrap();
        }
        let cases = load_cases(&dir);
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].request.system_prompt, "Title it.");

        let fresh = Environment::new();
        let agent = Agent::new(&transport, &fresh);
        let same = evaluate(
            &agent,
            &cases,
            |case| Some(Candidate::from_case(case)),
            &Unchanged,
            2,
        )
        .await;
        assert_eq!((same.ties, same.worse), (2, 0));

        let shout = evaluate(
            &agent,
            &cases,
            |case| {
                (case.request.user_prompt == "one").then(|| Candidate {
                    system_prompt: "Title it. SHOUT.".into(),
                    ..Candidate::from_case(case)
                })
            },
            &Unchanged,
            2,
        )
        .await;
        assert_eq!((shout.worse, shout.skipped), (1, 1));
        assert_eq!(shout.mean_score, -1.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_model_judge_maps_its_preference_back_through_the_order() {
        // The judge always prefers the answer holding "better".
        let transport = FnTransport(|request: Request<'_>| {
            let a_start = request.user_prompt.find("Answer A:").unwrap();
            let b_start = request.user_prompt.find("Answer B:").unwrap();
            let a = &request.user_prompt[a_start..b_start];
            let winner = if a.contains("better") { "A" } else { "B" };
            Ok(Answer::Json(
                serde_json::json!({ "winner": winner, "reason": "r" }),
            ))
        });
        let environment = Environment::new();
        let judge = ModelJudge::new(Agent::new(&transport, &environment), "judge", "quality");
        for file in ["a.json", "ab.json"] {
            let case = RecordedCase {
                file: file.into(),
                request: RecordedRequest {
                    label: "t".into(),
                    model: "m".into(),
                    system_prompt: "s".into(),
                    user_prompt: "u".into(),
                    images: Vec::new(),
                    format: "json".into(),
                    temperature: 0.0,
                    max_output_tokens: 10,
                    schema: None,
                },
                answer: Answer::Text("plain".into()),
            };
            let verdict = judge
                .judge(&case, &case.answer, &Answer::Text("better".into()))
                .await
                .unwrap();
            assert_eq!(verdict.score, 1.0, "{file}");
        }
    }
}
