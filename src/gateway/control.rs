//! Per-provider admission control: a concurrency semaphore keyed by the
//! normalized provider base URL, and a circuit breaker per (provider, model).
//!
//! The breaker is deliberately *waiting*, not fail-fast: while it is open, callers wait for
//! the cool-down and then a single half-open probe decides whether the
//! provider is healthy again. Failing every queued call immediately would turn
//! one gateway hiccup into a batch of degraded fallbacks.
//!
//! While the probe is in flight the other callers wait for its *outcome*
//! (a watch channel bumped on every breaker transition) rather than polling
//! on a short fixed budget: LLM calls take tens of seconds, so a
//! poll-then-give-up wait would fail every queued caller before the probe
//! could answer.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{watch, Semaphore};

pub const DEFAULT_BREAKER_THRESHOLD: u32 = 5;
pub const DEFAULT_BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open { until: Instant },
    HalfOpen { probe_in_flight: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Send now. `probe` marks the single half-open trial request.
    Allowed { probe: bool },
    /// The breaker is open; ask again after this delay.
    Wait(Duration),
    /// Another caller's half-open probe is in flight; wait for its outcome.
    AwaitProbe,
}

#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    threshold: u32,
    cooldown: Duration,
    consecutive_transient: u32,
    state: BreakerState,
}

impl CircuitBreaker {
    pub fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            threshold: threshold.max(1),
            cooldown,
            consecutive_transient: 0,
            state: BreakerState::Closed,
        }
    }

    pub fn state(&self) -> BreakerState {
        self.state
    }

    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    pub fn admit(&mut self, now: Instant) -> Admission {
        match self.state {
            BreakerState::Closed => Admission::Allowed { probe: false },
            BreakerState::Open { until } if now < until => Admission::Wait(until - now),
            BreakerState::Open { .. }
            | BreakerState::HalfOpen {
                probe_in_flight: false,
            } => {
                self.state = BreakerState::HalfOpen {
                    probe_in_flight: true,
                };
                Admission::Allowed { probe: true }
            }
            BreakerState::HalfOpen {
                probe_in_flight: true,
            } => Admission::AwaitProbe,
        }
    }

    /// The provider answered (successfully or with a non-transient error):
    /// it is reachable, so the breaker closes.
    pub fn record_reachable(&mut self) {
        self.consecutive_transient = 0;
        self.state = BreakerState::Closed;
    }

    pub fn record_transient(&mut self, now: Instant) {
        self.consecutive_transient = self.consecutive_transient.saturating_add(1);
        let probe_failed = matches!(self.state, BreakerState::HalfOpen { .. });
        if probe_failed || self.consecutive_transient >= self.threshold {
            self.state = BreakerState::Open {
                until: now + self.cooldown,
            };
        }
    }

    /// A half-open probe was abandoned (its future was dropped) without an
    /// outcome; let the next caller probe instead of blocking forever.
    pub fn release_probe(&mut self) {
        if matches!(
            self.state,
            BreakerState::HalfOpen {
                probe_in_flight: true
            }
        ) {
            self.state = BreakerState::HalfOpen {
                probe_in_flight: false,
            };
        }
    }
}

/// One circuit breaker plus the channel its waiting callers watch.
/// A breaker with a channel announcing its transitions.
pub struct BreakerCell {
    breaker: Mutex<CircuitBreaker>,
    /// Bumped after every recorded outcome or released probe, so callers
    /// waiting on a half-open probe wake as soon as it answers.
    outcomes: watch::Sender<u64>,
}

