use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    graph::{config, valid_name},
    Condition, FnWorkflowTask, NodeContext, NodeOutcome, TaskDefinition, TaskRecovery,
    WorkflowDefinition, WorkflowNode, WorkflowTasks,
};
use crate::runtime::{state::fingerprint, RunConfig, RuntimeError, ToolEffect};
use crate::CancelToken;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPlanTask {
    pub id: String,
    pub instructions: String,
    #[serde(default)]
    pub input: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPlan {
    pub tasks: Vec<PeerPlanTask>,
}

impl PeerPlan {
    pub fn schema(max_tasks: usize) -> Value {
        json!({"type":"object","properties":{"tasks":{"type":"array","minItems":1,"maxItems":max_tasks,"items":{"type":"object","properties":{"id":{"type":"string","pattern":"^[A-Za-z0-9_-]{1,64}$"},"instructions":{"type":"string","minLength":1},"input":{}},"required":["id","instructions"],"additionalProperties":false}}},"required":["tasks"],"additionalProperties":false})
    }
    fn validate(&self, max_tasks: usize) -> Result<(), RuntimeError> {
        let ids = self
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if self.tasks.is_empty()
            || self.tasks.len() > max_tasks
            || ids.len() != self.tasks.len()
            || self
                .tasks
                .iter()
                .any(|task| !valid_name(&task.id) || task.instructions.trim().is_empty())
        {
            return Err(config(
                "PEER plan needs bounded, uniquely named tasks with instructions",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerReview {
    pub approved: bool,
    pub feedback: String,
}
impl PeerReview {
    pub fn schema() -> Value {
        json!({"type":"object","properties":{"approved":{"type":"boolean"},"feedback":{"type":"string"}},"required":["approved","feedback"],"additionalProperties":false})
    }
}

/// Unrolls a bounded PEER-style DAG: plan, execute each task once in parallel,
/// write, review, then revise only the draft. Effects belong to executors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSpec {
    pub planner: String,
    pub executor: String,
    pub writer: String,
    pub reviewer: String,
    pub max_tasks: usize,
    /// Zero still performs one draft/review; n permits n rewrites.
    pub max_revisions: usize,
}
impl Default for PeerSpec {
    fn default() -> Self {
        Self {
            planner: "planner".into(),
            executor: "executor".into(),
            writer: "writer".into(),
            reviewer: "reviewer".into(),
            max_tasks: 4,
            max_revisions: 2,
        }
    }
}

impl PeerSpec {
    pub fn install(
        &self,
        tasks: &mut WorkflowTasks<'_>,
    ) -> Result<WorkflowDefinition, RuntimeError> {
        if self.max_tasks == 0 || self.max_tasks > 32 || self.max_revisions > 16 {
            return Err(config("PEER needs 1..=32 tasks and at most 16 revisions"));
        }
        for role in [&self.planner, &self.executor, &self.writer, &self.reviewer] {
            if !tasks.tasks.contains_key(role) {
                return Err(config("PEER role is not registered"));
            }
        }
        for role in [&self.planner, &self.writer, &self.reviewer] {
            let definition = &tasks.tasks[role].definition;
            if definition.recovery == TaskRecovery::Pure {
                continue;
            }
            if definition.recovery != TaskRecovery::DurableChild
                || definition.config.get("kind").and_then(Value::as_str) != Some("durable_agent")
            {
                return Err(config("PEER planning/writing/review requires pure transforms or native durable agent roles"));
            }
            let run: RunConfig = serde_json::from_value(
                definition.config.get("run").cloned().unwrap_or(Value::Null),
            )
            .map_err(|_| config("PEER role has no frozen run configuration"))?;
            if run
                .tools
                .iter()
                .any(|tool| tool.effect != ToolEffect::ReadOnly)
            {
                return Err(config("PEER planning/writing/review tools must be read-only; put effects in executors"));
            }
        }
        let prefix = format!("peer-{}", &fingerprint(self)?[..12]);
        let plan_task = format!("{prefix}-plan");
        let review_task = format!("{prefix}-review");
        let result_task = format!("{prefix}-result");
        let max_tasks = self.max_tasks;
        let plan_definition = builtin(
            &plan_task,
            json!({"max_tasks":max_tasks,"kind":"validate_plan"}),
        );
        if !install_existing(tasks, &plan_definition)? {
            tasks.register(FnWorkflowTask::new(
                plan_definition,
                move |context: NodeContext, _: CancelToken| async move {
                    let plan: PeerPlan = serde_json::from_value(
                        context
                            .dependencies
                            .get("raw-plan")
                            .cloned()
                            .ok_or_else(|| config("PEER planner output is missing"))?,
                    )
                    .map_err(|error| config(&format!("invalid PEER plan: {error}")))?;
                    plan.validate(max_tasks)?;
                    Ok(NodeOutcome::pure(
                        serde_json::to_value(plan).map_err(|e| config(&e.to_string()))?,
                    ))
                },
            ))?;
        }
        let review_definition = builtin(&review_task, json!({"kind":"validate_review"}));
        if !install_existing(tasks, &review_definition)? {
            tasks.register(FnWorkflowTask::new(
                review_definition,
                |context: NodeContext, _: CancelToken| async move {
                    let value = context
                        .dependencies
                        .get("review")
                        .cloned()
                        .ok_or_else(|| config("PEER reviewer output is missing"))?;
                    let review: PeerReview = serde_json::from_value(value)
                        .map_err(|error| config(&format!("invalid PEER review: {error}")))?;
                    Ok(NodeOutcome::pure(
                        serde_json::to_value(review).map_err(|e| config(&e.to_string()))?,
                    ))
                },
            ))?;
        }
        let max_revisions = self.max_revisions;
        let result_definition = builtin(
            &result_task,
            json!({"kind":"select_reviewed_result","max_revisions":max_revisions}),
        );
        if !install_existing(tasks, &result_definition)? {
            tasks.register(FnWorkflowTask::new(
                result_definition,
                move |context: NodeContext, _: CancelToken| async move {
                    for revision in 0..=max_revisions {
                        if let Some(value) = context
                            .dependencies
                            .get(&format!("check-review-{revision}"))
                        {
                            let review: PeerReview = serde_json::from_value(value.clone())
                                .map_err(|error| config(&error.to_string()))?;
                            if review.approved {
                                let answer = context
                                    .dependencies
                                    .get(&format!("draft-{revision}"))
                                    .cloned()
                                    .ok_or_else(|| config("approved PEER review has no draft"))?;
                                return Ok(NodeOutcome::pure(
                                    json!({"answer":answer,"revision":revision,"review":review}),
                                ));
                            }
                        }
                    }
                    Err(config(
                        "PEER revision limit reached without an approved draft",
                    ))
                },
            ))?;
        }
        let mut nodes = vec![
            WorkflowNode::new("raw-plan", &self.planner),
            WorkflowNode::new("plan", plan_task).after(["raw-plan"]),
        ];
        let mut worker_ids = Vec::new();
        for index in 0..self.max_tasks {
            let id = format!("worker-{index}");
            worker_ids.push(id.clone());
            nodes.push(
                WorkflowNode::new(id, &self.executor)
                    .after(["plan"])
                    .when(Condition::Exists {
                        node: "plan".into(),
                        pointer: format!("/tasks/{index}"),
                    })
                    .select("task", "plan", format!("/tasks/{index}"))
                    .with_params(json!({"index":index})),
            );
        }
        let mut final_inputs = Vec::new();
        for revision in 0..=self.max_revisions {
            let draft = format!("draft-{revision}");
            let review = format!("review-{revision}");
            let check = format!("check-review-{revision}");
            let mut dependencies = worker_ids.clone();
            dependencies.push("plan".into());
            let mut writer = WorkflowNode::new(&draft, &self.writer)
                .allow_skipped()
                .with_params(json!({"revision":revision}));
            if revision > 0 {
                let previous = format!("check-review-{}", revision - 1);
                dependencies.push(format!("draft-{}", revision - 1));
                dependencies.push(previous.clone());
                writer = writer.when(Condition::Equals {
                    node: previous,
                    pointer: "/approved".into(),
                    value: Value::Bool(false),
                });
            }
            nodes.push(writer.after(dependencies));
            nodes.push(
                WorkflowNode::new(&review, &self.reviewer)
                    .after([draft.clone()])
                    .select("draft", &draft, "")
                    .with_params(json!({"revision":revision})),
            );
            nodes.push(
                WorkflowNode::new(&check, &review_task)
                    .after([review.clone()])
                    .select("review", review, ""),
            );
            final_inputs.push(draft);
            final_inputs.push(check);
        }
        nodes.push(
            WorkflowNode::new("result", result_task)
                .after(final_inputs)
                .allow_skipped(),
        );
        Ok(WorkflowDefinition::new(nodes))
    }
}

fn builtin(name: &str, config_value: Value) -> TaskDefinition {
    TaskDefinition {
        name: name.into(),
        version: "peer-v1".into(),
        recovery: TaskRecovery::Pure,
        max_cost_micros: Some(0),
        config: config_value,
    }
}
fn install_existing(
    tasks: &WorkflowTasks<'_>,
    definition: &TaskDefinition,
) -> Result<bool, RuntimeError> {
    match tasks.tasks.get(&definition.name) {
        Some(existing) if existing.definition != *definition => {
            Err(config("PEER builtin name conflicts with a registered task"))
        }
        Some(_) => Ok(true),
        None => Ok(false),
    }
}
