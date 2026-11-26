//! Where the model time of a stage goes.
//!
//! A stage opens a [`CallLedger`] around its work with [`CallLedger::scope`];
//! every model request made underneath — on the same task, or on a task
//! spawned through [`propagate`] — is counted by kind with its wall-clock
//! time, and a stage can time its own phases with [`phase`]. Ledgers nest: a
//! request is recorded on every open ledger, so the story compile keeps its
//! own profile while the stage that called it keeps the whole stage's.
//!
//! Nothing here changes a request; without an open ledger it records nothing.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

tokio::task_local! {
    static OPEN_LEDGERS: Vec<Arc<CallLedger>>;
    /// The label of the [`timed_call`] a request is made under, so the one
    /// place that logs request sizes can name the caller's kind.
    static CURRENT_CALL: String;
    /// The usage meter of the innermost [`metered`] run.
    static USAGE_METER: Arc<Mutex<Usage>>;
}

/// The label of the innermost [`timed_call`] this code runs under, if any.
pub fn current_call_label() -> Option<String> {
    CURRENT_CALL.try_with(Clone::clone).ok()
}

/// Calls of one kind: how many, how many failed, and their wall-clock time.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CallKindStats {
    pub calls: usize,
    pub failures: usize,
    pub total_ms: u64,
    pub max_ms: u64,
    /// Provider-reported input tokens of the kind's answered requests.
    #[serde(skip_serializing_if = "is_zero")]
    pub input_tokens: u64,
    /// Provider-reported output tokens.
    #[serde(skip_serializing_if = "is_zero")]
    pub output_tokens: u64,
    /// Cost in millionths of the price table's currency unit (0 without a
    /// price for the model).
    #[serde(skip_serializing_if = "is_zero")]
    pub cost_micros: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// Tokens and cost of model requests.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Usage {
    /// Answered requests that reported usage.
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Millionths of the price table's currency unit.
    pub cost_micros: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.requests = self.requests.saturating_add(other.requests);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cost_micros = self.cost_micros.saturating_add(other.cost_micros);
    }

    /// The cost in the price table's currency unit.
    pub fn cost(&self) -> f64 {
        self.cost_micros as f64 / 1_000_000.0
    }
}

/// Wall-clock of one named phase of a stage, and how many times it ran.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PhaseStats {
    pub runs: usize,
    pub total_ms: u64,
}

/// A snapshot of a ledger, as written to traces and stage results.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CallProfile {
    pub calls: BTreeMap<String, CallKindStats>,
    pub phases: BTreeMap<String, PhaseStats>,
    /// Model calls a stage's scheduler must wait for one after another, as
    /// it planned them; the other calls run beside these.
    pub planned_sequential_depth: BTreeMap<String, usize>,
    /// Successful calls answered by a backup model or the secondary gateway,
    /// keyed `task|route|served_model`
    /// ([`degraded_call_key`]).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub degraded_calls: BTreeMap<String, u64>,
}

/// The profile of one step of a stage, as kept on the stage checkpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StepCallProfile {
    pub step: String,
    pub elapsed_ms: u64,
    pub profile: CallProfile,
}

impl CallProfile {
    pub fn total_calls(&self) -> usize {
        self.calls.values().map(|stats| stats.calls).sum()
    }

    /// Tokens and cost of every kind together.
    pub fn total_usage(&self) -> Usage {
        let mut total = Usage::default();
        for stats in self.calls.values() {
            total.add(Usage {
                requests: 0,
                input_tokens: stats.input_tokens,
                output_tokens: stats.output_tokens,
                cost_micros: stats.cost_micros,
            });
        }
        total
    }
}

#[derive(Debug, Default)]
pub struct CallLedger {
    profile: Mutex<CallProfile>,
}

impl CallLedger {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Runs `future` with this ledger open, on top of any already open.
    pub async fn scope<F: Future>(self: &Arc<Self>, future: F) -> F::Output {
        let mut ledgers = OPEN_LEDGERS
            .try_with(|ledgers| ledgers.clone())
            .unwrap_or_default();
        ledgers.push(Arc::clone(self));
        OPEN_LEDGERS.scope(ledgers, future).await
    }

    pub fn snapshot(&self) -> CallProfile {
        self.profile
            .lock()
            .map(|profile| profile.clone())
            .unwrap_or_default()
    }

    fn update(&self, apply: impl FnOnce(&mut CallProfile)) {
        if let Ok(mut profile) = self.profile.lock() {
            apply(&mut profile);
        }
    }
}

