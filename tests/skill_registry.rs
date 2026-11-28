#![cfg(feature = "skill-registry")]
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use taskloom::runtime::{skills::*, ToolPolicy};

// Public deterministic test seeds, never production publisher credentials.
fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}
fn trust(revision: u64) -> SkillRegistryTrust {
    SkillRegistryTrust {
        format: 1,
        revision,
        publishers: BTreeMap::from([(
            "publisher".into(),
            RegistryPublisher {
                keys: BTreeMap::from([(
                    "key-one".into(),
                    hex::encode(key(1).verifying_key().to_bytes()),
                )]),
                revoked_keys: BTreeSet::new(),
                allowed_packages: BTreeSet::from(["fixture-skill".into(), "dependency".into()]),
            },
        )]),
        revoked_digests: BTreeSet::new(),
    }
}
fn package(version: &str) -> SkillPackage {
    serde_json::from_value(json!({"manifest":{"format":1,"name":"fixture-skill","version":version,"description":"inert signed test package","files":{}},"files":{}})).unwrap()
}
fn archive_named(package: &SkillPackage, filename: &str) -> Vec<u8> {
    let body = serde_json::to_vec(&package.manifest).unwrap();
    let mut header = [0u8; 512];
    header[..filename.len()].copy_from_slice(filename.as_bytes());
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum = header.iter().map(|v| *v as u64).sum::<u64>();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut archive = header.to_vec();
    archive.extend_from_slice(&body);
    archive.resize(512 + body.len().div_ceil(512) * 512 + 1024, 0);
    archive
}
fn release(
    package: &SkillPackage,
    archive: &[u8],
    sequence: u64,
    trust_revision: u64,
    key_id: &str,
    seed: u8,
) -> SignedSkillRelease {
    let manifest = SkillRegistryRelease {
        format: 1,
        publisher: "publisher".into(),
        key_id: key_id.into(),
        trust_revision,
        sequence,
        expires_unix: 2000,
        package: package.reference().unwrap(),
        archive_sha256: hex::encode(Sha256::digest(archive)),
        archive_bytes: archive.len() as u64,
    };
    SignedSkillRelease {
        signature: hex::encode(
            key(seed)
                .sign(&manifest.signing_bytes().unwrap())
                .to_bytes(),
        ),
        manifest,
    }
}
fn write_release(directory: &Path, release: &SignedSkillRelease, archive: &[u8]) {
    std::fs::write(
        directory.join("release.json"),
        serde_json::to_vec(release).unwrap(),
    )
    .unwrap();
    std::fs::write(directory.join("package.tar"), archive).unwrap();
}
fn request(
    directory: &Path,
    registry_revision: u64,
    catalog_revision: u64,
) -> RegistryInstallRequest {
    RegistryInstallRequest {
        directory: directory.into(),
        metadata_file: "release.json".into(),
        archive_file: "package.tar".into(),
        expected_registry_revision: registry_revision,
        expected_catalog_revision: catalog_revision,
        now_unix: 1000,
    }
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
        tools: Vec::new(),
        policy: ToolPolicy::default(),
        credential_names: BTreeSet::new(),
    }
}

