#![cfg(feature = "runtime")]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::*;
use taskloom::{ledger, CancelToken};

#[derive(Default)]
struct TestModel {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    batch: bool,
    crash: bool,
}

impl Model for TestModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(10).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            if self.crash {
                std::process::exit(37);
            }
            let results = request
                .messages
                .iter()
                .filter_map(|m| match m {
                    Message::Tool { result } => Some(result),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let finish_only = request
                .messages
                .iter()
                .any(|m| matches!(m,Message::User {content} if content == "finish"));
            let turn = if finish_only || !results.is_empty() {
                if self.batch {
                    assert_eq!(
                        results.len(),
                        2,
                        "model must not run with an unfinished tool batch"
                    );
                }
                AssistantTurn::text("finished")
            } else {
                let mut calls = vec![ToolCall {
                    id: "write-a".into(),
                    name: "write".into(),
                    arguments: json!({"n":3}),
                }];
                if self.batch {
                    calls.push(ToolCall {
                        id: "write-b".into(),
                        name: "write".into(),
                        arguments: json!({"n":4}),
                    });
                }
                AssistantTurn {
                    content: String::new(),
                    tool_calls: calls,
                }
            };
            reservation.settle(1);
            ledger::record_usage(
                "durable_test",
                ledger::Usage {
                    requests: 1,
                    input_tokens: 10,
                    output_tokens: 1,
                    cost_micros: 1,
                },
            );
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}

fn definition(effect: ToolEffect) -> ToolDefinition {
    ToolDefinition {
        name: "write".into(),
        description: "Test operation v1".into(),
        effect,
        permissions: BTreeSet::from(["test".into()]),
        input_schema: json!({"type":"object","properties":{"n":{"type":"integer","minimum":0,"maximum":100}},"required":["n"],"additionalProperties":false}),
    }
}

fn tools(
    count: Arc<AtomicUsize>,
    effect: ToolEffect,
    marker: Option<PathBuf>,
    crash: bool,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(FnTool::new(
            definition(effect),
            move |args: Value, context: ToolContext| {
                let count = Arc::clone(&count);
                let marker = marker.clone();
                async move {
                    let key = serde_json::to_string(&(context.run_id, context.call_id)).unwrap();
                    let duplicate = effect == ToolEffect::Idempotent
                        && marker.as_ref().is_some_and(|path| {
                            std::fs::read_to_string(path)
                                .unwrap_or_default()
                                .lines()
                                .any(|line| line == key)
                        });
                    if !duplicate {
                        count.fetch_add(1, Ordering::SeqCst);
                        if let Some(path) = marker {
                            let mut file = std::fs::OpenOptions::new()
                                .append(true)
                                .create(true)
                                .open(path)
                                .unwrap();
                            writeln!(file, "{key}").unwrap();
                            file.sync_all().unwrap();
                        }
                    }
                    if crash {
                        std::process::exit(37);
                    }
                    Ok(json!(args["n"].as_i64().unwrap() * 2))
                }
            },
        ))
        .unwrap();
    registry
}

fn policy() -> ToolPolicy {
    ToolPolicy::default().allow("write").grant("test")
}
fn limits() -> RunLimits {
    RunLimits {
        max_cost_micros: Some(20),
        ..Default::default()
    }
}
fn messages() -> Vec<Message> {
    vec![Message::user("write")]
}
fn decision(approved: bool) -> ApprovalDecision {
    ApprovalDecision {
        approved,
        actor: "operator".into(),
        note: "reviewed exact input".into(),
    }
}
fn recovery_result() -> RecoveryDecision {
    RecoveryDecision::RecordResult {
        output: json!(6),
        is_error: false,
        actor: "operator".into(),
        note: "confirmed the external result".into(),
    }
}

#[tokio::test]
async fn exact_approval_survives_restart_and_cannot_be_reused() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let count = Arc::new(AtomicUsize::new(0));
    let tools = tools(Arc::clone(&count), ToolEffect::NonIdempotent, None, false);
    let agent = AgentLoop::new(&model, &tools, "test")
        .with_policy(policy())
        .with_limits(limits());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable.start("approval", messages()).await.unwrap();
    assert_eq!(state.status, RunStatus::WaitingApproval);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    let request = state.pending_approval().unwrap().clone();
    assert_eq!(request.call.arguments, json!({"n":3}));
    assert!(durable
        .decide("approval", "wrong-ticket", decision(true))
        .await
        .is_err());
    let reloaded = DurableAgent::new(&agent, &store)
        .resume("approval")
        .await
        .unwrap();
    assert_eq!(reloaded.revision, state.revision);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    let approved = durable
        .decide("approval", &request.request_id, decision(true))
        .await
        .unwrap();
    assert_eq!(approved.status, RunStatus::Paused);
    assert!(durable
        .decide("approval", &request.request_id, decision(true))
        .await
        .is_err());
    let completed = DurableAgent::new(&agent, &FileRunStore::new(dir.path()))
        .resume("approval")
        .await
        .unwrap();
    assert_eq!(completed.report.final_text(), Some("finished"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(completed.report.usage.requests, 2);
    assert_eq!(completed.report.budget.committed_micros, 2);
    assert_eq!(completed.tool_steps[0].approvals.len(), 1);
    let again = durable.resume("approval").await.unwrap();
    assert_eq!(again.revision, completed.revision);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejection_is_terminal_and_changed_configuration_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let count = Arc::new(AtomicUsize::new(0));
    let registry = tools(Arc::clone(&count), ToolEffect::NonIdempotent, None, false);
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable.start("reject", messages()).await.unwrap();
    let changed = AgentLoop::new(&model, &registry, "other-model")
        .with_policy(policy())
        .with_limits(limits());
    assert!(matches!(
        DurableAgent::new(&changed, &store).resume("reject").await,
        Err(RuntimeError::Configuration(_))
    ));
    let mut changed_tools = ToolRegistry::new();
    let mut def = definition(ToolEffect::NonIdempotent);
    def.description = "changed implementation contract v2".into();
    changed_tools
        .register(FnTool::new(def, |_: Value, _: ToolContext| async {
            Ok(Value::Null)
        }))
        .unwrap();
    let changed = AgentLoop::new(&model, &changed_tools, "test")
        .with_policy(policy())
        .with_limits(limits());
    assert!(matches!(
        DurableAgent::new(&changed, &store)
            .decide(
                "reject",
                &state.pending_approval().unwrap().request_id,
                decision(true)
            )
            .await,
        Err(RuntimeError::Configuration(_))
    ));
    let rejected = durable
        .decide(
            "reject",
            &state.pending_approval().unwrap().request_id,
            decision(false),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, RunStatus::Finished);
    assert_eq!(rejected.report.stop_reason, StopReason::PermissionDenied);
    assert!(rejected.pending_approval().is_none());
    durable.resume("reject").await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pause_inside_a_batch_resumes_only_remaining_tools() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel {
        batch: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let registry = tools(Arc::clone(&count), ToolEffect::ReadOnly, None, false);
    let pause = CancelToken::new();
    let sink = |event: &RunEvent| {
        if matches!(&event.kind,RunEventKind::ToolFinished {call_id,..} if call_id == "write-a") {
            pause.cancel();
        }
    };
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits())
        .with_events(&sink);
    let state = DurableAgent::new(&agent, &store)
        .with_pause(&pause)
        .start("batch", messages())
        .await
        .unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(state.report.tool_calls, 1);
    assert!(matches!(state.cursor, RunCursor::Tools { next: 1, .. }));
    let state = DurableAgent::new(&agent, &store)
        .resume("batch")
        .await
        .unwrap();
    assert_eq!(state.report.final_text(), Some("finished"));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(state.tool_steps.len(), 2);
    state.validate().unwrap();
}

#[tokio::test]
async fn run_lock_blocks_other_workers_but_inspection_remains_available() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let registry = tools(
        Arc::new(AtomicUsize::new(0)),
        ToolEffect::ReadOnly,
        None,
        false,
    );
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    DurableAgent::new(&agent, &store)
        .create("locked", messages())
        .await
        .unwrap();
    let lease = store.acquire("locked").await.unwrap();
    assert!(matches!(
        store.acquire("locked").await,
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(
        store.inspect("locked").await.unwrap().unwrap().status,
        RunStatus::Ready
    );
    drop(lease);
    assert_eq!(
        DurableAgent::new(&agent, &store)
            .resume("locked")
            .await
            .unwrap()
            .report
            .final_text(),
        Some("finished")
    );
}

#[tokio::test]
async fn state_and_approval_corruption_fail_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let count = Arc::new(AtomicUsize::new(0));
    let registry = tools(Arc::clone(&count), ToolEffect::NonIdempotent, None, false);
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable.start("corrupt", messages()).await.unwrap();
    let lease = store.acquire("corrupt").await.unwrap();
    let mut corrupted = state.clone();
    corrupted.revision += 1;
    corrupted.tool_steps[0].call.arguments = json!({"n":4});
    assert!(lease.save(&corrupted).await.is_err());
    drop(lease);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && !path.to_string_lossy().ends_with(".budget.json")
        })
        .unwrap();
    let mut json = serde_json::to_value(&state).unwrap();
    json["version"] = json!(RUN_STATE_VERSION + 1);
    let corrupted_bytes = serde_json::to_vec(&json).unwrap();
    std::fs::write(&file, &corrupted_bytes).unwrap();
    assert!(matches!(
        durable.resume("corrupt").await,
        Err(RuntimeError::Storage(_))
    ));
    assert_eq!(std::fs::read(file).unwrap(), corrupted_bytes);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

fn child(dir: &Path, id: &str, mode: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("durable_child_process")
        .arg("--nocapture")
        .env("TASKLOOM_DURABLE_TEST_DIR", dir)
        .env("TASKLOOM_DURABLE_TEST_ID", id)
        .env("TASKLOOM_DURABLE_TEST_MODE", mode)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "child failed before the injected crash: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn durable_child_process() {
    let Some(dir) = std::env::var_os("TASKLOOM_DURABLE_TEST_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let id = std::env::var("TASKLOOM_DURABLE_TEST_ID").unwrap();
    let mode = std::env::var("TASKLOOM_DURABLE_TEST_MODE").unwrap();
    let store = FileRunStore::new(dir.join("store"));
    let model = TestModel {
        crash: mode == "model",
        ..Default::default()
    };
    let effect = if mode == "idempotent" {
        ToolEffect::Idempotent
    } else {
        ToolEffect::NonIdempotent
    };
    let registry = tools(
        Arc::new(AtomicUsize::new(0)),
        effect,
        Some(dir.join("effects.log")),
        true,
    );
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    if mode == "model" {
        DurableAgent::new(&agent, &store)
            .start(&id, vec![Message::user("finish")])
            .await
            .unwrap();
    } else {
        DurableAgent::new(&agent, &store).resume(&id).await.unwrap();
    }
    panic!("expected the operation to terminate this process");
}

#[tokio::test]
async fn real_process_crash_after_write_requires_reconciliation_without_repeating_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path().join("store"));
    let model = TestModel::default();
    let count = Arc::new(AtomicUsize::new(0));
    let registry = tools(
        Arc::clone(&count),
        ToolEffect::NonIdempotent,
        Some(dir.path().join("effects.log")),
        false,
    );
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable.start("crashed-write", messages()).await.unwrap();
    durable
        .decide(
            "crashed-write",
            &state.pending_approval().unwrap().request_id,
            decision(true),
        )
        .await
        .unwrap();
    child(dir.path(), "crashed-write", "tool");
    let state = durable.resume("crashed-write").await.unwrap();
    assert_eq!(state.status, RunStatus::NeedsRecovery);
    assert!(matches!(
        state.tool_steps[0].progress,
        ToolProgress::Uncertain { .. }
    ));
    let same = durable.resume("crashed-write").await.unwrap();
    assert_eq!(same.revision, state.revision);
    assert!(matches!(
        durable
            .reconcile(
                "crashed-write",
                "write-a",
                RecoveryDecision::RetrySafe {
                    actor: "operator".into(),
                    note: "try again".into()
                }
            )
            .await,
        Err(RuntimeError::Permission(_))
    ));
    durable
        .reconcile("crashed-write", "write-a", recovery_result())
        .await
        .unwrap();
    let complete = durable.resume("crashed-write").await.unwrap();
    assert_eq!(complete.report.final_text(), Some("finished"));
    assert_eq!(complete.report.tool_calls, 1);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effects.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(complete.tool_steps[0].recoveries.len(), 1);
}

