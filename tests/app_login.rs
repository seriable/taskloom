#![cfg(all(feature = "app", feature = "oidc"))]
//! Real loopback HTTP fake IdP and SQL. Public test-only signing fixture;
//! no browser automatically opens an external IdP and no cloud is contacted.
use axum::{
    extract::{Query, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use taskloom::runtime::{
    app::{
        login::{self, BrowserLogin, BrowserLoginConfig},
        multiuser::{self, PrivateMemoryPolicy, ProjectApplication},
        oidc::*,
        AppConfig, Application,
    },
    database::*,
};
const DER:&str="302e020100300506032b657004220420999831564440061f00c105309cafd09a83b7f2a14eb8bd6f277cf3e395a6a8ba";
#[derive(Default)]
struct Idp {
    codes: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
    attempts: AtomicUsize,
    mode: AtomicUsize,
}
async fn authorize(
    State(idp): State<Arc<Idp>>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["scope"], "openid");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["client_id"], "browser-client");
    let code = uuid::Uuid::new_v4().simple().to_string();
    let mut location = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
    location
        .query_pairs_mut()
        .append_pair("state", &query["state"])
        .append_pair("code", &code);
    idp.codes.lock().unwrap().insert(code, query);
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, location.to_string())],
    )
        .into_response()
}
async fn exchange(State(idp): State<Arc<Idp>>, request: Request) -> Response {
    idp.attempts.fetch_add(1, Ordering::SeqCst);
    if idp.mode.load(Ordering::SeqCst) == 3 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let raw = axum::body::to_bytes(request.into_body(), 16384)
        .await
        .unwrap();
    let url = reqwest::Url::parse(&format!(
        "https://form.invalid/?{}",
        std::str::from_utf8(&raw).unwrap()
    ))
    .unwrap();
    let form = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], "browser-client");
    let Some(query) = idp.codes.lock().unwrap().remove(&form["code"]) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
    assert!(form["code_verifier"].len() >= 43);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(form["code_verifier"].as_bytes()));
    assert_eq!(challenge, query["code_challenge"]);
    let now = jsonwebtoken::get_current_timestamp();
    let mode = idp.mode.load(Ordering::SeqCst);
    let mut claims = json!({"iss":"https://idp-fixture.invalid/issuer","sub":"external-alice","aud":"browser-client","iat":now,"exp":if mode==2{now-1}else{now+300},"jti":uuid::Uuid::new_v4().simple().to_string(),"nonce":if mode==1{"invalid-nonce"}else{query["nonce"].as_str()},"roles":["operator"]});
    if mode == 4 {
        claims["aud"] = json!(["browser-client", "other-client"]);
        claims["azp"] = json!("other-client");
    }
    let mut header = Header::new(Algorithm::EdDSA);
    if mode == 5 {
        header.typ = Some("at+jwt".into());
    }
    header.kid = Some("fixture".into());
    let token = encode(
        &header,
        &claims,
        &EncodingKey::from_ed_der(&hex::decode(DER).unwrap()),
    )
    .unwrap();
    Json(json!({"id_token":token,"access_token":"NEVER-RETURN-THIS-ACCESS-TOKEN","refresh_token":"NEVER-RETURN-THIS-REFRESH-TOKEN","token_type":"Bearer"})).into_response()
}
struct Harness {
    _dir: tempfile::TempDir,
    db: SqlDatabase,
    login: Arc<BrowserLogin>,
    app: Arc<Application>,
    idp: Arc<Idp>,
    base: String,
    client: reqwest::Client,
    servers: Vec<tokio::task::JoinHandle<()>>,
}
impl Harness {
    async fn new(pending_ttl: u64, max_pending: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = SqlDatabase::sqlite(dir.path().join("login.sqlite"), DatabaseOptions::default())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        db.identities()
            .put_user(
                ServiceUser {
                    id: "alice".into(),
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
        let idp = Arc::new(Idp::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_base = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/authorize", get(authorize))
            .route("/token", post(exchange))
            .with_state(idp.clone());
        let idp_server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let policy = OidcPolicy {
            issuer: "https://idp-fixture.invalid/issuer".into(),
            audience: "browser-client".into(),
            algorithm: OidcAlgorithm::EdDSA,
            jwks: json!({"keys":[{"kid":"fixture","alg":"EdDSA","use":"sig","kty":"OKP","crv":"Ed25519","x":"X9uGsHHaYVpjr4h1NEQDlfZyatiKvhoB6svb3hbTnlg"}]}),
            bindings: vec![OidcSubjectBinding {
                issuer: "https://idp-fixture.invalid/issuer".into(),
                subject: "external-alice".into(),
                user: "alice".into(),
            }],
            revoked_jti: Default::default(),
            max_token_lifetime_seconds: 600,
        };
        let config = BrowserLoginConfig {
            authorization_endpoint: format!("{idp_base}/authorize"),
            token_endpoint: format!("{idp_base}/token"),
            client_id: "browser-client".into(),
            client_secret_env: None,
            redirect_uri: format!("{base}/auth/callback"),
            success_path: "/console".into(),
            pending_ttl_seconds: pending_ttl,
            session_ttl_seconds: 60,
            max_pending,
            max_sessions: 4,
            allow_http_loopback_for_tests: true,
        };
        let login = BrowserLogin::new(
            config,
            Arc::new(OidcVerifier::new(policy).unwrap()),
            db.identities(),
        )
        .unwrap();
        let app = Application::open(AppConfig {
            state_dir: dir.path().join("project"),
            knowledge: false,
            ..Default::default()
        })
        .await
        .unwrap();
        let api = multiuser::router_with_private_memory(
            db.identities(),
            vec![ProjectApplication {
                tenant: "tenant".into(),
                project: "project".into(),
                application: app.clone(),
            }],
            PrivateMemoryPolicy::default(),
        )
        .unwrap()
        .layer(axum::middleware::from_fn_with_state(
            login.clone(),
            login::cookie_bridge,
        ));
        let router = api.merge(login::router(login.clone()));
        let api_server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            _dir: dir,
            db,
            login,
            app,
            idp,
            base,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            servers: vec![idp_server, api_server],
        }
    }
    async fn flow(&self) -> (String, String) {
        let start = self
            .client
            .get(format!("{}/auth/login", self.base))
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), 303);
        let cookie = start.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
        let binding = cookie.split(';').next().unwrap().to_owned();
        let next = self
            .client
            .get(start.headers()[header::LOCATION].to_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(next.status(), 303);
        (
            next.headers()[header::LOCATION].to_str().unwrap().into(),
            binding,
        )
    }
    async fn complete(&self) -> (String, String) {
        let (callback, binding) = self.flow().await;
        let response = self
            .client
            .get(callback)
            .header(header::COOKIE, binding)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 303);
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(!cookie.contains("tlm_"));
        let cookie = cookie.split(';').next().unwrap().to_owned();
        assert_eq!(response.headers()[header::LOCATION], "/console");
        assert!(response.text().await.unwrap().is_empty());
        let session: Value = self
            .client
            .get(format!("{}/auth/session", self.base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session["user"], "alice");
        assert!(!session.to_string().contains("NEVER-RETURN"));
        assert!(!session.to_string().contains("tlm_"));
        (cookie, session["csrf"].as_str().unwrap().into())
    }
    async fn close(self) {
        self.login.shutdown().await.unwrap();
        for server in self.servers {
            server.abort();
            let _ = server.await;
        }
        self.app.shutdown(Duration::from_secs(1)).await.unwrap();
        self.db.close().await;
    }
}

#[tokio::test]
async fn real_pkce_callback_cookie_bridge_csrf_live_roles_and_logout() {
    let host = Harness::new(60, 8).await;
    let (cookie, csrf) = host.complete().await;
    assert_eq!(host.idp.attempts.load(Ordering::SeqCst), 1);
    let api = format!("{}/tenants/tenant/projects/project/api", host.base);
    let identity: Value = host
        .client
        .get(format!("{api}/identity"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(identity["user"], "alice");
    assert_eq!(identity["role"], "reader");
    assert_eq!(
        host.client
            .post(format!("{api}/memory/recall"))
            .header(header::COOKIE, &cookie)
            .json(&json!({"text":"preference"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        host.client
            .post(format!("{api}/memory/recall"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, "https://attacker.invalid")
            .header("x-taskloom-csrf", &csrf)
            .json(&json!({"text":"preference"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        host.client
            .post(format!("{api}/memory/recall"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &host.base)
            .header("x-taskloom-csrf", &csrf)
            .json(&json!({"text":"preference"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        host.client
            .post(format!("{api}/runs"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &host.base)
            .header("x-taskloom-csrf", &csrf)
            .json(&json!({"id":"claimrole","prompt":"denied"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let native = host
        .db
        .identities()
        .issue_token("alice", Duration::from_secs(60), "host")
        .await
        .unwrap();
    assert_eq!(
        host.client
            .get(format!("{api}/identity"))
            .bearer_auth(native.bearer.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        host.client
            .get(format!("{api}/identity"))
            .bearer_auth(native.bearer.as_str())
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        host.client
            .post(format!("{}/auth/logout", host.base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = host
        .client
        .post(format!("{}/auth/logout", host.base))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &host.base)
        .header("x-taskloom-csrf", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    assert!(response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        host.client
            .get(format!("{api}/identity"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(host
        .db
        .identities()
        .snapshot()
        .await
        .unwrap()
        .tokens
        .values()
        .any(|token| token.revoked));
    let (cookie, _) = host.complete().await;
    let store = host.db.identities();
    let revision = store.snapshot().await.unwrap().revision;
    store
        .put_user(
            ServiceUser {
                id: "alice".into(),
                enabled: false,
                memberships: vec![],
            },
            revision,
            "host",
        )
        .await
        .unwrap();
    assert_eq!(
        host.client
            .get(format!("{api}/identity"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    host.close().await;
}

#[tokio::test]
async fn state_browser_binding_nonce_expiry_and_exchange_failures_never_replay() {
    let host = Harness::new(60, 8).await;
    let (callback, binding) = host.flow().await;
    assert_eq!(host.client.get(&callback).header(header::COOKIE,"taskloom-dev-login=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").send().await.unwrap().status(),401);
    assert_eq!(host.idp.attempts.load(Ordering::SeqCst), 0);
    let (first, second) = tokio::join!(
        host.client
            .get(&callback)
            .header(header::COOKIE, &binding)
            .send(),
        host.client
            .get(&callback)
            .header(header::COOKIE, &binding)
            .send(),
    );
    let mut statuses = [
        first.unwrap().status().as_u16(),
        second.unwrap().status().as_u16(),
    ];
    statuses.sort();
    assert_eq!(statuses, [303, 401]);
    assert_eq!(
        host.client
            .get(&callback)
            .header(header::COOKIE, &binding)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(host.idp.attempts.load(Ordering::SeqCst), 1);
    for mode in [1, 2, 3, 4, 5] {
        host.idp.mode.store(mode, Ordering::SeqCst);
        let (callback, binding) = host.flow().await;
        let before = host.idp.attempts.load(Ordering::SeqCst);
        let response = host
            .client
            .get(&callback)
            .header(header::COOKIE, &binding)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        let body = response.text().await.unwrap();
        assert!(!body.contains("nonce") && !body.contains("NEVER-RETURN"));
        assert_eq!(
            host.client
                .get(&callback)
                .header(header::COOKIE, &binding)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(host.idp.attempts.load(Ordering::SeqCst), before + 1);
    }
    host.close().await;
}

#[tokio::test]
async fn pending_bounds_expiry_duplicate_inputs_and_shutdown_fail_closed() {
    let host = Harness::new(1, 1).await;
    let (callback, binding) = host.flow().await;
    assert_eq!(
        host.client
            .get(format!("{}/auth/login", host.base))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        host.client
            .get(&callback)
            .header(header::COOKIE, &binding)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(host.idp.attempts.load(Ordering::SeqCst), 0);
    let (callback, binding) = host.flow().await;
    assert_eq!(
        host.client
            .get(format!("{callback}&state=duplicate"))
            .header(header::COOKIE, &binding)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    host.login.shutdown().await.unwrap();
    assert_eq!(
        host.client
            .get(callback)
            .header(header::COOKIE, binding)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        host.client
            .get(format!("{}/auth/login", host.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    host.close().await;
}
