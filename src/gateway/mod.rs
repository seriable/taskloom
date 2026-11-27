//! The model gateway.
//!
//! One place that owns how a model call is admitted, sent, retried and
//! classified:
//!
//! 1. **Budget pre-check** – the input is estimated with [`TokenEstimator`]
//!    and refused with [`ModelError::ContextOverflow`] *without sending* when it
//!    exceeds `min(task.input_budget, model.context_window − output)`.
//! 2. **Provider admission** – a per-provider (base URL) concurrency
//!    semaphore and a waiting circuit breaker per (provider, model). A model
//!    whose breaker stays open hands the call to the next fallback model.
//! 3. **Retry** – only [`ModelError::Transient`] is retried, with exponential
//!    backoff from [`exponential_backoff`]; once the attempts are exhausted
//!    the fallback model chain is walked.
//!    `ContextOverflow`, `SchemaMismatch`, `ContentRejected`, `Auth` and
//!    `Misconfigured` are returned to the caller immediately.
//! 4. **Observability** – one structured `ai_gateway` tracing event per
//!    attempt plus [`Gateway::stats`]. A success served by a fallback
//!    model or the secondary text gateway is additionally recorded as
//!    degraded (`degraded = true`, requested vs served model).
//!
//! The transport itself is not here: [`Gateway::execute`] wraps it through
//! an attempt closure, so any HTTP client or provider protocol can use it.

mod control;
mod error;
mod estimator;
mod pricing;
mod profiles;
mod stats;

use std::{
    future::Future,
    time::{Duration, Instant},
};

pub use control::{
    Admission, BreakerState, CircuitBreaker, ControlConfig, ProviderControl,
    DEFAULT_BREAKER_COOLDOWN, DEFAULT_BREAKER_THRESHOLD,
};
pub use error::{ModelError, CIRCUIT_OPEN_MARKER, CONTEXT_OVERFLOW_MARKER};
pub use estimator::{
    is_cjk_token_char, TokenEstimator, DEFAULT_TOKENS_PER_IMAGE, LOW_DETAIL_TOKENS_PER_IMAGE,
};
pub use pricing::{ModelPrice, PriceTable};
pub use profiles::{
    parse_model_overrides, resolve_fallback_chain, BudgetMode, ModelProfile, ModelProfileOverride,
    ModelRegistry, TaskEntry, TaskProfile, TaskRegistry, DEFAULT_INPUT_BUDGET_TOKENS,
    DEFAULT_OUTPUT_TOKENS, DEFAULT_PROVIDER_CONCURRENCY, DEFAULT_TASK_TIMEOUT_SECONDS,
    HARD_INPUT_BUDGET_TOKENS,
};
pub use stats::{GatewayCallStats, TokenUsage};

use crate::locale::Locale;

/// Everything the gateway needs to admit and account for one logical call.
#[derive(Debug, Clone)]
pub struct CallContext {
    pub task: TaskProfile,
    /// Normalized provider base URL; the key for concurrency and breaker.
    pub provider: String,
    pub primary_model: String,
    pub estimated_input_tokens: usize,
    /// Output tokens the caller asked for; `0` means provider default, in
    /// which case the task's declared output is reserved.
    pub requested_output_tokens: usize,
    pub image_count: usize,
    /// Total attempts per model for `Transient` failures (≥ 1).
    pub transient_attempts: usize,
    /// Whether this call may walk the fallback model chain.
    pub allow_fallback: bool,
    /// Wall-clock cap for one attempt, including breaker and permit waits.
    pub attempt_timeout: Option<Duration>,
}

impl CallContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task: TaskProfile,
        provider: &str,
        model: &str,
        system: &str,
        user: &str,
        image_count: usize,
        requested_output_tokens: usize,
        estimator: TokenEstimator,
    ) -> Self {
        let allow_fallback = task.allow_fallback;
        Self {
            task,
            provider: provider.trim().trim_end_matches('/').to_string(),
            primary_model: model.trim().to_string(),
            estimated_input_tokens: estimator.estimate_request(system, user, image_count),
            requested_output_tokens,
            image_count,
            transient_attempts: 2,
            allow_fallback,
            attempt_timeout: None,
        }
    }
}

