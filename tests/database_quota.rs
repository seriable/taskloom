#![cfg(feature = "database")]
//! SQL admission and lifecycle controls. Models here are labelled fixtures.
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use taskloom::runtime::{database::*, *};

fn scope() -> DatabaseNamespace {
    DatabaseNamespace::new("quota-test", &uuid::Uuid::new_v4().to_string()).unwrap()
}
fn policy(cost: u64, attempts: u64) -> GenerationQuota {
    GenerationQuota {
        currency: "USD".into(),
        max_cost_micros: cost,
        max_outstanding_attempts: attempts,
    }
}
fn reservation(generation: u64, ceiling: u64) -> BudgetSnapshot {
    BudgetSnapshot {
        generation,
        in_flight_micros: ceiling,
        in_flight_attempts: 1,
        ..Default::default()
    }
}

struct Metered {
    calls: AtomicUsize,
    ceiling: u64,
    actual: u64,
}
impl Model for Metered {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(self.ceiling).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            reservation.settle(self.actual);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text("fixture"),
            })
        })
    }
}
async fn atomic_admission(db: &SqlDatabase, other: &SqlDatabase) {
    let ns = scope();
    let quota = db.generation_quota(ns.clone());
    assert_eq!(
        quota.snapshot().await.unwrap().usage,
        None,
        "unimported history is unknown"
    );
    quota
        .configure(Some(policy(10, 1)), 0, "operator", "initial cap")
        .await
        .unwrap();
    let runs = db.runs(ns.clone());
    let others = other.runs(ns.clone());
    let a = runs.acquire("a").await.unwrap();
    let b = others.acquire("b").await.unwrap();
    let first = reservation(1, 7);
    let (x, y) = tokio::join!(a.save_budget(&first), b.save_budget(&first));
    assert_eq!(usize::from(x.is_ok()) + usize::from(y.is_ok()), 1);
    assert!(matches!(
        if x.is_err() { &x } else { &y },
        Err(RuntimeError::Budget(_))
    ));
    let (winner, loser) = if x.is_ok() { (&a, &b) } else { (&b, &a) };
    assert!(
        loser.load_budget().await.unwrap().is_none(),
        "refused transaction leaves no journal"
    );
    let used = quota.snapshot().await.unwrap().usage.unwrap();
    assert_eq!(
        (
            used.held_micros().unwrap(),
            used.outstanding_attempts().unwrap()
        ),
        (7, 1)
    );
    let settled = BudgetSnapshot {
        generation: 2,
        committed_micros: 3,
        ..Default::default()
    };
    winner.save_budget(&settled).await.unwrap();
    winner.save_budget(&first).await.unwrap(); // stale persistence cannot revert accounting
    let used = quota.snapshot().await.unwrap().usage.unwrap();
    assert_eq!(
        (
            used.held_micros().unwrap(),
            used.outstanding_attempts().unwrap()
        ),
        (3, 0)
    );
    loser.save_budget(&first).await.unwrap(); // exact remaining cap, through another pool
    let mut collision = first.clone();
    collision.in_flight_micros = 1;
    assert!(loser.save_budget(&collision).await.is_err());
    let another = db.generation_quota(scope()).snapshot().await.unwrap();
    assert!(another.usage.is_none());
}
async fn refused_dispatch_and_overrun(db: &SqlDatabase) {
    let ns = scope();
    let quota = db.generation_quota(ns.clone());
    quota
        .configure(Some(policy(0, 0)), 0, "operator", "pause dispatch")
        .await
        .unwrap();
    let runs = db.runs(ns);
    let tools = ToolRegistry::new();
    let model = Metered {
        calls: AtomicUsize::new(0),
        ceiling: 0,
        actual: 0,
    };
    let agent = AgentLoop::new(&model, &tools, "fixture");
    let durable = DurableAgent::new(&agent, &runs);
    let result = durable
        .start("refused", vec![Message::user("fixture")])
        .await
        .unwrap();
    assert_eq!(result.report.stop_reason, StopReason::Budget);
    assert_eq!(result.report.budget.committed_micros, 0);
    assert_eq!(result.report.budget.uncertain_attempts, 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        durable.resume("refused").await.unwrap().report.stop_reason,
        StopReason::Budget
    );
    quota
        .configure(Some(policy(10, 1)), 1, "operator", "resume new runs")
        .await
        .unwrap();
    let model = Metered {
        calls: AtomicUsize::new(0),
        ceiling: 7,
        actual: 12,
    };
    let agent = AgentLoop::new(&model, &tools, "fixture");
    let result = DurableAgent::new(&agent, &runs)
        .start("overrun", vec![Message::user("fixture")])
        .await
        .unwrap();
    assert_eq!(
        result.report.budget.committed_micros, 12,
        "incurred overspend is saved even when policy is exceeded"
    );
    assert!(quota.snapshot().await.unwrap().overrun);
    let result = durable
        .start("after-overrun", vec![Message::user("fixture")])
        .await
        .unwrap();
    assert_eq!(result.report.stop_reason, StopReason::Budget);
    quota
        .configure(
            Some(policy(20, 1)),
            2,
            "operator",
            "review retained overspend",
        )
        .await
        .unwrap();
    assert!(!quota.snapshot().await.unwrap().overrun);
    assert_eq!(
        durable
            .start("after-review", vec![Message::user("fixture")])
            .await
            .unwrap()
            .report
            .stop_reason,
        StopReason::Completed
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);

    // Even with room under the project cap, reporting more than the admitted
    // reservation requires review before a fresh dispatch.
    let model = Metered {
        calls: AtomicUsize::new(0),
        ceiling: 1,
        actual: 2,
    };
    let agent = AgentLoop::new(&model, &tools, "fixture");
    DurableAgent::new(&agent, &runs)
        .start("ceiling-overrun", vec![Message::user("fixture")])
        .await
        .unwrap();
    assert!(quota.snapshot().await.unwrap().overrun);
    quota
        .configure(
            Some(policy(20, 1)),
            3,
            "operator",
            "review reported cost above admitted ceiling",
        )
        .await
        .unwrap();
    assert!(!quota.snapshot().await.unwrap().overrun);
}

