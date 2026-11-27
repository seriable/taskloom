use crate::runtime::{state::fingerprint, workflow::NodeContext, Message, RunState, RuntimeError};
use serde::{Deserialize, Serialize};

/// Host-created correlation metadata, never a tool grant or a model instruction.
/// Managed workflow/session entry points attach it before the first checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TraceBinding {
    Workflow {
        workflow_id: String,
        node_id: String,
    },
    Session {
        session_id: String,
        turn_id: String,
    },
}
impl TraceBinding {
    pub fn workflow(workflow_id: impl Into<String>, node_id: impl Into<String>) -> Self {
        Self::Workflow {
            workflow_id: workflow_id.into(),
            node_id: node_id.into(),
        }
    }
    pub fn session(session_id: impl Into<String>, turn_id: impl Into<String>) -> Self {
        Self::Session {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
        }
    }
    pub(super) fn root(&self) -> (&'static str, &str, &str) {
        match self {
            Self::Workflow {
                workflow_id,
                node_id,
            } => ("workflow", workflow_id, node_id),
            Self::Session {
                session_id,
                turn_id,
            } => ("session", session_id, turn_id),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), RuntimeError> {
        let (_, root, segment) = self.root();
        if root.trim().is_empty() || segment.trim().is_empty() {
            return Err(super::invalid("empty trace parent identity"));
        }
        Ok(())
    }
    pub(crate) fn validate_state(&self, state: &RunState) -> Result<(), RuntimeError> {
        self.validate()?;
        let (kind, root, segment) = self.root();
        let expected = format!("{kind}-{}", fingerprint(&(root, segment))?);
        if state.report.run_id != expected {
            return Err(super::invalid(
                "trace parent does not bind this durable run identity",
            ));
        }
        if matches!(self, Self::Workflow { .. }) {
            let Some(Message::User { content }) = state
                .report
                .messages
                .get(state.initial_message_count.saturating_sub(1))
            else {
                return Err(super::invalid("workflow trace has no frozen child input"));
            };
            let context: NodeContext = serde_json::from_str(content)
                .map_err(|_| super::invalid("workflow trace has no exact NodeContext input"))?;
            if context.workflow_id != root
                || context.node_id != segment
                || context.run_id != expected
            {
                return Err(super::invalid(
                    "workflow trace parent differs from frozen child input",
                ));
            }
        }
        Ok(())
    }
}
