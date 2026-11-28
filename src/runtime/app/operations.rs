//! Explicit project operational snapshot. No prompt, tool arguments, user IDs,
//! credentials, exporter or background upload. Capture only on authenticated read.
use super::Application;
use crate::runtime::{database::*, RuntimeError};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct OperationsSnapshot {
    pub version: u32,
    pub jobs: usize,
    pub states: BTreeMap<String, u64>,
    pub open: u64,
    pub attempts: u64,
    pub queue_enabled: bool,
    pub configured_max_open: Option<u32>,
    pub configured_max_running: Option<u32>,
    pub generation: OperationsGeneration,
    pub read_limit: usize,
    pub consistent_cross_record_snapshot: bool,
    pub model_or_service_dispatched: bool,
}
/// Explicit metric projection. Quota audit operator names and free-form notes
/// stay in the trusted administration store, outside Reader observations.
#[derive(Debug, Serialize)]
pub struct OperationsGeneration {
    pub revision: u64,
    pub policy_revision: u64,
    pub currency: Option<String>,
    pub policy: Option<GenerationQuota>,
    pub usage: Option<GenerationQuotaUsage>,
    pub overrun: bool,
}
impl From<GenerationQuotaSnapshot> for OperationsGeneration {
    fn from(snapshot: GenerationQuotaSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            policy_revision: snapshot.policy_revision,
            currency: snapshot.currency,
            policy: snapshot.policy,
            usage: snapshot.usage,
            overrun: snapshot.overrun,
        }
    }
}
pub async fn snapshot(app: &Application) -> Result<OperationsSnapshot, RuntimeError> {
    let (database, namespace) = app.stores.sql.as_ref().ok_or_else(|| {
        RuntimeError::Configuration("operational snapshots require SQL application storage".into())
    })?;
    let queue = app.queue_store()?;
    // list validates journal/index agreement and refuses oversized results;
    // do not quietly present a partial sample as a complete service total.
    let jobs = queue.list().await?;
    let policy = queue.policy().await?;
    let mut states = BTreeMap::new();
    let mut open = 0u64;
    let mut attempts = 0u64;
    for job in &jobs {
        let state = serde_json::to_value(job.state)
            .map_err(crate::runtime::store::storage)?
            .as_str()
            .ok_or_else(|| RuntimeError::Protocol("invalid queue state".into()))?
            .to_string();
        *states.entry(state).or_insert(0) += 1;
        open += u64::from(job.state.is_open());
        attempts = attempts
            .checked_add(job.attempts)
            .ok_or_else(|| RuntimeError::Protocol("attempt counter overflow".into()))?;
    }
    Ok(OperationsSnapshot {
        version: 1,
        jobs: jobs.len(),
        states,
        open,
        attempts,
        queue_enabled: policy.limits.is_some(),
        configured_max_open: policy.limits.as_ref().map(|p| p.max_open_jobs),
        configured_max_running: policy.limits.as_ref().map(|p| p.max_running),
        generation: database
            .generation_quota(namespace.clone())
            .snapshot()
            .await?
            .into(),
        read_limit: 1000,
        consistent_cross_record_snapshot: false,
        model_or_service_dispatched: false,
    })
}
/// Prometheus gauges, not cumulative observation counters. Repeated scrapes and
/// overlapping parent/child scopes must not be summed as new work.
pub fn prometheus(snapshot: &OperationsSnapshot) -> String {
    let mut text = String::from("# TYPE taskloom_queue_jobs gauge\n");
    for state in [
        "pending",
        "running",
        "paused",
        "waiting",
        "failed",
        "completed",
        "stopped",
        "abandoned",
    ] {
        text.push_str(&format!(
            "taskloom_queue_jobs{{state=\"{state}\"}} {}\n",
            snapshot.states.get(state).copied().unwrap_or(0)
        ));
    }
    text.push_str(&format!("# TYPE taskloom_queue_open gauge\ntaskloom_queue_open {}\n# TYPE taskloom_queue_enabled gauge\ntaskloom_queue_enabled {}\n",snapshot.open,u8::from(snapshot.queue_enabled)));
    if let Some(limit) = snapshot.configured_max_running {
        text.push_str(&format!(
            "# TYPE taskloom_queue_max_running gauge\ntaskloom_queue_max_running {limit}\n"
        ));
    }
    // Generation accounting is deliberately left as exact JSON u64 fields.
    // Floating Prometheus gauges are not a budget authorization mechanism.
    text
}
