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

#[tokio::test]
async fn reconcile_after_process_death_recovers_running_nodes_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let mut tasks = WorkflowTasks::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = Arc::clone(&started);
    tasks
        .register(FnWorkflowTask::new(
            definition("manual", TaskRecovery::Manual),
            move |context: NodeContext, _: CancelToken| {
                let signal = Arc::clone(&signal);
                async move {
                    if context.node_id == "a" {
                        signal.notified().await;
                        Err(taskloom::runtime::RuntimeError::Storage(
                            "effect outcome unknown".into(),
                        ))
                    } else {
                        signal.notify_one();
                        std::future::pending().await
                    }
                }
            },
        ))
        .unwrap();
    let runner = WorkflowRunner::new(&tasks, &store);
    let graph = WorkflowDefinition::new([
        WorkflowNode::new("a", "manual"),
        WorkflowNode::new("b", "manual"),
    ]);
    // Drop the controller (a simulated process death) once `a` is uncertain
    // while `b` is still running.
    tokio::select! {
        _ = runner.start("crash", graph, Value::Null) => panic!("workflow must not stop"),
        () = async {
            loop {
                let state = store.inspect("crash").await.unwrap();
                if state.is_some_and(|s| s.node("a").unwrap().status == NodeStatus::Uncertain) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        } => {}
    }
    let crashed = store.inspect("crash").await.unwrap().unwrap();
    assert_eq!(crashed.status, WorkflowStatus::Running);
    assert_eq!(crashed.node("b").unwrap().status, NodeStatus::Running);
    let mut stopped = crashed.clone();
    stopped.status = WorkflowStatus::Paused;
    stopped.invocation_started_unix_ms = None;
    assert!(
        stopped.validate().is_err(),
        "stopped state with a running node"
    );
    tokio::time::sleep(Duration::from_millis(20)).await;

    let state = runner
        .reconcile(
            "crash",
            "a",
            NodeRecovery::RecordOutput {
                output: json!("observed"),
                cost_micros: Some(0),
                usage: Default::default(),
                actor: "operator".into(),
                note: "checked".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::NeedsRecovery);
    assert_eq!(state.node("a").unwrap().status, NodeStatus::Complete);
    assert_eq!(state.node("b").unwrap().status, NodeStatus::Uncertain);
    assert!(state.elapsed_ms >= 20, "downtime must be charged");
    let resumed = runner.resume("crash").await.unwrap();
    assert_eq!(resumed.status, WorkflowStatus::NeedsRecovery);
    assert_eq!(resumed.node("b").unwrap().dispatches, 1);
}

#[tokio::test]
async fn resume_that_changes_nothing_does_not_grow_the_event_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            definition("task", TaskRecovery::Pure),
            |_: NodeContext, _: CancelToken| async { Ok(NodeOutcome::pure(json!(1))) },
        ))
        .unwrap();
    let pause = CancelToken::new();
    pause.cancel();
    let seen = std::sync::Mutex::new(Vec::new());
    let sink = |event: &WorkflowEvent| seen.lock().unwrap().push(event.clone());
    let runner = WorkflowRunner::new(&tasks, &store)
        .with_pause(&pause)
        .with_events(&sink);
    let paused = runner
        .start(
            "idle",
            WorkflowDefinition::new([WorkflowNode::new("a", "task")]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(paused.status, WorkflowStatus::Paused);
    let logged = paused.events.len();
    assert_eq!(*seen.lock().unwrap(), paused.events);
    for _ in 0..3 {
        let again = runner.resume("idle").await.unwrap();
        assert_eq!(again.status, WorkflowStatus::Paused);
        assert_eq!(again.events.len(), logged);
    }
    assert_eq!(*seen.lock().unwrap(), paused.events);
    let done = WorkflowRunner::new(&tasks, &store)
        .with_events(&sink)
        .resume("idle")
        .await
        .unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!(*seen.lock().unwrap(), done.events);
}

#[tokio::test]
async fn a_stale_workflow_save_is_a_conflict_not_a_silent_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileWorkflowStore::new(dir.path());
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            definition("pure", TaskRecovery::Pure),
            |_: NodeContext, _: CancelToken| async { Ok(NodeOutcome::pure(json!(7))) },
        ))
        .unwrap();
    let graph = WorkflowDefinition::new([WorkflowNode::new("node", "pure")]);
    let runner = WorkflowRunner::new(&tasks, &store);
    let created = runner.create("stale", graph, Value::Null).await.unwrap();
    let finished = runner.resume("stale").await.unwrap();
    assert!(finished.revision > created.revision);
    let lease = WorkflowStore::acquire(&store, "stale").await.unwrap();
    let error = lease.save(&created).await.unwrap_err();
    assert!(
        matches!(error, taskloom::runtime::RuntimeError::Conflict(_)),
        "{error:?}"
    );
    assert_eq!(
        store.inspect("stale").await.unwrap().unwrap().revision,
        finished.revision
    );
}
