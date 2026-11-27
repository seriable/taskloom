use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::RuntimeError;

/// Model-spend accounting in millionths of the configured currency unit.
/// `committed_micros` includes uncertain attempts charged at their ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetSnapshot {
    pub limit_micros: Option<u64>,
    pub committed_micros: u64,
    pub in_flight_micros: u64,
    pub uncertain_micros: u64,
    pub uncertain_attempts: u64,
    pub denial: Option<String>,
}

/// Shared by every model attempt in one run. A dropped reservation remains
/// charged: a cancelled connection can still have incurred provider cost.
#[derive(Debug, Clone)]
pub struct RunBudget(Arc<Mutex<BudgetSnapshot>>);

impl RunBudget {
    pub fn new(limit_micros: Option<u64>) -> Self {
        Self(Arc::new(Mutex::new(BudgetSnapshot {
            limit_micros,
            ..Default::default()
        })))
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn refuse(&self, detail: String) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).denial = Some(detail);
    }

    /// Reserve a configured upper bound before dispatch. Unknown prices must
    /// be rejected by a metered model when a limit is configured.
    pub fn reserve(&self, ceiling_micros: u64) -> Result<Reservation, RuntimeError> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
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
            return Err(RuntimeError::Budget(detail));
        }
        state.in_flight_micros += ceiling_micros;
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
        let mut state = self.budget.0.lock().unwrap_or_else(|e| e.into_inner());
        state.in_flight_micros -= self.ceiling_micros;
        state.committed_micros = state.committed_micros.saturating_add(actual_micros);
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
            let mut state = self.budget.0.lock().unwrap_or_else(|e| e.into_inner());
            state.in_flight_micros -= self.ceiling_micros;
            state.committed_micros = state.committed_micros.saturating_add(self.ceiling_micros);
            state.uncertain_micros = state.uncertain_micros.saturating_add(self.ceiling_micros);
            state.uncertain_attempts = state.uncertain_attempts.saturating_add(1);
        }
    }
}