/// Result of a successful budget check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetCheck {
    pub estimated_input_tokens: usize,
    /// The limit actually applied (task budget when enforced, else window).
    pub limit_tokens: usize,
    /// True when the input exceeds the task budget but it was only observed.
    pub over_task_budget: bool,
}

/// Pure budget decision for one model: `min(task budget, window − output)`
/// when the task budget is enforced, otherwise only the model window.
pub fn check_budget(
    ctx: &CallContext,
    profile: &ModelProfile,
    mode: BudgetMode,
    locale: Locale,
) -> Result<BudgetCheck, ModelError> {
    let requested_output = if ctx.requested_output_tokens > 0 {
        ctx.requested_output_tokens
    } else {
        ctx.task.output_tokens
    };
    let output = requested_output.min(profile.max_output.max(1));
    let window_limit = profile.context_window.saturating_sub(output);
    let enforced = ctx.task.budget_enforced(mode);
    let limit = if enforced {
        ctx.task.input_budget_tokens.min(window_limit)
    } else {
        window_limit
    };
    let estimated = ctx.estimated_input_tokens;
    if estimated > limit {
        return Err(ModelError::ContextOverflow {
            required_tokens: Some(estimated),
            limit_tokens: Some(limit),
            detail: locale.budget_refused(
                &ctx.task.label,
                &profile.id,
                estimated,
                limit,
                ctx.task.input_budget_tokens,
                profile.context_window,
                output,
            ),
        });
    }
    Ok(BudgetCheck {
        estimated_input_tokens: estimated,
        limit_tokens: limit,
        over_task_budget: estimated > ctx.task.input_budget_tokens,
    })
}

/// Value produced by one provider attempt plus the usage it reported.
#[derive(Debug, Clone)]
pub struct AttemptOutput<T> {
    pub value: T,
    pub usage: Option<TokenUsage>,
}

impl<T> AttemptOutput<T> {
    pub fn new(value: T, usage: Option<TokenUsage>) -> Self {
        Self { value, usage }
    }
}

/// Bounded exponential backoff before retry number `attempt` (0-based):
/// `2^attempt` seconds, the exponent capped at 8 and the delay at 300 s.
pub fn exponential_backoff(attempt: u32) -> Duration {
    let bounded_attempt = attempt.min(8);
    Duration::from_secs(2u64.saturating_pow(bounded_attempt).min(300))
}

/// Backoff before retrying a transient failure. Rate limits are account-wide
/// windows, so they wait at least 30 s per attempt.
pub fn transient_backoff(error: &ModelError, attempt: usize) -> Duration {
    let base = exponential_backoff(u32::try_from(attempt).unwrap_or(u32::MAX));
    match error {
        ModelError::Transient {
            status: Some(429), ..
        } => base.max(Duration::from_secs(30 * (attempt as u64 + 1))),
        _ => base,
    }
}

fn circuit_open_error(locale: Locale, provider: &str, model: &str, waited: Duration) -> ModelError {
    ModelError::transient(
        None,
        locale.circuit_open(CIRCUIT_OPEN_MARKER, provider, model, waited.as_secs()),
    )
}

/// How long a queued caller waits for another caller's half-open probe when
/// the call has no attempt timeout of its own.
const DEFAULT_PROBE_WAIT: Duration = Duration::from_secs(180);
/// Slack on top of the probe's own timeout: the probe may still be waiting
/// for a provider permit before it sends.
const PROBE_WAIT_MARGIN: Duration = Duration::from_secs(15);

/// Longest a queued caller waits for an in-flight half-open probe: the
/// probe's own request timeout plus a margin.
fn probe_wait_budget(attempt_timeout: Option<Duration>) -> Duration {
    attempt_timeout.unwrap_or(DEFAULT_PROBE_WAIT) + PROBE_WAIT_MARGIN
}

