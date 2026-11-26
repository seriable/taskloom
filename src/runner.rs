//! Running a task: model chain, protocol, deadline, hedge, ledger,
//! recording and step cache.

use crate::environment::{Environment, Lookup, RequestKey};
use crate::gateway::ModelError;
use crate::task::{AgentTask, Answer, ModelChoice, Protocol, TaskSpec};
use crate::transport::{Format, ModelResolver, NoRoles, Phase, Request, Response, Transport};
use crate::{Rejection, TaskError};

/// A task's result with how it was obtained.
#[derive(Debug)]
pub struct TaskOutcome<T> {
    pub output: T,
    /// The model whose answer was used.
    pub model: String,
    /// Entries tried in the task's model chain, the one used included.
    /// Internal gateway retries and fallback models are not counted here.
    pub models_tried: usize,
    /// Served from a recording or the step cache, not from the provider.
    pub reused: bool,
    /// Tokens and cost of the requests this run sent (repairs and
    /// fallbacks included; nothing when every answer was reused).
    pub usage: crate::ledger::Usage,
}

static NO_ROLES: NoRoles = NoRoles;

/// What runs tasks: a transport, the model roles and the environment.
///
/// It only borrows them, so building one per call is free: a caller whose
/// transport depends on per-call settings builds the transport and the
/// agent where it runs the task.
#[derive(Clone, Copy)]
pub struct Agent<'a> {
    transport: &'a dyn Transport,
    models: &'a dyn ModelResolver,
    environment: &'a Environment,
    cancel: Option<&'a crate::CancelToken>,
    progress: Option<&'a Progress<'a>>,
}

/// The vectors of an [`Agent::embed`] call, in input order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Embeddings {
    pub vectors: Vec<Vec<f32>>,
    pub model: String,
    /// Inputs served from a recording or the cache.
    pub reused: usize,
    /// Tokens and cost of the request sent (if any).
    pub usage: crate::ledger::Usage,
}

/// A progress callback: `(label, model, text delta)` as a streaming
/// transport receives the answer. Hedged copies report interleaved.
pub type Progress<'a> = dyn Fn(&str, &str, &str) + Send + Sync + 'a;

/// The `error.type` of a failed task.
fn error_type(error: &TaskError) -> &'static str {
    match error {
        TaskError::Misconfigured(_) => "misconfigured",
        TaskError::Provider(error) => error.kind(),
        TaskError::Rejected(_) => "rejected",
        TaskError::Deadline(_) => "deadline",
        TaskError::NotRecorded(_) => "not_recorded",
        TaskError::Cancelled => "cancelled",
        TaskError::Storage { .. } => "storage",
        TaskError::Other(_) => "other",
    }
}

fn vector_of(answer: &Answer) -> Option<Vec<f32>> {
    match answer {
        Answer::Json(serde_json::Value::Array(numbers)) => numbers
            .iter()
            .map(|number| number.as_f64().map(|number| number as f32))
            .collect(),
        _ => None,
    }
}

/// The answer as text for a repair prompt, bounded.
fn previous_text(answer: &Answer) -> String {
    const LIMIT: usize = 6_000;
    let text = match answer {
        Answer::Json(value) => value.to_string(),
        Answer::Text(text) => text.clone(),
    };
    if text.chars().count() <= LIMIT {
        text
    } else {
        let mut cut = text.chars().take(LIMIT).collect::<String>();
        cut.push_str(" …");
        cut
    }
}

impl<'a> Agent<'a> {
    pub fn new(transport: &'a dyn Transport, environment: &'a Environment) -> Self {
        Self {
            transport,
            models: &NO_ROLES,
            environment,
            cancel: None,
            progress: None,
        }
    }

