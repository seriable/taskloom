#![cfg(all(feature = "app", feature = "oidc"))]
//! Synthetic public test keys and signed JWT fixtures. No real IdP, discovery,
//! browser login, network key download or external cloud request occurs.
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use taskloom::runtime::{app::oidc::*, database::*};

// Generated solely for these tests, public source fixtures; never real keys.
const TEST_DER:&str="302e020100300506032b657004220420999831564440061f00c105309cafd09a83b7f2a14eb8bd6f277cf3e395a6a8ba";
const ROTATED_DER:&str="302e020100300506032b657004220420983e64431c2c578b689f28a1f33460c269f141882506cd9ba69b69c100d07c1b";
fn key(kid: &str, x: &str) -> Value {
    json!({"kid":kid,"alg":"EdDSA","use":"sig","kty":"OKP","crv":"Ed25519","x":x})
}
fn policy() -> OidcPolicy {
    OidcPolicy {
        issuer: "https://fixture.invalid/issuer".into(),
        audience: "taskloom-fixture-api".into(),
        algorithm: OidcAlgorithm::EdDSA,
        jwks: json!({"keys":[key("fixture-key","X9uGsHHaYVpjr4h1NEQDlfZyatiKvhoB6svb3hbTnlg")]}),
        bindings: vec![OidcSubjectBinding {
            issuer: "https://fixture.invalid/issuer".into(),
            subject: "external-subject".into(),
            user: "local-alice".into(),
        }],
        revoked_jti: Default::default(),
        max_token_lifetime_seconds: 600,
    }
}
fn claims() -> Value {
    let now = jsonwebtoken::get_current_timestamp();
    json!({"iss":"https://fixture.invalid/issuer","sub":"external-subject","aud":"taskloom-fixture-api","iat":now,"nbf":now,"exp":now+300,"jti":"fixture-token-id","roles":["operator"],"tenant":"attacker","user":"not-the-host-user"})
}
fn signed(value: &Value, kid: &str, der: &str) -> String {
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(kid.into());
    encode(
        &header,
        value,
        &EncodingKey::from_ed_der(&hex::decode(der).unwrap()),
    )
    .unwrap()
}

#[test]
fn signature_issuer_audience_expiry_nbf_iat_and_subject_fail_closed() {
    let verifier = OidcVerifier::new(policy()).unwrap();
    let good = signed(&claims(), "fixture-key", TEST_DER);
    let verified = verifier.verify(&good).unwrap();
    assert_eq!(verified.subject(), "external-subject");
    assert!(!format!("{verifier:?}").contains("external-subject"));
    let now = jsonwebtoken::get_current_timestamp();
    for (field, value) in [
        ("iss", json!("https://other.invalid")),
        ("aud", json!("another-api")),
        ("sub", json!("not-mapped")),
        ("exp", json!(now - 1)),
        ("nbf", json!(now + 60)),
        ("iat", json!(now + 60)),
        ("iat", json!(now - 1000)),
    ] {
        let mut bad = claims();
        bad[field] = value;
        assert!(
            verifier
                .verify(&signed(&bad, "fixture-key", TEST_DER))
                .is_err(),
            "{field}"
        );
    }
    for field in ["iss", "sub", "aud", "exp", "iat", "jti"] {
        let mut bad = claims();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            verifier
                .verify(&signed(&bad, "fixture-key", TEST_DER))
                .is_err(),
            "missing {field}"
        );
    }
    let mut parts = good.split('.').map(str::to_owned).collect::<Vec<_>>();
    use base64::Engine;
    let mut altered = claims();
    altered["sub"] = json!("administrator");
    parts[1] = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(altered.to_string());
    let error = verifier.verify(&parts.join(".")).err().unwrap();
    assert!(!error.to_string().contains(&good));
    assert!(!error.to_string().contains("administrator"));
}