fn each_open_ledger(apply: impl Fn(&CallLedger)) {
    let _ = OPEN_LEDGERS.try_with(|ledgers| ledgers.iter().for_each(|ledger| apply(ledger)));
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// The kind a request is counted under: its label with batch and item
/// numbers folded, so `roles_batch_3` and `roles_batch_4` are one kind.
pub fn call_kind(label: &str) -> String {
    let mut kind = String::with_capacity(label.len());
    let mut in_number = false;
    for character in label.trim().chars() {
        if character.is_ascii_digit() {
            if !in_number {
                kind.push('N');
            }
            in_number = true;
        } else {
            in_number = false;
            kind.push(character);
        }
    }
    kind
}

/// Records one finished model request on every open ledger.
pub fn record_call(label: &str, elapsed: Duration, succeeded: bool) {
    let kind = call_kind(label);
    let elapsed_ms = millis(elapsed);
    each_open_ledger(|ledger| {
        ledger.update(|profile| {
            let stats = profile.calls.entry(kind.clone()).or_default();
            stats.calls += 1;
            stats.failures += usize::from(!succeeded);
            stats.total_ms = stats.total_ms.saturating_add(elapsed_ms);
            stats.max_ms = stats.max_ms.max(elapsed_ms);
        });
    });
}

/// Records the usage of one answered request: on every open ledger under
/// the kind of the innermost [`timed_call`] (else `label`), and on the
/// innermost [`metered`] run.
pub fn record_usage(label: &str, usage: Usage) {
    let kind = call_kind(&current_call_label().unwrap_or_else(|| label.to_string()));
    each_open_ledger(|ledger| {
        ledger.update(|profile| {
            let stats = profile.calls.entry(kind.clone()).or_default();
            stats.input_tokens = stats.input_tokens.saturating_add(usage.input_tokens);
            stats.output_tokens = stats.output_tokens.saturating_add(usage.output_tokens);
            stats.cost_micros = stats.cost_micros.saturating_add(usage.cost_micros);
        });
    });
    let _ = USAGE_METER.try_with(|meter| {
        if let Ok(mut total) = meter.lock() {
            total.add(usage);
        }
    });
}

/// Runs `future` and returns the usage of every request it made (nested
/// meters each see their own requests and the outer meters do not).
pub async fn metered<F: Future>(future: F) -> (F::Output, Usage) {
    let meter = Arc::new(Mutex::new(Usage::default()));
    let output = USAGE_METER.scope(Arc::clone(&meter), future).await;
    let usage = meter.lock().map(|usage| *usage).unwrap_or_default();
    (output, usage)
}

/// The key one degraded call is counted under: `task|route|served_model`.
pub fn degraded_call_key(task: &str, route: &str, served_model: &str) -> String {
    format!("{}|{}|{}", task.trim(), route.trim(), served_model.trim())
}

/// Records one call that succeeded on a degraded route (backup model or the
/// secondary gateway) on every open ledger, so the run that made it can keep
/// the count with its own results instead of only in process-wide stats.
pub fn record_degraded(task: &str, route: &str, served_model: &str) {
    let key = degraded_call_key(task, route, served_model);
    each_open_ledger(|ledger| {
        ledger.update(|profile| {
            *profile.degraded_calls.entry(key.clone()).or_default() += 1;
        });
    });
}

/// The degraded calls recorded so far on the outermost open ledger: the
/// whole run when a run opened one around all its work.
pub fn outermost_degraded_calls() -> Option<BTreeMap<String, u64>> {
    OPEN_LEDGERS
        .try_with(|ledgers| {
            ledgers
                .first()
                .map(|ledger| ledger.snapshot().degraded_calls)
        })
        .ok()
        .flatten()
}

/// Times `future` and records its outcome as one call of `label`.
pub async fn timed_call<T, E>(
    label: &str,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let started = Instant::now();
    let result = CURRENT_CALL.scope(label.to_string(), future).await;
    record_call(label, started.elapsed(), result.is_ok());
    result
}

/// Times one phase of a stage (its wall-clock, whatever runs inside).
pub async fn phase<F: Future>(name: &str, future: F) -> F::Output {
    let started = Instant::now();
    let output = future.await;
    let elapsed_ms = millis(started.elapsed());
    each_open_ledger(|ledger| {
        ledger.update(|profile| {
            let stats = profile.phases.entry(name.to_string()).or_default();
            stats.runs += 1;
            stats.total_ms = stats.total_ms.saturating_add(elapsed_ms);
        });
    });
    output
}

/// Records the sequential depth a scheduler planned for one of its phases.
pub fn note_planned_depth(name: &str, depth: usize) {
    each_open_ledger(|ledger| {
        ledger.update(|profile| {
            profile
                .planned_sequential_depth
                .insert(name.to_string(), depth);
        });
    });
}

/// Runs `future` under a fresh ledger (nested in any open one), writes its
/// profile to the trace under `step`, and hands it back with the output.
pub async fn profiled<F: Future>(step: &str, future: F) -> (F::Output, StepCallProfile) {
    let ledger = CallLedger::new();
    let started = Instant::now();
    let output = ledger.scope(future).await;
    let profile = StepCallProfile {
        step: step.to_string(),
        elapsed_ms: millis(started.elapsed()),
        profile: ledger.snapshot(),
    };
    tracing::info!(
        step,
        elapsed_ms = profile.elapsed_ms,
        model_calls = profile.profile.total_calls(),
        profile = %serde_json::to_string(&profile.profile).unwrap_or_default(),
        "model-call profile"
    );
    (output, profile)
}

/// Carries the open ledgers and the ambient cancel token
/// ([`crate::cancel::scope`]) into a future that will be spawned on another
/// task (task-locals do not cross `spawn`). A feature with task-locals of its
/// own wraps this.
pub fn propagate<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let ledgers = OPEN_LEDGERS
        .try_with(|ledgers| ledgers.clone())
        .unwrap_or_default();
    OPEN_LEDGERS.scope(ledgers, crate::cancel::propagate(future))
}

