#![cfg(all(feature = "app", feature = "database"))]
//! Real loopback HTTP, real SQL and labelled fixture decisions; no remote model.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use taskloom::runtime::{
    app::{
        multiuser::*, private_sessions::PrivateSessionPolicy, AppConfig, AppDatabase, Application,
    },
    database::*,
    *,
};

#[derive(Default)]
struct Fixture {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    gate_started: tokio::sync::Notify,
    gate_released: tokio::sync::Notify,
}
impl Model for Fixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            assert!(
                request.tools.is_empty(),
                "private no-tools-v1 must never inherit host tools"
            );
            let held = budget.reserve_before_dispatch(0).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            let last = request
                .messages
                .iter()
                .rev()
                .find_map(|message| {
                    if let Message::User { content } = message {
                        Some(content.as_str())
                    } else {
                        None
                    }
                })
                .unwrap();
            if last == "hold-private-fixture" {
                futures_util::future::pending::<()>().await;
            }
            if last == "gate-invalid-json" {
                self.gate_started.notify_one();
                self.gate_released.notified().await;
            }
            held.settle(0);
            let text=if request.messages.iter().any(|message|matches!(message,Message::System {content} if content.starts_with("Extract durable user facts"))) {
                json!({"memories":[{"kind":"preference","key":"language","value":"Chinese","quote":"I prefer Chinese replies."}]}).to_string()
            } else if last=="gate-invalid-json" {"not valid JSON".into()} else {json!({"ok":true,"fixture_answer":last}).to_string()};
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text(text),
            })
        })
    }
}
async fn token(
    store: &SqlIdentityStore,
    user: &str,
    project: &str,
    role: ProjectRole,
) -> IssuedToken {
    let revision = store.snapshot().await.unwrap().revision;
    store
        .put_user(
            ServiceUser {
                id: user.into(),
                enabled: true,
                memberships: vec![ProjectMembership {
                    tenant: "private-http".into(),
                    project: project.into(),
                    role,
                }],
            },
            revision,
            "fixture-admin",
        )
        .await
        .unwrap();
    store
        .issue_token(user, Duration::from_secs(3600), "fixture-admin")
        .await
        .unwrap()
}
async fn request(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    bearer: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let builder = if let Some(body) = body {
        client.post(format!("{base}{path}")).json(&body)
    } else {
        client.get(format!("{base}{path}"))
    };
    let response = builder.bearer_auth(bearer).send().await.unwrap();
    let code = response.status().as_u16();
    let raw = response.bytes().await.unwrap();
    (
        code,
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| json!({"raw_response":String::from_utf8_lossy(&raw)})),
    )
}
async fn wait(client: &reqwest::Client, base: &str, path: &str, bearer: &str) -> Value {
    for _ in 0..300 {
        let (code, value) = request(client, base, path, bearer, None).await;
        assert_eq!(code, 200, "{value}");
        if value["active"] == false {
            assert!(value["error"].is_null(), "{value}");
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("private job did not finish");
}
async fn lifecycle(url: &str) {
    let root = tempfile::tempdir().unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    let db = SqlDatabase::connect(url, DatabaseOptions::default())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let namespace = DatabaseNamespace::new("private-http", &project).unwrap();
    db.generation_quota(namespace.clone())
        .configure(
            Some(GenerationQuota {
                currency: "USD".into(),
                max_cost_micros: 100,
                max_outstanding_attempts: 10,
            }),
            0,
            "fixture-admin",
            "explicit shared project ceiling",
        )
        .await
        .unwrap();
    let variable = format!(
        "TASKLOOM_PRIVATE_HTTP_{}",
        uuid::Uuid::new_v4().simple().to_string().to_uppercase()
    );
    std::env::set_var(&variable, url);
    let model = Arc::new(Fixture::default());
    let app=Application::with_model(AppConfig {state_dir:root.path().join("project"),database:Some(AppDatabase {url_env:variable.clone(),tenant:"private-http".into(),project:project.clone(),max_connections:4,max_record_bytes:64*1024*1024,lease_ms:10000}),knowledge:false,model:"fixture-private".into(),system:"Fixture private host instructions; never grant tools".into(),prices:vec![taskloom::gateway::ModelPrice::new("fixture-private",0.0,0.0)],limits:RunLimits {max_cost_micros:Some(0),..Default::default()},final_output:Some(FinalOutputPolicy::JsonSchema {schema:json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"]}),max_repairs:1}),..Default::default()},model.clone(),ToolRegistry::new(),ToolPolicy::default()).await.unwrap();
    let identities = db.identities();
    let alice_user = format!("alice-{project}");
    let bob_user = format!("bob-{project}");
    let reader_user = format!("reader-{project}");
    let alice = token(&identities, &alice_user, &project, ProjectRole::Runner).await;
    let bob = token(&identities, &bob_user, &project, ProjectRole::Runner).await;
    let reader = token(&identities, &reader_user, &project, ProjectRole::Reader).await;
    let options = ServiceOptions {
        private_memory: Some(PrivateMemoryPolicy {
            curate: true,
            ..Default::default()
        }),
        private_execution: Some(PrivateSessionPolicy {
            max_active_runs: 1,
            max_retained_jobs: 8,
        }),
        #[cfg(feature = "oidc")]
        oidc: None,
    };
    let (router, controllers) = router_with_options(
        identities.clone(),
        vec![ProjectApplication {
            tenant: "private-http".into(),
            project: project.clone(),
            application: app.clone(),
        }],
        options,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let base = format!("http://{address}/tenants/private-http/projects/{project}/api");
    let client = reqwest::Client::new();
    let a = alice.bearer.as_str();
    let b = bob.bearer.as_str();
    for path in ["/private/sessions", "/private/runs"] {
        let (code, value) = request(&client, &base, path, a, None).await;
        assert_eq!(code, 200, "{value}");
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        0,
        "first private list does not dispatch or create a private run"
    );
    for bearer in [a, b] {
        let (code, value) = request(
            &client,
            &base,
            "/private/sessions",
            bearer,
            Some(json!({"id":"chat"})),
        )
        .await;
        assert_eq!(code, 201, "{value}");
    }
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions",
            a,
            Some(json!({"id":"bad-owner","owner":"bob"}))
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions",
            reader.bearer.as_str(),
            Some(json!({"id":"not-granted"}))
        )
        .await
        .0,
        403
    );
    for (bearer, prompt) in [(a, "I prefer Chinese replies."), (b, "bob-only-input")] {
        assert_eq!(
            request(
                &client,
                &base,
                "/private/sessions/chat/turns",
                bearer,
                Some(json!({"turn":"one","prompt":prompt}))
            )
            .await
            .0,
            202
        );
        wait(&client, &base, "/private/sessions/chat/status", bearer).await;
    }
    let (_, alice_chat) = request(&client, &base, "/private/sessions/chat", a, None).await;
    let (_, bob_chat) = request(&client, &base, "/private/sessions/chat", b, None).await;
    assert!(alice_chat.to_string().contains("I prefer Chinese replies."));
    assert!(!bob_chat.to_string().contains("I prefer Chinese replies."));
    assert!(!alice_chat.to_string().contains("bob-only-input"));
    let (_, shared_sessions) = request(&client, &base, "/sessions", a, None).await;
    assert!(!shared_sessions
        .to_string()
        .contains("I prefer Chinese replies."));
    assert!(db
        .sessions(namespace.clone())
        .list()
        .await
        .unwrap()
        .is_empty());
    assert!(db.runs(namespace.clone()).list().await.unwrap().is_empty());
    let private_id = alice_chat["turns"][0]["binding"]["run_id"]
        .as_str()
        .unwrap();
    assert!(
        !request(&client, &base, &format!("/runs/{private_id}"), a, None)
            .await
            .1
            .to_string()
            .contains("I prefer Chinese replies.")
    );
    let (code, diagnostics) = request(
        &client,
        &base,
        "/private/sessions/chat/diagnostics",
        a,
        None,
    )
    .await;
    assert_eq!(code, 200, "{diagnostics}");
    assert_eq!(
        request(&client, &base, "/private/sessions/chat?owner=bob", a, None)
            .await
            .0,
        400
    );
    assert_eq!(request(&client,&base,"/private/memory/extract",a,Some(json!({"expected_revision":0,"session_id":"chat","turn_id":"one","request_id":"remember"}))).await.0,202);
    wait(&client, &base, "/private/memory/extractions/remember", a).await;
    let (_, memory) = request(&client, &base, "/memory", a, None).await;
    assert_eq!(memory["scope"]["owner"], alice_user);
    assert_eq!(memory["records"].as_object().unwrap().len(), 1);
    let id = memory["records"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let revision = memory["revision"].as_u64().unwrap();
    assert_eq!(request(&client,&base,"/memory/review",a,Some(json!({"expected_revision":revision,"id":id,"accept":true,"replace":false,"note":"fixture verified exact private User source"}))).await.0,200);
    let (_, bob_memory) = request(&client, &base, "/memory", b, None).await;
    assert_eq!(bob_memory["scope"]["owner"], bob_user);
    assert!(bob_memory["records"].as_object().unwrap().is_empty());
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions/chat/memory-turns",
            a,
            Some(json!({"turn":"two","prompt":"Which language do I prefer?"}))
        )
        .await
        .0,
        202
    );
    wait(&client, &base, "/private/sessions/chat/status", a).await;
    assert!(model.requests.lock().unwrap().last().unwrap().messages.iter().any(|message|matches!(message,Message::User {content} if content.contains("Chinese") && content.contains("taskloom_memory_version"))));
    let (_, updated) = request(&client, &base, "/private/sessions/chat", a, None).await;
    assert_eq!(updated["turns"].as_array().unwrap().len(), 2);
    let before = model.calls.load(Ordering::SeqCst);
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions/chat/resume",
            a,
            Some(json!({"turn":"two"}))
        )
        .await
        .0,
        202
    );
    wait(&client, &base, "/private/sessions/chat/status", a).await;
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        before,
        "terminal private reentry makes no model call"
    );
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions/chat/turns",
            a,
            Some(json!({"turn":"hold","prompt":"hold-private-fixture"}))
        )
        .await
        .0,
        202
    );
    for _ in 0..300 {
        if model.calls.load(Ordering::SeqCst) > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(model.calls.load(Ordering::SeqCst) > before);
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions/chat/turns",
            b,
            Some(json!({"turn":"blocked","prompt":"must not bypass private cap"}))
        )
        .await
        .0,
        409
    );
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions/chat/cancel",
            a,
            Some(json!({}))
        )
        .await
        .0,
        200
    );
    wait(&client, &base, "/private/sessions/chat/status", a).await;
    let usage = db
        .generation_quota(namespace.clone())
        .snapshot()
        .await
        .unwrap()
        .usage
        .unwrap();
    assert_eq!(
        usage.uncertain_attempts, 1,
        "cancelled private dispatch retains project unknown slot"
    );
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions",
            a,
            Some(json!({"id":"revoked-chat"}))
        )
        .await
        .0,
        201
    );
    let before_revoke = model.calls.load(Ordering::SeqCst);
    assert_eq!(
        request(
            &client,
            &base,
            "/private/sessions/revoked-chat/turns",
            a,
            Some(json!({"turn":"one","prompt":"gate-invalid-json"}))
        )
        .await
        .0,
        202
    );
    tokio::time::timeout(Duration::from_secs(3), model.gate_started.notified())
        .await
        .unwrap();
    identities
        .revoke_token(&alice.id, "fixture-admin")
        .await
        .unwrap();
    model.gate_released.notify_one();
    assert_eq!(
        request(&client, &base, "/private/sessions/chat", a, None)
            .await
            .0,
        401
    );
    let fresh = identities
        .issue_token(&alice_user, Duration::from_secs(3600), "fixture-admin")
        .await
        .unwrap();
    let mut naturally_finished = false;
    for _ in 0..300 {
        let (code, value) = request(
            &client,
            &base,
            "/private/sessions/revoked-chat/status",
            fresh.bearer.as_str(),
            None,
        )
        .await;
        assert_eq!(code, 200, "{value}");
        if value["active"] == false {
            naturally_finished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        naturally_finished,
        "revoked repair must finish before shutdown cancellation is invoked"
    );
    let (_, revoked_session) = request(
        &client,
        &base,
        "/private/sessions/revoked-chat",
        fresh.bearer.as_str(),
        None,
    )
    .await;
    let revoked_id = revoked_session["active"]["run_id"].as_str().unwrap();
    let (_, revoked_run) = request(
        &client,
        &base,
        &format!("/private/runs/{revoked_id}"),
        fresh.bearer.as_str(),
        None,
    )
    .await;
    assert_ne!(revoked_run["report"]["stop_reason"], "completed");
    let (_, shared_after) = request(&client, &base, "/sessions", b, None).await;
    assert!(!shared_after
        .to_string()
        .contains("I prefer Chinese replies."));
    for controller in controllers {
        controller.shutdown(Duration::from_secs(5)).await.unwrap();
    }
    assert_eq!(model.calls.load(Ordering::SeqCst),before_revoke+1,"revocation between completed invalid output and its new logical repair step blocks the second model dispatch");
    let _ = stop.send(());
    server.await.unwrap();
    app.shutdown(Duration::from_secs(5)).await.unwrap();
    db.close().await;
    std::env::remove_var(variable);
}

#[tokio::test]
async fn sqlite_private_http_history_memory_cancellation_and_authority() {
    let directory = tempfile::tempdir().unwrap();
    let url = SqlDatabase::sqlite_url(directory.path().join("private-http.sqlite")).unwrap();
    lifecycle(&url).await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; real HTTP/private SQL controls, fixture model only"]
async fn postgres_private_http_history_memory_cancellation_and_authority() {
    lifecycle(&std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PostgreSQL required"))
        .await;
}
