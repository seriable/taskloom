#![cfg(feature = "runtime")]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::workflow::*;
use taskloom::runtime::*;
use taskloom::CancelToken;

#[derive(Default)]
struct ModelFixture(AtomicUsize);
impl Model for ModelFixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(3).await?;
            self.0.fetch_add(1, Ordering::SeqCst);
            let turn = if request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }))
            {
                AssistantTurn::text("done")
            } else {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "effect-1".into(),
                        name: "effect".into(),
                        arguments: json!({"value":7}),
                    }],
                }
            };
            reservation.settle(1);
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}

fn registry(count: Arc<AtomicUsize>, crash_marker: Option<PathBuf>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {
        name: "effect".into(), description: "effect v1".into(),
        effect: ToolEffect::NonIdempotent, permissions: BTreeSet::new(),
        input_schema: json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}),
    }, move |args: Value, _: ToolContext| {
        let count = Arc::clone(&count);
        let marker = crash_marker.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if let Some(marker) = marker {
                // The external effect is durable before forcibly killing the
                // whole process, including both child and workflow controllers.
                let file = std::fs::OpenOptions::new().write(true).create_new(true).open(marker).unwrap();
                file.sync_all().unwrap();
                std::process::exit(37);
            }
            Ok(args["value"].clone())
        }
    })).unwrap();
    tools
}
fn run_limits() -> RunLimits {
    RunLimits {
        max_cost_micros: Some(10),
        ..Default::default()
    }
}
fn approval() -> ApprovalDecision {
    ApprovalDecision {
        approved: true,
        actor: "operator".into(),
        note: "checked exact arguments".into(),
    }
}
fn graph() -> WorkflowDefinition {
    WorkflowDefinition::new([WorkflowNode::new("worker", "agent")])
}
fn register<'a, 'm: 'a>(agent: &'a AgentLoop<'m>, store: &'a dyn RunStore) -> WorkflowTasks<'a> {
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(
            AgentWorkflowTask::new("agent", agent, store, "execute", WorkflowOutput::Text).unwrap(),
        )
        .unwrap();
    tasks
}

#[tokio::test]
async fn native_child_approval_reuses_exact_input_and_never_replays_finished_effects() {
    let dir = tempfile::tempdir().unwrap();
    let runs = FileRunStore::new(dir.path().join("runs"));
    let workflows = FileWorkflowStore::new(dir.path().join("workflows"));
    let model = ModelFixture::default();
    let count = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&count), None);
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_policy(ToolPolicy::default().allow("effect"))
        .with_limits(run_limits());
    let tasks = register(&agent, &runs);
    let runner = WorkflowRunner::new(&tasks, &workflows);
    let waiting = runner
        .start("approval", graph(), json!({"goal":"run"}))
        .await
        .unwrap();
    assert_eq!(waiting.status, WorkflowStatus::Waiting);
    let id = &waiting
        .node("worker")
        .unwrap()
        .child
        .as_ref()
        .unwrap()
        .run_id;
    let child = runs.inspect(id).await.unwrap().unwrap();
    assert_eq!(child.status, RunStatus::WaitingApproval);
    assert_eq!(waiting.budget().unwrap().committed_micros, 1);
    assert_eq!(waiting.budget().unwrap().reserved_micros, 9);
    assert_eq!(
        runner.resume("approval").await.unwrap().status,
        WorkflowStatus::Waiting
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
    DurableAgent::new(&agent, &runs)
        .decide(
            id,
            &child.pending_approval().unwrap().request_id,
            approval(),
        )
        .await
        .unwrap();
    let completed = runner.resume("approval").await.unwrap();
    assert_eq!(completed.status, WorkflowStatus::Completed);
    assert_eq!(completed.output("worker"), Some(&json!("done")));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(completed.budget().unwrap().committed_micros, 2);
    assert_eq!(completed.budget().unwrap().reserved_micros, 0);
    let final_child = runs.inspect(id).await.unwrap().unwrap();
    assert_eq!(
        child.report.messages[..child.initial_message_count],
        final_child.report.messages[..final_child.initial_message_count]
    );
    runner.resume("approval").await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn owned_child_is_waiting_and_keeps_its_full_budget_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let runs = FileRunStore::new(dir.path().join("runs"));
    let workflows = FileWorkflowStore::new(dir.path().join("workflows"));
    let model = ModelFixture::default();
    let tools = registry(Arc::new(AtomicUsize::new(0)), None);
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_policy(ToolPolicy::default().allow("effect"))
        .with_limits(run_limits());
    let tasks = register(&agent, &runs);
    let runner = WorkflowRunner::new(&tasks, &workflows);
    let waiting = runner.start("busy", graph(), Value::Null).await.unwrap();
    let id = &waiting
        .node("worker")
        .unwrap()
        .child
        .as_ref()
        .unwrap()
        .run_id;
    let lease = runs.acquire(id).await.unwrap();
    let busy = runner.resume("busy").await.unwrap();
    assert_eq!(busy.status, WorkflowStatus::Waiting);
    let reference = busy.node("worker").unwrap().child.as_ref().unwrap();
    assert!(reference.status.is_none());
    assert!(!busy.node("worker").unwrap().needs_recovery);
    assert_eq!(
        busy.budget().unwrap().committed_micros + busy.budget().unwrap().reserved_micros,
        10
    );
    drop(lease);
    let waiting = runner.resume("busy").await.unwrap();
    assert_eq!(
        waiting
            .node("worker")
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .status,
        Some(RunStatus::WaitingApproval)
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
}

#[test]
fn workflow_crash_worker() {
    let Ok(root) = std::env::var("TASKLOOM_WORKFLOW_CRASH_ROOT") else {
        return;
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let root = PathBuf::from(root);
            let runs = FileRunStore::new(root.join("runs"));
            let workflows = FileWorkflowStore::new(root.join("workflows"));
            let model = ModelFixture::default();
            let tools = registry(
                Arc::new(AtomicUsize::new(0)),
                Some(root.join("external-effect")),
            );
            let agent = AgentLoop::new(&model, &tools, "fixture")
                .with_policy(ToolPolicy::default().allow("effect"))
                .with_limits(run_limits());
            let tasks = register(&agent, &runs);
            let runner = WorkflowRunner::new(&tasks, &workflows);
            let waiting = runner.start("crash", graph(), Value::Null).await.unwrap();
            let id = &waiting
                .node("worker")
                .unwrap()
                .child
                .as_ref()
                .unwrap()
                .run_id;
            let child = runs.inspect(id).await.unwrap().unwrap();
            DurableAgent::new(&agent, &runs)
                .decide(
                    id,
                    &child.pending_approval().unwrap().request_id,
                    approval(),
                )
                .await
                .unwrap();
            runner.resume("crash").await.unwrap();
            panic!("effect should terminate this child process");
        });
}

