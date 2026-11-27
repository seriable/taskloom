#![cfg(feature = "runtime")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use taskloom::runtime::workflow::*;
use taskloom::CancelToken;

fn definition(name: &str, recovery: TaskRecovery) -> TaskDefinition {
    TaskDefinition {
        name: name.into(),
        version: "test-v1".into(),
        recovery,
        max_cost_micros: Some(0),
        config: Value::Null,
    }
}

#[tokio::test]
async fn create_persists_validated_ready_state_without_dispatch_and_releases_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            definition("pure", TaskRecovery::Pure),
            move |_: NodeContext, _: CancelToken| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutcome::pure(json!(7)))
                }
            },
        ))
        .unwrap();
    let graph = WorkflowDefinition::new([WorkflowNode::new("node", "pure")]);
    let runner = WorkflowRunner::new(&tasks, &store);
    let state = runner
        .create("created", graph.clone(), Value::Null)
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Ready);
    assert_eq!(state.revision, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(runner.create("created", graph, Value::Null).await.is_err());
    assert_eq!(
        runner.resume("created").await.unwrap().status,
        WorkflowStatus::Completed
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn parallel_branches_join_exact_outputs_and_skip_false_conditions() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = WorkflowTasks::new();
    for name in ["a", "b"] {
        let barrier = Arc::clone(&barrier);
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        tasks
            .register(FnWorkflowTask::new(
                definition(name, TaskRecovery::Pure),
                move |context: NodeContext, _: CancelToken| {
                    let barrier = Arc::clone(&barrier);
                    let active = Arc::clone(&active);
                    let peak = Arc::clone(&peak);
                    async move {
                        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(count, Ordering::SeqCst);
                        barrier.wait().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(NodeOutcome::pure(
                            json!({"name":context.node_id,"value":context.input["value"]}),
                        ))
                    }
                },
            ))
            .unwrap();
    }
    tasks
        .register(FnWorkflowTask::new(
            definition("join", TaskRecovery::Pure),
            |context: NodeContext, _: CancelToken| async move {
                assert_eq!(context.dependencies["left"], 7);
                assert_eq!(context.dependencies["right"], 7);
                Ok(NodeOutcome::pure(json!({"approved":true,"sum":14})))
            },
        ))
        .unwrap();
    tasks
        .register(FnWorkflowTask::new(
            definition("never", TaskRecovery::Pure),
            |_: NodeContext, _: CancelToken| async {
                panic!("false branch must never execute");
                #[allow(unreachable_code)]
                Ok(NodeOutcome::pure(Value::Null))
            },
        ))
        .unwrap();
    let graph = WorkflowDefinition::new([
        WorkflowNode::new("a", "a"),
        WorkflowNode::new("b", "b"),
        WorkflowNode::new("join", "join")
            .after(["a", "b"])
            .select("left", "a", "/value")
            .select("right", "b", "/value"),
        WorkflowNode::new("skip", "never")
            .after(["join"])
            .when(Condition::Equals {
                node: "join".into(),
                pointer: "/approved".into(),
                value: json!(false),
            }),
    ]);
    let runner = WorkflowRunner::new(&tasks, &store).with_limits(WorkflowLimits {
        max_parallel: 2,
        max_cost_micros: Some(0),
        ..Default::default()
    });
    let state = runner
        .start("parallel", graph, json!({"value":7}))
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(state.output("join").unwrap()["sum"], 14);
    assert_eq!(state.node("skip").unwrap().status, NodeStatus::Skipped);
    state.validate().unwrap();
    assert_eq!(
        runner.resume("parallel").await.unwrap().revision,
        state.revision
    );
}

#[tokio::test]
async fn malformed_graphs_and_unbounded_cost_tasks_fail_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let count = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&count);
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            definition("task", TaskRecovery::Pure),
            move |_: NodeContext, _: CancelToken| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutcome::pure(json!(1)))
                }
            },
        ))
        .unwrap();
    let runner = WorkflowRunner::new(&tasks, &store);
    for (id, nodes) in [
        (
            "cycle",
            vec![
                WorkflowNode::new("a", "task").after(["b"]),
                WorkflowNode::new("b", "task").after(["a"]),
            ],
        ),
        (
            "missing",
            vec![WorkflowNode::new("a", "task").after(["missing"])],
        ),
        (
            "duplicate",
            vec![
                WorkflowNode::new("a", "task"),
                WorkflowNode::new("a", "task"),
            ],
        ),
        ("unknown", vec![WorkflowNode::new("a", "absent")]),
    ] {
        assert!(runner
            .start(id, WorkflowDefinition::new(nodes), Value::Null)
            .await
            .is_err());
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let mut unlimited = definition("unlimited", TaskRecovery::Manual);
    unlimited.max_cost_micros = None;
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            unlimited,
            |_: NodeContext, _: CancelToken| async { Ok(NodeOutcome::pure(Value::Null)) },
        ))
        .unwrap();
    let runner = WorkflowRunner::new(&tasks, &store).with_limits(WorkflowLimits {
        max_cost_micros: Some(10),
        ..Default::default()
    });
    assert!(runner
        .start(
            "no-price",
            WorkflowDefinition::new([WorkflowNode::new("a", "unlimited")]),
            Value::Null
        )
        .await
        .is_err());
}

