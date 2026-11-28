#![cfg(feature = "database")]
//! Real SQL transaction/isolation controls; fixture models are not native quality.
use futures_util::future::BoxFuture;
use serde_json::json;
use sqlx::Row;
use std::sync::atomic::{AtomicUsize, Ordering};
use taskloom::runtime::{database::*, workflow::*, *};

fn policy(attempts: u64) -> GenerationQuota {
    GenerationQuota {
        currency: "USD".into(),
        max_cost_micros: 100,
        max_outstanding_attempts: attempts,
    }
}
fn reservation(generation: u64) -> BudgetSnapshot {
    BudgetSnapshot {
        generation,
        in_flight_attempts: 1,
        ..Default::default()
    }
}
struct Metered {
    calls: AtomicUsize,
}
impl Model for Metered {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text("fixture completed"),
            })
        })
    }
}

async fn controls(database: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let namespace = DatabaseNamespace::new("private-test", &project).unwrap();
    let quota = database.generation_quota(namespace.clone());
    assert!(
        database
            .private_stores_if_exists("private-test", &project, "alice")
            .await
            .unwrap()
            .is_none(),
        "first read must not initialize quota or private history"
    );
    assert!(database
        .private_stores("private-test", &project, "alice")
        .await
        .is_err());
    assert!(database
        .open_private_stores("private-test", &project, "alice")
        .await
        .is_err());
    assert!(
        quota.snapshot().await.unwrap().usage.is_none(),
        "refused factory must not initialize accounting"
    );
    quota
        .configure(
            Some(policy(10)),
            0,
            "operator",
            "explicit project admission",
        )
        .await
        .unwrap();
    let alice = database
        .private_stores("private-test", &project, "alice")
        .await
        .unwrap();
    let bob = database
        .private_stores("private-test", &project, "bob")
        .await
        .unwrap();
    let alice_runs = alice.runs();
    let bob_runs = bob.runs();
    let shared_runs = database.runs(namespace.clone());
    let model = Metered {
        calls: AtomicUsize::new(0),
    };
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "fixture").with_limits(RunLimits {
        max_cost_micros: Some(0),
        ..Default::default()
    });
    DurableAgent::new(&agent, &alice_runs)
        .start("same", vec![Message::user("alice-private")])
        .await
        .unwrap();
    DurableAgent::new(&agent, &bob_runs)
        .start("same", vec![Message::user("bob-private")])
        .await
        .unwrap();
    DurableAgent::new(&agent, &shared_runs)
        .start("same", vec![Message::user("project-shared")])
        .await
        .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(alice_runs.list().await.unwrap().len(), 1);
    assert_eq!(bob_runs.list().await.unwrap().len(), 1);
    assert_eq!(shared_runs.list().await.unwrap().len(), 1);
    assert!(
        serde_json::to_string(&alice_runs.inspect("same").await.unwrap())
            .unwrap()
            .contains("alice-private")
    );
    assert!(!serde_json::to_string(&bob_runs.list().await.unwrap())
        .unwrap()
        .contains("alice-private"));
    assert!(!serde_json::to_string(&shared_runs.list().await.unwrap())
        .unwrap()
        .contains("alice-private"));
    let alice_sessions = alice.sessions();
    let bob_sessions = bob.sessions();
    SessionManager::new(&alice_sessions)
        .create("chat", vec!["alice-private-session".into()])
        .await
        .unwrap();
    SessionManager::new(&bob_sessions)
        .create("chat", vec!["bob-private-session".into()])
        .await
        .unwrap();
    assert!(database
        .sessions(namespace.clone())
        .list()
        .await
        .unwrap()
        .is_empty());
    assert!(!serde_json::to_string(&bob_sessions.list().await.unwrap())
        .unwrap()
        .contains("alice-private-session"));
    assert!(database
        .workflows(namespace.clone())
        .list()
        .await
        .unwrap()
        .is_empty());
    let workflows = alice.workflows();
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(
            AgentWorkflowTask::new(
                "private-worker",
                &agent,
                &alice_runs,
                "private fixture role",
                WorkflowOutput::Text,
            )
            .unwrap(),
        )
        .unwrap();
    let state = WorkflowRunner::new(&tasks, &workflows)
        .with_limits(WorkflowLimits {
            max_cost_micros: Some(0),
            ..Default::default()
        })
        .start(
            "flow",
            WorkflowDefinition::new([WorkflowNode::new("one", "private-worker")]),
            json!({"goal":"alice-private-workflow"}),
        )
        .await
        .unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(workflows.list().await.unwrap().len(), 1);
    assert!(bob.workflows().list().await.unwrap().is_empty());
    assert!(database
        .workflows(namespace.clone())
        .list()
        .await
        .unwrap()
        .is_empty());
    let child = &state.node("one").unwrap().child.as_ref().unwrap().run_id;
    assert!(alice_runs.inspect(child).await.unwrap().is_some());
    assert!(bob_runs.inspect(child).await.unwrap().is_none());
    assert!(shared_runs.inspect(child).await.unwrap().is_none());
    assert_eq!(quota.snapshot().await.unwrap().usage.unwrap().runs, 4);

    // Even a shared SDK caller selecting the exact hashed private accounting ID
    // gets a distinct family. Accounting identity is not just a hash prefix.
    let raw = sqlx::AnyPool::connect(url).await.unwrap();
    let project_json = serde_json::to_string(&("private-test", &project)).unwrap();
    let account_id: String = sqlx::query_scalar("SELECT object_id FROM taskloom_records WHERE namespace=$1 AND family='quota_private_run' ORDER BY object_id LIMIT 1").bind(&project_json).fetch_one(&raw).await.unwrap();
    let collision = shared_runs.acquire(&account_id).await.unwrap();
    collision.save_budget(&reservation(1)).await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM taskloom_records WHERE namespace=$1 AND object_id=$2 AND family IN ('quota_run','quota_private_run')").bind(&project_json).bind(&account_id).fetch_one(&raw).await.unwrap();
    assert_eq!(
        rows, 2,
        "shared and private accounts use separate SQL families"
    );
    drop(collision);

    // Frozen project mapping refuses corruption instead of changing a quota
    // destination on resume. The already-existing private journal stays intact.
    let owner_json = serde_json::to_string(&("private-test", &project, "alice")).unwrap();
    let row = sqlx::query(
        "SELECT body FROM taskloom_records WHERE namespace=$1 AND family='private_binding'",
    )
    .bind(&owner_json)
    .fetch_one(&raw)
    .await
    .unwrap();
    let original: String = row.try_get("body").unwrap();
    let mut changed: serde_json::Value = serde_json::from_str(&original).unwrap();
    changed["value"]["project_namespace"] =
        json!(serde_json::to_string(&("private-test", "other-project")).unwrap());
    sqlx::query(
        "UPDATE taskloom_records SET body=$1 WHERE namespace=$2 AND family='private_binding'",
    )
    .bind(changed.to_string())
    .bind(&owner_json)
    .execute(&raw)
    .await
    .unwrap();
    assert!(alice_runs.inspect("same").await.is_err());
    assert!(database
        .open_private_stores("private-test", &project, "alice")
        .await
        .is_err());
    sqlx::query(
        "UPDATE taskloom_records SET body=$1 WHERE namespace=$2 AND family='private_binding'",
    )
    .bind(&original)
    .bind(&owner_json)
    .execute(&raw)
    .await
    .unwrap();
    assert!(alice_runs.inspect("same").await.unwrap().is_some());
    sqlx::query("DELETE FROM taskloom_records WHERE namespace=$1 AND family='private_binding'")
        .bind(&owner_json)
        .execute(&raw)
        .await
        .unwrap();
    assert!(
        database
            .private_stores_if_exists("private-test", &project, "alice")
            .await
            .is_err(),
        "lost binding with retained history is not an empty first visit"
    );
    assert!(
        database
            .private_stores("private-test", &project, "alice")
            .await
            .is_err(),
        "missing immutable mapping cannot be silently rebound"
    );
    sqlx::query("INSERT INTO taskloom_records(namespace,family,object_id,revision,body) VALUES($1,'private_binding','project',1,$2)")
        .bind(&owner_json).bind(&original).execute(&raw).await.unwrap();

    // A lost quota-control row must not turn retained private spending into an
    // incomplete legacy import. The transaction rolls back the seeded control.
    sqlx::query("DELETE FROM taskloom_records WHERE namespace=$1 AND family='quota'")
        .bind(&project_json)
        .execute(&raw)
        .await
        .unwrap();
    assert!(quota
        .configure(
            Some(policy(10)),
            0,
            "operator",
            "must not erase private history"
        )
        .await
        .is_err());
    assert!(quota.snapshot().await.unwrap().usage.is_none());
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM taskloom_records WHERE namespace=$1 AND family='quota_private_run'",
    )
    .bind(&project_json)
    .fetch_one(&raw)
    .await
    .unwrap();
    assert_eq!(remaining, 3);
    raw.close().await;
}