#[cfg(all(feature = "database", feature = "app"))]
#[tokio::test]
async fn sql_app_enforces_signed_root_and_dependencies_but_retains_recovery_after_revocation() {
    use taskloom::runtime::{app::*, database::*};
    let directory = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(directory.path().join("skills.sqlite")).unwrap();
    let db = SqlDatabase::connect(&url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let ns = DatabaseNamespace::new("tenant", "project").unwrap();
    let store = db.skills(ns);
    let registry_root = directory.path().join("registry");
    let registry = FileSkillRegistry::new(&registry_root).unwrap();
    registry.set_trust(trust(1), 0).await.unwrap();
    let dependency:SkillPackage=serde_json::from_value(json!({"manifest":{"format":1,"name":"dependency","version":"1.0.0","description":"dependency fixture","files":{}},"files":{}})).unwrap();
    store.install(dependency.clone(), 0).await.unwrap();
    let mut root = package("1.0.0");
    root.manifest.dependencies = vec![dependency.reference().unwrap()];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let make_release = |package: &SkillPackage, bytes: &[u8]| {
        let mut signed = release(package, bytes, 1, 1, "key-one", 1);
        signed.manifest.expires_unix = now + 3600;
        signed.signature = hex::encode(
            key(1)
                .sign(&signed.manifest.signing_bytes().unwrap())
                .to_bytes(),
        );
        signed
    };
    let root_bytes = archive_named(&root, "skill.json");
    let signed = make_release(&root, &root_bytes);
    write_release(directory.path(), &signed, &root_bytes);
    let mut install = request(directory.path(), 1, 1);
    install.now_unix = now;
    registry
        .install_from_directory(&store, install)
        .await
        .unwrap();
    let env = format!("TASKLOOM_SIGNED_SKILLS_{}", uuid::Uuid::new_v4().simple());
    std::env::set_var(&env, &url);
    let config = AppConfig {
        state_dir: directory.path().join("app"),
        knowledge: false,
        skill_registry: Some(registry_root.clone()),
        database: Some(AppDatabase {
            url_env: env.clone(),
            tenant: "tenant".into(),
            project: "project".into(),
            max_connections: 4,
            max_record_bytes: 64 * 1024 * 1024,
            lease_ms: 10_000,
        }),
        skills: vec![AppSkill {
            package: root.reference().unwrap(),
            mount: "signed".into(),
            grant: grant(),
            mode: SkillActivationMode::Active,
        }],
        ..Default::default()
    };
    // Root is signed, but a raw SQL dependency installation has no verified
    // receipt: both the SDK and application startup must reject activation.
    assert!(registry
        .activate(
            &store,
            &root.reference().unwrap(),
            grant(),
            host(),
            SkillActivationMode::Active,
            now
        )
        .await
        .is_err());
    assert!(Application::open(config.clone()).await.is_err());
    let bytes = archive_named(&dependency, "skill.json");
    let signed_dependency = make_release(&dependency, &bytes);
    write_release(directory.path(), &signed_dependency, &bytes);
    let config_path = directory.path().join("app.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let installed = std::process::Command::new(env!("CARGO_BIN_EXE_taskloom"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "skill-registry-install",
            directory.path().to_str().unwrap(),
            "release.json",
            "package.tar",
            "3",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let app = Application::open(config.clone()).await.unwrap();
    app.shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    let config_path = directory.path().join("app.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let denied = std::process::Command::new(env!("CARGO_BIN_EXE_taskloom"))
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "skill-install",
            directory.path().join("package.tar").to_str().unwrap(),
            "2",
        ])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("unsigned installation is disabled"));
    let catalog = std::process::Command::new(env!("CARGO_BIN_EXE_taskloom"))
        .args(["--config", config_path.to_str().unwrap(), "skill-registry"])
        .output()
        .unwrap();
    assert!(
        catalog.status.success(),
        "{}",
        String::from_utf8_lossy(&catalog.stderr)
    );
    let mut revoke = trust(2);
    revoke
        .publishers
        .get_mut("publisher")
        .unwrap()
        .revoked_keys
        .insert("key-one".into());
    registry.set_trust(revoke, 5).await.unwrap();
    assert!(Application::open(config.clone()).await.is_err());
    let mut recovery = config;
    recovery.skills[0].mode = SkillActivationMode::RecoveryOnly;
    let app = Application::open(recovery).await.unwrap();
    assert!(app.skill_templates()["templates"]
        .as_array()
        .unwrap()
        .is_empty());
    app.shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(store.catalog().await.unwrap().entries.len(), 2);
    std::env::remove_var(env);
    db.close().await;
}