#[tokio::test]
async fn process_death_recovers_child_effect_only_after_operator_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "workflow_crash_worker", "--nocapture"])
        .env("TASKLOOM_WORKFLOW_CRASH_ROOT", dir.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(37));
    assert!(dir.path().join("external-effect").exists());
    let runs = FileRunStore::new(dir.path().join("runs"));
    let workflows = FileWorkflowStore::new(dir.path().join("workflows"));
    let model = ModelFixture::default();
    let count = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&count), None);
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_policy(ToolPolicy::default().allow("effect"))
        .with_limits(run_limits());
    let tasks = register(&agent, &runs);
    let runner = WorkflowRunner::new(&tasks, &workflows);
    let recovery = runner.resume("crash").await.unwrap();
    assert_eq!(recovery.status, WorkflowStatus::NeedsRecovery);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    let id = &recovery
        .node("worker")
        .unwrap()
        .child
        .as_ref()
        .unwrap()
        .run_id;
    DurableAgent::new(&agent, &runs)
        .reconcile(
            id,
            "effect-1",
            RecoveryDecision::RecordResult {
                output: json!(7),
                is_error: false,
                actor: "operator".into(),
                note: "confirmed external-effect marker".into(),
            },
        )
        .await
        .unwrap();
    let done = runner.resume("crash").await.unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!(done.output("worker"), Some(&json!("done")));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
}

fn paid_definition(name: &str) -> TaskDefinition {
    TaskDefinition {
        name: name.into(),
        version: "1".into(),
        recovery: TaskRecovery::Manual,
        max_cost_micros: Some(10),
        config: Value::Null,
    }
}