async fn shared_atomic_unknown_and_disable(database: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let namespace = DatabaseNamespace::new("private-admission", &project).unwrap();
    let quota = database.generation_quota(namespace.clone());
    quota
        .configure(
            Some(policy(1)),
            0,
            "operator",
            "one project slot across private and shared",
        )
        .await
        .unwrap();
    let other = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    let alice = database
        .private_stores("private-admission", &project, "alice")
        .await
        .unwrap();
    let bob = other
        .private_stores("private-admission", &project, "bob")
        .await
        .unwrap();
    let a = alice.runs();
    let b = bob.runs();
    let shared = database.runs(namespace.clone());
    let x = a.acquire("same").await.unwrap();
    let y = b.acquire("same").await.unwrap();
    let z = shared.acquire("same").await.unwrap();
    let value = reservation(1);
    let (p, q, r) = tokio::join!(
        x.save_budget(&value),
        y.save_budget(&value),
        z.save_budget(&value)
    );
    assert_eq!(
        [p.is_ok(), q.is_ok(), r.is_ok()]
            .into_iter()
            .filter(|ok| *ok)
            .count(),
        1
    );
    let winner = if p.is_ok() {
        &x
    } else if q.is_ok() {
        &y
    } else {
        &z
    };
    let unknown = BudgetSnapshot {
        generation: 2,
        uncertain_attempts: 1,
        ..Default::default()
    };
    winner.save_budget(&unknown).await.unwrap();
    let usage = quota.snapshot().await.unwrap().usage.unwrap();
    assert_eq!(
        (
            usage.held_micros().unwrap(),
            usage.outstanding_attempts().unwrap()
        ),
        (0, 1)
    );
    assert_eq!(
        usize::from(x.load_budget().await.unwrap().is_some())
            + usize::from(y.load_budget().await.unwrap().is_some())
            + usize::from(z.load_budget().await.unwrap().is_some()),
        1,
        "refused admission leaves no private or shared journal"
    );
    drop((x, y, z));

    quota
        .configure(None, 1, "operator", "disable new private dispatch")
        .await
        .unwrap();
    assert!(database
        .private_stores("private-admission", &project, "new-owner")
        .await
        .is_err());
    let reopened = database
        .open_private_stores("private-admission", &project, "alice")
        .await
        .unwrap();
    let reopened_runs = reopened.runs();
    let settled = reopened_runs.acquire("settle-only").await.unwrap();
    // Settlement/checkpoint persistence is allowed, but any fresh reservation
    // is rejected even while ordinary project enforcement is disabled.
    settled
        .save_budget(&BudgetSnapshot {
            generation: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(matches!(
        settled.save_budget(&reservation(2)).await,
        Err(RuntimeError::Budget(_))
    ));
    assert_eq!(
        settled
            .load_budget()
            .await
            .unwrap()
            .unwrap()
            .in_flight_attempts,
        0
    );
    assert_eq!(
        quota
            .snapshot()
            .await
            .unwrap()
            .usage
            .unwrap()
            .outstanding_attempts()
            .unwrap(),
        1,
        "disabling preserves unknown zero-price occupancy"
    );
    quota
        .configure(
            Some(policy(1)),
            2,
            "operator",
            "review retained unknown attempt",
        )
        .await
        .unwrap();
    assert!(matches!(
        settled.save_budget(&reservation(2)).await,
        Err(RuntimeError::Budget(_))
    ));
    let usage = quota.snapshot().await.unwrap().usage.unwrap();
    assert_eq!(usage.uncertain_attempts, 1);
    assert_eq!(usage.outstanding_attempts().unwrap(), 1);
    drop(settled);
    // Deterministically add a private unknown attempt, regardless of which
    // branch won the three-way race above; a shared writer must see its slot.
    quota
        .configure(
            Some(policy(2)),
            3,
            "operator",
            "allow exactly one further private attempt",
        )
        .await
        .unwrap();
    let private_unknown = reopened_runs.acquire("private-unknown").await.unwrap();
    private_unknown.save_budget(&reservation(1)).await.unwrap();
    private_unknown
        .save_budget(&BudgetSnapshot {
            generation: 2,
            uncertain_attempts: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    drop(private_unknown);
    let cold = other
        .open_private_stores("private-admission", &project, "alice")
        .await
        .unwrap()
        .runs();
    assert_eq!(
        cold.inspect_budget("private-unknown")
            .await
            .unwrap()
            .unwrap()
            .uncertain_attempts,
        1
    );
    let blocked_shared = shared.acquire("blocked-by-private-unknown").await.unwrap();
    assert!(matches!(
        blocked_shared.save_budget(&reservation(1)).await,
        Err(RuntimeError::Budget(_))
    ));
    assert!(blocked_shared.load_budget().await.unwrap().is_none());
    let usage = quota.snapshot().await.unwrap().usage.unwrap();
    assert_eq!(
        (
            usage.held_micros().unwrap(),
            usage.uncertain_attempts,
            usage.outstanding_attempts().unwrap()
        ),
        (0, 2, 2)
    );
    drop(blocked_shared);
    other.close().await;
}

#[tokio::test]
async fn sqlite_private_sessions_and_project_quota_are_isolated_and_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(directory.path().join("private.sqlite")).unwrap();
    let database = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    database.migrate().await.unwrap();
    controls(&database, &url).await;
    shared_atomic_unknown_and_disable(&database, &url).await;
    database.close().await;
}

#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; modifies only unique owned test scopes"]
async fn postgres_private_sessions_and_project_quota_are_isolated_and_atomic() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PostgreSQL required");
    let database = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    database.migrate().await.unwrap();
    controls(&database, &url).await;
    shared_atomic_unknown_and_disable(&database, &url).await;
    database.close().await;
}
