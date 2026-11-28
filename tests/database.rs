#![cfg(feature = "database")]
//! Transaction/storage controls use fixture models. Native evidence is separate.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use taskloom::runtime::{database::*, workflow::*, *};

#[derive(Default)]
struct TextModel {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}
impl Model for TextModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(7).await?.settle(3);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text("fixture completed"),
            })
        })
    }
}
fn options() -> DatabaseOptions {
    DatabaseOptions {
        lease_duration: Duration::from_millis(600),
        ..Default::default()
    }
}
fn scope(project: &str) -> DatabaseNamespace {
    DatabaseNamespace::new("fixture-tenant", project).unwrap()
}
fn project() -> String {
    uuid::Uuid::new_v4().to_string()
}
async fn acquire(store: &SqlRunStore, id: &str) -> Arc<dyn RunLease> {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match store.acquire(id).await {
                Ok(lease) => return lease,
                Err(RuntimeError::Conflict(_)) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await
    .unwrap()
}
async fn sql(db_url: &str) -> sqlx::AnyPool {
    sqlx::any::install_default_drivers();
    sqlx::AnyPool::connect(db_url).await.unwrap()
}

async fn roundtrip(db: &SqlDatabase) {
    let name = project();
    let ns = scope(&name);
    let runs = db.runs(ns.clone());
    let workflows = db.workflows(ns.clone());
    let sessions = db.sessions(ns);
    let model = TextModel::default();
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "fixture").with_limits(RunLimits {
        max_cost_micros: Some(20),
        ..Default::default()
    });
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(
            AgentWorkflowTask::new(
                "worker",
                &agent,
                &runs,
                "fixture role",
                WorkflowOutput::Text,
            )
            .unwrap(),
        )
        .unwrap();
    let graph = WorkflowDefinition::new([
        WorkflowNode::new("first", "worker").with_params(
            serde_json::from_str(r#"{"decimal":1.0,"large":9007199254740993}"#).unwrap(),
        ),
        WorkflowNode::new("second", "worker").after(["first"]),
    ]);
    let state = WorkflowRunner::new(&tasks, &workflows)
        .with_limits(WorkflowLimits {
            max_cost_micros: Some(40),
            ..Default::default()
        })
        .start("workflow", graph, json!({"goal":"fixture"}))
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    let child_id = state
        .node("first")
        .unwrap()
        .child
        .as_ref()
        .unwrap()
        .run_id
        .clone();
    let child = runs.inspect(&child_id).await.unwrap().unwrap();
    assert!(child.trace_binding.is_some());
    assert_eq!(child.report.budget.committed_micros, 3);
    assert!(db
        .runs(scope(&project()))
        .inspect(&child_id)
        .await
        .unwrap()
        .is_none());
    assert!(db
        .workflows(scope(&project()))
        .inspect("workflow")
        .await
        .unwrap()
        .is_none());
    {
        let requests = model.requests.lock().unwrap();
        let Message::User { content } = requests[0].messages.last().unwrap() else {
            panic!()
        };
        assert!(content.contains("9007199254740993"));
        assert!(content.contains("\"decimal\":1.0"));
    }
    let lease = acquire(&runs, &child_id).await;
    lease.save(&child).await.unwrap();
    let mut changed = child.clone();
    changed.trace_binding = None;
    changed.revision += 1;
    assert!(matches!(
        lease.save(&changed).await,
        Err(RuntimeError::Storage(_))
    ));
    let mut changed = child.clone();
    changed.revision += 1;
    lease.save(&changed).await.unwrap();
    assert!(matches!(
        lease.save(&child).await,
        Err(RuntimeError::Conflict(_))
    ));
    let mut collision = changed.clone();
    collision.report.elapsed_ms += 1;
    assert!(matches!(
        lease.save(&collision).await,
        Err(RuntimeError::Storage(_))
    ));
    drop(lease);
    let manager = SessionManager::new(&sessions);
    manager
        .create("conversation", vec!["fixture system".into()])
        .await
        .unwrap();
    let durable = DurableAgent::new(&agent, &runs);
    let turn = manager
        .run_turn(&durable, "conversation", "first", "precise input")
        .await
        .unwrap();
    assert_eq!(turn.session.turns.len(), 1);
    assert!(turn.session.active.is_none());
    let turn2 = manager
        .run_turn(&durable, "conversation", "second", "followup")
        .await
        .unwrap();
    assert_eq!(turn2.session.turns.len(), 2);
    assert!(turn2.session.messages.starts_with(&turn.session.messages));
    assert!(db
        .sessions(scope(&project()))
        .load("conversation")
        .await
        .unwrap()
        .is_none());
    let before = model.calls.load(Ordering::SeqCst);
    manager
        .run_turn(&durable, "conversation", "second", "followup")
        .await
        .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), before);
    assert!(manager
        .begin_turn("conversation", "second", "changed input")
        .await
        .is_err());
    // Both callers use the exact same valid new session: only one may create it.
    let concurrent = Session {
        version: SESSION_VERSION,
        revision: 1,
        id: "cas-create".into(),
        system_message_count: 0,
        messages: vec![],
        turns: vec![],
        active: None,
    };
    let (a, b) = tokio::join!(
        sessions.save(&concurrent, None),
        sessions.save(&concurrent, None)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let (a, b) = tokio::join!(
        manager.begin_turn("cas-create", "a", "one"),
        manager.begin_turn("cas-create", "b", "two")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
}

async fn ledger_and_lease(db: &SqlDatabase) {
    let runs = db.runs(scope(&project()));
    let lease = runs.acquire("ledger").await.unwrap();
    let first = BudgetSnapshot {
        generation: 1,
        limit_micros: Some(20),
        in_flight_micros: 7,
        in_flight_attempts: 1,
        ..Default::default()
    };
    lease.save_budget(&first).await.unwrap();
    assert_eq!(lease.load_budget().await.unwrap(), Some(first.clone()));
    let second = BudgetSnapshot {
        generation: 2,
        limit_micros: Some(20),
        committed_micros: 3,
        ..Default::default()
    };
    lease.save_budget(&second).await.unwrap();
    lease.save_budget(&first).await.unwrap();
    assert_eq!(lease.load_budget().await.unwrap(), Some(second.clone()));
    let mut collision = second.clone();
    collision.committed_micros = 4;
    assert!(lease.save_budget(&collision).await.is_err());
    tokio::time::sleep(Duration::from_millis(1400)).await;
    assert!(
        matches!(runs.acquire("ledger").await, Err(RuntimeError::Conflict(_))),
        "heartbeat must retain ownership beyond two TTLs"
    );
    drop(lease);
    let successor = acquire(&runs, "ledger").await;
    assert_eq!(successor.load_budget().await.unwrap(), Some(second));
    drop(successor);
}

struct SlowProbe {
    calls: AtomicUsize,
}
impl Model for SlowProbe {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(7).await?;
            let number = self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(250)).await;
            reservation.settle(3);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: if number == 0 {
                    AssistantTurn {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "probe-call".into(),
                            name: "probe".into(),
                            arguments: json!({}),
                        }],
                    }
                } else {
                    AssistantTurn::text("recovered")
                },
            })
        })
    }
}
async fn fencing_and_corruption(db: &SqlDatabase, url: &str) {
    let name = project();
    let runs = db.runs(scope(&name));
    let admin = sql(url).await;
    let model = SlowProbe {
        calls: AtomicUsize::new(0),
    };
    let callbacks = Arc::new(AtomicUsize::new(0));
    let seen = callbacks.clone();
    let mut tools = ToolRegistry::new();
    tools
        .register(FnTool::new(
            ToolDefinition {
                name: "probe".into(),
                description: "read-only dispatch counter fixture".into(),
                effect: ToolEffect::ReadOnly,
                permissions: Default::default(),
                input_schema: json!({"type":"object","additionalProperties":false}),
            },
            move |_, _| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Ok(Value::Null)
                }
            },
        ))
        .unwrap();
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_policy(ToolPolicy::default().allow("probe"))
        .with_limits(RunLimits {
            max_cost_micros: Some(20),
            ..Default::default()
        });
    let durable = DurableAgent::new(&agent, &runs);
    let ns = serde_json::to_string(&("fixture-tenant", &name)).unwrap();
    let tamper = async {
        while model.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        sqlx::query("UPDATE taskloom_leases SET owner='forced-successor',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='run' AND object_id='stale'")
            .bind(&ns).execute(&admin).await.unwrap();
        runs.acquire("stale").await.unwrap()
    };
    let (outcome, successor) =
        tokio::join!(durable.start("stale", vec![Message::user("probe")]), tamper);
    assert!(outcome.is_err());
    assert_eq!(callbacks.load(Ordering::SeqCst), 0);
    assert!(
        matches!(runs.acquire("stale").await, Err(RuntimeError::Conflict(_))),
        "a delayed old-owner drop must not release its successor"
    );
    assert!(successor.load_budget().await.unwrap().is_some());
    drop(successor);
    let lease = acquire(&runs, "stale").await;
    drop(lease);
    // Allow asynchronous conditional release to finish before SDK recovery.
    let resumed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match durable.resume("stale").await {
                Ok(state) => return state,
                Err(RuntimeError::Conflict(_)) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(resumed.report.stop_reason, StopReason::Completed);
    assert_eq!(resumed.report.budget.uncertain_attempts, 1);
    assert_eq!(callbacks.load(Ordering::SeqCst), 0);
    let other = project();
    let other_ns = serde_json::to_string(&("fixture-tenant", &other)).unwrap();
    sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) SELECT $1,family,object_id,revision,body FROM taskloom_records WHERE namespace=$2 AND family='run' AND object_id='stale'")
        .bind(&other_ns).bind(&ns).execute(&admin).await.unwrap();
    assert!(
        matches!(
            db.runs(scope(&other)).inspect("stale").await,
            Err(RuntimeError::Storage(_))
        ),
        "copied payload must not transplant its namespace envelope"
    );
    let limited = SqlDatabase::connect(
        url,
        DatabaseOptions {
            max_record_bytes: 1024,
            ..options()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        limited.runs(scope(&name)).inspect("stale").await,
        Err(RuntimeError::StorageLimit(_))
    ));
    limited.close().await;
    admin.close().await;
}