/// A task spawned with the open ledgers, cancelled when dropped unawaited: a
/// caller that returns early leaves no request running behind it.
pub struct ScopedTask<T>(tokio::task::JoinHandle<T>);

/// Spawns `future` on its own task, carrying the open ledgers.
pub fn spawn_scoped<F>(future: F) -> ScopedTask<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    ScopedTask(tokio::spawn(propagate(future)))
}

/// A [`ScopedTask`] that panicked or was cancelled.
#[derive(Debug)]
pub struct ScopedTaskFailed(pub String);

impl std::fmt::Display for ScopedTaskFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "background model task ended: {}", self.0)
    }
}

impl std::error::Error for ScopedTaskFailed {}

impl<T> Future for ScopedTask<T> {
    type Output = Result<T, ScopedTaskFailed>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0)
            .poll(context)
            .map(|joined| joined.map_err(|error| ScopedTaskFailed(error.to_string())))
    }
}

impl<T> Drop for ScopedTask<T> {
    fn drop(&mut self) {
        // Aborting a finished task is a no-op.
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_labels_fold_into_one_kind() {
        assert_eq!(
            call_kind("summary_bridge_batch_12"),
            "summary_bridge_batch_N"
        );
        assert_eq!(call_kind("stage-state"), "stage-state");
        assert_eq!(call_kind("roles_batch_3_repair"), "roles_batch_N_repair");
    }

    #[tokio::test]
    async fn nested_ledgers_both_see_a_call_and_spawned_tasks_keep_them() {
        let outer = CallLedger::new();
        let inner = CallLedger::new();
        outer
            .scope(async {
                record_call("thread", Duration::from_millis(5), true);
                inner
                    .scope(async {
                        let handle = tokio::spawn(propagate(async {
                            record_call("batch_1", Duration::from_millis(7), false);
                        }));
                        handle.await.expect("spawned");
                        phase("units", async {}).await;
                    })
                    .await;
            })
            .await;
        let outer = outer.snapshot();
        let inner = inner.snapshot();
        assert_eq!(outer.total_calls(), 2);
        assert_eq!(inner.total_calls(), 1);
        assert_eq!(inner.calls["batch_N"].failures, 1);
        assert_eq!(inner.calls["batch_N"].max_ms, 7);
        assert_eq!(outer.phases["units"].runs, 1);
        // Nothing is recorded, and nothing fails, without an open ledger.
        record_call("orphan", Duration::from_millis(1), true);
    }

    #[tokio::test]
    async fn a_scoped_task_returns_its_output_and_is_cancelled_when_dropped() {
        let ledger = CallLedger::new();
        let output = ledger
            .scope(async {
                spawn_scoped(async {
                    record_call("series", Duration::from_millis(3), true);
                    41 + 1
                })
                .await
            })
            .await
            .expect("joined");
        assert_eq!(output, 42);
        assert_eq!(ledger.snapshot().calls["series"].calls, 1);

        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let task = spawn_scoped(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = sender.send(());
        });
        drop(task);
        // The sender is dropped with the aborted task, never used.
        assert!(receiver.await.is_err());
    }

    #[tokio::test]
    async fn a_profiled_step_keeps_its_own_calls() {
        let outer = CallLedger::new();
        let (value, profile) = outer
            .scope(profiled("edit_plan", async {
                timed_call("critical_interpretation", async { Ok::<_, ()>(7) }).await
            }))
            .await;
        assert_eq!(value, Ok(7));
        assert_eq!(profile.step, "edit_plan");
        assert_eq!(profile.profile.total_calls(), 1);
        assert_eq!(outer.snapshot().total_calls(), 1);
        let decoded: StepCallProfile =
            serde_json::from_value(serde_json::to_value(&profile).unwrap()).unwrap();
        assert_eq!(decoded, profile);
    }
}