#[tokio::test]
async fn graceful_pause_retains_finished_nodes_and_resume_only_runs_successors() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let count = Arc::new(AtomicUsize::new(0));
    let mut tasks = WorkflowTasks::new();
    let calls = Arc::clone(&count);
    tasks
        .register(FnWorkflowTask::new(
            definition("task", TaskRecovery::Pure),
            move |context: NodeContext, _: CancelToken| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutcome::pure(json!(context.node_id)))
                }
            },
        ))
        .unwrap();
    let pause = CancelToken::new();
    let sink = |event: &WorkflowEvent| {
        if matches!(&event.kind,WorkflowEventKind::NodeCompleted {node} if node=="first") {
            pause.cancel();
        }
    };
    let graph = WorkflowDefinition::new([
        WorkflowNode::new("first", "task"),
        WorkflowNode::new("second", "task").after(["first"]),
    ]);
    let paused = WorkflowRunner::new(&tasks, &store)
        .with_pause(&pause)
        .with_events(&sink)
        .start("pause", graph, Value::Null)
        .await
        .unwrap();
    assert_eq!(paused.status, WorkflowStatus::Paused);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let complete = WorkflowRunner::new(&tasks, &store)
        .resume("pause")
        .await
        .unwrap();
    assert_eq!(complete.status, WorkflowStatus::Completed);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(complete.node("first").unwrap().dispatches, 1);
}

fn peer_tasks<'a>(rejected: bool, counts: Arc<Vec<AtomicUsize>>) -> WorkflowTasks<'a> {
    let mut tasks = WorkflowTasks::new();
    for (index, name) in ["planner", "executor", "writer", "reviewer"]
        .into_iter()
        .enumerate()
    {
        let counts = Arc::clone(&counts);
        tasks.register(FnWorkflowTask::new(definition(name,TaskRecovery::Pure),move |context:NodeContext,_:CancelToken| {
            let counts=Arc::clone(&counts);
            async move {
                counts[index].fetch_add(1,Ordering::SeqCst);
                let output=match index {
                    0=>json!({"tasks":[{"id":"a","instructions":"first"},{"id":"b","instructions":"second"}]}),
                    1=>{assert!(context.dependencies.contains_key("task"));json!(format!("executed {}",context.dependencies["task"]["id"]))},
                    2=>{
                        let revision=context.params["revision"].as_u64().unwrap();
                        if revision>0 {assert!(context.dependencies.contains_key(&format!("check-review-{}",revision-1)));}
                        json!(format!("draft-{revision}"))
                    }
                    _=>json!({"approved":!rejected && context.params["revision"]==1,"feedback":"add evidence"}),
                };
                Ok(NodeOutcome::pure(output))
            }
        })).unwrap();
    }
    tasks
}

