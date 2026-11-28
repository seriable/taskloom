#![cfg(all(feature = "app", feature = "database"))]
use std::time::Duration;
use taskloom::runtime::{app::*, database::*};
#[tokio::test]
async fn durable_operations_counts_are_scoped_exact_and_do_not_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let db = SqlDatabase::sqlite(
        directory.path().join("operations.db"),
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    let variable = format!("TASKLOOM_OPERATIONS_TEST_{}", std::process::id());
    std::env::set_var(
        &variable,
        SqlDatabase::sqlite_url(directory.path().join("operations.db")).unwrap(),
    );
    let mut config = AppConfig {
        state_dir: directory.path().join("state"),
        database: Some(AppDatabase {
            url_env: variable.clone(),
            tenant: "tenant".into(),
            project: "project".into(),
            max_connections: 4,
            max_record_bytes: 64 * 1024 * 1024,
            lease_ms: 1000,
        }),
        ..Default::default()
    };
    config.limits.max_cost_micros = Some(0);
    db.generation_quota(DatabaseNamespace::new("tenant", "project").unwrap())
        .configure(
            Some(GenerationQuota {
                currency: "USD".into(),
                max_cost_micros: 0,
                max_outstanding_attempts: 2,
            }),
            0,
            "PRIVATE_QUOTA_OPERATOR",
            "PRIVATE_QUOTA_NOTE",
        )
        .await
        .unwrap();
    let app = Application::open(config).await.unwrap();
    app.configure_queue(
        Some(QueueLimits {
            max_open_jobs: 4,
            max_running: 2,
        }),
        0,
        "host",
        "reviewed limits",
    )
    .await
    .unwrap();
    app.enqueue_task(
        queue::QueuedTask::Run {
            id: "one".into(),
            prompt: "PRIVATE_PROMPT_MUST_NOT_APPEAR".into(),
        },
        "alice",
    )
    .await
    .unwrap();
    let result = operations::snapshot(&app).await.unwrap();
    assert_eq!(result.open, 1);
    assert_eq!(result.jobs, 1);
    assert_eq!(result.states["pending"], 1);
    assert_eq!(result.attempts, 0);
    assert!(!result.model_or_service_dispatched);
    assert!(!result.consistent_cross_record_snapshot);
    let json = serde_json::to_string(&result).unwrap();
    let metrics = operations::prometheus(&result);
    assert!(serde_json::to_value(&result).unwrap()["generation"]
        .get("events")
        .is_none());
    for secret in [
        "PRIVATE_PROMPT_MUST_NOT_APPEAR",
        "alice",
        "operations.db",
        "PRIVATE_QUOTA_OPERATOR",
        "PRIVATE_QUOTA_NOTE",
    ] {
        assert!(!json.contains(secret));
        assert!(!metrics.contains(secret));
    }
    assert_eq!(
        db.generation_quota(DatabaseNamespace::new("tenant", "project").unwrap())
            .snapshot()
            .await
            .unwrap()
            .events[0]
            .note,
        "PRIVATE_QUOTA_NOTE"
    );
    assert!(metrics.contains("taskloom_queue_jobs{state=\"pending\"} 1"));
    assert!(metrics.contains("taskloom_queue_max_running 2"));
    assert!(db
        .queue(DatabaseNamespace::new("tenant", "different").unwrap())
        .list()
        .await
        .unwrap()
        .is_empty());
    app.shutdown(Duration::from_secs(2)).await.unwrap();
    db.close().await;
    std::env::remove_var(variable);
}