#[cfg(feature = "database")]
#[tokio::test]
async fn sql_commit_with_lost_ack_keeps_pending_watermark_and_requires_exact_explicit_retry() {
    use futures_util::future::BoxFuture;
    use std::sync::atomic::{AtomicBool, Ordering};
    use taskloom::runtime::{database::*, RuntimeError};
    struct LostAck {
        store: SqlSkillStore,
        once: AtomicBool,
    }
    impl RegistrySkillStore for LostAck {
        fn registry_catalog(&self) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
            Box::pin(self.store.catalog())
        }
        fn registry_install(
            &self,
            package: SkillPackage,
            expected: u64,
        ) -> BoxFuture<'_, Result<SkillCatalog, RuntimeError>> {
            Box::pin(async move {
                let actual = self.store.install(package, expected).await?;
                if self.once.swap(false, Ordering::SeqCst) {
                    Err(RuntimeError::Storage(
                        "injected lost SQL acknowledgement after actual commit".into(),
                    ))
                } else {
                    Ok(actual)
                }
            })
        }
        fn registry_inspect<'a>(
            &'a self,
            reference: &'a SkillRef,
        ) -> BoxFuture<'a, Result<SkillPackage, RuntimeError>> {
            Box::pin(self.store.inspect(reference))
        }
        fn registry_activate<'a>(
            &'a self,
            reference: &'a SkillRef,
            grant: SkillGrant,
            host: SkillHost,
            mode: SkillActivationMode,
        ) -> BoxFuture<'a, Result<ActivatedSkill, RuntimeError>> {
            Box::pin(self.store.activate(reference, grant, host, mode))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let db = SqlDatabase::sqlite(
        directory.path().join("lost-ack.sqlite"),
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    let store = LostAck {
        store: db.skills(DatabaseNamespace::new("tenant", "lost-ack").unwrap()),
        once: AtomicBool::new(true),
    };
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    registry.set_trust(trust(1), 0).await.unwrap();
    let package = package("1.0.0");
    let bytes = archive_named(&package, "skill.json");
    let signed = release(&package, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .is_err());
    assert_eq!(store.store.catalog().await.unwrap().revision, 1);
    let pending = registry.snapshot().await.unwrap();
    assert_eq!(pending.revision, 2);
    assert_eq!(pending.watermarks.len(), 1);
    assert!(!pending.releases[&signed.manifest.package.digest].installed);
    assert!(registry
        .activate(
            &store,
            &signed.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000
        )
        .await
        .is_err());
    let complete = registry
        .install_from_directory(&store, request(directory.path(), 2, 1))
        .await
        .unwrap();
    assert!(complete.releases[&signed.manifest.package.digest].installed);
    assert_eq!(store.store.catalog().await.unwrap().revision, 1);
    registry
        .activate(
            &store,
            &signed.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000,
        )
        .await
        .unwrap();
    db.close().await;
}