#[tokio::test]
async fn idempotent_recovery_preserves_key_and_requires_fresh_attempt_approval() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path().join("store"));
    let model = TestModel::default();
    let count = Arc::new(AtomicUsize::new(0));
    let registry = tools(
        Arc::clone(&count),
        ToolEffect::Idempotent,
        Some(dir.path().join("effects.log")),
        false,
    );
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable.start("idempotent", messages()).await.unwrap();
    let original = state.pending_approval().unwrap().request_id.clone();
    durable
        .decide("idempotent", &original, decision(true))
        .await
        .unwrap();
    child(dir.path(), "idempotent", "idempotent");
    durable.resume("idempotent").await.unwrap();
    durable
        .reconcile(
            "idempotent",
            "write-a",
            RecoveryDecision::RetrySafe {
                actor: "operator".into(),
                note: "tool deduplicates this context key".into(),
            },
        )
        .await
        .unwrap();
    let state = durable.resume("idempotent").await.unwrap();
    let request = state.pending_approval().unwrap();
    assert_eq!(request.attempt, 2);
    assert_ne!(request.request_id, original);
    assert!(durable
        .decide("idempotent", &original, decision(true))
        .await
        .is_err());
    durable
        .decide("idempotent", &request.request_id, decision(true))
        .await
        .unwrap();
    let state = durable.resume("idempotent").await.unwrap();
    assert_eq!(state.report.final_text(), Some("finished"));
    assert_eq!(state.report.tool_calls, 2);
    assert_eq!(state.tool_steps[0].approvals.len(), 2);
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "the original effect must be deduplicated"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effects.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn real_process_model_crash_preserves_reserved_spending_and_step_count() {
    let dir = tempfile::tempdir().unwrap();
    child(dir.path(), "model-crash", "model");
    let store = FileRunStore::new(dir.path().join("store"));
    let model = TestModel::default();
    let registry = tools(
        Arc::new(AtomicUsize::new(0)),
        ToolEffect::NonIdempotent,
        None,
        false,
    );
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits());
    let state = DurableAgent::new(&agent, &store)
        .resume("model-crash")
        .await
        .unwrap();
    assert_eq!(state.report.final_text(), Some("finished"));
    assert_eq!(state.report.steps, 2);
    assert_eq!(state.report.usage.requests, 1);
    assert_eq!(state.report.budget.committed_micros, 11);
    assert_eq!(state.report.budget.uncertain_micros, 10);
    assert_eq!(state.report.budget.uncertain_attempts, 1);
    assert_eq!(state.report.budget.in_flight_micros, 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn hashed_run_ids_and_monotonic_writes_keep_atomic_complete_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let registry = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &registry, "test").with_limits(limits());
    let state = DurableAgent::new(&agent, &store)
        .create("../outside", vec![Message::user("finish")])
        .await
        .unwrap();
    let lease = store.acquire("../outside").await.unwrap();
    let mut newer = state.clone();
    newer.revision += 1;
    lease.save(&newer).await.unwrap();
    lease.save(&state).await.unwrap();
    assert_eq!(
        lease.load().await.unwrap().unwrap().revision,
        newer.revision
    );
    let mut conflict = newer.clone();
    conflict.report.detail = Some("conflicting data".into());
    assert!(lease.save(&conflict).await.is_err());
    let reader = async {
        for _ in 0..40 {
            store
                .inspect("../outside")
                .await
                .unwrap()
                .unwrap()
                .validate()
                .unwrap();
        }
    };
    let writer = async {
        for _ in 0..40 {
            newer.revision += 1;
            lease.save(&newer).await.unwrap();
        }
    };
    tokio::join!(reader, writer);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(name.starts_with(|c: char| c.is_ascii_hexdigit()));
        assert_eq!(name.split('.').next().unwrap().len(), 64);
    }
}