struct NoDispatch;
impl taskloom::Provider for NoDispatch {
    fn id(&self) -> &str {
        "http://quota.fixture.invalid"
    }
    fn complete<'a>(
        &'a self,
        _: &'a taskloom::ChatRequest,
        _: Option<taskloom::Streaming<'a>>,
    ) -> BoxFuture<'a, Result<taskloom::ChatResponse, taskloom::ModelError>> {
        Box::pin(async { panic!("unexpected typed API dispatch") })
    }
    fn converse<'a>(
        &'a self,
        _: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelReply, taskloom::ModelError>> {
        Box::pin(async {
            panic!("missing prices or project cap must refuse before provider dispatch")
        })
    }
}
async fn priced_ceiling_without_run_limit(db: &SqlDatabase) {
    use std::sync::Arc;
    use taskloom::{
        gateway::{ModelPrice, PriceTable},
        Gateway, GatewayConfig, GatewayTransport,
    };
    let ns = scope();
    let quota = db.generation_quota(ns.clone());
    quota
        .configure(Some(policy(1, 2)), 0, "operator", "global monetary cap")
        .await
        .unwrap();
    let runs = db.runs(ns);
    let tools = ToolRegistry::new();
    for priced in [false, true] {
        let gateway = Gateway::new(GatewayConfig {
            prices: PriceTable::new(if priced {
                vec![ModelPrice::new("fixture", 1.0, 1.0)]
            } else {
                vec![]
            }),
            ..Default::default()
        });
        let model = GatewayTransport::new(Arc::new(gateway), NoDispatch);
        let agent = AgentLoop::new(&model, &tools, "fixture").with_limits(RunLimits {
            max_cost_micros: None,
            ..Default::default()
        });
        let result = DurableAgent::new(&agent, &runs)
            .start(
                if priced { "priced" } else { "unpriced" },
                vec![Message::user("fixture")],
            )
            .await
            .unwrap();
        assert_eq!(result.report.stop_reason, StopReason::Budget);
        assert_eq!(result.report.budget.uncertain_attempts, 0);
        assert_eq!(result.report.budget.committed_micros, 0);
    }
}
async fn import_disable_and_crash_occupancy(db: &SqlDatabase) {
    let ns = scope();
    let runs = db.runs(ns.clone());
    let quota = db.generation_quota(ns);
    let lease = runs.acquire("orphan-reservation").await.unwrap();
    let in_flight = reservation(1, 0);
    lease.save_budget(&in_flight).await.unwrap();
    assert!(quota.snapshot().await.unwrap().usage.is_none());
    let snapshot = quota
        .configure(
            Some(policy(0, 1)),
            0,
            "operator",
            "import existing zero-price attempt",
        )
        .await
        .unwrap();
    assert_eq!(snapshot.usage.unwrap().outstanding_attempts().unwrap(), 1);
    let unknown = BudgetSnapshot {
        generation: 2,
        uncertain_attempts: 1,
        ..Default::default()
    };
    lease.save_budget(&unknown).await.unwrap();
    let second = runs.acquire("another").await.unwrap();
    assert!(matches!(
        second.save_budget(&in_flight).await,
        Err(RuntimeError::Budget(_))
    ));
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
        "crash conversion retains slot"
    );
    quota
        .configure(None, 1, "operator", "explicitly disable enforcement")
        .await
        .unwrap();
    second.save_budget(&in_flight).await.unwrap();
    let used = quota.snapshot().await.unwrap().usage.unwrap();
    assert_eq!(
        used.outstanding_attempts().unwrap(),
        2,
        "disabled enforcement keeps accounting"
    );
    assert!(matches!(
        quota
            .configure(Some(policy(0, 3)), 1, "operator", "stale CAS")
            .await,
        Err(RuntimeError::Conflict(_))
    ));
    let mut currency = policy(0, 3);
    currency.currency = "CNY".into();
    assert!(quota
        .configure(Some(currency), 2, "operator", "cannot reprice history")
        .await
        .is_err());
    let snapshot = quota
        .configure(
            Some(policy(0, 1)),
            2,
            "operator",
            "cap lower than retained usage",
        )
        .await
        .unwrap();
    assert!(snapshot.overrun);
    assert_eq!(snapshot.events.len(), 3);
    let mut corrupt = unknown.clone();
    corrupt.generation = 3;
    corrupt.uncertain_attempts = 0;
    assert!(
        lease.save_budget(&corrupt).await.is_err(),
        "unknown attempts cannot silently disappear"
    );
}