/// The gateway's configuration.
#[derive(Debug, Clone, Default)]
pub struct GatewayConfig {
    pub tasks: TaskRegistry,
    pub models: ModelRegistry,
    pub budget_mode: BudgetMode,
    pub control: ControlConfig,
    /// Prices for the cost of answered requests (empty: no cost).
    pub prices: PriceTable,
    pub locale: Locale,
    /// Divides every real-time wait (backoff, breaker cool-down waits) — for
    /// tests of retry and breaker behaviour; `0` and `1` wait in real time.
    pub time_divisor: u32,
}

/// Admission, retry, fallback and accounting of model calls.
///
/// One gateway is shared by every caller of the same providers: its
/// breakers and concurrency limits are per provider, so two gateways for
/// one provider would not see each other's failures.
pub struct Gateway {
    config: GatewayConfig,
    controls: control::Controls,
    stats: stats::Stats,
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new(GatewayConfig::default())
    }
}

impl Gateway {
    pub fn new(config: GatewayConfig) -> Self {
        Self {
            controls: control::Controls::new(config.control),
            stats: stats::Stats::default(),
            config,
        }
    }

    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    pub fn locale(&self) -> Locale {
        self.config.locale
    }

    /// The profile of the task labelled `label`.
    pub fn task_profile(&self, label: &str) -> TaskProfile {
        self.config.tasks.profile(label)
    }

    /// The profile of model `model`.
    pub fn model_profile(&self, model: &str) -> ModelProfile {
        self.config.models.profile(model)
    }

    /// The fallback chain of `primary`.
    pub fn fallback_models(&self, primary: &str, needs_images: bool) -> Vec<String> {
        self.config.models.fallback_models(primary, needs_images)
    }

    /// A call context for task `task_label` (see [`CallContext::new`]).
    #[allow(clippy::too_many_arguments)]
    pub fn call_context(
        &self,
        task_label: &str,
        provider: &str,
        model: &str,
        system: &str,
        user: &str,
        image_count: usize,
        requested_output_tokens: usize,
        estimator: TokenEstimator,
    ) -> CallContext {
        CallContext::new(
            self.task_profile(task_label),
            provider,
            model,
            system,
            user,
            image_count,
            requested_output_tokens,
            estimator,
        )
    }

    /// The budget check of `ctx` on `profile` under this gateway's mode.
    pub fn check_budget(
        &self,
        ctx: &CallContext,
        profile: &ModelProfile,
    ) -> Result<BudgetCheck, ModelError> {
        check_budget(ctx, profile, self.config.budget_mode, self.config.locale)
    }

    /// Budget pre-check for the primary model, recording a rejection. Callers
    /// run it before expensive preparation (image uploads).
    pub fn precheck(&self, ctx: &CallContext) -> Result<BudgetCheck, ModelError> {
        let profile = self.model_profile(&ctx.primary_model);
        self.check_budget(ctx, &profile).inspect_err(|error| {
            self.stats.record_budget_rejection(
                &ctx.task.label,
                &profile.id,
                ctx.estimated_input_tokens,
                error_limit(error),
            )
        })
    }

    /// Whether `provider` (a normalized base URL) is blocked by an open
    /// breaker or an in-flight half-open probe right now. Optional calls with
    /// a safe fallback of their own skip the model instead of queuing behind
    /// the breaker.
    pub fn provider_blocked_by_breaker(&self, provider: &str) -> bool {
        self.controls
            .provider_breaker_blocks(provider.trim().trim_end_matches('/'), Instant::now())
    }

    /// Installs a provider with explicit limits (tests, warm-up).
    pub fn install_provider_control(
        &self,
        provider: &str,
        permits: usize,
        breaker: CircuitBreaker,
    ) -> std::sync::Arc<ProviderControl> {
        self.controls.install(provider, permits, breaker)
    }

    /// A snapshot of the call counters.
    pub fn stats(&self) -> GatewayCallStats {
        self.stats.snapshot()
    }

    /// Records a success served by another model or provider than the one
    /// asked for (`route` names how), for transports with routes of their own.
    pub fn record_degraded_success(
        &self,
        task: &str,
        requested_model: &str,
        served_model: &str,
        provider: &str,
        route: &str,
    ) {
        self.stats
            .record_degraded_success(task, requested_model, served_model, provider, route);
    }