impl BreakerCell {
    fn new(breaker: CircuitBreaker) -> Self {
        Self {
            breaker: Mutex::new(breaker),
            outcomes: watch::channel(0).0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CircuitBreaker> {
        self.breaker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn admit(&self, now: Instant) -> Admission {
        self.lock().admit(now)
    }

    pub fn cooldown(&self) -> Duration {
        self.lock().cooldown()
    }

    pub fn record_reachable(&self) {
        self.lock().record_reachable();
        self.announce_outcome();
    }

    pub fn record_transient(&self, now: Instant) {
        self.lock().record_transient(now);
        self.announce_outcome();
    }

    pub fn release_probe(&self) {
        self.lock().release_probe();
        self.announce_outcome();
    }

    /// Receiver for breaker outcomes. Subscribe *before* calling
    /// [`Self::admit`] so an outcome landing in between is not missed.
    pub fn subscribe_outcomes(&self) -> watch::Receiver<u64> {
        self.outcomes.subscribe()
    }

    pub fn state(&self) -> BreakerState {
        self.lock().state()
    }

    /// Whether a call sent now would only wait behind this breaker: it is
    /// open and its cool-down has not passed, or another caller's half-open
    /// probe is in flight.
    pub fn blocks(&self, now: Instant) -> bool {
        match self.state() {
            BreakerState::Open { until } => now < until,
            BreakerState::HalfOpen { probe_in_flight } => probe_in_flight,
            BreakerState::Closed => false,
        }
    }

    fn announce_outcome(&self) {
        self.outcomes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

/// Admission control for one provider (base URL). Concurrency is a property
/// of the provider, so the semaphore is shared by all of its models. The
/// breaker is not: one model failing (a fast model the account cannot use, an
/// overloaded primary) must not stop the provider's other models, which are
/// exactly the fallback chain a failing call walks next. Each model therefore
/// gets its own breaker, built from the provider breaker's settings.
pub struct ProviderControl {
    pub(crate) semaphore: Arc<Semaphore>,
    /// The provider-wide breaker: the settings every model breaker is built
    /// from, and a provider-wide signal read by [`provider_breaker_blocks`].
    provider: BreakerCell,
    models: Mutex<HashMap<String, Arc<BreakerCell>>>,
}

impl ProviderControl {
    fn with_breaker(permits: usize, breaker: CircuitBreaker) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(permits.max(1))),
            provider: BreakerCell::new(breaker),
            models: Mutex::new(HashMap::new()),
        }
    }

    /// The breaker of one model at this provider, created on first use with
    /// the provider breaker's threshold and cool-down.
    pub fn model_breaker(&self, model: &str) -> Arc<BreakerCell> {
        let template = {
            let provider = self.provider.lock();
            CircuitBreaker::new(provider.threshold, provider.cooldown)
        };
        self.models
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(model.trim().to_string())
            .or_insert_with(|| Arc::new(BreakerCell::new(template)))
            .clone()
    }

    /// Whether the provider-wide breaker or any one of its model breakers
    /// blocks right now.
    fn any_breaker_blocks(&self, now: Instant) -> bool {
        if self.provider.blocks(now) {
            return true;
        }
        self.models
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
            .any(|cell| cell.blocks(now))
    }
}

/// Breaker and concurrency settings of new providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlConfig {
    /// Consecutive transient failures that open a breaker.
    pub breaker_threshold: u32,
    pub breaker_cooldown: Duration,
    /// Concurrency of every provider; `None` takes the first requesting
    /// model's profile concurrency.
    pub provider_concurrency: Option<usize>,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            breaker_threshold: DEFAULT_BREAKER_THRESHOLD,
            breaker_cooldown: DEFAULT_BREAKER_COOLDOWN,
            provider_concurrency: None,
        }
    }
}

/// The control blocks of one gateway, by provider.
#[derive(Default)]
pub(crate) struct Controls {
    config: ControlConfig,
    providers: Mutex<HashMap<String, Arc<ProviderControl>>>,
}

impl Controls {
    pub(crate) fn new(config: ControlConfig) -> Self {
        Self {
            config,
            providers: Mutex::new(HashMap::new()),
        }
    }

    fn providers(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<ProviderControl>>> {
        self.providers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Control block for a provider. The semaphore size is fixed when the
    /// provider is first seen: the configured concurrency if set, else the
    /// first requesting model's profile concurrency.
    pub(crate) fn provider_control(
        &self,
        provider: &str,
        profile_concurrency: usize,
    ) -> Arc<ProviderControl> {
        let config = self.config;
        self.providers()
            .entry(provider.to_string())
            .or_insert_with(|| {
                let permits = config
                    .provider_concurrency
                    .filter(|value| *value > 0)
                    .unwrap_or(profile_concurrency.max(1));
                Arc::new(ProviderControl::with_breaker(
                    permits,
                    CircuitBreaker::new(config.breaker_threshold, config.breaker_cooldown),
                ))
            })
            .clone()
    }

    /// Whether a call to `provider` sent now would only wait behind a
    /// breaker: the provider-wide breaker, or the breaker of any model at the
    /// provider, is open within its cool-down or has a half-open probe in
    /// flight. Never creates a control block, so a provider nothing has
    /// called yet is not blocked.
    pub(crate) fn provider_breaker_blocks(&self, provider: &str, now: Instant) -> bool {
        let control = self.providers().get(provider).cloned();
        control.is_some_and(|control| control.any_breaker_blocks(now))
    }

    /// Installs a provider with explicit limits (tests, warm-up).
    pub(crate) fn install(
        &self,
        provider: &str,
        permits: usize,
        breaker: CircuitBreaker,
    ) -> Arc<ProviderControl> {
        let control = Arc::new(ProviderControl::with_breaker(permits, breaker));
        self.providers()
            .insert(provider.to_string(), control.clone());
        control
    }
}

/// The provider-wide breaker, for callers (and tests) that signal a whole
/// provider at once; model calls go through [`ProviderControl::model_breaker`].
impl ProviderControl {
    pub fn record_reachable(&self) {
        self.provider.record_reachable();
    }

    pub fn record_transient(&self, now: Instant) {
        self.provider.record_transient(now);
    }

    pub fn breaker_state(&self) -> BreakerState {
        self.provider.state()
    }

    pub fn model_breaker_state(&self, model: &str) -> BreakerState {
        self.model_breaker(model).state()
    }
}

/// Releases an abandoned half-open probe when dropped without an outcome.
pub(crate) struct ProbeGuard<'a> {
    control: &'a BreakerCell,
    armed: bool,
}

impl<'a> ProbeGuard<'a> {
    pub(crate) fn new(control: &'a BreakerCell, probe: bool) -> Self {
        Self {
            control,
            armed: probe,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.control.release_probe();
        }
    }
}