#[tokio::test]
async fn global_budget_reserves_full_node_caps_then_releases_unused_funds() {
    for (id, cost, expected, calls) in [
        ("settle", Some(1), WorkflowStatus::Completed, 2),
        ("spent", Some(10), WorkflowStatus::BudgetLimit, 1),
        ("unknown", None, WorkflowStatus::BudgetLimit, 1),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = FileWorkflowStore::new(dir.path());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let count = Arc::new(AtomicUsize::new(0));
        let mut tasks = WorkflowTasks::new();
        let (a, p, c) = (Arc::clone(&active), Arc::clone(&peak), Arc::clone(&count));
        tasks
            .register(FnWorkflowTask::new(
                paid_definition("paid"),
                move |_: NodeContext, _: CancelToken| {
                    let (a, p, c) = (Arc::clone(&a), Arc::clone(&p), Arc::clone(&c));
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        p.fetch_max(a.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        a.fetch_sub(1, Ordering::SeqCst);
                        let mut result = NodeOutcome::pure(json!("done"));
                        result.cost_micros = cost;
                        Ok(result)
                    }
                },
            ))
            .unwrap();
        let state = WorkflowRunner::new(&tasks, &store)
            .with_limits(WorkflowLimits {
                max_parallel: 2,
                max_cost_micros: Some(11),
                ..Default::default()
            })
            .start(
                id,
                WorkflowDefinition::new([
                    WorkflowNode::new("a", "paid"),
                    WorkflowNode::new("b", "paid"),
                ]),
                Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(state.status, expected);
        assert_eq!(count.load(Ordering::SeqCst), calls);
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.budget().unwrap().uncertain_micros,
            if cost.is_none() { 10 } else { 0 }
        );
    }
}

#[tokio::test]
async fn ignored_cancellation_requires_manual_reconciliation_and_never_repeats_effect() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let cancel = CancelToken::new();
    let signal = cancel.clone();
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            paid_definition("paid"),
            move |_: NodeContext, _: CancelToken| {
                let signal = signal.clone();
                async move {
                    signal.cancel();
                    std::future::pending().await
                }
            },
        ))
        .unwrap();
    let runner = WorkflowRunner::new(&tasks, &store).with_limits(WorkflowLimits {
        shutdown_grace: Duration::from_millis(5),
        ..Default::default()
    });
    let state = WorkflowRunner::new(&tasks, &store)
        .with_cancel(&cancel)
        .with_limits(WorkflowLimits {
            shutdown_grace: Duration::from_millis(5),
            ..Default::default()
        })
        .start(
            "cancel",
            WorkflowDefinition::new([WorkflowNode::new("a", "paid")]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert_eq!(state.node("a").unwrap().status, NodeStatus::Uncertain);
    assert_eq!(state.budget().unwrap().uncertain_micros, 10);
    runner
        .reconcile(
            "cancel",
            "a",
            NodeRecovery::RecordOutput {
                output: json!("observed"),
                cost_micros: Some(2),
                usage: Default::default(),
                actor: "operator".into(),
                note: "confirmed externally".into(),
            },
        )
        .await
        .unwrap();
    let settled = runner.resume("cancel").await.unwrap();
    assert_eq!(settled.status, WorkflowStatus::Cancelled);
    assert_eq!(settled.node("a").unwrap().dispatches, 1);
    assert_eq!(settled.budget().unwrap().committed_micros, 2);
}

struct FailingStore {
    inner: FileWorkflowStore,
    fail_status: NodeStatus,
    armed: Arc<std::sync::atomic::AtomicBool>,
}
struct FailingLease {
    inner: Arc<dyn WorkflowLease>,
    fail_status: NodeStatus,
    armed: Arc<std::sync::atomic::AtomicBool>,
}
impl WorkflowStore for FailingStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn WorkflowLease>, RuntimeError>> {
        Box::pin(async move {
            Ok(Arc::new(FailingLease {
                inner: self.inner.acquire(id).await?,
                fail_status: self.fail_status,
                armed: Arc::clone(&self.armed),
            }) as Arc<dyn WorkflowLease>)
        })
    }
    fn inspect<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<WorkflowState>, RuntimeError>> {
        self.inner.inspect(id)
    }
}
impl WorkflowLease for FailingLease {
    fn load(&self) -> BoxFuture<'_, Result<Option<WorkflowState>, RuntimeError>> {
        self.inner.load()
    }
    fn save<'a>(&'a self, state: &'a WorkflowState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            if state.nodes.iter().any(|n| n.status == self.fail_status)
                && self.armed.swap(false, Ordering::SeqCst)
            {
                return Err(RuntimeError::Storage(
                    "injected workflow save failure".into(),
                ));
            }
            self.inner.save(state).await
        })
    }
}

