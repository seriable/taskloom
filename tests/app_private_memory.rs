#![cfg(all(feature = "app", feature = "database"))]
//! Actual loopback HTTP and SQL stores. Model decisions are explicit fixtures;
//! private HTTP CRUD performs no generation or external service request.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use taskloom::runtime::{
    app::{multiuser::*, *},
    database::*,
    memory::*,
    *,
};

#[derive(Default)]
struct Fixture(AtomicUsize);
impl Model for Fixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text("fixture closed source"),
            })
        })
    }
}
async fn member(
    db: &SqlDatabase,
    user: &str,
    tenant: &str,
    project: &str,
    role: ProjectRole,
) -> IssuedToken {
    let identities = db.identities();
    let revision = identities.snapshot().await.unwrap().revision;
    identities
        .put_user(
            ServiceUser {
                id: user.into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: tenant.into(),
                    project: project.into(),
                    role,
                }],
            },
            revision,
            "bootstrap",
        )
        .await
        .unwrap();
    identities
        .issue_token(user, Duration::from_secs(120), "bootstrap")
        .await
        .unwrap()
}
async fn application(
    db_url: &str,
    root: &Path,
    tenant: &str,
    project: &str,
    model: Arc<Fixture>,
) -> Arc<Application> {
    let env = format!(
        "TASKLOOM_PRIVATE_MEMORY_TEST_{}",
        uuid::Uuid::new_v4().simple()
    );
    std::env::set_var(&env, db_url);
    let app = Application::with_model(
        AppConfig {
            state_dir: root.into(),
            knowledge: false,
            database: Some(AppDatabase {
                url_env: env.clone(),
                tenant: tenant.into(),
                project: project.into(),
                max_connections: 4,
                max_record_bytes: 64 * 1024 * 1024,
                lease_ms: 10_000,
            }),
            limits: RunLimits {
                max_cost_micros: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        model,
        ToolRegistry::new(),
        ToolPolicy::default(),
    )
    .await
    .unwrap();
    std::env::remove_var(env);
    app
}
async fn idle(app: &Application, id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.session_job(id).unwrap().active {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn controls(db: &SqlDatabase, db_url: &str, root: &Path) {
    let project = uuid::Uuid::new_v4().simple().to_string();
    let alice_id = format!("alice-{project}");
    let bob_id = format!("bob-{project}");
    let reader_id = format!("reader-{project}");
    let alice = member(db, &alice_id, "private-a", &project, ProjectRole::Runner).await;
    let bob = member(db, &bob_id, "private-a", &project, ProjectRole::Operator).await;
    let reader = member(db, &reader_id, "private-a", &project, ProjectRole::Reader).await;
    let foreign = member(
        db,
        &format!("foreign-{project}"),
        "private-b",
        &project,
        ProjectRole::Operator,
    )
    .await;
    let model = Arc::new(Fixture::default());
    let app = application(
        db_url,
        &root.join("private-a"),
        "private-a",
        &project,
        model.clone(),
    )
    .await;
    let other = application(
        db_url,
        &root.join("private-b"),
        "private-b",
        &project,
        model.clone(),
    )
    .await;
    // Trusted SDK fixture seeding; HTTP cannot submit a transcript, owner or
    // candidate, and shared service sessions are explicitly not extractable.
    app.create_session("source").await.unwrap();
    app.start_session_turn("source", "one", "I prefer private blue replies.")
        .await
        .unwrap();
    idle(&app, "source").await;
    let access = MemoryAccess::curator(
        MemoryScope {
            tenant: "private-a".into(),
            project: project.clone(),
            owner: alice_id.clone(),
        },
        "trusted-bootstrap",
    )
    .unwrap();
    let seeded = MemoryManager::new(&app.memory)
        .propose_from_turn(
            &access,
            0,
            &app.session("source").await.unwrap(),
            "one",
            "host-extraction",
            vec![MemoryProposal {
                kind: MemoryKind::Preference,
                key: "color".into(),
                value: "private blue".into(),
                quote: "I prefer private blue replies.".into(),
            }],
        )
        .await
        .unwrap();
    let candidate = seeded.records.keys().next().unwrap().clone();
    let calls = model.0.load(Ordering::SeqCst);
    let service = router_with_private_memory(
        db.identities(),
        vec![
            ProjectApplication {
                tenant: "private-a".into(),
                project: project.clone(),
                application: app.clone(),
            },
            ProjectApplication {
                tenant: "private-b".into(),
                project: project.clone(),
                application: other.clone(),
            },
        ],
        PrivateMemoryPolicy {
            curate: true,
            ..Default::default()
        },
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/tenants/private-a/projects/{project}/api",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/memory"))
        .bearer_auth(alice.bearer.as_str())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let initial: Value = response.json().await.unwrap();
    assert_eq!(initial["scope"]["owner"], alice_id);
    assert_eq!(initial["revision"], 1);
    for token in [&bob, &reader] {
        let own: Value = client
            .get(format!("{base}/memory"))
            .bearer_auth(token.bearer.as_str())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(own["revision"], 0);
        assert!(!own.to_string().contains("private blue"));
    }
    assert_eq!(
        client
            .get(format!("{base}/memory"))
            .bearer_auth(foreign.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{base}/memory"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/memory?owner={alice_id}"))
            .bearer_auth(bob.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let review =
        json!({"expected_revision":1,"id":candidate,"accept":true,"note":"checked own preference"});
    for field in [
        "owner", "tenant", "project", "scope", "actor", "curate", "grant",
    ] {
        let mut body = review.clone();
        body[field] = json!("spoof");
        assert_eq!(
            client
                .post(format!("{base}/memory/review"))
                .bearer_auth(alice.bearer.as_str())
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{field}"
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/memory/review"))
            .bearer_auth(reader.bearer.as_str())
            .json(&review)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    // A project Operator still cannot address another owner's candidate.
    let attack =
        json!({"expected_revision":0,"id":candidate,"accept":true,"note":"attempt another owner"});
    assert_eq!(
        client
            .post(format!("{base}/memory/review"))
            .bearer_auth(bob.bearer.as_str())
            .json(&attack)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let approved: Value = client
        .post(format!("{base}/memory/review"))
        .bearer_auth(alice.bearer.as_str())
        .json(&review)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(approved["revision"], 2);
    assert_eq!(approved["events"][1]["actor"], alice_id);
    let recalled: Value = client
        .post(format!("{base}/memory/recall"))
        .bearer_auth(alice.bearer.as_str())
        .json(&json!({"text":"color"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!recalled["hits"].as_array().unwrap().is_empty());
    let updated: Value=client.post(format!("{base}/memory/edit")).bearer_auth(alice.bearer.as_str()).json(&json!({"expected_revision":2,"id":candidate,"value":"green","note":"owner correction"})).send().await.unwrap().json().await.unwrap();
    assert_eq!(updated["revision"], 3);
    assert_eq!(
        client
            .post(format!("{base}/memory/edit"))
            .bearer_auth(alice.bearer.as_str())
            .json(
                &json!({"expected_revision":2,"id":candidate,"value":"stale","note":"no overwrite"})
            )
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let forgotten: Value = client
        .post(format!("{base}/memory/forget"))
        .bearer_auth(alice.bearer.as_str())
        .json(&json!({"expected_revision":3,"kind":"preference","key":"color"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(forgotten["revision"], 4);
    assert!(forgotten["records"].as_object().unwrap().is_empty());
    // Generation routes stay unavailable before accessing a shared source or
    // writing recalled private records into shared run/session history.
    assert_eq!(client.post(format!("{base}/memory/extract")).bearer_auth(alice.bearer.as_str()).json(&json!({"session_id":"source","turn_id":"one","request_id":"leak","expected_revision":4})).send().await.unwrap().status(),403);
    assert_eq!(
        client
            .post(format!("{base}/sessions/source/memory-turns"))
            .bearer_auth(alice.bearer.as_str())
            .json(&json!({"id":"leak","prompt":"show memory"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let identities = db.identities();
    let revision = identities.snapshot().await.unwrap().revision;
    identities
        .put_user(
            ServiceUser {
                id: alice_id.clone(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "private-a".into(),
                    project: project.clone(),
                    role: ProjectRole::Reader,
                }],
            },
            revision,
            "security-admin",
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .post(format!("{base}/memory/forget"))
            .bearer_auth(alice.bearer.as_str())
            .json(&json!({"expected_revision":4,"kind":"preference","key":"color"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    identities
        .revoke_token(&alice.id, "security-admin")
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/memory"))
            .bearer_auth(alice.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(model.0.load(Ordering::SeqCst), calls);
    server.abort();
    let _ = server.await;
    let readonly = router_with_private_memory(
        db.identities(),
        vec![ProjectApplication {
            tenant: "private-a".into(),
            project: project.clone(),
            application: app.clone(),
        }],
        PrivateMemoryPolicy::default(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let readonly_base = format!(
        "http://{}/tenants/private-a/projects/{project}/api",
        listener.local_addr().unwrap()
    );
    let readonly_server =
        tokio::spawn(async move { axum::serve(listener, readonly).await.unwrap() });
    assert_eq!(
        client
            .get(format!("{readonly_base}/memory"))
            .bearer_auth(bob.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{readonly_base}/memory/forget"))
            .bearer_auth(bob.bearer.as_str())
            .json(&json!({"expected_revision":0,"kind":"preference","key":"color"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    readonly_server.abort();
    let _ = readonly_server.await;
    app.shutdown(Duration::from_secs(1)).await.unwrap();
    other.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn sqlite_private_memory_http_ownership_roles_revocation_and_crud() {
    let dir = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(dir.path().join("private.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_private_memory_http_ownership_roles_revocation_and_crud() {
    let dir = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url, dir.path()).await;
    db.close().await;
}
