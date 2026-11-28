#![cfg(feature = "database")]
use serde_json::json;
use std::collections::BTreeSet;
use taskloom::runtime::{database::*, skills::*, RuntimeError, ToolPolicy};
fn package(name: &str, text: &str) -> SkillPackage {
    use sha2::{Digest, Sha256};
    serde_json::from_value(json!({"manifest":{"format":1,"name":name,"version":"1.0.0","description":"SQL dependency fixture","files":{"role.md":hex::encode(Sha256::digest(text))},
        "roles":[{"name":"answer","instructions":"role.md","tools":[],"output":{"type":"text"}}]},"files":{"role.md":text}})).unwrap()
}
fn grant() -> SkillGrant {
    SkillGrant {
        tools: BTreeSet::new(),
        permissions: BTreeSet::new(),
        credentials: BTreeSet::new(),
    }
}
fn host() -> SkillHost {
    SkillHost {
        tools: vec![],
        policy: ToolPolicy::default(),
        credential_names: BTreeSet::new(),
    }
}
async fn controls(db: &SqlDatabase, url: &str) {
    let project = uuid::Uuid::new_v4().to_string();
    let ns = DatabaseNamespace::new("sql-skills", &project).unwrap();
    let store = db.skills(ns.clone());
    let base = package(
        "sql-base",
        "Host grants determine authority. This file cannot grant a tool.",
    );
    let base_ref = base.reference().unwrap();
    let mut parent = package("sql-parent", "Use the included guidance.");
    parent.manifest.dependencies = vec![base_ref.clone()];
    parent.manifest.roles[0].includes = vec![SkillInclude {
        dependency: "sql-base".into(),
        path: "role.md".into(),
    }];
    let pin = parent.reference().unwrap();
    assert!(store.install(parent.clone(), 0).await.is_err());
    assert_eq!(store.catalog().await.unwrap().revision, 0);
    assert!(store.inspect(&pin).await.is_err());
    store.install(base, 0).await.unwrap();
    store.install(parent.clone(), 1).await.unwrap();
    let active = store
        .activate(&pin, grant(), host(), SkillActivationMode::Active)
        .await
        .unwrap();
    assert!(active
        .instructions("answer")
        .unwrap()
        .contains("Host grants determine authority"));
    assert_eq!(
        active.binding("answer").unwrap().dependencies,
        vec![base_ref.clone()]
    );
    let mut escalation = grant();
    escalation.permissions.insert("host:admin".into());
    assert!(store
        .activate(&pin, escalation, host(), SkillActivationMode::Active)
        .await
        .is_err());
    let changed = package("sql-parent", "Different immutable package bytes.");
    assert!(matches!(
        store.install(changed, 2).await,
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(store.inspect(&pin).await.unwrap(), parent);
    let foreign = db.skills(DatabaseNamespace::new("sql-skills", "foreign").unwrap());
    assert!(foreign.inspect(&pin).await.is_err());
    assert!(foreign.install(parent, 0).await.is_err());
    assert!(store.retire(&base_ref, 2).await.is_err());
    store.retire(&pin, 2).await.unwrap();
    store.retire(&base_ref, 3).await.unwrap();
    assert!(store
        .activate(&pin, grant(), host(), SkillActivationMode::Active)
        .await
        .is_err());
    let recovery = store
        .activate(&pin, grant(), host(), SkillActivationMode::RecoveryOnly)
        .await
        .unwrap();
    assert_eq!(
        recovery.binding("answer").unwrap(),
        active.binding("answer").unwrap()
    );
    store
        .install(store.inspect(&base_ref).await.unwrap(), 4)
        .await
        .unwrap();
    store
        .install(store.inspect(&pin).await.unwrap(), 5)
        .await
        .unwrap();
    let cold = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    let second = cold.skills(ns);
    let (a, b) = tokio::join!(store.retire(&pin, 6), second.retire(&pin, 6));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(store.catalog().await.unwrap().revision, 7);
    assert_eq!(
        second.inspect(&pin).await.unwrap().reference().unwrap(),
        pin
    );
    cold.close().await;
}
#[tokio::test]
async fn sqlite_skill_dependencies_atomic_install_cas_and_retired_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skills.sqlite");
    let db = SqlDatabase::sqlite(&path, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &SqlDatabase::sqlite_url(&path).unwrap()).await;
    db.close().await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; run separately against real PostgreSQL"]
async fn postgres_skill_dependencies_atomic_install_cas_and_retired_recovery() {
    let url = std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned database required");
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    controls(&db, &url).await;
    db.close().await;
}