#[derive(Clone, Copy)]
enum Fault {
    BeforeModel,
    BeforeTool,
    AfterTool,
    Budget,
}
struct FaultStore {
    inner: FileRunStore,
    fault: Fault,
}
struct FaultLease {
    inner: Arc<dyn RunLease>,
    fault: Fault,
}

impl RunStore for FaultStore {
    fn acquire<'a>(
        &'a self,
        run_id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RunLease>, RuntimeError>> {
        Box::pin(async move {
            Ok(Arc::new(FaultLease {
                inner: self.inner.acquire(run_id).await?,
                fault: self.fault,
            }) as Arc<dyn RunLease>)
        })
    }
    fn inspect<'a>(
        &'a self,
        run_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RunState>, RuntimeError>> {
        self.inner.inspect(run_id)
    }
}
impl RunLease for FaultLease {
    fn load(&self) -> BoxFuture<'_, Result<Option<RunState>, RuntimeError>> {
        self.inner.load()
    }
    fn load_budget(&self) -> BoxFuture<'_, Result<Option<BudgetSnapshot>, RuntimeError>> {
        self.inner.load_budget()
    }
    fn save<'a>(&'a self, state: &'a RunState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        let fail = match self.fault {
            Fault::BeforeModel => {
                state.status == RunStatus::Running
                    && matches!(state.cursor, RunCursor::ModelInFlight { .. })
            }
            Fault::BeforeTool => {
                state.status == RunStatus::Running
                    && state.current_tool_index().is_some_and(|i| {
                        matches!(state.tool_steps[i].progress, ToolProgress::InFlight { .. })
                    })
            }
            Fault::AfterTool => state
                .tool_steps
                .iter()
                .any(|step| matches!(step.progress, ToolProgress::Complete { .. })),
            Fault::Budget => false,
        };
        if fail {
            Box::pin(async { Err(RuntimeError::Storage("injected checkpoint failure".into())) })
        } else {
            self.inner.save(state)
        }
    }
    fn save_budget<'a>(
        &'a self,
        snapshot: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        if matches!(self.fault, Fault::Budget) {
            Box::pin(async {
                Err(RuntimeError::Storage(
                    "injected budget journal failure".into(),
                ))
            })
        } else {
            self.inner.save_budget(snapshot)
        }
    }
}

