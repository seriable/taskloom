use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::RuntimeError;

/// Conditions read only declared, completed dependencies. Missing/skipped
/// output is absent; it is not replaced with invented data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Condition {
    Always,
    Exists {
        node: String,
        pointer: String,
    },
    Equals {
        node: String,
        pointer: String,
        value: Value,
    },
    All {
        conditions: Vec<Condition>,
    },
    Not {
        condition: Box<Condition>,
    },
}

impl Condition {
    pub(crate) fn evaluate(&self, outputs: &BTreeMap<String, Value>) -> bool {
        match self {
            Self::Always => true,
            Self::Exists { node, pointer } => outputs
                .get(node)
                .and_then(|value| value.pointer(pointer))
                .is_some(),
            Self::Equals {
                node,
                pointer,
                value,
            } => outputs.get(node).and_then(|data| data.pointer(pointer)) == Some(value),
            Self::All { conditions } => conditions
                .iter()
                .all(|condition| condition.evaluate(outputs)),
            Self::Not { condition } => !condition.evaluate(outputs),
        }
    }
    fn validate(&self, dependencies: &BTreeSet<&str>, depth: usize) -> Result<(), RuntimeError> {
        if depth > 32 {
            return Err(config("workflow condition nesting exceeds 32"));
        }
        match self {
            Self::Always => {}
            Self::Exists { node, pointer } | Self::Equals { node, pointer, .. } => {
                if !dependencies.contains(node.as_str()) || !valid_pointer(pointer) {
                    return Err(config(
                        "condition must reference a declared dependency and JSON pointer",
                    ));
                }
            }
            Self::All { conditions } => {
                for condition in conditions {
                    condition.validate(dependencies, depth + 1)?;
                }
            }
            Self::Not { condition } => condition.validate(dependencies, depth + 1)?,
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRef {
    pub node: String,
    pub pointer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub task: String,
    pub depends_on: Vec<String>,
    pub condition: Condition,
    /// Allows joins after optional branches. Failed dependencies never count
    /// as optional successful input.
    pub allow_skipped_dependencies: bool,
    /// Empty passes all completed dependencies by node id. Nonempty maps
    /// explicit aliases to selected dependency values.
    pub select: BTreeMap<String, OutputRef>,
    pub params: Value,
}

impl WorkflowNode {
    pub fn new(id: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            task: task.into(),
            depends_on: Vec::new(),
            condition: Condition::Always,
            allow_skipped_dependencies: false,
            select: BTreeMap::new(),
            params: Value::Null,
        }
    }
    pub fn after(mut self, dependencies: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.depends_on = dependencies.into_iter().map(Into::into).collect();
        self
    }
    pub fn when(mut self, condition: Condition) -> Self {
        self.condition = condition;
        self
    }
    pub fn allow_skipped(mut self) -> Self {
        self.allow_skipped_dependencies = true;
        self
    }
    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }
    pub fn select(
        mut self,
        alias: impl Into<String>,
        node: impl Into<String>,
        pointer: impl Into<String>,
    ) -> Self {
        self.select.insert(
            alias.into(),
            OutputRef {
                node: node.into(),
                pointer: pointer.into(),
            },
        );
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub nodes: Vec<WorkflowNode>,
}

impl WorkflowDefinition {
    pub fn new(nodes: impl IntoIterator<Item = WorkflowNode>) -> Self {
        Self {
            nodes: nodes.into_iter().collect(),
        }
    }
    pub fn validate(&self, limits: &WorkflowLimits) -> Result<(), RuntimeError> {
        if self.nodes.is_empty() || self.nodes.len() > limits.max_nodes {
            return Err(config("workflow has no nodes or exceeds the node limit"));
        }
        let ids = self
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>();
        if ids.len() != self.nodes.len() {
            return Err(config("duplicate workflow node id"));
        }
        for node in &self.nodes {
            if !valid_name(&node.id) || !valid_name(&node.task) {
                return Err(config(
                    "node/task names need 1..=64 ASCII letters, digits, '_' or '-'",
                ));
            }
            let dependencies = node
                .depends_on
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if dependencies.len() != node.depends_on.len()
                || dependencies.contains(node.id.as_str())
                || dependencies.iter().any(|id| !ids.contains(id))
            {
                return Err(config("invalid workflow dependency"));
            }
            node.condition.validate(&dependencies, 0)?;
            for (alias, selection) in &node.select {
                if alias.is_empty()
                    || !dependencies.contains(selection.node.as_str())
                    || !valid_pointer(&selection.pointer)
                {
                    return Err(config(
                        "input selector must reference a declared dependency",
                    ));
                }
            }
        }
        let mut done = BTreeSet::new();
        while done.len() < self.nodes.len() {
            let before = done.len();
            for node in &self.nodes {
                if node.depends_on.iter().all(|id| done.contains(id.as_str())) {
                    done.insert(node.id.as_str());
                }
            }
            if before == done.len() {
                return Err(config("workflow dependency graph has a cycle"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLimits {
    pub max_nodes: usize,
    pub max_parallel: usize,
    /// Invocations, including child-run resumes, not just distinct nodes.
    pub max_dispatches: usize,
    pub max_duration: Duration,
    /// Bounded time for child cancellation/checkpointing after interruption.
    pub shutdown_grace: Duration,
    pub max_output_bytes: usize,
    pub max_cost_micros: Option<u64>,
}

impl Default for WorkflowLimits {
    fn default() -> Self {
        Self {
            max_nodes: 128,
            max_parallel: 4,
            max_dispatches: 1024,
            max_duration: Duration::from_secs(600),
            shutdown_grace: Duration::from_secs(5),
            max_output_bytes: 128 * 1024,
            max_cost_micros: None,
        }
    }
}
impl WorkflowLimits {
    pub(crate) fn validate(&self) -> Result<(), RuntimeError> {
        if self.max_nodes == 0
            || self.max_parallel == 0
            || self.max_output_bytes == 0
            || tokio::time::Instant::now()
                .checked_add(self.max_duration)
                .is_none()
            || tokio::time::Instant::now()
                .checked_add(self.shutdown_grace)
                .is_none()
        {
            return Err(config(
                "invalid workflow node, parallelism, output or duration limits",
            ));
        }
        Ok(())
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_pointer(pointer: &str) -> bool {
    pointer.is_empty() || pointer.starts_with('/')
}
pub(crate) fn config(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(detail.into())
}