#[tokio::test]
async fn signed_local_install_is_inert_idempotent_and_uses_original_host_authority() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let package = package("1.0.0");
    let bytes = archive_named(&package, "skill.json");
    let signed = release(&package, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    let installed = registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .unwrap();
    assert_eq!(installed.revision, 3);
    assert!(installed.releases[&signed.manifest.package.digest].installed);
    assert_eq!(store.catalog().await.unwrap().revision, 1);
    let again = registry
        .install_from_directory(&store, request(directory.path(), 3, 1))
        .await
        .unwrap();
    assert_eq!(store.catalog().await.unwrap().revision, 1);
    assert_eq!(again.watermarks.len(), 1);
    registry
        .activate(
            &store,
            &signed.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000,
        )
        .await
        .unwrap();
    let mut forbidden = grant();
    forbidden.tools.insert("write".into());
    assert!(registry
        .activate(
            &store,
            &signed.manifest.package,
            forbidden,
            host(),
            SkillActivationMode::Active,
            1000
        )
        .await
        .is_err());
    assert_eq!(
        store.inspect(&signed.manifest.package).await.unwrap(),
        package
    );
}
#[tokio::test]
async fn tampering_expiry_wrong_key_identity_and_archive_fail_before_any_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let package = package("1.0.0");
    let bytes = archive_named(&package, "skill.json");
    let original = release(&package, &bytes, 1, 1, "key-one", 1);
    for kind in 0..7 {
        let mut signed = original.clone();
        let mut archive = bytes.clone();
        match kind {
            0 => signed.manifest.sequence += 1,
            1 => signed.signature.replace_range(0..2, "00"),
            2 => signed = release(&package, &bytes, 1, 1, "unknown-key", 1),
            3 => signed = release(&package, &bytes, 1, 1, "key-one", 2),
            4 => {
                signed.manifest.expires_unix = 1000;
                signed.signature = hex::encode(
                    key(1)
                        .sign(&signed.manifest.signing_bytes().unwrap())
                        .to_bytes(),
                );
            }
            5 => archive[600] ^= 1,
            _ => {
                signed.manifest.package.digest = "0".repeat(64);
                signed.signature = hex::encode(
                    key(1)
                        .sign(&signed.manifest.signing_bytes().unwrap())
                        .to_bytes(),
                );
            }
        }
        write_release(directory.path(), &signed, &archive);
        assert!(
            registry
                .install_from_directory(&store, request(directory.path(), 1, 0))
                .await
                .is_err(),
            "kind{kind}"
        );
        assert_eq!(registry.snapshot().await.unwrap().revision, 1);
        assert_eq!(store.catalog().await.unwrap().revision, 0);
    }
    let bad = archive_named(&package, "../skill.json");
    let signed = release(&package, &bad, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bad);
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .is_err());
}
#[tokio::test]
async fn key_rotation_revocation_and_expiry_stop_new_activation_but_preserve_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let first = package("1.0.0");
    let bytes = archive_named(&first, "skill.json");
    let original = release(&first, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &original, &bytes);
    registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .unwrap();
    assert!(registry
        .activate(
            &store,
            &original.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            2000
        )
        .await
        .is_err());
    let mut rotated = trust(2);
    rotated
        .publishers
        .get_mut("publisher")
        .unwrap()
        .keys
        .insert(
            "key-two".into(),
            hex::encode(key(2).verifying_key().to_bytes()),
        );
    rotated
        .publishers
        .get_mut("publisher")
        .unwrap()
        .revoked_keys
        .insert("key-one".into());
    registry.set_trust(rotated.clone(), 3).await.unwrap();
    assert!(registry
        .activate(
            &store,
            &original.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000
        )
        .await
        .is_err());
    let recovery = registry
        .activate(
            &store,
            &original.manifest.package,
            grant(),
            host(),
            SkillActivationMode::RecoveryOnly,
            3000,
        )
        .await
        .unwrap();
    assert_eq!(recovery.mode, SkillActivationMode::RecoveryOnly);
    assert_eq!(
        store.inspect(&original.manifest.package).await.unwrap(),
        first
    );
    let next = package("1.1.0");
    let bytes = archive_named(&next, "skill.json");
    let signed = release(&next, &bytes, 2, 2, "key-two", 2);
    write_release(directory.path(), &signed, &bytes);
    registry
        .install_from_directory(&store, request(directory.path(), 4, 1))
        .await
        .unwrap();
    registry
        .activate(
            &store,
            &signed.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000,
        )
        .await
        .unwrap();
    rotated.revision = 3;
    rotated
        .revoked_digests
        .insert(signed.manifest.package.digest.clone());
    registry.set_trust(rotated, 6).await.unwrap();
    assert!(registry
        .activate(
            &store,
            &signed.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000
        )
        .await
        .is_err());
}
#[tokio::test]
async fn release_sequence_semantic_version_and_trust_revision_rollbacks_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let next = package("2.0.0");
    let bytes = archive_named(&next, "skill.json");
    let signed = release(&next, &bytes, 2, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .unwrap();
    for (version, sequence) in [("1.0.0", 3), ("2.1.0", 1), ("2.1.0", 2)] {
        let package = package(version);
        let bytes = archive_named(&package, "skill.json");
        let signed = release(&package, &bytes, sequence, 1, "key-one", 1);
        write_release(directory.path(), &signed, &bytes);
        assert!(registry
            .install_from_directory(&store, request(directory.path(), 3, 1))
            .await
            .is_err());
        assert_eq!(registry.snapshot().await.unwrap().revision, 3);
    }
    assert!(registry.set_trust(trust(1), 3).await.is_err());
    assert!(registry.set_trust(trust(3), 3).await.is_err());
    assert!(registry.set_trust(trust(2), 2).await.is_err());
}
#[tokio::test]
async fn failed_dependency_install_retains_watermark_and_only_exact_retry_can_finish() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let mut dependency = package("1.0.0");
    dependency.manifest.name = "dependency".into();
    let mut parent = package("2.0.0");
    parent
        .manifest
        .dependencies
        .push(dependency.reference().unwrap());
    let parent_archive = archive_named(&parent, "skill.json");
    let parent_release = release(&parent, &parent_archive, 2, 1, "key-one", 1);
    write_release(directory.path(), &parent_release, &parent_archive);
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .is_err());
    let pending = registry.snapshot().await.unwrap();
    assert_eq!(pending.revision, 2);
    assert!(!pending.releases[&parent_release.manifest.package.digest].installed);
    let old = package("1.0.0");
    let bytes = archive_named(&old, "skill.json");
    let signed = release(&old, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 2, 0))
        .await
        .is_err());
    let bytes = archive_named(&dependency, "skill.json");
    let signed = release(&dependency, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    registry
        .install_from_directory(&store, request(directory.path(), 2, 0))
        .await
        .unwrap();
    write_release(directory.path(), &parent_release, &parent_archive);
    let completed = registry
        .install_from_directory(&store, request(directory.path(), 4, 1))
        .await
        .unwrap();
    assert_eq!(completed.revision, 6);
    registry
        .activate(
            &store,
            &parent_release.manifest.package,
            grant(),
            host(),
            SkillActivationMode::Active,
            1000,
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn local_paths_bounds_and_catalog_cas_are_checked_before_registry_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let package = package("1.0.0");
    let bytes = archive_named(&package, "skill.json");
    let signed = release(&package, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    let mut bad = request(directory.path(), 1, 0);
    bad.metadata_file = "../release.json".into();
    assert!(registry.install_from_directory(&store, bad).await.is_err());
    let mut bad = request(directory.path(), 1, 0);
    bad.archive_file = "file.tar:stream".into();
    assert!(registry.install_from_directory(&store, bad).await.is_err());
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 1))
        .await
        .is_err());
    std::fs::write(
        directory.path().join("release.json"),
        vec![b' '; 128 * 1024 + 1],
    )
    .unwrap();
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .is_err());
    assert_eq!(registry.snapshot().await.unwrap().revision, 1);
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_metadata_archive_and_state_paths_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let registry = FileSkillRegistry::new(directory.path().join("registry")).unwrap();
    let store = FileSkillStore::new(directory.path().join("skills"));
    registry.set_trust(trust(1), 0).await.unwrap();
    let package = package("1.0.0");
    let bytes = archive_named(&package, "skill.json");
    let signed = release(&package, &bytes, 1, 1, "key-one", 1);
    write_release(directory.path(), &signed, &bytes);
    std::fs::rename(
        directory.path().join("release.json"),
        directory.path().join("actual.json"),
    )
    .unwrap();
    std::os::unix::fs::symlink("actual.json", directory.path().join("release.json")).unwrap();
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .is_err());
    std::fs::remove_file(directory.path().join("release.json")).unwrap();
    std::fs::rename(
        directory.path().join("actual.json"),
        directory.path().join("release.json"),
    )
    .unwrap();
    std::fs::rename(
        directory.path().join("package.tar"),
        directory.path().join("actual.tar"),
    )
    .unwrap();
    std::os::unix::fs::symlink("actual.tar", directory.path().join("package.tar")).unwrap();
    assert!(registry
        .install_from_directory(&store, request(directory.path(), 1, 0))
        .await
        .is_err());
    let state = directory.path().join("registry/registry.json");
    std::fs::rename(&state, directory.path().join("old-state.json")).unwrap();
    std::os::unix::fs::symlink(directory.path().join("old-state.json"), state).unwrap();
    assert!(registry.snapshot().await.is_err());
}