    /// Reports answer text as it streams in (transports that stream; see
    /// [`Progress`]). The final answer is decoded and validated as usual.
    pub fn with_progress(mut self, progress: &'a Progress<'a>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Runs tasks under `token`: cancelling it stops them. Without one, an
    /// ambient token ([`crate::cancel::scope`]) applies.
    pub fn with_cancel(mut self, token: &'a crate::CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Resolves [`ModelChoice::Role`]s through `models`.
    pub fn with_models(mut self, models: &'a dyn ModelResolver) -> Self {
        self.models = models;
        self
    }

    pub fn environment(&self) -> &Environment {
        self.environment
    }

    fn timeout_for(
        &self,
        label: &str,
        explicit: Option<std::time::Duration>,
    ) -> Option<std::time::Duration> {
        match (explicit, self.transport.task_timeout(label)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn storage_key(&self, key: RequestKey) -> RequestKey {
        match self.transport.cache_namespace() {
            Some(namespace) => key.in_namespace(&namespace),
            None => key,
        }
    }

    /// Runs `task` and returns its decoded result.
    pub async fn run<T: AgentTask>(&self, task: &T) -> Result<T::Output, TaskError> {
        self.run_traced(task).await.map(|outcome| outcome.output)
    }

    /// [`Agent::run`], also saying which model answered.
    pub async fn run_traced<T: AgentTask>(
        &self,
        task: &T,
    ) -> Result<TaskOutcome<T::Output>, TaskError> {
        let spec = task.spec();
        let deadline = self.timeout_for(task.label(), spec.deadline);
        // One span per task, named and fielded after the OpenTelemetry GenAI
        // conventions so a `tracing-opentelemetry` layer exports it as is.
        let span = tracing::info_span!(
            "gen_ai.task",
            otel.name = %format!("task {}", task.label()),
            gen_ai.operation.name = "chat",
            taskloom.task = task.label(),
            gen_ai.response.model = tracing::field::Empty,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            taskloom.cost_micros = tracing::field::Empty,
            taskloom.models_tried = tracing::field::Empty,
            taskloom.reused = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        let work = async {
            let (result, usage) = crate::ledger::metered(async {
                match deadline {
                    Some(limit) => tokio::time::timeout(limit, self.ask_models(task, &spec))
                        .await
                        .map_err(|_| TaskError::Deadline(limit))?,
                    None => self.ask_models(task, &spec).await,
                }
            })
            .await;
            result.map(|mut outcome| {
                outcome.usage = usage;
                outcome
            })
        };
        let work = async {
            let result = tracing::Instrument::instrument(work, span.clone()).await;
            match &result {
                Ok(outcome) => {
                    span.record("gen_ai.response.model", outcome.model.as_str());
                    span.record("gen_ai.usage.input_tokens", outcome.usage.input_tokens);
                    span.record("gen_ai.usage.output_tokens", outcome.usage.output_tokens);
                    span.record("taskloom.cost_micros", outcome.usage.cost_micros);
                    span.record("taskloom.models_tried", outcome.models_tried);
                    span.record("taskloom.reused", outcome.reused);
                }
                Err(error) => {
                    span.record("error.type", error_type(error));
                }
            }
            result
        };
        crate::cancel::run_under(self.cancel, work).await
    }

    /// Embeds `inputs` with `model` through the transport. With `cache`
    /// (and a step-cache directory), each input's vector is kept and served
    /// again for the same text; recordings and replay work per input. Inputs
    /// not served from a recording or the cache go out in one request.
    pub async fn embed(
        &self,
        label: &str,
        model: impl Into<ModelChoice>,
        inputs: &[String],
        cache: bool,
    ) -> Result<Embeddings, TaskError> {
        let model = self.resolve(&model.into());
        if model.is_empty() {
            return Err(TaskError::Provider(ModelError::misconfigured(
                self.environment.locale().model_not_configured(),
            )));
        }
        let work = async {
            let mut vectors: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
            let mut keys = Vec::with_capacity(inputs.len());
            let mut pending = Vec::new();
            for (index, input) in inputs.iter().enumerate() {
                let key =
                    self.storage_key(RequestKey::for_payload(label, &model, "embedding", input));
                match self.environment.lookup(&key, cache).await {
                    Lookup::Found(response) => {
                        vectors[index] = Some(vector_of(&response.answer).ok_or_else(|| {
                            TaskError::Rejected(Rejection::new(
                                "stored embedding is not a numeric vector",
                            ))
                        })?);
                    }
                    Lookup::MissingInReplay => return Err(TaskError::NotRecorded(key.describe())),
                    Lookup::Ask => {}
                }
                if vectors[index].is_none() {
                    pending.push(index);
                }
                keys.push(key);
            }
            let reused = inputs.len() - pending.len();
            let (sent, usage) = crate::ledger::metered(async {
                if pending.is_empty() {
                    return Ok(Vec::new());
                }
                let texts = pending
                    .iter()
                    .map(|index| inputs[*index].clone())
                    .collect::<Vec<_>>();
                let options = crate::TransportOptions::default();
                let request = crate::transport::EmbedRequest {
                    label,
                    model: &model,
                    inputs: &texts,
                    options: &options,
                };
                crate::ledger::timed_call(label, self.transport.embed(request))
                    .await
                    .map(|embedded| embedded.vectors)
            })
            .await;
            let sent = sent?;
            if sent.len() != pending.len() {
                return Err(TaskError::Rejected(Rejection::new(format!(
                    "expected {} embeddings, got {}",
                    pending.len(),
                    sent.len()
                ))));
            }
            let mut dimension = None;
            for vector in vectors.iter().flatten().chain(sent.iter()) {
                if vector.is_empty() || vector.iter().any(|value| !value.is_finite()) {
                    return Err(TaskError::Rejected(Rejection::new(
                        "an embedding must be nonempty and finite",
                    )));
                }
                if dimension.is_some_and(|expected| vector.len() != expected) {
                    return Err(TaskError::Rejected(Rejection::new(
                        "embedding dimensions do not match",
                    )));
                }
                dimension = Some(vector.len());
            }
            for (index, vector) in pending.into_iter().zip(sent) {
                let answer = Answer::Json(serde_json::json!(vector));
                self.environment
                    .record_payload(&keys[index], &answer)
                    .await?;
                if cache {
                    self.environment.cache(&keys[index], &answer).await;
                }
                vectors[index] = Some(vector);
            }
            Ok(Embeddings {
                vectors: vectors.into_iter().map(Option::unwrap_or_default).collect(),
                model: model.clone(),
                reused,
                usage,
            })
        };
        let work = async {
            match self.timeout_for(label, None) {
                Some(limit) => tokio::time::timeout(limit, work)
                    .await
                    .map_err(|_| TaskError::Deadline(limit))?,
                None => work.await,
            }
        };
        crate::cancel::run_under(self.cancel, work).await
    }

    /// The model id of `choice`, or empty when it has none.
    pub fn resolve(&self, choice: &ModelChoice) -> String {
        let role = |role: &str| {
            self.models
                .resolve_role(role)
                .map(|model| model.trim().to_string())
                .filter(|model| !model.is_empty())
        };
        match choice {
            ModelChoice::Named(model) => model.trim().to_string(),
            ModelChoice::Role(name) => role(name).unwrap_or_default(),
            ModelChoice::RoleOr(name, fallback) => {
                role(name).unwrap_or_else(|| fallback.trim().to_string())
            }
        }
    }

    async fn ask_models<T: AgentTask>(
        &self,
        task: &T,
        spec: &TaskSpec,
    ) -> Result<TaskOutcome<T::Output>, TaskError> {
        let label = task.label();
        let system_prompt = task.system_prompt();
        let user_prompt = task.user_prompt()?;
        let mut models = Vec::<String>::new();
        for choice in &spec.models {
            let model = self.resolve(choice);
            if !model.is_empty() && !models.contains(&model) {
                models.push(model);
            }
        }
        if models.is_empty() {
            return Err(TaskError::Provider(ModelError::misconfigured(
                self.environment.locale().model_not_configured(),
            )));
        }
        let locale = self.environment.locale();
        let mut last_error = None;
        'models: for (index, model) in models.iter().enumerate() {
            let has_next = index + 1 < models.len();
            let forward = |delta: &str| {
                if let Some(progress) = self.progress {
                    progress(label, model, delta);
                }
            };
            let on_delta: Option<&(dyn Fn(&str) + Send + Sync)> = self
                .progress
                .map(|_| &forward as &(dyn Fn(&str) + Send + Sync));
            let request = Request {
                label,
                model,
                system_prompt: &system_prompt,
                user_prompt: &user_prompt,
                images: task.images(),
                attachment: spec.attachment.as_ref(),
                format: match spec.protocol {
                    Protocol::Text => Format::Text,
                    Protocol::Json | Protocol::ResilientJson => Format::Json,
                },
                phase: Phase::Single,
                temperature: spec.temperature,
                max_output_tokens: spec.max_output_tokens,
                schema: spec.schema.as_ref(),
                options: &spec.options,
                on_delta,
            };
            let key = self.storage_key(RequestKey::for_request(&request, spec.protocol));
            let (mut answer, mut reused) = match self
                .obtain(request, &key, spec, spec.protocol)
                .await
            {
                Ok(obtained) => obtained,
                Err(error @ TaskError::NotRecorded(_)) => return Err(error),
                Err(
                    error @ (TaskError::Deadline(_)
                    | TaskError::Cancelled
                    | TaskError::Storage { .. }
                    | TaskError::Provider(ModelError::Deadline { .. })),
                ) => return Err(error),
                Err(error) => {
                    if has_next {
                        tracing::warn!(task = label, model, %error, "agent task failed on this model; trying the next");
                    }
                    last_error = Some(error);
                    continue;
                }
            };
            let mut all_reused = reused;
            let mut round = 0u32;
            loop {
                let fresh = (spec.step_cache && !all_reused).then(|| answer.clone());
                let previous = answer.clone();
                match task.decode(answer.answer) {
                    Ok(output) => {
                        if let Some(answer) = fresh {
                            self.environment.cache_response(&key, &answer).await;
                        }
                        return Ok(TaskOutcome {
                            output,
                            model: previous.model,
                            models_tried: index + 1,
                            reused: all_reused,
                            usage: crate::ledger::Usage::default(),
                        });
                    }
                    Err(rejection) if round < spec.repair_rounds => {
                        round += 1;
                        tracing::warn!(task = label, model, round, reason = %rejection, "agent task answer rejected; asking the model to repair it");
                        let repair_prompt = format!(
                            "{user_prompt}\n\n{}",
                            locale.repair(&rejection.0, &previous_text(&previous.answer))
                        );
                        let repair = Request {
                            model: &previous.model,
                            user_prompt: &repair_prompt,
                            phase: Phase::Repair { round },
                            ..request
                        };
                        let repair_key =
                            self.storage_key(RequestKey::for_request(&repair, spec.protocol));
                        let protocol = match spec.protocol {
                            Protocol::ResilientJson => Protocol::Json,
                            other => other,
                        };
                        match self.obtain(repair, &repair_key, spec, protocol).await {
                            Ok((repaired, was_reused)) => {
                                answer = repaired;
                                reused = was_reused;
                                all_reused = all_reused && reused;
                            }
                            Err(error @ TaskError::NotRecorded(_)) => return Err(error),
                            Err(
                                error @ (TaskError::Deadline(_)
                                | TaskError::Cancelled
                                | TaskError::Storage { .. }
                                | TaskError::Provider(ModelError::Deadline { .. })),
                            ) => return Err(error),
                            Err(error) => {
                                last_error = Some(error);
                                continue 'models;
                            }
                        }
                    }
                    Err(rejection) => {
                        if has_next {
                            tracing::warn!(task = label, model, reason = %rejection, "agent task answer rejected; trying the next model");
                        }
                        last_error = Some(TaskError::Rejected(rejection));
                        continue 'models;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| TaskError::Rejected(Rejection::new("no model answered"))))
    }

    /// The answer to `request`: from a recording or the step cache, else
    /// from the transport (recorded in recording mode, timed in the ledger).
    async fn obtain(
        &self,
        request: Request<'_>,
        key: &RequestKey,
        spec: &TaskSpec,
        protocol: Protocol,
    ) -> Result<(Response, bool), TaskError> {
        match self.environment.lookup(key, spec.step_cache).await {
            Lookup::Found(answer) => Ok((answer, true)),
            Lookup::MissingInReplay => Err(TaskError::NotRecorded(key.describe())),
            Lookup::Ask => {
                let sent = self.send(request, spec, protocol);
                let sent = if spec.ledger {
                    crate::ledger::timed_call(request.label, sent).await
                } else {
                    sent.await
                };
                let answer = sent?;
                self.environment.record(key, &answer, &request).await?;
                Ok((answer, false))
            }
        }
    }

    /// One model's answer under the task's protocol, hedged when asked.
    async fn send(
        &self,
        request: Request<'_>,
        spec: &TaskSpec,
        protocol: Protocol,
    ) -> Result<Response, TaskError> {
        let once = || self.send_once(request, protocol);
        match spec.hedge {
            Some(hedge) => crate::hedge::hedged(hedge.after, hedge.copies, once).await,
            None => once().await,
        }
    }

    async fn send_once(
        &self,
        request: Request<'_>,
        protocol: Protocol,
    ) -> Result<Response, TaskError> {
        match protocol {
            Protocol::Json | Protocol::Text => self.transport.send_with_metadata(request).await,
            Protocol::ResilientJson => self.resilient_json(request).await,
        }
    }

    /// [`Protocol::ResilientJson`]: a structured request, asked once more
    /// with another seed after an unusable answer; a provider that keeps
    /// failing the structured protocol gets one plain-text turn whose JSON
    /// object is recovered locally. Context overflow, auth, configuration
    /// and content rejection fail at once: the text protocol would fail the
    /// same way.
    async fn resilient_json(&self, base: Request<'_>) -> Result<Response, TaskError> {
        let label = base.label;
        // Every such request passes here: its size in logs shows which
        // requests are big enough to stall a provider.
        tracing::info!(
            task_kind = label,
            model = base.model,
            system_chars = base.system_prompt.chars().count(),
            user_chars = base.user_prompt.chars().count(),
            max_tokens = base.max_output_tokens,
            "resilient JSON agent request"
        );
        let mut last_error: Option<ModelError> = None;
        for attempt in 1..=2u32 {
            let request = Request {
                format: Format::Json,
                phase: Phase::Structured { attempt },
                ..base
            };
            match crate::retry::with_retry_salt(
                attempt - 1,
                self.transport.send_with_metadata(request),
            )
            .await
            {
                Ok(answer) => return Ok(answer),
                Err(TaskError::Provider(error @ ModelError::SchemaMismatch { .. })) => {
                    tracing::warn!(attempt, task_kind = label, error = %error, "agent returned an unusable structured answer; retrying once");
                    last_error = Some(error);
                }
                Err(TaskError::Provider(error @ ModelError::Transient { .. })) => {
                    tracing::warn!(attempt, task_kind = label, error = %error, "structured protocol failed transiently after provider retries; trying the plain-text protocol");
                    last_error = Some(error);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        let user_prompt = format!(
            "{}\n\n{}",
            base.user_prompt,
            self.environment.locale().json_text_fallback()
        );
        let request = Request {
            user_prompt: &user_prompt,
            format: Format::Text,
            phase: Phase::TextFallback,
            ..base
        };
        match self.transport.send_with_metadata(request).await {
            Ok(answer) => {
                let text = answer.answer.into_text().unwrap_or_default();
                crate::json_text::object_in_text(&text)
                    .map(|value| Response { answer: Answer::Json(value), model: answer.model })
                    .map_err(|parse_error| {
                        TaskError::Provider(last_error.unwrap_or_else(|| {
                            ModelError::schema_mismatch(format!(
                                "{label} returned no usable JSON after structured and text fallback: {parse_error}"
                            ))
                        }))
                    })
            }
            Err(TaskError::Provider(error)) => match last_error {
                Some(primary_error) => {
                    let detail = format!(
                        "primary structured request failed: {primary_error}; plain-text fallback failed: {error}"
                    );
                    Err(TaskError::Provider(error.with_detail(detail)))
                }
                None => Err(TaskError::Provider(error)),
            },
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use futures_util::future::BoxFuture;

    use super::*;
    use crate::environment::Recording;
    use crate::task::TransportOptions;

    struct Count {
        label: &'static str,
        models: Vec<ModelChoice>,
        prompt: &'static str,
        protocol: Protocol,
    }

    impl Count {
        fn new(label: &'static str, models: Vec<ModelChoice>, prompt: &'static str) -> Self {
            Self {
                label,
                models,
                prompt,
                protocol: Protocol::ResilientJson,
            }
        }
    }

    impl AgentTask for Count {
        type Output = u64;
        fn label(&self) -> &str {
            self.label
        }
        fn spec(&self) -> TaskSpec {
            let mut spec = TaskSpec::new(self.models[0].clone(), self.protocol, 0.2, 100);
            for model in &self.models[1..] {
                spec = spec.then(model.clone());
            }
            spec
        }
        fn system_prompt(&self) -> Cow<'_, str> {
            Cow::Borrowed("count")
        }
        fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError> {
            Ok(Cow::Borrowed(self.prompt))
        }
        fn decode(&self, answer: Answer) -> Result<u64, Rejection> {
            answer
                .into_json()?
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| Rejection::new("no count"))
        }
    }

    /// Answers from a script, in order, and keeps what it was sent.
    #[derive(Default)]
    struct Scripted {
        answers: Mutex<Vec<Result<Answer, TaskError>>>,
        sent: Mutex<Vec<(String, Format, Phase, String)>>,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn new(answers: Vec<Result<Answer, TaskError>>) -> Self {
            Self {
                answers: Mutex::new(answers),
                ..Self::default()
            }
        }
    }

    impl Transport for Scripted {
        fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.sent.lock().unwrap().push((
                request.model.to_string(),
                request.format,
                request.phase,
                request.user_prompt.to_string(),
            ));
            let next = {
                let mut answers = self.answers.lock().unwrap();
                if answers.is_empty() {
                    Err(TaskError::Provider(ModelError::transient(
                        None,
                        "script ended",
                    )))
                } else {
                    answers.remove(0)
                }
            };
            Box::pin(async move { next })
        }
    }

    fn replay_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("taskloom-replay-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn recorded(dir: &std::path::Path, task: &Count, model: &str, answer: serde_json::Value) {
        let options = TransportOptions::default();
        let request = Request {
            label: task.label,
            model,
            system_prompt: "count",
            user_prompt: task.prompt,
            images: &[],
            attachment: None,
            format: Format::Json,
            phase: Phase::Single,
            temperature: 0.2,
            max_output_tokens: 100,
            schema: None,
            options: &options,
            on_delta: None,
        };
        let key = RequestKey::for_request(&request, task.protocol);
        Environment::write_answer(dir, &key, &Answer::Json(answer)).await;
    }

    #[tokio::test]
    async fn a_recorded_answer_is_decoded_without_the_network() {
        let dir = replay_dir("recorded");
        let environment = Environment::new().with_recording(Recording::Replay(dir.clone()));
        let transport = Scripted::default();
        let task = Count::new("test_count_recorded", vec!["model-a".into()], "one");
        recorded(&dir, &task, "model-a", serde_json::json!({ "count": 3 })).await;
        let outcome = Agent::new(&transport, &environment)
            .run_traced(&task)
            .await
            .unwrap();
        assert_eq!(outcome.output, 3);
        assert_eq!(outcome.model, "model-a");
        assert!(outcome.reused);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_rejected_answer_hands_the_task_to_the_next_model() {
        let environment = Environment::new();
        let transport = Scripted::new(vec![
            Ok(Answer::Json(serde_json::json!({ "wrong": true }))),
            Ok(Answer::Json(serde_json::json!({ "count": 7 }))),
        ]);
        let task = Count::new(
            "test_count_fallback",
            vec!["fast".into(), "main".into()],
            "two",
        );
        let outcome = Agent::new(&transport, &environment)
            .run_traced(&task)
            .await
            .unwrap();
        assert_eq!(
            (outcome.output, outcome.model.as_str(), outcome.models_tried),
            (7, "main", 2)
        );
    }

    #[tokio::test]
    async fn the_same_model_twice_is_asked_once() {
        let environment = Environment::new();
        let transport = Scripted::new(vec![Ok(Answer::Json(serde_json::json!({ "wrong": 1 })))]);
        let roles = std::collections::HashMap::from([("fast".to_string(), "same".to_string())]);
        let task = Count::new(
            "test_count_duplicate",
            vec!["same".into(), ModelChoice::role_or("fast", "other")],
            "three",
        );
        let error = Agent::new(&transport, &environment)
            .with_models(&roles)
            .run(&task)
            .await
            .unwrap_err();
        assert!(matches!(error, TaskError::Rejected(_)), "{error}");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn replay_without_a_recording_fails_instead_of_asking() {
        let environment =
            Environment::new().with_recording(Recording::Replay(replay_dir("missing")));
        let transport = Scripted::default();
        let task = Count::new(
            "test_count_missing",
            vec!["model-a".into()],
            "never recorded",
        );
        let error = Agent::new(&transport, &environment)
            .run(&task)
            .await
            .unwrap_err();
        assert!(matches!(error, TaskError::NotRecorded(_)), "{error}");
    }

    #[tokio::test]
    async fn no_model_configured_is_a_provider_misconfiguration_in_the_locale() {
        let environment = Environment::new().with_locale(crate::Locale::Chinese);
        let transport = Scripted::default();
        let task = Count::new(
            "test_count_unconfigured",
            vec![ModelChoice::Named("  ".into()), ModelChoice::role("writer")],
            "four",
        );
        let error = Agent::new(&transport, &environment)
            .run(&task)
            .await
            .unwrap_err();
        match error {
            TaskError::Provider(ModelError::Misconfigured { detail }) => {
                assert_eq!(detail, "请先在设置中配置模型、API 地址和密钥")
            }
            other => panic!("{other}"),
        }
    }

    #[tokio::test]
    async fn resilient_json_retries_a_bad_answer_then_falls_back_to_text() {
        let environment = Environment::new();
        let transport = Scripted::new(vec![
            Err(TaskError::Provider(ModelError::schema_mismatch("bad"))),
            Err(TaskError::Provider(ModelError::transient(
                Some(502),
                "down",
            ))),
            Ok(Answer::Text("ok:\n```json\n{\"count\": 5}\n```".into())),
        ]);
        let task = Count::new("test_count_resilient", vec!["m".into()], "five");
        let output = Agent::new(&transport, &environment)
            .run(&task)
            .await
            .unwrap();
        assert_eq!(output, 5);
        let sent = transport.sent.lock().unwrap();
        let phases = sent
            .iter()
            .map(|(_, format, phase, _)| (*format, *phase))
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            vec![
                (Format::Json, Phase::Structured { attempt: 1 }),
                (Format::Json, Phase::Structured { attempt: 2 }),
                (Format::Text, Phase::TextFallback),
            ]
        );
        assert!(sent[2]
            .3
            .starts_with("five\n\nStructured output is unavailable."));
    }

    #[tokio::test]
    async fn resilient_json_fails_at_once_on_context_overflow() {
        let environment = Environment::new();
        let transport = Scripted::new(vec![Err(TaskError::Provider(
            ModelError::ContextOverflow {
                required_tokens: None,
                limit_tokens: None,
                detail: "too big".into(),
            },
        ))]);
        let task = Count::new("test_count_overflow", vec!["m".into()], "six");
        let error = Agent::new(&transport, &environment)
            .run(&task)
            .await
            .unwrap_err();
        assert!(error.is_context_overflow(), "{error}");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_deadline_bounds_the_whole_task() {
        struct Slow;
        impl Transport for Slow {
            fn send<'a>(&'a self, _: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    Ok(Answer::Json(serde_json::json!({ "count": 1 })))
                })
            }
        }
        struct Bounded(Count);
        impl AgentTask for Bounded {
            type Output = u64;
            fn label(&self) -> &str {
                self.0.label()
            }
            fn spec(&self) -> TaskSpec {
                self.0.spec().deadline(std::time::Duration::from_millis(20))
            }
            fn system_prompt(&self) -> Cow<'_, str> {
                self.0.system_prompt()
            }
            fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError> {
                self.0.user_prompt()
            }
            fn decode(&self, answer: Answer) -> Result<u64, Rejection> {
                self.0.decode(answer)
            }
        }
        let environment = Environment::new();
        let mut inner = Count::new("test_count_deadline", vec!["m".into()], "seven");
        inner.protocol = Protocol::Json;
        let error = Agent::new(&Slow, &environment)
            .run(&Bounded(inner))
            .await
            .unwrap_err();
        assert!(matches!(error, TaskError::Deadline(_)), "{error}");
    }

    #[tokio::test]
    async fn a_cancelled_run_stops_at_once() {
        struct Never;
        impl Transport for Never {
            fn send<'a>(&'a self, _: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
                Box::pin(std::future::pending())
            }
        }
        let environment = Environment::new();
        let token = crate::CancelToken::new();
        let mut task = Count::new("test_count_cancel", vec!["m".into()], "eight");
        task.protocol = Protocol::Json;
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            canceller.cancel();
        });
        let error = Agent::new(&Never, &environment)
            .with_cancel(&token)
            .run(&task)
            .await
            .unwrap_err();
        assert!(matches!(error, TaskError::Cancelled), "{error}");
        // An already cancelled token refuses before sending.
        let transport = Scripted::default();
        let error = Agent::new(&transport, &environment)
            .with_cancel(&token)
            .run(&task)
            .await
            .unwrap_err();
        assert!(matches!(error, TaskError::Cancelled), "{error}");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_rejected_answer_is_repaired_by_the_same_model() {
        struct Repairable(Count);
        impl AgentTask for Repairable {
            type Output = u64;
            fn label(&self) -> &str {
                self.0.label()
            }
            fn spec(&self) -> TaskSpec {
                self.0.spec().repair(1)
            }
            fn system_prompt(&self) -> Cow<'_, str> {
                self.0.system_prompt()
            }
            fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError> {
                self.0.user_prompt()
            }
            fn decode(&self, answer: Answer) -> Result<u64, Rejection> {
                self.0.decode(answer)
            }
        }
        let environment = Environment::new();
        let transport = Scripted::new(vec![
            Ok(Answer::Json(serde_json::json!({ "total": 4 }))),
            Ok(Answer::Json(serde_json::json!({ "count": 4 }))),
        ]);
        let mut inner = Count::new(
            "test_count_repair",
            vec!["m".into(), "other".into()],
            "nine",
        );
        inner.protocol = Protocol::Json;
        let outcome = Agent::new(&transport, &environment)
            .run_traced(&Repairable(inner))
            .await
            .unwrap();
        assert_eq!((outcome.output, outcome.model.as_str()), (4, "m"));
        let sent = transport.sent.lock().unwrap();
        assert_eq!(sent[1].2, Phase::Repair { round: 1 });
        assert_eq!(sent[1].0, "m");
        assert!(sent[1].3.contains("no count"), "{}", sent[1].3);
        assert!(sent[1].3.contains("\"total\":4"), "{}", sent[1].3);
    }

    #[tokio::test]
    async fn streamed_text_reaches_the_progress_callback() {
        struct Streams;
        impl Transport for Streams {
            fn send<'a>(
                &'a self,
                request: Request<'a>,
            ) -> BoxFuture<'a, Result<Answer, TaskError>> {
                Box::pin(async move {
                    if let Some(on_delta) = request.on_delta {
                        on_delta("{\"count\":");
                        on_delta(" 2}");
                    }
                    Ok(Answer::Json(serde_json::json!({ "count": 2 })))
                })
            }
        }
        let environment = Environment::new();
        let seen = Mutex::new(String::new());
        let progress = |label: &str, model: &str, delta: &str| {
            assert_eq!((label, model), ("test_count_progress", "m"));
            seen.lock().unwrap().push_str(delta);
        };
        let mut task = Count::new("test_count_progress", vec!["m".into()], "ten");
        task.protocol = Protocol::Json;
        let output = Agent::new(&Streams, &environment)
            .with_progress(&progress)
            .run(&task)
            .await
            .unwrap();
        assert_eq!(output, 2);
        assert_eq!(*seen.lock().unwrap(), "{\"count\": 2}");
    }

    #[tokio::test]
    async fn embeddings_are_cached_per_input() {
        #[derive(Default)]
        struct Embeds(AtomicUsize);
        impl Transport for Embeds {
            fn send<'a>(&'a self, _: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
                Box::pin(async { Err(TaskError::Misconfigured("chat".into())) })
            }
            fn embed<'a>(
                &'a self,
                request: crate::transport::EmbedRequest<'a>,
            ) -> BoxFuture<'a, Result<crate::transport::Embedded, TaskError>> {
                self.0.fetch_add(request.inputs.len(), Ordering::SeqCst);
                let vectors = request
                    .inputs
                    .iter()
                    .map(|input| vec![input.len() as f32])
                    .collect();
                Box::pin(async move { Ok(crate::transport::Embedded { vectors }) })
            }
        }
        let dir = replay_dir("embed-cache");
        let _ = std::fs::remove_dir_all(&dir);
        let environment = Environment::new().with_step_cache_dir(dir.clone());
        let transport = Embeds::default();
        let agent = Agent::new(&transport, &environment);
        let inputs = vec!["a".to_string(), "abc".to_string()];
        let first = agent.embed("test_embed", "e", &inputs, true).await.unwrap();
        assert_eq!(first.vectors, vec![vec![1.0], vec![3.0]]);
        assert_eq!(first.reused, 0);
        let more = vec!["abc".to_string(), "abcd".to_string()];
        let second = agent.embed("test_embed", "e", &more, true).await.unwrap();
        assert_eq!(second.vectors, vec![vec![3.0], vec![4.0]]);
        assert_eq!(second.reused, 1);
        // Only the new input was sent.
        assert_eq!(transport.0.load(Ordering::SeqCst), 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