#[tokio::test]
async fn checkpoint_failures_prevent_dispatch_and_failed_result_writes_require_recovery() {
    for fault in [Fault::BeforeModel, Fault::BeforeTool, Fault::AfterTool] {
        let dir = tempfile::tempdir().unwrap();
        let base = FileRunStore::new(dir.path());
        let broken = FaultStore {
            inner: base.clone(),
            fault,
        };
        let model = TestModel::default();
        let count = Arc::new(AtomicUsize::new(0));
        let registry = tools(Arc::clone(&count), ToolEffect::NonIdempotent, None, false);
        let agent = AgentLoop::new(&model, &registry, "test")
            .with_policy(policy())
            .with_limits(limits());
        let approvals = ApprovalPolicy {
            read_only: false,
            idempotent: false,
            non_idempotent: false,
            tools: BTreeSet::new(),
        };
        let attempt = DurableAgent::new(&agent, &broken)
            .with_approvals(approvals.clone())
            .start("fault", messages())
            .await;
        let durable = DurableAgent::new(&agent, &base).with_approvals(approvals);
        if matches!(fault, Fault::AfterTool) {
            assert!(matches!(attempt, Err(RuntimeError::Storage(_))));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            let recovered = durable.resume("fault").await.unwrap();
            assert_eq!(recovered.status, RunStatus::NeedsRecovery);
            assert_eq!(count.load(Ordering::SeqCst), 1);
        } else {
            let stopped = attempt.unwrap();
            assert_eq!(stopped.report.stop_reason, StopReason::StorageError);
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(
                model.calls.load(Ordering::SeqCst),
                if matches!(fault, Fault::BeforeModel) {
                    0
                } else {
                    1
                }
            );
            durable.resume("fault").await.unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn native_gateway_will_not_send_when_reservation_journaling_fails() {
    use taskloom::gateway::{ModelPrice, PriceTable};
    use taskloom::{
        ChatRequest, ChatResponse, Gateway, GatewayConfig, GatewayTransport, ModelError, Provider,
        Streaming,
    };
    struct CountingProvider(Arc<AtomicUsize>);
    impl Provider for CountingProvider {
        fn id(&self) -> &str {
            "https://journal-failure.test"
        }
        fn complete<'a>(
            &'a self,
            _: &'a ChatRequest,
            _: Option<Streaming<'a>>,
        ) -> BoxFuture<'a, Result<ChatResponse, ModelError>> {
            Box::pin(async { unreachable!() })
        }
        fn converse<'a>(
            &'a self,
            _: &'a ModelRequest,
        ) -> BoxFuture<'a, Result<ModelReply, ModelError>> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("provider must not be called before reservation is durable")
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = FaultStore {
        inner: FileRunStore::new(dir.path()),
        fault: Fault::Budget,
    };
    let count = Arc::new(AtomicUsize::new(0));
    let gateway = Arc::new(Gateway::new(GatewayConfig {
        prices: PriceTable::new([ModelPrice::new("test", 0.0, 0.0)]),
        ..Default::default()
    }));
    let model = GatewayTransport::new(gateway, CountingProvider(Arc::clone(&count)));
    let registry = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &registry, "test").with_limits(limits());
    let state = DurableAgent::new(&agent, &store)
        .start("journal", vec![Message::user("finish")])
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::StorageError);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(state.report.budget.uncertain_attempts, 1);
}

