use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::RuntimeError;

/// Model-spend accounting in millionths of the configured currency unit.
/// `committed_micros` includes uncertain attempts charged at their ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetSnapshot {
    /// Monotonic revision, used to reconcile a crash-time reservation journal
    /// with a later run checkpoint containing settled usage.
    pub generation: u64,
    pub limit_micros: Option<u64>,
    pub committed_micros: u64,
    pub in_flight_micros: u64,
    pub in_flight_attempts: u64,
    pub uncertain_micros: u64,
    pub uncertain_attempts: u64,
    pub denial: Option<String>,
}

/// Shared by every model attempt in one run. A dropped reservation remains
/// charged: a cancelled connection can still have incurred provider cost.
#[derive(Clone)]
pub struct RunBudget(Arc<BudgetInner>);

struct BudgetInner {
    state: Mutex<BudgetSnapshot>,
    journal: Option<Arc<dyn BudgetJournal>>,
    journal_failure: Mutex<Option<String>>,
}

impl std::fmt::Debug for RunBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RunBudget").field(&self.snapshot()).finish()
    }
}

/// Persist a reservation before a paid request may be dispatched. Settlements
/// are included in subsequent reservations or the next full run checkpoint.
pub trait BudgetJournal: Send + Sync {
    fn persist<'a>(
        &'a self,
        snapshot: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
}

impl RunBudget {
    pub fn new(limit_micros: Option<u64>) -> Self {
        Self(Arc::new(BudgetInner {
            state: Mutex::new(BudgetSnapshot {
                limit_micros,
                ..Default::default()
            }),
            journal: None,
            journal_failure: Mutex::new(None),
        }))
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn refuse(&self, detail: String) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.denial = Some(detail);
        state.generation = state.generation.saturating_add(1);
    }

    pub(crate) fn restore(
        mut snapshot: BudgetSnapshot,
        journal: Arc<dyn BudgetJournal>,
    ) -> Result<Self, RuntimeError> {
        if snapshot.generation >= u64::MAX - 1024 {
            return Err(RuntimeError::Storage("invalid budget generation".into()));
        }
        if snapshot.uncertain_micros > snapshot.committed_micros
            || snapshot
                .committed_micros
                .checked_add(snapshot.in_flight_micros)
                .is_none()
        {
            return Err(RuntimeError::Storage(
                "invalid budget journal amounts".into(),
            ));
        }
        if snapshot.in_flight_micros > 0 && snapshot.in_flight_attempts == 0 {
            return Err(RuntimeError::Storage(
                "budget reservation has no attempt".into(),
            ));
        }
        if snapshot.in_flight_attempts > 0 {
            snapshot.committed_micros = snapshot
                .committed_micros
                .checked_add(snapshot.in_flight_micros)
                .ok_or_else(|| RuntimeError::Storage("budget accounting overflow".into()))?;
            snapshot.uncertain_micros = snapshot
                .uncertain_micros
                .checked_add(snapshot.in_flight_micros)
                .ok_or_else(|| RuntimeError::Storage("budget uncertainty overflow".into()))?;
            snapshot.uncertain_attempts = snapshot
                .uncertain_attempts
                .saturating_add(snapshot.in_flight_attempts);
            snapshot.in_flight_micros = 0;
            snapshot.in_flight_attempts = 0;
            snapshot.generation += 1;
        }
        Ok(Self(Arc::new(BudgetInner {
            state: Mutex::new(snapshot),
            journal: Some(journal),
            journal_failure: Mutex::new(None),
        })))
    }

    pub(crate) fn journal_failure(&self) -> Option<String> {
        self.0
            .journal_failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The dispatch API for persistent runs. A custom model must await this
    /// method for every paid attempt; synchronous `reserve` is intentionally
    /// unavailable when a persistent journal is attached.
    pub async fn reserve_before_dispatch(
        &self,
        ceiling_micros: u64,
    ) -> Result<Reservation, RuntimeError> {
        let reservation = self.reserve_inner(ceiling_micros)?;
        if let Some(journal) = &self.0.journal {
            if let Err(error) = journal.persist(&self.snapshot()).await {
                let detail = error.to_string();
                *self
                    .0
                    .journal_failure
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(detail.clone());
                return Err(RuntimeError::Storage(detail));
            }
        }
        Ok(reservation)
    }

    /// Reserve a configured upper bound before dispatch. Unknown prices must
    /// be rejected by a metered model when a limit is configured.
    pub fn reserve(&self, ceiling_micros: u64) -> Result<Reservation, RuntimeError> {
        if self.0.journal.is_some() {
            return Err(RuntimeError::Configuration(
                "persistent models must await reserve_before_dispatch".into(),
            ));
        }
        self.reserve_inner(ceiling_micros)
    }

    fn reserve_inner(&self, ceiling_micros: u64) -> Result<Reservation, RuntimeError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(detail) = &state.denial {
            return Err(RuntimeError::Budget(detail.clone()));
        }
        let total = state
            .committed_micros
            .checked_add(state.in_flight_micros)
            .and_then(|total| total.checked_add(ceiling_micros));
        if total.is_none()
            || state
                .limit_micros
                .is_some_and(|limit| total.unwrap() > limit)
        {
            let detail = format!("model spend budget cannot reserve {ceiling_micros} micro-units");
            state.denial = Some(detail.clone());
            state.generation = state.generation.saturating_add(1);
            return Err(RuntimeError::Budget(detail));
        }
        state.in_flight_micros += ceiling_micros;
        state.in_flight_attempts = state.in_flight_attempts.saturating_add(1);
        state.generation = state.generation.saturating_add(1);
        Ok(Reservation {
            budget: self.clone(),
            ceiling_micros,
            settled: false,
        })
    }
}

/// Settle with known cost, or drop to conservatively charge the reservation.
pub struct Reservation {
    budget: RunBudget,
    ceiling_micros: u64,
    settled: bool,
}

impl Reservation {
    pub fn settle(mut self, actual_micros: u64) {
        let mut state = self
            .budget
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.in_flight_micros -= self.ceiling_micros;
        state.in_flight_attempts -= 1;
        state.committed_micros = state.committed_micros.saturating_add(actual_micros);
        state.generation = state.generation.saturating_add(1);
        if state.limit_micros.is_some_and(|limit| {
            state
                .committed_micros
                .saturating_add(state.in_flight_micros)
                > limit
        }) {
            state.denial =
                Some("reported model cost exceeded the configured reservation budget".into());
        }
        self.settled = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.settled {
            let mut state = self
                .budget
                .0
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.in_flight_micros -= self.ceiling_micros;
            state.in_flight_attempts -= 1;
            state.committed_micros = state.committed_micros.saturating_add(self.ceiling_micros);
            state.uncertain_micros = state.uncertain_micros.saturating_add(self.ceiling_micros);
            state.uncertain_attempts = state.uncertain_attempts.saturating_add(1);
            state.generation = state.generation.saturating_add(1);
        }
    }
}