async fn import_newest_generations_and_refuse_conflicting_history(db: &SqlDatabase) {
    let ns = scope();
    let runs = db.runs(ns.clone());
    let tools = ToolRegistry::new();
    let model = Metered {
        calls: AtomicUsize::new(0),
        ceiling: 7,
        actual: 3,
    };
    let agent = AgentLoop::new(&model, &tools, "fixture");
    DurableAgent::new(&agent, &runs)
        .start("settled", vec![Message::user("fixture")])
        .await
        .unwrap();
    let lease = runs.acquire("orphan").await.unwrap();
    lease.save_budget(&reservation(1, 7)).await.unwrap();
    let quota = db.generation_quota(ns);
    let value = quota
        .configure(
            Some(policy(100, 2)),
            0,
            "operator",
            "coalesce newest checkpoint and orphan reservation",
        )
        .await
        .unwrap();
    let usage = value.usage.unwrap();
    assert_eq!(
        (usage.runs, usage.committed_micros, usage.in_flight_micros),
        (2, 3, 7)
    );

    let ns = scope();
    let runs = db.runs(ns.clone());
    DurableAgent::new(&agent, &runs)
        .create("conflict", vec![Message::user("fixture")])
        .await
        .unwrap();
    let lease = runs.acquire("conflict").await.unwrap();
    lease
        .save_budget(&BudgetSnapshot {
            committed_micros: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let quota = db.generation_quota(ns);
    assert!(matches!(
        quota
            .configure(
                Some(policy(100, 2)),
                0,
                "operator",
                "reject equal-generation conflict"
            )
            .await,
        Err(RuntimeError::Storage(_))
    ));
    assert!(
        quota.snapshot().await.unwrap().usage.is_none(),
        "failed bootstrap is transactional and cannot reset or partially import history"
    );
}

async fn lifecycle(db: &SqlDatabase, url: &str) {
    let other = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    atomic_admission(db, &other).await;
    refused_dispatch_and_overrun(db).await;
    import_disable_and_crash_occupancy(db).await;
    priced_ceiling_without_run_limit(db).await;
    import_newest_generations_and_refuse_conflicting_history(db).await;
    other.close().await;
}
#[tokio::test]
async fn sqlite_project_quota_atomic_admission_and_accounting() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("quota.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    lifecycle(&db, &url).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; real backend acceptance is separate"]
async fn postgres_project_quota_atomic_admission_and_accounting() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    lifecycle(&db, &url).await;
    db.close().await;
}

#[tokio::test]
async fn sqlite_v1_upgrade_requires_quiescence_and_preserves_dataset_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("v1.sqlite")).unwrap();
    version_barrier(&url).await;
}
#[tokio::test]
#[ignore = "temporarily changes schema metadata: run alone against an explicitly owned, otherwise idle PostgreSQL fixture"]
async fn postgres_v1_upgrade_requires_quiescence_and_preserves_dataset_and_history() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned idle database required");
    version_barrier(&url).await;
}
async fn version_barrier(url: &str) {
    let db = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let identity = db.identity().await.unwrap();
    let ns = scope();
    let runs = db.runs(ns.clone());
    let owner = runs.acquire("legacy").await.unwrap();
    owner.save_budget(&reservation(1, 7)).await.unwrap();
    let admin = sqlx::AnyPool::connect(url).await.unwrap();
    sqlx::query("UPDATE taskloom_meta SET version=1")
        .execute(&admin)
        .await
        .unwrap();
    assert!(matches!(db.migrate().await, Err(RuntimeError::Conflict(_))));
    assert!(
        owner.verify_ownership().await.is_err(),
        "version barrier fences dispatch"
    );
    drop(owner);
    db.close().await;
    let db = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    assert_eq!(db.identity().await.unwrap(), identity);
    let snapshot = db
        .generation_quota(ns)
        .configure(
            Some(policy(10, 1)),
            0,
            "operator",
            "import migrated v1 ledger",
        )
        .await
        .unwrap();
    assert_eq!(snapshot.usage.unwrap().held_micros().unwrap(), 7);
    admin.close().await;
    db.close().await;
}