    fn scaled(&self, delay: Duration) -> Duration {
        match self.config.time_divisor {
            0 | 1 => delay,
            divisor => delay / divisor,
        }
    }

    /// Wait while the provider breaker is open. Returns whether this caller is
    /// the half-open probe, or a circuit-open `Transient` error.
    ///
    /// Two separate budgets apply. While the breaker is *open* a caller waits at
    /// most one cool-down (+5 s). While another caller's *half-open probe* is in
    /// flight it waits for that probe's outcome, bounded by the probe's own
    /// request timeout: if the probe closes the breaker the caller proceeds, if
    /// the probe re-opens it the caller fails fast instead of sleeping through
    /// another cool-down.
    async fn wait_for_admission(
        &self,
        control: &control::BreakerCell,
        provider: &str,
        model: &str,
        attempt_timeout: Option<Duration>,
    ) -> Result<bool, ModelError> {
        let open_budget = self.scaled(control.cooldown() + Duration::from_secs(5));
        let probe_budget = self.scaled(probe_wait_budget(attempt_timeout));
        let started = Instant::now();
        let mut open_waited = Duration::ZERO;
        let mut probe_waited = Duration::ZERO;
        let mut saw_probe = false;
        let mut outcomes = control.subscribe_outcomes();
        loop {
            match control.admit(Instant::now()) {
                Admission::Allowed { probe } => return Ok(probe),
                Admission::Wait(_) if saw_probe => {
                    // The probe we were waiting on failed and re-opened the
                    // breaker: the provider is still down.
                    return Err(circuit_open_error(
                        self.config.locale,
                        provider,
                        model,
                        started.elapsed(),
                    ));
                }
                Admission::Wait(delay) => {
                    if open_waited >= open_budget {
                        return Err(circuit_open_error(
                            self.config.locale,
                            provider,
                            model,
                            started.elapsed(),
                        ));
                    }
                    let step = self
                        .scaled(delay)
                        .max(Duration::from_millis(1))
                        .min(open_budget - open_waited);
                    tokio::time::sleep(step).await;
                    open_waited += step;
                }
                Admission::AwaitProbe => {
                    saw_probe = true;
                    if probe_waited >= probe_budget {
                        return Err(circuit_open_error(
                            self.config.locale,
                            provider,
                            model,
                            started.elapsed(),
                        ));
                    }
                    let remaining = probe_budget - probe_waited;
                    let wait_started = Instant::now();
                    // A closed channel cannot happen while `control` is alive; a
                    // timeout is handled by the budget check on the next turn.
                    let _ = tokio::time::timeout(remaining, outcomes.changed()).await;
                    probe_waited += wait_started.elapsed().max(Duration::from_millis(1));
                }
            }
        }
    }

    /// Run one logical model call through budget check, provider admission,
    /// transient retry and the fallback chain. `attempt` performs a single
    /// provider request for the given model id.
    pub async fn execute<T, F, Fut>(&self, ctx: &CallContext, attempt: F) -> Result<T, ModelError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<AttemptOutput<T>, ModelError>>,
    {
        self.execute_validated(ctx, attempt, Ok).await
    }

    /// Like [`Gateway::execute`], with response validation inside the
    /// attempt's accounting. Usage is retained even when `validate` rejects
    /// an answered request; retry and breaker decisions use that rejection.
    pub async fn execute_validated<T, U, F, Fut, V>(
        &self,
        ctx: &CallContext,
        mut attempt: F,
        validate: V,
    ) -> Result<U, ModelError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<AttemptOutput<T>, ModelError>>,
        V: FnMut(T) -> Result<U, ModelError>,
    {
        let mut models = vec![ctx.primary_model.clone()];
        if ctx.allow_fallback && ctx.task.allow_fallback {
            models.extend(self.fallback_models(&ctx.primary_model, ctx.image_count > 0));
        }
        self.execute_models_validated(ctx, &models, &mut attempt, validate)
            .await
    }

    /// [`Gateway::execute`] over an explicit model chain (no profile fallback).
    pub async fn execute_models<T, F, Fut>(
        &self,
        ctx: &CallContext,
        models: &[String],
        attempt: &mut F,
    ) -> Result<T, ModelError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<AttemptOutput<T>, ModelError>>,
    {
        self.execute_models_validated(ctx, models, attempt, Ok)
            .await
    }