#[tokio::test]
async fn peer_review_revises_only_the_draft_and_skips_later_rounds_after_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let counts = Arc::new((0..4).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
    let mut tasks = peer_tasks(false, Arc::clone(&counts));
    let graph = PeerSpec::default().install(&mut tasks).unwrap();
    let state = WorkflowRunner::new(&tasks, &store)
        .start("peer", graph, json!({"goal":"research"}))
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(state.output("result").unwrap()["answer"], "draft-1");
    assert_eq!(state.output("result").unwrap()["revision"], 1);
    assert_eq!(
        counts
            .iter()
            .map(|n| n.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        vec![1, 2, 2, 2]
    );
    assert_eq!(state.node("worker-2").unwrap().status, NodeStatus::Skipped);
    assert_eq!(state.node("draft-2").unwrap().status, NodeStatus::Skipped);
}

#[tokio::test]
async fn peer_revision_limit_does_not_report_an_unapproved_draft_as_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let counts = Arc::new((0..4).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
    let mut tasks = peer_tasks(true, Arc::clone(&counts));
    let graph = PeerSpec {
        max_revisions: 1,
        ..Default::default()
    }
    .install(&mut tasks)
    .unwrap();
    let state = WorkflowRunner::new(&tasks, &store)
        .start("limit", graph, Value::Null)
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Failed);
    assert!(state.output("result").is_none());
    assert!(state
        .node("result")
        .unwrap()
        .detail
        .as_ref()
        .unwrap()
        .contains("revision limit"));
    assert_eq!(
        counts
            .iter()
            .map(|n| n.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        vec![1, 2, 2, 2]
    );
}

#[tokio::test]
async fn workflow_limits_stop_before_task_dispatch() {
    for (id, limits, expected) in [
        (
            "steps",
            WorkflowLimits {
                max_dispatches: 0,
                ..Default::default()
            },
            WorkflowStatus::DispatchLimit,
        ),
        (
            "deadline",
            WorkflowLimits {
                max_duration: Duration::ZERO,
                ..Default::default()
            },
            WorkflowStatus::Deadline,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = FileWorkflowStore::new(dir.path());
        let count = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&count);
        let mut tasks = WorkflowTasks::new();
        tasks
            .register(FnWorkflowTask::new(
                definition("task", TaskRecovery::Pure),
                move |_: NodeContext, _: CancelToken| {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(NodeOutcome::pure(Value::Null))
                    }
                },
            ))
            .unwrap();
        let state = WorkflowRunner::new(&tasks, &store)
            .with_limits(limits)
            .start(
                id,
                WorkflowDefinition::new([WorkflowNode::new("node", "task")]),
                Value::Null,
            )
            .await
            .unwrap();
        assert_eq!(state.status, expected);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn cancellation_after_an_intent_does_not_start_unpolled_effects() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let count = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&count);
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            definition("effect", TaskRecovery::Manual),
            move |_: NodeContext, _: CancelToken| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutcome::pure(Value::Null))
                }
            },
        ))
        .unwrap();
    let cancel = CancelToken::new();
    let sink = |event: &WorkflowEvent| {
        if matches!(event.kind, WorkflowEventKind::NodeStarted { .. }) {
            cancel.cancel();
        }
    };
    let state = WorkflowRunner::new(&tasks, &store)
        .with_cancel(&cancel)
        .with_events(&sink)
        .start(
            "cancel",
            WorkflowDefinition::new([
                WorkflowNode::new("a", "effect"),
                WorkflowNode::new("b", "effect"),
            ]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn json_null_is_a_completed_output_not_a_missing_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            definition("null", TaskRecovery::Pure),
            |_: NodeContext, _: CancelToken| async { Ok(NodeOutcome::pure(Value::Null)) },
        ))
        .unwrap();
    tasks
        .register(FnWorkflowTask::new(
            definition("check", TaskRecovery::Pure),
            |context: NodeContext, _: CancelToken| async move {
                assert_eq!(context.dependencies.get("value"), Some(&Value::Null));
                Ok(NodeOutcome::pure(json!("present")))
            },
        ))
        .unwrap();
    let state = WorkflowRunner::new(&tasks, &store)
        .start(
            "null",
            WorkflowDefinition::new([
                WorkflowNode::new("value", "null"),
                WorkflowNode::new("check", "check").after(["value"]),
            ]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        store
            .inspect("null")
            .await
            .unwrap()
            .unwrap()
            .output("value"),
        Some(&Value::Null)
    );
}