#[test]
fn algorithm_confusion_header_urls_unknown_kids_and_unsafe_jwks_are_rejected() {
    let verifier = OidcVerifier::new(policy()).unwrap();
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("fixture-key".into());
    let hmac = encode(
        &header,
        &claims(),
        &EncodingKey::from_secret(b"not-the-public-key"),
    )
    .unwrap();
    assert!(verifier.verify(&hmac).is_err());
    assert!(verifier
        .verify(&signed(&claims(), "unknown-key", TEST_DER))
        .is_err());
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("fixture-key".into());
    header.jku = Some("https://attacker.invalid/keys".into());
    assert!(verifier
        .verify(
            &encode(
                &header,
                &claims(),
                &EncodingKey::from_ed_der(&hex::decode(TEST_DER).unwrap())
            )
            .unwrap()
        )
        .is_err());
    for (field, value) in [
        ("d", json!("private-key-field")),
        ("k", json!("shared-secret")),
        ("x5u", json!("https://attacker.invalid")),
        ("use", json!("enc")),
    ] {
        let mut bad = policy();
        bad.jwks["keys"][0][field] = value;
        assert!(OidcVerifier::new(bad).is_err(), "{field}");
    }
    let mut duplicate = policy();
    let first = duplicate.jwks["keys"][0].clone();
    duplicate.jwks["keys"].as_array_mut().unwrap().push(first);
    assert!(OidcVerifier::new(duplicate).is_err());
}

#[test]
fn explicit_rotation_and_jti_revocation_never_fetch_remote_keys() {
    let original = OidcVerifier::new(policy()).unwrap();
    let next = signed(&claims(), "rotated-key", ROTATED_DER);
    assert!(original.verify(&next).is_err());
    let mut rotated = policy();
    rotated.jwks =
        json!({"keys":[key("rotated-key","Xr9hMswBDO9fFkongt7IpoIigOrcNZP7fAf-_pyJw1w")]});
    let verifier = OidcVerifier::new(rotated).unwrap();
    assert!(verifier.verify(&next).is_ok());
    assert!(verifier
        .verify(&signed(&claims(), "fixture-key", TEST_DER))
        .is_err());
    let mut revoked = policy();
    revoked.revoked_jti.insert("fixture-token-id".into());
    assert!(OidcVerifier::new(revoked)
        .unwrap()
        .verify(&signed(&claims(), "fixture-key", TEST_DER))
        .is_err());
}

#[tokio::test]
async fn host_subject_mapping_uses_live_sql_membership_and_disable_not_claim_roles() {
    let dir = tempfile::tempdir().unwrap();
    let db = SqlDatabase::sqlite(dir.path().join("oidc.sqlite"), DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let store = db.identities();
    store
        .put_user(
            ServiceUser {
                id: "local-alice".into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "tenant".into(),
                    project: "project".into(),
                    role: ProjectRole::Reader,
                }],
            },
            0,
            "host-provisioning",
        )
        .await
        .unwrap();
    let verifier = OidcVerifier::new(policy()).unwrap();
    let bearer = signed(&claims(), "fixture-key", TEST_DER);
    let actor = verifier.authenticate(&store, &bearer).await.unwrap();
    assert_eq!(actor.user_id(), "local-alice");
    let namespace = DatabaseNamespace::new("tenant", "project").unwrap();
    assert_eq!(
        store
            .authorize(&actor, &namespace, ProjectPermission::Read)
            .await
            .unwrap(),
        ProjectRole::Reader
    );
    assert!(store
        .authorize(&actor, &namespace, ProjectPermission::Submit)
        .await
        .is_err());
    assert!(store
        .authorize(
            &actor,
            &DatabaseNamespace::new("other", "project").unwrap(),
            ProjectPermission::Read
        )
        .await
        .is_err());
    let revision = store.snapshot().await.unwrap().revision;
    store
        .put_user(
            ServiceUser {
                id: "local-alice".into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "tenant".into(),
                    project: "project".into(),
                    role: ProjectRole::Runner,
                }],
            },
            revision,
            "host-admin",
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .authorize(&actor, &namespace, ProjectPermission::Submit)
            .await
            .unwrap(),
        ProjectRole::Runner
    );
    let revision = store.snapshot().await.unwrap().revision;
    store
        .put_user(
            ServiceUser {
                id: "local-alice".into(),
                enabled: false,
                memberships: vec![],
            },
            revision,
            "host-admin",
        )
        .await
        .unwrap();
    assert!(store
        .authorize(&actor, &namespace, ProjectPermission::Read)
        .await
        .is_err());
    assert!(verifier.authenticate(&store, &bearer).await.is_err());
    db.close().await;
}