    async fn execute_models_validated<T, U, F, Fut, V>(
        &self,
        ctx: &CallContext,
        models: &[String],
        attempt: &mut F,
        validate: V,
    ) -> Result<U, ModelError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<AttemptOutput<T>, ModelError>>,
        V: FnMut(T) -> Result<U, ModelError>,
    {
        let work = self.execute_models_inner(ctx, models, attempt, validate);
        let Some(limit) = ctx.task.timeout else {
            return work.await;
        };
        tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
            Err(ModelError::Deadline {
                limit,
                detail: format!(
                    "{} exceeded its whole-call timeout of {:?}",
                    ctx.task.label, limit
                ),
            })
        })
    }

    async fn execute_models_inner<T, U, F, Fut, V>(
        &self,
        ctx: &CallContext,
        models: &[String],
        attempt: &mut F,
        mut validate: V,
    ) -> Result<U, ModelError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<AttemptOutput<T>, ModelError>>,
        V: FnMut(T) -> Result<U, ModelError>,
    {
        let attempts = ctx.transient_attempts.max(1);
        let mut last_error: Option<ModelError> = None;
        for (model_index, model) in models.iter().enumerate() {
            let profile = self.model_profile(model);
            if model_index > 0 && ctx.image_count > profile.max_images {
                continue;
            }
            let budget = match self.check_budget(ctx, &profile) {
                Ok(budget) => budget,
                Err(error) => {
                    self.stats.record_budget_rejection(
                        &ctx.task.label,
                        model,
                        ctx.estimated_input_tokens,
                        error_limit(&error),
                    );
                    if model_index == 0 {
                        return Err(error);
                    }
                    continue;
                }
            };
            if model_index == 0 && budget.over_task_budget {
                self.stats.record_over_budget_observed(
                    &ctx.task.label,
                    model,
                    ctx.estimated_input_tokens,
                    ctx.task.input_budget_tokens,
                );
            }
            if model_index > 0 {
                tracing::warn!(
                    target: "ai_gateway",
                    task = %ctx.task.label,
                    primary_model = %ctx.primary_model,
                    fallback_model = %model,
                    error = %last_error.as_ref().map(ToString::to_string).unwrap_or_default(),
                    "primary model exhausted transient retries; trying fallback model"
                );
            }
            let control = self
                .controls
                .provider_control(&ctx.provider, profile.concurrency);
            let breaker = control.model_breaker(model);
            // A model whose breaker is open right now is skipped at once while
            // the chain still has another model to try; only the last model of
            // the chain waits for its breaker.
            if model_index + 1 < models.len() && breaker.blocks(Instant::now()) {
                tracing::warn!(
                    target: "ai_gateway",
                    task = %ctx.task.label,
                    model = %model,
                    "model breaker is open; trying the next model of the chain"
                );
                last_error = Some(circuit_open_error(
                    self.config.locale,
                    &ctx.provider,
                    model,
                    Duration::ZERO,
                ));
                continue;
            }
            for attempt_index in 0..attempts {
                // Admission (breaker and concurrency permit) is not part of
                // the attempt: the breaker wait bounds itself and returns the
                // circuit-open error, and a queued request waits for its
                // permit. Only the whole-call timeout, when the task
                // declares one, covers the queue.
                // An open breaker is about this model only: the rest of the
                // fallback chain has breakers of its own, so the call moves on
                // instead of returning the circuit-open error to the caller.
                let admission = self
                    .wait_for_admission(&breaker, &ctx.provider, model, ctx.attempt_timeout)
                    .await;
                let probe = match admission {
                    Ok(probe) => probe,
                    Err(error) => {
                        tracing::warn!(
                            target: "ai_gateway",
                            task = %ctx.task.label,
                            model = %model,
                            error = %error,
                            "model breaker is open; moving on to the next model of the chain"
                        );
                        last_error = Some(error);
                        break;
                    }
                };
                let mut probe_guard = control::ProbeGuard::new(&breaker, probe);
                let permit = match control.semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        last_error = Some(ModelError::transient(
                            None,
                            "AI gateway provider semaphore closed",
                        ));
                        break;
                    }
                };
                let attempt_deadline = ctx
                    .attempt_timeout
                    .map(|limit| tokio::time::Instant::now() + limit);
                let started = Instant::now();
                let span = tracing::info_span!(
                    "gen_ai.request",
                    otel.name = %format!("chat {model}"),
                    gen_ai.operation.name = "chat",
                    gen_ai.request.model = model.as_str(),
                    server.address = ctx.provider.as_str(),
                    taskloom.task = ctx.task.label.as_str(),
                    taskloom.attempt = attempt_index + 1,
                    gen_ai.usage.input_tokens = tracing::field::Empty,
                    gen_ai.usage.output_tokens = tracing::field::Empty,
                    error.type = tracing::field::Empty,
                );
                let request = tracing::Instrument::instrument(attempt(model.clone()), span.clone());
                let result = match attempt_deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline, request)
                        .await
                        .unwrap_or_else(|_| {
                            Err(ModelError::transient(
                                None,
                                format!(
                                    "{} request exceeded transport timeout of {}s",
                                    ctx.task.label,
                                    ctx.attempt_timeout.unwrap().as_secs()
                                ),
                            ))
                        }),
                    None => request.await,
                };
                drop(permit);
                let latency = started.elapsed();
                let usage = result.as_ref().ok().and_then(|output| output.usage);
                let result = result.and_then(|output| validate(output.value));
                if let Some(usage) = usage {
                    span.record("gen_ai.usage.input_tokens", usage.prompt_tokens);
                    span.record("gen_ai.usage.output_tokens", usage.completion_tokens);
                    let usage = crate::ledger::Usage {
                        requests: 1,
                        input_tokens: usage.prompt_tokens.unwrap_or(0),
                        output_tokens: usage.completion_tokens.unwrap_or(0),
                        cost_micros: self.config.prices.cost_micros(model, &usage),
                    };
                    self.stats.record_usage(usage);
                    crate::ledger::record_usage(&ctx.task.label, usage);
                }
                match &result {
                    Ok(_) => {}
                    Err(error) => {
                        span.record("error.type", error.kind());
                    }
                }
                match &result {
                    Err(error) if error.is_transient() => breaker.record_transient(Instant::now()),
                    _ => breaker.record_reachable(),
                }
                probe_guard.disarm();
                self.stats.record_sent(stats::CallRecord {
                    task: &ctx.task.label,
                    model,
                    provider: &ctx.provider,
                    estimated_input_tokens: ctx.estimated_input_tokens,
                    budget_tokens: budget.limit_tokens,
                    attempt: attempt_index + 1,
                    usage,
                    latency,
                    outcome: match &result {
                        Ok(_) => "ok",
                        Err(error) => error.kind(),
                    },
                });
                match result {
                    Ok(value) => {
                        if model_index > 0 {
                            self.stats.record_degraded_success(
                                &ctx.task.label,
                                &ctx.primary_model,
                                model,
                                &ctx.provider,
                                "fallback_model",
                            );
                        }
                        return Ok(value);
                    }
                    Err(error) if error.is_transient() => {
                        if attempt_index + 1 < attempts {
                            let delay = transient_backoff(&error, attempt_index);
                            tracing::warn!(
                                target: "ai_gateway",
                                task = %ctx.task.label,
                                model = %model,
                                attempt = attempt_index + 1,
                                max_attempts = attempts,
                                delay_ms = delay.as_millis() as u64,
                                error = %error,
                                "transient provider failure; retrying unchanged request"
                            );
                            tokio::time::sleep(self.scaled(delay)).await;
                        }
                        last_error = Some(error);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            ModelError::misconfigured(self.config.locale.no_model_available(&ctx.task.label))
        }))
    }
}

fn error_limit(error: &ModelError) -> usize {
    match error {
        ModelError::ContextOverflow {
            limit_tokens: Some(limit),
            ..
        } => *limit,
        _ => 0,
    }
}

#[cfg(test)]
mod tests;