#[tokio::test]
async fn resuming_does_not_reset_model_step_limits() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let count = Arc::new(AtomicUsize::new(0));
    let registry = tools(Arc::clone(&count), ToolEffect::ReadOnly, None, false);
    let pause = CancelToken::new();
    let sink = |event: &RunEvent| {
        if matches!(event.kind, RunEventKind::ModelFinished { .. }) {
            pause.cancel();
        }
    };
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(RunLimits {
            max_steps: 1,
            ..limits()
        })
        .with_events(&sink);
    let paused = DurableAgent::new(&agent, &store)
        .with_pause(&pause)
        .start("limited", messages())
        .await
        .unwrap();
    assert_eq!(paused.report.steps, 1);
    let stopped = DurableAgent::new(&agent, &store)
        .resume("limited")
        .await
        .unwrap();
    assert_eq!(stopped.report.stop_reason, StopReason::StepLimit);
    assert_eq!(stopped.report.steps, 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_of_an_active_tool_persists_uncertainty_and_is_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = TestModel::default();
    let cancel = CancelToken::new();
    let entered = Arc::new(tokio::sync::Notify::new());
    let mut registry = ToolRegistry::new();
    let signal = Arc::clone(&entered);
    registry
        .register(FnTool::new(
            definition(ToolEffect::ReadOnly),
            move |_: Value, _: ToolContext| {
                let signal = Arc::clone(&signal);
                async move {
                    signal.notify_one();
                    std::future::pending::<Result<Value, ToolError>>().await
                }
            },
        ))
        .unwrap();
    let agent = AgentLoop::new(&model, &registry, "test")
        .with_policy(policy())
        .with_limits(limits())
        .with_cancel(&cancel);
    let cancel_work = async {
        entered.notified().await;
        cancel.cancel();
    };
    let run = async {
        DurableAgent::new(&agent, &store)
            .start("cancelled", messages())
            .await
            .unwrap()
    };
    let (state, ()) = tokio::join!(run, cancel_work);
    assert_eq!(state.status, RunStatus::Finished);
    assert_eq!(state.report.stop_reason, StopReason::Cancelled);
    assert!(matches!(
        state.tool_steps[0].progress,
        ToolProgress::Uncertain { .. }
    ));
    let same = DurableAgent::new(&agent, &store)
        .resume("cancelled")
        .await
        .unwrap();
    assert_eq!(same.revision, state.revision);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_batches_and_reused_ids_stop_with_a_valid_checkpoint() {
    struct BadModel {
        inner: TestModel,
        reuse: bool,
    }
    impl Model for BadModel {
        fn generate<'a>(
            &'a self,
            request: &'a ModelRequest,
            budget: &'a RunBudget,
        ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
            Box::pin(async move {
                let mut output = self.inner.generate(request, budget).await?;
                if self.reuse && output.turn.tool_calls.is_empty() {
                    output.turn = AssistantTurn {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "write-a".into(),
                            name: "write".into(),
                            arguments: json!({"n":3}),
                        }],
                    };
                } else if !self.reuse {
                    output.turn.tool_calls.push(ToolCall {
                        id: "write-b".into(),
                        name: "write".into(),
                        arguments: json!({"n":-1}),
                    });
                }
                Ok(output)
            })
        }
    }
    for reuse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = FileRunStore::new(dir.path());
        let model = BadModel {
            inner: TestModel::default(),
            reuse,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let registry = tools(Arc::clone(&count), ToolEffect::ReadOnly, None, false);
        let agent = AgentLoop::new(&model, &registry, "test")
            .with_policy(policy())
            .with_limits(limits());
        let state = DurableAgent::new(&agent, &store)
            .start("invalid", messages())
            .await
            .unwrap();
        assert_eq!(
            state.report.stop_reason,
            if reuse {
                StopReason::ProtocolError
            } else {
                StopReason::ToolError
            }
        );
        state.validate().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), if reuse { 1 } else { 0 });
        assert_eq!(state.report.usage.requests, if reuse { 2 } else { 1 });
        assert_eq!(
            store.inspect("invalid").await.unwrap().unwrap().revision,
            state.revision
        );
    }
}