#[tokio::test]
async fn storage_failure_before_intent_prevents_callback_and_after_result_reuses_child() {
    for fail_status in [NodeStatus::Running, NodeStatus::Complete] {
        let dir = tempfile::tempdir().unwrap();
        let runs = FileRunStore::new(dir.path().join("runs"));
        let store = FailingStore {
            inner: FileWorkflowStore::new(dir.path().join("workflows")),
            fail_status,
            armed: Arc::new(std::sync::atomic::AtomicBool::new(
                fail_status == NodeStatus::Running,
            )),
        };
        let model = ModelFixture::default();
        let count = Arc::new(AtomicUsize::new(0));
        let tools = registry(Arc::clone(&count), None);
        let agent = AgentLoop::new(&model, &tools, "fixture")
            .with_policy(ToolPolicy::default().allow("effect"))
            .with_limits(run_limits());
        let tasks = register(&agent, &runs);
        let runner = WorkflowRunner::new(&tasks, &store);
        if fail_status == NodeStatus::Running {
            assert!(runner.start("failure", graph(), Value::Null).await.is_err());
            assert_eq!(model.0.load(Ordering::SeqCst), 0);
            assert_eq!(count.load(Ordering::SeqCst), 0);
        } else {
            runner.start("failure", graph(), Value::Null).await.unwrap();
        }
        let waiting = runner.resume("failure").await.unwrap();
        let id = &waiting
            .node("worker")
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .run_id;
        let child = runs.inspect(id).await.unwrap().unwrap();
        DurableAgent::new(&agent, &runs)
            .decide(
                id,
                &child.pending_approval().unwrap().request_id,
                approval(),
            )
            .await
            .unwrap();
        if fail_status == NodeStatus::Complete {
            store.armed.store(true, Ordering::SeqCst);
            assert!(runner.resume("failure").await.is_err());
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert_eq!(
                store.inspect("failure").await.unwrap().unwrap().status,
                WorkflowStatus::Running
            );
        }
        let done = runner.resume("failure").await.unwrap();
        assert_eq!(done.status, WorkflowStatus::Completed);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(model.0.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn workflow_ownership_revision_and_configuration_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            TaskDefinition {
                name: "pure".into(),
                version: "1".into(),
                recovery: TaskRecovery::Pure,
                max_cost_micros: Some(0),
                config: Value::Null,
            },
            |_: NodeContext, _: CancelToken| async { Ok(NodeOutcome::pure(json!(1))) },
        ))
        .unwrap();
    let pause = CancelToken::new();
    pause.cancel();
    let state = WorkflowRunner::new(&tasks, &store)
        .with_pause(&pause)
        .start(
            "state",
            WorkflowDefinition::new([WorkflowNode::new("a", "pure")]),
            Value::Null,
        )
        .await
        .unwrap();
    let lease = store.acquire("state").await.unwrap();
    assert!(matches!(
        store.acquire("state").await,
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(
        store.inspect("state").await.unwrap().unwrap().revision,
        state.revision
    );
    let mut newer = state.clone();
    newer.revision += 1;
    lease.save(&newer).await.unwrap();
    // A stale save is a conflict, never a silent success.
    assert!(matches!(
        lease.save(&state).await,
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(
        lease.load().await.unwrap().unwrap().revision,
        newer.revision
    );
    let mut conflict = newer.clone();
    conflict.detail = Some("changed at same revision".into());
    assert!(lease.save(&conflict).await.is_err());
    let mut corrupt = newer.clone();
    corrupt.version += 1;
    corrupt.revision += 1;
    assert!(lease.save(&corrupt).await.is_err());
    drop(lease);
    assert!(WorkflowRunner::new(&tasks, &store)
        .with_limits(WorkflowLimits {
            max_parallel: 1,
            ..Default::default()
        })
        .resume("state")
        .await
        .is_err());
    assert_eq!(
        WorkflowRunner::new(&tasks, &store)
            .resume("state")
            .await
            .unwrap()
            .status,
        WorkflowStatus::Completed
    );
}

#[tokio::test]
async fn peer_refuses_mutating_writer_tools() {
    let dir = tempfile::tempdir().unwrap();
    let runs = FileRunStore::new(dir.path());
    let model = ModelFixture::default();
    let tools = registry(Arc::new(AtomicUsize::new(0)), None);
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_policy(ToolPolicy::default().allow("effect"))
        .with_limits(run_limits());
    let mut tasks = WorkflowTasks::new();
    for name in ["planner", "executor", "writer", "reviewer"] {
        tasks
            .register(
                AgentWorkflowTask::new(name, &agent, &runs, name, WorkflowOutput::Text).unwrap(),
            )
            .unwrap();
    }
    assert!(PeerSpec::default().install(&mut tasks).is_err());
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
}
