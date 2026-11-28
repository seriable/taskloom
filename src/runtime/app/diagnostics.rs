use super::{config::config_error, AppConfig, AppStores, Application};
use crate::runtime::{
    observability::*, workflow::*, RunState, RunStore, RuntimeError, SessionStore,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppObservability {
    /// Preserve this namespace if journals move to another deployment path.
    /// Without it, trace identity is namespaced by the absolute state root.
    pub namespace: Option<String>,
    /// Only an explicit export operation sends data. Merely configuring a
    /// destination never starts a poller or an external telemetry subscription.
    pub otlp: Option<OtlpHttpConfig>,
}
impl AppObservability {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self
            .namespace
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 512)
        {
            return Err(config_error("invalid diagnostic namespace"));
        }
        if let Some(config) = &self.otlp {
            config.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticTarget {
    Run,
    Workflow,
    Session,
}
pub(super) fn export_binding(
    config: &AppConfig,
    snapshot: &TraceSnapshot,
) -> Result<Option<String>, RuntimeError> {
    config
        .observability
        .as_ref()
        .and_then(|c| c.otlp.as_ref())
        .map(|c| {
            crate::runtime::state::fingerprint(&(
                "otlp-export-v1",
                c,
                &snapshot.source_revision_key,
            ))
        })
        .transpose()
}
impl DiagnosticTarget {
    pub fn parse(value: &str) -> Result<Self, RuntimeError> {
        match value {
            "run" => Ok(Self::Run),
            "workflow" => Ok(Self::Workflow),
            "session" => Ok(Self::Session),
            _ => Err(config_error(
                "diagnostic target must be run, workflow or session",
            )),
        }
    }
}
/// Read without initializing a model, MCP, embedding service or exporter. All
/// state is bounded and validated, and parent bindings are verified in the SDK.
pub async fn diagnostic_snapshot(
    config: &AppConfig,
    target: DiagnosticTarget,
    id: &str,
) -> Result<TraceSnapshot, RuntimeError> {
    let stores = AppStores::open(config).await?;
    let result = with_stores(config, &stores, target, id).await;
    stores.close().await;
    result
}
impl Application {
    pub async fn diagnostic_snapshot(
        &self,
        target: DiagnosticTarget,
        id: &str,
    ) -> Result<TraceSnapshot, RuntimeError> {
        with_stores(&self.config, &self.stores, target, id).await
    }
}
async fn with_stores(
    config: &AppConfig,
    stores: &AppStores,
    target: DiagnosticTarget,
    id: &str,
) -> Result<TraceSnapshot, RuntimeError> {
    match target {
        DiagnosticTarget::Run => super::valid_read_id(id)?,
        DiagnosticTarget::Workflow | DiagnosticTarget::Session => super::valid_id(id)?,
    }
    let runs = &stores.runs;
    let declared_namespace = config
        .observability
        .as_ref()
        .and_then(|c| c.namespace.clone());
    let namespace = match declared_namespace {
        Some(namespace) if config.database.is_some() => crate::runtime::state::fingerprint(&(
            "app-sql-observability-v1",
            stores.identity(),
            namespace,
        ))?,
        Some(namespace) => namespace,
        None => stores.identity().into(),
    };
    match target {
        DiagnosticTarget::Run => TraceSnapshot::run(
            &namespace,
            &runs
                .inspect(id)
                .await?
                .ok_or_else(|| config_error("diagnostic run not found"))?,
        ),
        DiagnosticTarget::Workflow => {
            let state = stores
                .workflows
                .inspect(id)
                .await?
                .ok_or_else(|| config_error("diagnostic workflow not found"))?;
            state.validate()?;
            let ids = state
                .nodes
                .iter()
                .filter_map(|n| workflow_run_id(&state, n))
                .collect::<Vec<_>>();
            let children = load_children(runs, &ids).await?;
            TraceSnapshot::workflow(&namespace, &state, &children)
        }
        DiagnosticTarget::Session => {
            let session = stores
                .sessions
                .load(id)
                .await?
                .ok_or_else(|| config_error("diagnostic session not found"))?;
            session.validate()?;
            let ids = session
                .turns
                .iter()
                .map(|t| t.binding.run_id.as_str())
                .chain(session.active.iter().map(|t| t.run_id.as_str()))
                .collect::<Vec<_>>();
            let children = load_children(runs, &ids).await?;
            TraceSnapshot::session(&namespace, &session, &children)
        }
    }
}
async fn load_children(runs: &dyn RunStore, ids: &[&str]) -> Result<Vec<RunState>, RuntimeError> {
    if ids.len() > 256 {
        return Err(config_error(
            "diagnostics support at most 256 children; inspect individual runs",
        ));
    }
    let mut result = Vec::new();
    let mut bytes = 0usize;
    for id in ids {
        if let Some(state) = runs.inspect(id).await? {
            bytes = bytes.saturating_add(
                serde_json::to_vec(&state)
                    .map_err(crate::runtime::store::storage)?
                    .len(),
            );
            if bytes > 64 * 1024 * 1024 {
                return Err(config_error(
                    "diagnostic child snapshots exceed 64 MiB; inspect individual runs",
                ));
            }
            result.push(state);
        }
    }
    Ok(result)
}
