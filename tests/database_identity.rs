#![cfg(feature = "database")]
use std::time::Duration;
use taskloom::runtime::{database::*, RuntimeError};

fn user(id: &str, project: &str, role: ProjectRole) -> ServiceUser {
    ServiceUser {
        id: id.into(),
        enabled: true,
        memberships: vec![ProjectMembership {
            tenant: "identity-tenant".into(),
            project: project.into(),
            role,
        }],
    }
}
async fn controls(db: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let id = format!("user-{project}");
    let store = db.identities();
    let ns = DatabaseNamespace::new("identity-tenant", &project).unwrap();
    let rev = store.snapshot().await.unwrap().revision;
    let account = user(&id, &project, ProjectRole::Reader);
    store
        .put_user(account.clone(), rev, "bootstrap")
        .await
        .unwrap();
    let issued = store
        .issue_token(&id, Duration::from_secs(60), "bootstrap")
        .await
        .unwrap();
    let actor = store.authenticate(issued.bearer.as_str()).await.unwrap();
    assert_eq!(actor.user_id(), id);
    assert_eq!(
        store
            .authorize(&actor, &ns, ProjectPermission::Read)
            .await
            .unwrap(),
        ProjectRole::Reader
    );
    for permission in [
        ProjectPermission::Submit,
        ProjectPermission::Approve,
        ProjectPermission::Control,
        ProjectPermission::ManageKnowledge,
        ProjectPermission::ExportTelemetry,
    ] {
        assert!(matches!(
            store.authorize(&actor, &ns, permission).await,
            Err(RuntimeError::Permission(_))
        ));
    }
    assert!(store
        .authorize(
            &actor,
            &DatabaseNamespace::new("foreign", "project").unwrap(),
            ProjectPermission::Read
        )
        .await
        .is_err());
    let cold = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    let other = cold.identities();
    assert_eq!(
        other
            .authenticate(issued.bearer.as_str())
            .await
            .unwrap()
            .user_id(),
        id
    );
    assert!(!serde_json::to_string(&store.snapshot().await.unwrap())
        .unwrap()
        .contains(issued.bearer.as_str()));
    for bad in [
        "",
        "tlm_short",
        "Bearer tlm_x",
        &format!("tlm_{}", "0".repeat(64)),
    ] {
        let error = store.authenticate(bad).await.err().unwrap().to_string();
        assert_eq!(
            error,
            "identity authentication or project permission refused"
        );
    }
    for role in [
        ProjectRole::Runner,
        ProjectRole::Approver,
        ProjectRole::Operator,
    ] {
        let revision = store.snapshot().await.unwrap().revision;
        other
            .put_user(user(&id, &project, role), revision, "bootstrap")
            .await
            .unwrap();
        assert_eq!(
            store
                .authorize(&actor, &ns, ProjectPermission::Read)
                .await
                .unwrap(),
            role
        );
        assert_eq!(
            store
                .authorize(&actor, &ns, ProjectPermission::Submit)
                .await
                .is_ok(),
            matches!(role, ProjectRole::Runner | ProjectRole::Operator)
        );
        assert_eq!(
            store
                .authorize(&actor, &ns, ProjectPermission::Approve)
                .await
                .is_ok(),
            matches!(role, ProjectRole::Approver | ProjectRole::Operator)
        );
        assert_eq!(
            store
                .authorize(&actor, &ns, ProjectPermission::Control)
                .await
                .is_ok(),
            role == ProjectRole::Operator
        );
    }
    let another = store
        .issue_token(&id, Duration::from_secs(60), "bootstrap")
        .await
        .unwrap();
    assert_ne!(issued.bearer.as_str(), another.bearer.as_str());
    other
        .revoke_token(&issued.id, "security-admin")
        .await
        .unwrap();
    assert!(
        store
            .authorize(&actor, &ns, ProjectPermission::Read)
            .await
            .is_err(),
        "already constructed actor must be rechecked"
    );
    assert!(other.authenticate(issued.bearer.as_str()).await.is_err());
    assert!(other.authenticate(another.bearer.as_str()).await.is_ok());
    let revision = store.snapshot().await.unwrap().revision;
    let mut disabled = account.clone();
    disabled.enabled = false;
    other
        .put_user(disabled, revision, "bootstrap")
        .await
        .unwrap();
    assert!(store.authenticate(another.bearer.as_str()).await.is_err());
    store
        .put_user(account.clone(), revision + 1, "bootstrap")
        .await
        .unwrap();
    assert!(
        other.authenticate(another.bearer.as_str()).await.is_err(),
        "re-enabling does not resurrect revoked credentials"
    );
    let revision = store.snapshot().await.unwrap().revision;
    let (a, b) = tokio::join!(
        store.put_user(account.clone(), revision, "admin-a"),
        other.put_user(account.clone(), revision, "admin-b")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let revision = store.snapshot().await.unwrap().revision;
    let mut duplicate = account.clone();
    duplicate.memberships.push(duplicate.memberships[0].clone());
    assert!(store
        .put_user(duplicate, revision, "bootstrap")
        .await
        .is_err());
    assert_eq!(store.snapshot().await.unwrap().revision, revision);
    let short = other
        .issue_token(&id, Duration::from_secs(1), "bootstrap")
        .await
        .unwrap();
    let short_actor = store.authenticate(short.bearer.as_str()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(store.authenticate(short.bearer.as_str()).await.is_err());
    assert!(other
        .authorize(&short_actor, &ns, ProjectPermission::Read)
        .await
        .is_err());
    cold.close().await;
}
#[tokio::test]
async fn sqlite_identity_roles_revocation_expiry_and_cas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identities.sqlite");
    let db = SqlDatabase::sqlite(&path, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &SqlDatabase::sqlite_url(&path).unwrap()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_identity_roles_revocation_expiry_and_cas() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url).await;
    db.close().await;
}