#[derive(Clone)]
struct FenceAfterSave {
    admin: sqlx::AnyPool,
    namespace: String,
    fired: Arc<AtomicBool>,
}
impl FenceAfterSave {
    async fn fire(&self, family: &str, id: &str) {
        if !self.fired.swap(true, Ordering::SeqCst) {
            sqlx::query("UPDATE taskloom_leases SET owner='injected-after-intent',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family=$2 AND object_id=$3")
                .bind(&self.namespace).bind(family).bind(id).execute(&self.admin).await.unwrap();
        }
    }
}
struct GuardStore {
    inner: SqlRunStore,
    fault: FenceAfterSave,
    model_boundary: bool,
}
struct GuardLease {
    inner: Arc<dyn RunLease>,
    fault: FenceAfterSave,
    model_boundary: bool,
}
impl RunStore for GuardStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn RunLease>, RuntimeError>> {
        Box::pin(async move {
            Ok(Arc::new(GuardLease {
                inner: self.inner.acquire(id).await?,
                fault: self.fault.clone(),
                model_boundary: self.model_boundary,
            }) as Arc<dyn RunLease>)
        })
    }
    fn inspect<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<RunState>, RuntimeError>> {
        self.inner.inspect(id)
    }
}
impl RunLease for GuardLease {
    fn verify_ownership(&self) -> BoxFuture<'_, Result<(), RuntimeError>> {
        self.inner.verify_ownership()
    }
    fn load(&self) -> BoxFuture<'_, Result<Option<RunState>, RuntimeError>> {
        self.inner.load()
    }
    fn save<'a>(&'a self, state: &'a RunState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            self.inner.save(state).await?;
            let trigger = if self.model_boundary {
                matches!(state.cursor, RunCursor::ModelInFlight { .. })
            } else {
                state
                    .tool_steps
                    .iter()
                    .any(|s| matches!(s.progress, ToolProgress::InFlight { .. }))
            };
            if trigger {
                self.fault.fire("run", &state.report.run_id).await;
            }
            Ok(())
        })
    }
    fn load_budget(&self) -> BoxFuture<'_, Result<Option<BudgetSnapshot>, RuntimeError>> {
        self.inner.load_budget()
    }
    fn save_budget<'a>(
        &'a self,
        value: &'a BudgetSnapshot,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        self.inner.save_budget(value)
    }
    fn max_bytes(&self) -> Option<u64> {
        self.inner.max_bytes()
    }
}
struct GuardWorkflowStore {
    inner: SqlWorkflowStore,
    fault: FenceAfterSave,
}
struct GuardWorkflowLease {
    inner: Arc<dyn WorkflowLease>,
    fault: FenceAfterSave,
}
impl WorkflowStore for GuardWorkflowStore {
    fn acquire<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn WorkflowLease>, RuntimeError>> {
        Box::pin(async move {
            Ok(Arc::new(GuardWorkflowLease {
                inner: self.inner.acquire(id).await?,
                fault: self.fault.clone(),
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
impl WorkflowLease for GuardWorkflowLease {
    fn verify_ownership(&self) -> BoxFuture<'_, Result<(), RuntimeError>> {
        self.inner.verify_ownership()
    }
    fn load(&self) -> BoxFuture<'_, Result<Option<WorkflowState>, RuntimeError>> {
        self.inner.load()
    }
    fn save<'a>(&'a self, state: &'a WorkflowState) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            self.inner.save(state).await?;
            if state.nodes.iter().any(|n| n.status == NodeStatus::Running) {
                self.fault.fire("workflow", &state.id).await;
            }
            Ok(())
        })
    }
}
async fn dispatch_boundary_guards(db: &SqlDatabase, url: &str) {
    let admin = sql(url).await;
    for model_boundary in [true, false] {
        let name = project();
        let fault = FenceAfterSave {
            admin: admin.clone(),
            namespace: serde_json::to_string(&("fixture-tenant", &name)).unwrap(),
            fired: Arc::new(AtomicBool::new(false)),
        };
        let store = GuardStore {
            inner: db.runs(scope(&name)),
            fault: fault.clone(),
            model_boundary,
        };
        let model = SlowProbe {
            calls: AtomicUsize::new(0),
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let callback = calls.clone();
        let mut tools = ToolRegistry::new();
        tools
            .register(FnTool::new(
                ToolDefinition {
                    name: "probe".into(),
                    description: "callback boundary fixture".into(),
                    effect: ToolEffect::ReadOnly,
                    permissions: Default::default(),
                    input_schema: json!({"type":"object","additionalProperties":false}),
                },
                move |_, _| {
                    let callback = callback.clone();
                    async move {
                        callback.fetch_add(1, Ordering::SeqCst);
                        Ok(Value::Null)
                    }
                },
            ))
            .unwrap();
        let agent = AgentLoop::new(&model, &tools, "fixture")
            .with_policy(ToolPolicy::default().allow("probe"))
            .with_limits(RunLimits {
                max_cost_micros: Some(20),
                ..Default::default()
            });
        assert!(DurableAgent::new(&agent, &store)
            .start("after-intent", vec![Message::user("fixture")])
            .await
            .is_err());
        assert!(fault.fired.load(Ordering::SeqCst));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            model.calls.load(Ordering::SeqCst),
            usize::from(!model_boundary)
        );
    }
    let name = project();
    let fault = FenceAfterSave {
        admin: admin.clone(),
        namespace: serde_json::to_string(&("fixture-tenant", &name)).unwrap(),
        fired: Arc::new(AtomicBool::new(false)),
    };
    let store = GuardWorkflowStore {
        inner: db.workflows(scope(&name)),
        fault: fault.clone(),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let callback = calls.clone();
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(FnWorkflowTask::new(
            TaskDefinition {
                name: "pure".into(),
                version: "fixture-v1".into(),
                recovery: TaskRecovery::Pure,
                max_cost_micros: Some(0),
                config: Value::Null,
            },
            move |_: NodeContext, _: taskloom::CancelToken| {
                let callback = callback.clone();
                async move {
                    callback.fetch_add(1, Ordering::SeqCst);
                    Ok(NodeOutcome::pure(Value::Null))
                }
            },
        ))
        .unwrap();
    assert!(WorkflowRunner::new(&tasks, &store)
        .start(
            "after-intent",
            WorkflowDefinition::new([WorkflowNode::new("work", "pure")]),
            Value::Null
        )
        .await
        .is_err());
    assert!(fault.fired.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    admin.close().await;
}
async fn close_drains_releases(db: &SqlDatabase, url: &str) {
    let closing = SqlDatabase::connect(url, options()).await.unwrap();
    let name = project();
    let store = closing.runs(scope(&name));
    let lease = store.acquire("close-owner").await.unwrap();
    let admin = sql(url).await;
    let ns = serde_json::to_string(&("fixture-tenant", &name)).unwrap();
    let mut blocker = admin
        .begin_with(if url.starts_with("sqlite:") {
            "BEGIN IMMEDIATE"
        } else {
            "BEGIN"
        })
        .await
        .unwrap();
    if !url.starts_with("sqlite:") {
        let _:i64=sqlx::query_scalar("SELECT epoch FROM taskloom_leases WHERE namespace=$1 AND family='run' AND object_id='close-owner' FOR UPDATE")
            .bind(&ns).fetch_one(&mut *blocker).await.unwrap();
    }
    // Let the first heartbeat enter real database I/O while another transaction
    // holds its row/write lock. Its future must not keep the owner Arc alive.
    tokio::time::sleep(Duration::from_millis(250)).await;
    drop(lease);
    let (_, released) = tokio::join!(closing.close(), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        blocker.commit().await
    });
    released.unwrap();
    let expiry:i64=sqlx::query_scalar("SELECT expires_ms FROM taskloom_leases WHERE namespace=$1 AND family='run' AND object_id='close-owner'")
        .bind(&ns).fetch_one(&admin).await.unwrap();
    assert_eq!(
        expiry, 0,
        "close must drain dropped-owner release even when heartbeat was in I/O"
    );
    let successor = db.runs(scope(&name)).acquire("close-owner").await.unwrap();
    drop(successor);
    admin.close().await;
}

async fn immediate_owner_reentry(db: &SqlDatabase) {
    let store = db.runs(scope(&project()));
    for _ in 0..64 {
        let owner = store
            .acquire("hot-reentry")
            .await
            .expect("a dropped owner must not cause spurious local occupancy");
        assert!(
            matches!(
                store.acquire("hot-reentry").await,
                Err(RuntimeError::Conflict(_))
            ),
            "a live owner must still exclude a second invocation"
        );
        drop(owner);
    }
}
#[tokio::test]
async fn sqlite_transactions_leases_namespaces_and_durable_sdk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime ü space.sqlite");
    let url = SqlDatabase::sqlite_url(&path).unwrap();
    let db = SqlDatabase::sqlite(&path, options()).await.unwrap();
    assert!(db
        .runs(scope("before-migration"))
        .inspect("absent")
        .await
        .is_err());
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    roundtrip(&db).await;
    ledger_and_lease(&db).await;
    fencing_and_corruption(&db, &url).await;
    dispatch_boundary_guards(&db, &url).await;
    close_drains_releases(&db, &url).await;
    immediate_owner_reentry(&db).await;
    let admin = sql(&url).await;
    sqlx::query("UPDATE taskloom_meta SET version=4 WHERE singleton=1")
        .execute(&admin)
        .await
        .unwrap();
    assert!(db.migrate().await.is_err());
    assert!(db.runs(scope("future-schema")).acquire("x").await.is_err());
    admin.close().await;
    db.close().await;
}

#[tokio::test]
#[ignore = "requires explicitly owned PostgreSQL TASKLOOM_TEST_POSTGRES_URL; run separately for real backend acceptance"]
async fn postgres_real_transactions_leases_namespaces_and_durable_sdk() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL")
        .expect("provide owned test database environment");
    let db = SqlDatabase::connect(&url, options()).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    roundtrip(&db).await;
    ledger_and_lease(&db).await;
    fencing_and_corruption(&db, &url).await;
    dispatch_boundary_guards(&db, &url).await;
    close_drains_releases(&db, &url).await;
    immediate_owner_reentry(&db).await;
    db.close().await;
}
