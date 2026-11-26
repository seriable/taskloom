//! Structured call log and in-process counters.
//!
//! Every gateway attempt emits one `tracing` event (target
//! `ai_gateway`) with task, model, estimated tokens, provider usage, latency
//! and outcome. The counters back [`Gateway::stats`](super::Gateway::stats), which gives the
//! evidence ("max input ≤ 16K") without parsing logs.

use std::{collections::BTreeMap, sync::Mutex, time::Duration};

use serde::{Deserialize, Serialize};

/// Provider-reported token usage (`usage` in an OpenAI-compatible response).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default, alias = "input_tokens")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, alias = "output_tokens")]
    pub completion_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GatewayCallStats {
    /// Requests that reached the provider (every attempt counts).
    pub sent_attempts: u64,
    /// Requests refused before sending because they exceeded the budget.
    pub budget_rejections: u64,
    /// Requests over their task budget that were still sent (observe mode).
    pub over_budget_observed: u64,
    /// Largest estimated input among *sent* requests.
    pub max_estimated_input_tokens: usize,
    pub max_estimated_input_task: Option<String>,
    /// Largest provider-reported prompt size among sent requests.
    pub max_actual_prompt_tokens: Option<u64>,
    /// Sum of provider prompt tokens / sum of estimates for the same calls,
    /// in thousandths; used to calibrate the estimator.
    pub actual_to_estimate_ratio_milli: Option<u64>,
    /// Outcome kind -> count (`ok`, `transient`, `context_overflow`, ...).
    pub outcomes: BTreeMap<String, u64>,
    /// Successful calls answered by a model other than the one the caller
    /// asked for (fallback chain or secondary text gateway).
    pub degraded_successes: u64,
    /// `task -> served model` -> count of those degraded successes.
    pub degraded_served_by: BTreeMap<String, u64>,
    /// Tokens and cost of every answered request that reported usage.
    pub usage: crate::ledger::Usage,
}

#[derive(Default)]
struct StatsState {
    stats: GatewayCallStats,
    usage_prompt_sum: u64,
    usage_estimate_sum: u64,
}

/// The counters of one gateway.
#[derive(Default)]
pub(crate) struct Stats {
    state: Mutex<StatsState>,
}

pub(crate) struct CallRecord<'a> {
    pub task: &'a str,
    pub model: &'a str,
    pub provider: &'a str,
    pub estimated_input_tokens: usize,
    pub budget_tokens: usize,
    pub attempt: usize,
    pub usage: Option<TokenUsage>,
    pub latency: Duration,
    pub outcome: &'a str,
}

impl Stats {
    fn state(&self) -> std::sync::MutexGuard<'_, StatsState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn record_sent(&self, record: CallRecord<'_>) {
        tracing::info!(
            target: "ai_gateway",
            task = record.task,
            model = record.model,
            provider = record.provider,
            est_input_tokens = record.estimated_input_tokens,
            budget_tokens = record.budget_tokens,
            attempt = record.attempt,
            usage_prompt_tokens = record.usage.and_then(|usage| usage.prompt_tokens),
            usage_completion_tokens = record.usage.and_then(|usage| usage.completion_tokens),
            latency_ms = record.latency.as_millis() as u64,
            outcome = record.outcome,
            "ai gateway call"
        );
        let mut guard = self.state();
        let state = &mut *guard;
        state.stats.sent_attempts += 1;
        *state
            .stats
            .outcomes
            .entry(record.outcome.to_string())
            .or_default() += 1;
        if record.estimated_input_tokens > state.stats.max_estimated_input_tokens {
            state.stats.max_estimated_input_tokens = record.estimated_input_tokens;
            state.stats.max_estimated_input_task = Some(record.task.to_string());
        }
        if let Some(prompt) = record.usage.and_then(|usage| usage.prompt_tokens) {
            state.stats.max_actual_prompt_tokens = Some(
                state
                    .stats
                    .max_actual_prompt_tokens
                    .unwrap_or(0)
                    .max(prompt),
            );
            state.usage_prompt_sum = state.usage_prompt_sum.saturating_add(prompt);
            state.usage_estimate_sum = state
                .usage_estimate_sum
                .saturating_add(record.estimated_input_tokens as u64);
            state.stats.actual_to_estimate_ratio_milli = state
                .usage_prompt_sum
                .saturating_mul(1000)
                .checked_div(state.usage_estimate_sum);
        }
    }

    pub(crate) fn record_budget_rejection(
        &self,
        task: &str,
        model: &str,
        estimated: usize,
        limit: usize,
    ) {
        tracing::warn!(
            target: "ai_gateway",
            task,
            model,
            est_input_tokens = estimated,
            budget_tokens = limit,
            outcome = "context_overflow",
            "ai gateway refused an over-budget request before sending"
        );
        let mut guard = self.state();
        guard.stats.budget_rejections += 1;
        *guard
            .stats
            .outcomes
            .entry("budget_rejected".to_string())
            .or_default() += 1;
    }

    pub(crate) fn record_over_budget_observed(
        &self,
        task: &str,
        model: &str,
        estimated: usize,
        budget: usize,
    ) {
        tracing::warn!(
            target: "ai_gateway",
            task,
            model,
            est_input_tokens = estimated,
            budget_tokens = budget,
            "ai gateway request exceeds its task budget (observe mode; sent anyway)"
        );
        self.state().stats.over_budget_observed += 1;
    }

    /// A call succeeded, but on a model (or provider) other than the one the
    /// caller asked for. The result text itself does not say so, so this is where
    /// quality review can see the degradation.
    pub(crate) fn record_degraded_success(
        &self,
        task: &str,
        requested_model: &str,
        served_model: &str,
        provider: &str,
        route: &str,
    ) {
        tracing::warn!(
            target: "ai_gateway",
            task,
            requested_model,
            served_model,
            provider,
            route,
            degraded = true,
            "ai gateway call succeeded on a degraded route"
        );
        crate::ledger::record_degraded(task, route, served_model);
        let mut guard = self.state();
        guard.stats.degraded_successes += 1;
        *guard
            .stats
            .degraded_served_by
            .entry(format!("{task} -> {served_model}"))
            .or_default() += 1;
    }

    pub(crate) fn record_usage(&self, usage: crate::ledger::Usage) {
        self.state().stats.usage.add(usage);
    }

    /// Snapshot of the counters.
    pub(crate) fn snapshot(&self) -> GatewayCallStats {
        self.state().stats.clone()
    }
}
