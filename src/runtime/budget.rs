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
    admission: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for RunBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RunBudget").field(&self.snapshot()).finish()
    }
}

/// Persist a reservation before a paid request may be dispatched. Settlements
/// are included in subsequent reservations or the next full run checkpoint.
/// `RuntimeError::Budget` means admission was atomically refused and nothing
/// was written. Other errors may be ambiguous and retain uncertain spending.
pub trait BudgetJournal: Send + Sync {
    /// SQL journals require priced ceilings even before a project administrator
    /// enables a quota. A later policy change cannot race an unpriced dispatch.
    fn requires_cost_ceiling(&self) -> bool {
        false
    }
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
            admission: tokio::sync::Mutex::new(()),
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

    /// Model adapters must require known prices and reserve a conservative
    /// ceiling whenever this is true, including all retries and fallbacks.
    pub fn requires_cost_ceiling(&self) -> bool {
        self.snapshot().limit_micros.is_some()
            || self
                .0
                .journal
                .as_ref()
                .is_some_and(|journal| journal.requires_cost_ceiling())
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
            admission: tokio::sync::Mutex::new(()),
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
        // Serialize reservation registration, not provider execution. A denied
        // snapshot must never contain another, still-unadmitted reservation.
        let _admission = self.0.admission.lock().await;
        let reservation = self.reserve_inner(ceiling_micros)?;
        if let Some(journal) = &self.0.journal {
            if let Err(error) = journal.persist(&self.snapshot()).await {
                if let RuntimeError::Budget(detail) = error {
                    reservation.refuse_before_dispatch(detail.clone());
                    return Err(RuntimeError::Budget(detail));
                }
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
    fn refuse_before_dispatch(mut self, detail: String) {
        let mut state = self
            .budget
            .0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.in_flight_micros -= self.ceiling_micros;
        state.in_flight_attempts -= 1;
        state.denial = Some(detail);
        state.generation = state.generation.saturating_add(1);
        self.settled = true;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Journal {
        calls: AtomicUsize,
        unknown: bool,
    }
    impl BudgetJournal for Journal {
        fn persist<'a>(&'a self, _: &'a BudgetSnapshot) -> BoxFuture<'a, Result<(), RuntimeError>> {
            Box::pin(async move {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                if self.unknown {
                    return Err(RuntimeError::Storage("lost commit response".into()));
                }
                if call == 0 {
                    Ok(())
                } else {
                    Err(RuntimeError::Budget("known atomic refusal".into()))
                }
            })
        }
    }
    #[tokio::test]
    async fn atomic_refusal_retains_only_the_other_admitted_attempt() {
        let budget = RunBudget::restore(
            BudgetSnapshot::default(),
            Arc::new(Journal {
                calls: AtomicUsize::new(0),
                unknown: false,
            }),
        )
        .unwrap();
        let (a, b) = tokio::join!(
            budget.reserve_before_dispatch(7),
            budget.reserve_before_dispatch(9)
        );
        assert!(matches!(b, Err(RuntimeError::Budget(_))));
        assert!(budget.journal_failure().is_none());
        let state = budget.snapshot();
        assert_eq!(
            (
                state.in_flight_attempts,
                state.in_flight_micros,
                state.uncertain_attempts
            ),
            (1, 7, 0)
        );
        a.unwrap().settle(3);
        let state = budget.snapshot();
        assert_eq!(
            (
                state.committed_micros,
                state.in_flight_attempts,
                state.uncertain_attempts
            ),
            (3, 0, 0)
        );
    }
    #[tokio::test]
    async fn ambiguous_journal_failure_remains_uncertain() {
        let budget = RunBudget::restore(
            BudgetSnapshot::default(),
            Arc::new(Journal {
                calls: AtomicUsize::new(0),
                unknown: true,
            }),
        )
        .unwrap();
        assert!(matches!(
            budget.reserve_before_dispatch(7).await,
            Err(RuntimeError::Storage(_))
        ));
        assert!(budget.journal_failure().is_some());
        let state = budget.snapshot();
        assert_eq!(
            (
                state.committed_micros,
                state.uncertain_micros,
                state.uncertain_attempts
            ),
            (7, 7, 1)
        );
    }
}
