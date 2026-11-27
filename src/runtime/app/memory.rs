use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{config::config_error, Application};
use crate::runtime::{memory::*, *};

/// Single-operator host scope. Multi-user deployments must obtain access from
/// authenticated server context instead; request/model JSON never selects it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppMemory {
    pub scope: MemoryScope,
    pub actor: String,
    #[serde(default)]
    pub curate: bool,
    #[serde(default)]
    pub recall: MemoryRecallPolicy,
}
impl AppMemory {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.access()?;
        self.recall.validate()
    }
    pub fn access(&self) -> Result<MemoryAccess, RuntimeError> {
        if self.curate {
            MemoryAccess::curator(self.scope.clone(), self.actor.clone())
        } else {
            MemoryAccess::reader(self.scope.clone(), self.actor.clone())
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReview {
    pub expected_revision: u64,
    pub id: String,
    pub accept: bool,
    #[serde(default)]
    pub replace: bool,
    pub note: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEdit {
    pub expected_revision: u64,
    pub id: String,
    pub value: String,
    pub note: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryForget {
    pub expected_revision: u64,
    pub kind: MemoryKind,
    pub key: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryExtract {
    pub expected_revision: u64,
    pub session_id: String,
    pub turn_id: String,
    pub request_id: String,
}

impl Application {
    pub fn memory_config(&self) -> Result<&AppMemory, RuntimeError> {
        self.config.memory.as_ref().ok_or_else(|| {
            RuntimeError::Permission("long-term memory is not enabled by the host".into())
        })
    }
    pub async fn memory_snapshot(&self) -> Result<MemorySnapshot, RuntimeError> {
        MemoryManager::new(&self.memory)
            .snapshot(&self.memory_config()?.access()?)
            .await
    }
    pub async fn recall_memory(&self, query: &str) -> Result<MemoryRecall, RuntimeError> {
        let config = self.memory_config()?;
        MemoryManager::new(&self.memory)
            .recall(&config.access()?, query, &config.recall)
            .await
    }
    pub async fn review_memory(&self, input: MemoryReview) -> Result<MemorySnapshot, RuntimeError> {
        MemoryManager::new(&self.memory)
            .review(
                &self.memory_config()?.access()?,
                input.expected_revision,
                &input.id,
                MemoryReviewDecision {
                    accept: input.accept,
                    replace: input.replace,
                    note: input.note,
                },
            )
            .await
    }
    pub async fn edit_memory(&self, input: MemoryEdit) -> Result<MemorySnapshot, RuntimeError> {
        MemoryManager::new(&self.memory)
            .edit(
                &self.memory_config()?.access()?,
                input.expected_revision,
                &input.id,
                &input.value,
                &input.note,
            )
            .await
    }
    pub async fn forget_memory(&self, input: MemoryForget) -> Result<MemorySnapshot, RuntimeError> {
        MemoryManager::new(&self.memory)
            .forget(
                &self.memory_config()?.access()?,
                input.expected_revision,
                input.kind,
                &input.key,
            )
            .await
    }
    pub async fn extract_memory(
        self: &Arc<Self>,
        input: MemoryExtract,
    ) -> Result<MemoryExtraction, RuntimeError> {
        let access = self.memory_config()?.access()?;
        access.require_curator()?;
        super::valid_id(&input.request_id)?;
        super::valid_id(&input.session_id)?;
        super::valid_id(&input.turn_id)?;
        let key = format!("memory:{}", input.request_id);
        let job = self.admit_key(&key)?;
        let mut guard = ExtractionAdmission {
            app: self.clone(),
            key,
            finished: false,
        };
        let result = async {
            let session = self.session(&input.session_id).await?;
            let agent = self.agent().with_cancel(&job.cancel);
            MemoryManager::new(&self.memory)
                .extract_from_turn(
                    &access,
                    &session,
                    &MemoryExtractionRequest {
                        expected_revision: input.expected_revision,
                        turn_id: input.turn_id.clone(),
                        request_id: input.request_id.clone(),
                    },
                    &agent,
                    &self.runs,
                )
                .await
        }
        .await;
        // Preserve the actual error for the caller, and release the global
        // slot even when an HTTP caller drops the future during generation.
        let status = result
            .as_ref()
            .map(|_| ())
            .map_err(|e| config_error(&e.to_string()));
        self.finish_job(&guard.key, status);
        guard.finished = true;
        result
    }
    pub async fn start_session_turn_with_memory(
        self: &Arc<Self>,
        id: &str,
        turn: &str,
        prompt: &str,
    ) -> Result<RunState, RuntimeError> {
        super::valid_id(id)?;
        super::valid_id(turn)?;
        let config = self.memory_config()?;
        let access = config.access()?;
        let key = format!("session:{id}");
        let job = self.admit_key(&key)?;
        let result = async {
            let sessions = SessionManager::new(&self.sessions);
            let prepared = MemoryManager::new(&self.memory)
                .begin_turn_with_recall(&access, &sessions, id, turn, prompt, &config.recall)
                .await?;
            if let Some(state) = self.runs.inspect(&prepared.run_id).await? {
                return Ok(state);
            }
            DurableAgent::new(&self.agent(), &self.runs)
                .with_trace_binding(crate::runtime::observability::TraceBinding::session(
                    id, turn,
                ))?
                .create(&prepared.run_id, prepared.messages)
                .await
        }
        .await;
        match result {
            Ok(state) => {
                self.spawn_session(id, turn, job);
                Ok(state)
            }
            Err(error) => {
                self.jobs()?.remove(&key);
                Err(error)
            }
        }
    }
}
struct ExtractionAdmission {
    app: Arc<Application>,
    key: String,
    finished: bool,
}
impl Drop for ExtractionAdmission {
    fn drop(&mut self) {
        if !self.finished {
            self.app.finish_job(
                &self.key,
                Err(config_error(
                    "memory extraction caller dropped; inspect its durable run before retrying",
                )),
            );
        }
    }
}
