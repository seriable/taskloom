#![cfg(feature = "database")]
use serde_json::json;
use std::time::Duration;
use taskloom::runtime::{database::*, *};

fn ns() -> DatabaseNamespace {
    DatabaseNamespace::new("queue-fixture", &uuid::Uuid::new_v4().to_string()).unwrap()
}
fn binding() -> String {
    "a".repeat(64)
}
fn limits(open: u32, running: u32) -> QueueLimits {
    QueueLimits {
        max_open_jobs: open,
        max_running: running,
    }
}
async fn controls(db: &SqlDatabase, url: &str) {
    let scope = ns();
    let queue = db.queue(scope.clone());
    let other = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    let peer = other.queue(scope.clone());
    queue
        .configure(
            Some(limits(2, 1)),
            &binding(),
            0,
            "operator",
            "bounded queue",
        )
        .await
        .unwrap();
    let first = queue
        .enqueue(
            "one",
            "session:shared",
            json!({"exact":9007199254740993_u64,"decimal":1.0}),
            &binding(),
            "alice",
        )
        .await
        .unwrap();
    assert_eq!(
        queue
            .enqueue(
                "one",
                "session:shared",
                first.payload.clone(),
                &binding(),
                "bob"
            )
            .await
            .unwrap(),
        first,
        "idempotence does not change original submission actor"
    );
    assert!(peer
        .enqueue("two", "session:shared", json!({}), &binding(), "alice")
        .await
        .is_err());
    assert!(peer
        .enqueue(
            "one",
            "session:shared",
            json!({"changed":true}),
            &binding(),
            "alice"
        )
        .await
        .is_err());
    peer.enqueue("two", "run:second", json!({}), &binding(), "alice")
        .await
        .unwrap();
    assert!(queue
        .enqueue("three", "run:third", json!({}), &binding(), "alice")
        .await
        .is_err());
    let host_binding = binding();
    let (a, b) = tokio::join!(
        queue.claim(&host_binding, "worker-a"),
        peer.claim(&host_binding, "worker-b")
    );
    assert_eq!(
        usize::from(a.as_ref().unwrap().is_some()) + usize::from(b.as_ref().unwrap().is_some()),
        1
    );
    let owner = a.unwrap().or(b.unwrap()).unwrap();
    let claimed = owner.snapshot().await.unwrap();
    assert_eq!(claimed.id, "one");
    let waiting = owner
        .finish(QueueState::Waiting, "await exact input approval")
        .await
        .unwrap();
    assert!(
        owner.verify_ownership().await.is_err(),
        "finished queue claims cannot authorize more work"
    );
    assert!(
        queue
            .enqueue("three", "run:third", json!({}), &binding(), "alice")
            .await
            .is_err(),
        "waiting approval retains open capacity"
    );
    drop(owner);
    let second = peer.claim(&binding(), "worker-c").await.unwrap().unwrap();
    assert_eq!(second.snapshot().await.unwrap().id, "two");
    second
        .finish(QueueState::Completed, "fixture completed")
        .await
        .unwrap();
    drop(second);
    let resumed = queue
        .resume(
            &waiting.id,
            waiting.revision,
            &binding(),
            "runner",
            "approved externally; explicit resume",
        )
        .await
        .unwrap();
    assert!(queue
        .resume(
            &waiting.id,
            waiting.revision,
            &binding(),
            "runner",
            "stale revision"
        )
        .await
        .is_err());
    let pause = peer
        .control(
            "one",
            resumed.revision,
            QueueControl::Pause {
                actor: "operator".into(),
                note: "operator pause".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(pause.state, QueueState::Paused);
    assert!(queue.claim(&binding(), "worker-d").await.unwrap().is_none());
    let resumed = queue
        .resume(
            "one",
            pause.revision,
            &binding(),
            "runner",
            "explicit restart",
        )
        .await
        .unwrap();
    let cancelled = peer
        .control(
            "one",
            resumed.revision,
            QueueControl::Cancel {
                actor: "operator".into(),
                note: "original cancellation".into(),
            },
        )
        .await
        .unwrap();
    assert!(queue
        .control(
            "one",
            cancelled.revision,
            QueueControl::Cancel {
                actor: "spoofed".into(),
                note: "replace attribution".into()
            }
        )
        .await
        .is_err());
    let owner = queue.claim(&binding(), "worker-e").await.unwrap().unwrap();
    assert!(
        matches!(owner.snapshot().await.unwrap().control,Some(QueueControl::Cancel{actor,..}) if actor=="operator")
    );
    queue
        .configure(None, &binding(), 1, "operator", "disable new admission")
        .await
        .unwrap();
    assert!(peer.claim(&binding(), "worker-f").await.is_err());
    owner
        .finish(QueueState::Stopped, "known cancellation boundary")
        .await
        .unwrap();
    drop(owner);
    assert!(db.queue(ns()).inspect("one").await.unwrap().is_none());
    let jobs = queue.list().await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(!jobs.iter().any(|job| job.state.is_open()));
    other.close().await;
}
async fn fenced_runtime(db: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let scope = DatabaseNamespace::new("queue-fixture", &project).unwrap();
    let queue = db.queue(scope.clone());
    queue
        .configure(
            Some(limits(10, 2)),
            &binding(),
            0,
            "operator",
            "fencing probe",
        )
        .await
        .unwrap();
    queue
        .enqueue("guard", "run:guard", json!({}), &binding(), "runner")
        .await
        .unwrap();
    let owner = queue
        .claim(&binding(), "old-worker")
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.runs(ns()).with_queue_lease(&owner).is_err(),
        "namespace is part of the capability"
    );
    let guarded = db.runs(scope.clone()).with_queue_lease(&owner).unwrap();
    let lease = guarded.acquire("guard").await.unwrap();
    lease.verify_ownership().await.unwrap();
    let reservation = BudgetSnapshot {
        generation: 1,
        in_flight_attempts: 1,
        ..Default::default()
    };
    lease.save_budget(&reservation).await.unwrap();
    let admin = sqlx::AnyPool::connect(url).await.unwrap();
    sqlx::query("UPDATE taskloom_leases SET owner='injected-fence',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='queue_job' AND object_id='guard'").bind(serde_json::to_string(&("queue-fixture",&project)).unwrap()).execute(&admin).await.unwrap();
    assert!(lease.verify_ownership().await.is_err());
    let next = BudgetSnapshot {
        generation: 2,
        in_flight_attempts: 2,
        ..Default::default()
    };
    assert!(
        lease.save_budget(&next).await.is_err(),
        "stale queue owner cannot reserve more cost"
    );
    assert_eq!(
        lease.load_budget().await.unwrap(),
        Some(reservation.clone())
    );
    assert!(
        queue
            .claim(&binding(), "successor")
            .await
            .unwrap()
            .is_none(),
        "live runtime owner prevents premature queue takeover"
    );
    drop(lease);
    drop(guarded);
    drop(owner);
    let successor = queue.claim(&binding(), "successor").await.unwrap().unwrap();
    assert_eq!(successor.snapshot().await.unwrap().attempts, 2);
    let guarded = db.runs(scope.clone()).with_queue_lease(&successor).unwrap();
    let lease = guarded.acquire("guard").await.unwrap();
    assert_eq!(lease.load_budget().await.unwrap(), Some(reservation));
    drop(lease);
    drop(guarded);
    let bare = db.runs(scope.clone());
    let lease = bare.acquire("guard").await.unwrap();
    assert!(
        matches!(
            lease.verify_ownership().await,
            Err(RuntimeError::Permission(_))
        ),
        "removing a queue capability cannot bypass its persisted binding"
    );
    assert!(lease.save_budget(&next).await.is_err());
    drop(lease);
    let waiting = successor
        .finish(
            QueueState::Waiting,
            "unknown attempt retained for runtime recovery",
        )
        .await
        .unwrap();
    drop(successor);
    let quota = db.generation_quota(scope.clone());
    quota
        .configure(
            Some(GenerationQuota {
                currency: "USD".into(),
                max_cost_micros: 0,
                max_outstanding_attempts: 1,
            }),
            0,
            "operator",
            "import retained crash reservation before scheduling abandonment",
        )
        .await
        .unwrap();
    let abandoned=queue.abandon("guard",waiting.revision,"operator","original host contract unavailable; archive scheduling without guessing the external outcome").await.unwrap();
    assert_eq!(abandoned.state, QueueState::Abandoned);
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
        "abandon does not refund unknown generation occupancy"
    );
    assert!(queue
        .resume(
            "guard",
            abandoned.revision,
            &binding(),
            "runner",
            "cannot resurrect archived scheduling"
        )
        .await
        .is_err());
    queue
        .configure(
            Some(limits(10, 2)),
            &"b".repeat(64),
            1,
            "operator",
            "new host contract after archived scheduling",
        )
        .await
        .unwrap();
    admin.close().await;
}
async fn lowered_cap_applies_to_recovery(db: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let scope = DatabaseNamespace::new("queue-fixture", &project).unwrap();
    let queue = db.queue(scope);
    queue
        .configure(
            Some(limits(4, 2)),
            &binding(),
            0,
            "operator",
            "two running slots",
        )
        .await
        .unwrap();
    for id in ["a", "b"] {
        queue
            .enqueue(id, &format!("run:{id}"), json!({}), &binding(), "runner")
            .await
            .unwrap();
    }
    let a = queue.claim(&binding(), "a").await.unwrap().unwrap();
    let live = a.snapshot().await.unwrap();
    assert!(queue
        .abandon(
            &live.id,
            live.revision,
            "operator",
            "cannot archive a live owner"
        )
        .await
        .is_err());
    let b = queue.claim(&binding(), "b").await.unwrap().unwrap();
    let lost = b.snapshot().await.unwrap().id;
    let admin = sqlx::AnyPool::connect(url).await.unwrap();
    sqlx::query("UPDATE taskloom_leases SET owner='lower-cap-fault',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='queue_job' AND object_id=$2").bind(serde_json::to_string(&("queue-fixture",&project)).unwrap()).bind(&lost).execute(&admin).await.unwrap();
    queue
        .configure(
            Some(limits(4, 1)),
            &binding(),
            1,
            "operator",
            "lower cap while one original worker remains live",
        )
        .await
        .unwrap();
    assert!(
        queue.claim(&binding(), "recovery").await.unwrap().is_none(),
        "recovering a crashed job cannot exceed the newly lowered active cap"
    );
    a.finish(QueueState::Completed, "original worker completed")
        .await
        .unwrap();
    drop(a);
    drop(b);
    let recovered = queue.claim(&binding(), "recovery").await.unwrap().unwrap();
    assert_eq!(recovered.snapshot().await.unwrap().id, lost);
    recovered
        .finish(QueueState::Stopped, "bounded recovery verified")
        .await
        .unwrap();
    drop(recovered);
    queue
        .enqueue(
            "abandon-dead",
            "run:abandon-dead",
            json!({}),
            &binding(),
            "runner",
        )
        .await
        .unwrap();
    let dead = queue
        .claim(&binding(), "dead-owner")
        .await
        .unwrap()
        .unwrap();
    let view = dead.snapshot().await.unwrap();
    sqlx::query("UPDATE taskloom_leases SET owner='abandon-dead-fault',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='queue_job' AND object_id=$2").bind(serde_json::to_string(&("queue-fixture",&project)).unwrap()).bind(&view.id).execute(&admin).await.unwrap();
    assert_eq!(
        queue
            .abandon(
                &view.id,
                view.revision,
                "operator",
                "unavailable old host; scheduling only, no external outcome asserted"
            )
            .await
            .unwrap()
            .state,
        QueueState::Abandoned
    );
    assert!(dead.verify_ownership().await.is_err());
    drop(dead);
    admin.close().await;
}
#[tokio::test]
async fn sqlite_queue_capacity_controls_and_runtime_fencing() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("queue.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url).await;
    fenced_runtime(&db, &url).await;
    lowered_cap_applies_to_recovery(&db, &url).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_queue_capacity_controls_and_runtime_fencing() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url).await;
    fenced_runtime(&db, &url).await;
    lowered_cap_applies_to_recovery(&db, &url).await;
    db.close().await;
}
#[tokio::test]
async fn sqlite_queue_heartbeat_renews_beyond_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqlDatabase::sqlite(
        dir.path().join("ttl.sqlite"),
        DatabaseOptions {
            lease_duration: Duration::from_secs(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    let queue = db.queue(ns());
    queue
        .configure(
            Some(limits(1, 1)),
            &binding(),
            0,
            "operator",
            "heartbeat probe",
        )
        .await
        .unwrap();
    queue
        .enqueue("renew", "run:renew", json!({}), &binding(), "runner")
        .await
        .unwrap();
    let owner = queue.claim(&binding(), "worker").await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(2400)).await;
    owner.verify_ownership().await.unwrap();
    assert!(queue.claim(&binding(), "other").await.unwrap().is_none());
    owner
        .finish(QueueState::Completed, "heartbeat retained ownership")
        .await
        .unwrap();
    drop(owner);
    db.close().await;
}