#[tokio::test]
async fn oidc_real_http_boundary_rechecks_membership_and_binds_private_owner() {
    use std::sync::Arc;
    use taskloom::runtime::app::{multiuser::*, AppConfig, Application};
    let dir = tempfile::tempdir().unwrap();
    let db = SqlDatabase::sqlite(
        dir.path().join("http-oidc.sqlite"),
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    db.migrate().await.unwrap();
    let identities = db.identities();
    identities
        .put_user(
            ServiceUser {
                id: "local-alice".into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "tenant".into(),
                    project: "project".into(),
                    role: ProjectRole::Reader,
                }],
            },
            0,
            "host",
        )
        .await
        .unwrap();
    let app = Application::open(AppConfig {
        state_dir: dir.path().join("project"),
        knowledge: false,
        ..Default::default()
    })
    .await
    .unwrap();
    let router = router_with_oidc(
        identities.clone(),
        vec![ProjectApplication {
            tenant: "tenant".into(),
            project: "project".into(),
            application: app.clone(),
        }],
        Some(PrivateMemoryPolicy::default()),
        Arc::new(OidcVerifier::new(policy()).unwrap()),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/tenants/tenant/projects/project/api",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let bearer = signed(&claims(), "fixture-key", TEST_DER);
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/identity"))
        .bearer_auth(&bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let who: Value = response.json().await.unwrap();
    assert_eq!(who["user"], "local-alice");
    assert_eq!(who["role"], "reader");
    assert!(!who.to_string().contains(&bearer));
    assert_eq!(
        client
            .get(format!("{base}/runs"))
            .bearer_auth(&bearer)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let native = identities
        .issue_token("local-alice", std::time::Duration::from_secs(60), "host")
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/identity"))
            .bearer_auth(native.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let mut wrong_issuer = claims();
    wrong_issuer["iss"] = json!("https://attacker.invalid");
    let invalid_tokens = [
        "invalid-not-a-jwt".to_owned(),
        signed(&wrong_issuer, "fixture-key", TEST_DER),
        format!("{bearer}tampered"),
    ];
    for invalid in invalid_tokens {
        let response = client
            .get(format!("{base}/identity"))
            .bearer_auth(&invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert!(!response.text().await.unwrap().contains(&invalid));
    }
    let own: Value = client
        .get(format!("{base}/memory"))
        .bearer_auth(&bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(own["scope"]["owner"], "local-alice");
    assert_eq!(
        client
            .post(format!("{base}/runs"))
            .bearer_auth(&bearer)
            .json(&json!({"id":"claim-role-must-not-grant","prompt":"denied"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let revision = identities.snapshot().await.unwrap().revision;
    identities
        .put_user(
            ServiceUser {
                id: "local-alice".into(),
                enabled: true,
                memberships: vec![],
            },
            revision,
            "host",
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/identity"))
            .bearer_auth(&bearer)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let revision = identities.snapshot().await.unwrap().revision;
    identities
        .put_user(
            ServiceUser {
                id: "local-alice".into(),
                enabled: false,
                memberships: vec![],
            },
            revision,
            "host",
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/identity"))
            .bearer_auth(&bearer)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
    let _ = server.await;
    app.shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    db.close().await;
}
