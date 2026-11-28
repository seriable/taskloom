//! Explicit SQL project service host and non-dispatching deployment probes.
use super::{multiuser, AppConfig, AppStores, Application};
use crate::runtime::database::{DatabaseOptions, SqlDatabase, SqlIdentityStore};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceHost {
    pub listen: SocketAddr,
    #[serde(default = "database_environment")]
    pub database_url_env: String,
    pub projects: Vec<ServiceProject>,
    #[serde(default = "grace")]
    pub shutdown_grace_seconds: u64,
    #[serde(default)]
    pub console: bool,
    /// Trusted deployment policy; request bodies cannot change it.
    #[serde(default)]
    pub private_memory: Option<HostPrivateMemory>,
    #[serde(default)]
    pub private_execution: Option<super::private_sessions::PrivateSessionPolicy>,
    #[cfg(feature = "oidc")]
    #[serde(default)]
    pub oidc: Option<super::oidc::OidcPolicy>,
    #[cfg(feature = "oidc")]
    #[serde(default)]
    pub browser_login: Option<HostBrowserLogin>,
}
#[cfg(feature = "oidc")]
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBrowserLogin {
    pub configuration: super::login::BrowserLoginConfig,
    pub id_token_policy: super::oidc::OidcPolicy,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostPrivateMemory {
    pub curate: bool,
    pub recall: crate::runtime::memory::MemoryRecallPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<crate::runtime::memory::SemanticMemoryPolicy>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceProject {
    pub tenant: String,
    pub project: String,
    pub config: PathBuf,
}
fn database_environment() -> String {
    "TASKLOOM_DATABASE_URL".into()
}
fn grace() -> u64 {
    20
}

async fn configuration(
    path: &Path,
) -> Result<(ServiceHost, Vec<AppConfig>), Box<dyn std::error::Error>> {
    let path = std::fs::canonicalize(path)?;
    let source = path.clone();
    let raw = crate::runtime::store::blocking(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(source)
            .map_err(crate::runtime::store::storage)?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(crate::runtime::store::storage)?;
        Ok(bytes)
    })
    .await?;
    if raw.len() > 1024 * 1024 {
        return Err("service host configuration exceeds 1MiB".into());
    }
    let host: ServiceHost = serde_json::from_slice(&raw)?;
    #[cfg(feature = "oidc")]
    if let Some(login) = &host.browser_login {
        login.configuration.validate()?;
        let verifier = super::oidc::OidcVerifier::new(login.id_token_policy.clone())?;
        if verifier.audience() != login.configuration.client_id {
            return Err("browser ID-token audience must equal client ID".into());
        }
    }
    #[cfg(feature = "oidc")]
    if let Some(policy) = &host.oidc {
        super::oidc::OidcVerifier::new(policy.clone())?;
    }
    if let Some(policy) = &host.private_memory {
        policy.recall.validate()?;
        if let Some(semantic) = &policy.semantic {
            semantic.validate()?;
        }
    }
    if host.projects.is_empty()
        || host.projects.len() > 64
        || !(1..=300).contains(&host.shutdown_grace_seconds)
    {
        return Err("service requires 1..64 projects and 1..300 shutdown grace seconds".into());
    }
    let mut configs = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    for project in &host.projects {
        let source = if project.config.is_absolute() {
            project.config.clone()
        } else {
            path.parent().unwrap().join(&project.config)
        };
        let config = AppConfig::load(&source).await?;
        if host
            .private_memory
            .as_ref()
            .is_some_and(|policy| policy.semantic.is_some())
            && config.retrieval.embedding.is_none()
        {
            return Err("private semantic memory requires explicit metered retrieval.embedding configuration".into());
        }
        let database = config
            .database
            .as_ref()
            .ok_or("service host requires explicit SQL Applications")?;
        if database.tenant != project.tenant
            || database.project != project.project
            || config.memory.is_some()
            || !identities.insert((&project.tenant, &project.project))
        {
            return Err(
                "service project SQL scope mismatch, duplicate or fixed private memory".into(),
            );
        }
        configs.push(config);
    }
    Ok((host, configs))
}

async fn identity_database(host: &ServiceHost) -> Result<SqlDatabase, Box<dyn std::error::Error>> {
    let value = super::config::secret(&host.database_url_env)?;
    Ok(SqlDatabase::connect(&value, DatabaseOptions::default()).await?)
}

/// Validate configuration, schema, dataset and queue policies without model,
/// MCP, embedding or telemetry initialization. This never applies migrations.
pub async fn check(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (host, configs) = configuration(path).await?;
    let database = identity_database(&host).await?;
    let checked = async {
        database.identity().await?;
        database.identities().snapshot().await?;
        for config in configs {
            let stores = AppStores::open(&config).await?;
            let result = async {
                let (db, scope) = stores.sql.as_ref().ok_or("SQL store unavailable")?;
                let policy = db.queue(scope.clone()).policy().await?;
                if policy.host_binding.is_none() || policy.limits.is_none() {
                    return Err("service requires an explicitly enabled queue policy".into());
                }
                Ok::<_, Box<dyn std::error::Error>>(())
            }
            .await;
            stores.close().await;
            result?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    database.close().await;
    checked
}

#[derive(Clone)]
struct Health {
    identities: SqlIdentityStore,
    apps: Vec<Arc<Application>>,
    draining: Arc<AtomicBool>,
}
async fn ready(State(health): State<Health>) -> (StatusCode, Json<serde_json::Value>) {
    let checked = tokio::time::timeout(Duration::from_secs(3), async {
        if health.draining.load(Ordering::Acquire) {
            return false;
        }
        if health.identities.snapshot().await.is_err() {
            return false;
        }
        for app in &health.apps {
            let Some((database, _)) = &app.stores.sql else {
                return false;
            };
            if database.identity().await.is_err() {
                return false;
            }
            let Ok(queue) = app.queue_store() else {
                return false;
            };
            let Ok(policy) = queue.policy().await else {
                return false;
            };
            if policy.limits.is_none()
                || app.queue_binding().ok().as_ref() != policy.host_binding.as_ref()
            {
                return false;
            }
        }
        !health.draining.load(Ordering::Acquire)
    })
    .await
    .unwrap_or(false);
    (
        if checked {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({"ready":checked})),
    )
}

/// Start authenticated project JSON APIs and the explicitly enabled project
/// console. Identity administration stays outside HTTP; TLS termination remains
/// an explicit reverse-proxy responsibility.
pub async fn serve(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    check(path).await?;
    let (host, configs) = configuration(path).await?;
    let database = identity_database(&host).await?;
    let mut apps = Vec::new();
    let mut private_controllers = Vec::new();
    #[cfg(feature = "oidc")]
    let mut browser_session = None;
    let setup = async {
        let mut projects = Vec::new();
        for (project, config) in host.projects.iter().zip(configs) {
            let app = Box::pin(Application::open(config)).await?;
            projects.push(multiuser::ProjectApplication {
                tenant: project.tenant.clone(),
                project: project.project.clone(),
                application: Arc::clone(&app),
            });
            apps.push(app);
        }
        let health = Health {
            identities: database.identities(),
            apps: apps.clone(),
            draining: Arc::new(AtomicBool::new(false)),
        };
        let (project_router, controllers) = multiuser::router_with_options(
            database.identities(),
            projects,
            multiuser::ServiceOptions {
                private_memory: host.private_memory.as_ref().map(|p| {
                    multiuser::PrivateMemoryPolicy {
                        curate: p.curate,
                        recall: p.recall.clone(),
                        semantic: p.semantic.clone(),
                    }
                }),
                private_execution: host.private_execution.clone(),
                #[cfg(feature = "oidc")]
                oidc: host
                    .oidc
                    .as_ref()
                    .map(|p| super::oidc::OidcVerifier::new(p.clone()).map(Arc::new))
                    .transpose()?,
            },
        )?;
        private_controllers = controllers;
        #[cfg(feature = "oidc")]
        let project_router = if let Some(configuration) = &host.browser_login {
            let login = super::login::BrowserLogin::new(
                configuration.configuration.clone(),
                Arc::new(super::oidc::OidcVerifier::new(
                    configuration.id_token_policy.clone(),
                )?),
                database.identities(),
            )?;
            browser_session = Some(login.clone());
            project_router.layer(axum::middleware::from_fn_with_state(
                login,
                super::login::cookie_bridge,
            ))
        } else {
            project_router
        };
        let router = project_router.merge(
            Router::new()
                .route("/health/live", get(|| async { Json(json!({"live":true})) }))
                .route("/health/ready", get(ready))
                .with_state(health.clone()),
        );
        let router = if host.console {
            router
                .route(
                    "/console",
                    get(|| async { axum::response::Html(include_str!("team.html")) }),
                )
                .route(
                    "/console.js",
                    get(|| async {
                        (
                            [(
                                axum::http::header::CONTENT_TYPE,
                                "text/javascript; charset=utf-8",
                            )],
                            include_str!("team.js"),
                        )
                    }),
                )
                .route(
                    "/console-json.js",
                    get(|| async {
                        (
                            [(
                                axum::http::header::CONTENT_TYPE,
                                "text/javascript; charset=utf-8",
                            )],
                            include_str!("planning-json.js"),
                        )
                    }),
                )
        } else {
            router
        };
        #[cfg(feature = "oidc")]
        let router = if let Some(login) = &browser_session {
            router.merge(super::login::router(login.clone()))
        } else {
            router
        };
        let router = router.layer(axum::middleware::from_fn(super::service::security_headers));
        if ready(State(health.clone())).await.0 != StatusCode::OK {
            return Err("service readiness refused before bind".into());
        }
        let listener = tokio::net::TcpListener::bind(host.listen).await?;
        println!(
            "taskloom project service listening on {}",
            listener.local_addr()?
        );
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            super::cli::shutdown_signal().await;
            health.draining.store(true, Ordering::Release);
            let _ = sender.send(());
        });
        let shutdown = async {
            if receiver.await.is_ok() {
                #[cfg(feature = "oidc")]
                if let Some(login) = &browser_session {
                    login.shutdown().await?;
                }
                for controller in &private_controllers {
                    controller
                        .shutdown(Duration::from_secs(host.shutdown_grace_seconds))
                        .await?;
                }
                for app in &apps {
                    app.shutdown(Duration::from_secs(host.shutdown_grace_seconds))
                        .await?;
                }
            }
            Ok::<_, crate::runtime::RuntimeError>(())
        };
        let (served, stopped) =
            tokio::join!(std::future::IntoFuture::into_future(server), shutdown);
        served?;
        stopped?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    for controller in private_controllers {
        let _ = controller
            .shutdown(Duration::from_secs(host.shutdown_grace_seconds))
            .await;
    }
    #[cfg(feature = "oidc")]
    if let Some(login) = browser_session {
        let _ = login.shutdown().await;
    }
    for app in apps {
        let _ = app
            .shutdown(Duration::from_secs(host.shutdown_grace_seconds))
            .await;
    }
    database.close().await;
    setup
}

/// Recognize deployment commands before loading the ordinary app config.
pub async fn command(args: &[String]) -> Result<bool, Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("service-serve") if args.len() == 2 => serve(Path::new(&args[1])).await?,
        Some("service-check") if args.len() == 2 => { check(Path::new(&args[1])).await?; println!("service configuration, SQL schema and queues ready; no model dispatch"); },
        Some("health-check") if args.len() == 2 => {
            let url = reqwest::Url::parse(&args[1])?;
            if url.scheme() != "http" || url.host_str().is_none_or(|host| !matches!(host,"127.0.0.1"|"localhost"|"::1")) || !url.username().is_empty() || url.password().is_some() || !matches!(url.path(),"/health/live"|"/health/ready") || url.query().is_some() || url.fragment().is_some() { return Err("health-check requires a loopback HTTP health route without credentials".into()); }
            let expected_field = if url.path() == "/health/live" { "live" } else { "ready" };
            let mut response = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(4)).build()?.get(url).send().await?;
            if !response.status().is_success() { return Err("health endpoint is not ready".into()); }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if body.len().saturating_add(chunk.len()) > 4096 { return Err("health response exceeds 4KiB".into()); }
                body.extend_from_slice(&chunk);
            }
            let status: serde_json::Value = serde_json::from_slice(&body)?;
            if status.get(expected_field) != Some(&serde_json::Value::Bool(true)) { return Err("health endpoint did not confirm readiness".into()); }
        },
        Some("service-serve"|"service-check"|"health-check") => return Err("usage: service-serve HOST.json | service-check HOST.json | health-check http://127.0.0.1:PORT/health/live|ready".into()),
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(all(test, feature = "oidc"))]
mod login_header_tests {
    use super::*;
    #[tokio::test]
    async fn integrated_host_security_layer_preserves_strict_auth_csp_and_secure_cookie() {
        use crate::runtime::app::{
            login::{self, BrowserLogin, BrowserLoginConfig},
            oidc::*,
        };
        let dir = tempfile::tempdir().unwrap();
        let database = SqlDatabase::sqlite(
            dir.path().join("login-headers.sqlite"),
            DatabaseOptions::default(),
        )
        .await
        .unwrap();
        database.migrate().await.unwrap();
        let verifier=OidcVerifier::new(OidcPolicy {
            issuer:"https://identity.invalid/issuer".into(),audience:"client".into(),algorithm:OidcAlgorithm::EdDSA,
            jwks:json!({"keys":[{"kid":"test","alg":"EdDSA","use":"sig","kty":"OKP","crv":"Ed25519","x":"X9uGsHHaYVpjr4h1NEQDlfZyatiKvhoB6svb3hbTnlg"}]}),
            bindings:vec![OidcSubjectBinding{issuer:"https://identity.invalid/issuer".into(),subject:"test-subject".into(),user:"test-user".into()}],
            revoked_jti:Default::default(),max_token_lifetime_seconds:600,
        }).unwrap();
        let login = BrowserLogin::new(
            BrowserLoginConfig {
                authorization_endpoint: "https://identity.invalid/authorize".into(),
                token_endpoint: "https://identity.invalid/token".into(),
                client_id: "client".into(),
                client_secret_env: None,
                redirect_uri: "https://service.invalid/auth/callback".into(),
                success_path: "/console".into(),
                pending_ttl_seconds: 60,
                session_ttl_seconds: 300,
                max_pending: 1,
                max_sessions: 1,
                allow_http_loopback_for_tests: false,
            },
            Arc::new(verifier),
            database.identities(),
        )
        .unwrap();
        let router = login::router(login.clone()).layer(axum::middleware::from_fn(
            super::super::service::security_headers,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/auth/login", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let response = client.get(url).send().await.unwrap();
        assert_eq!(response.status(), 303);
        let csp = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert_eq!(
            csp,
            "default-src 'none'; frame-ancestors 'none'; form-action 'none'"
        );
        assert!(!csp.contains("unsafe-inline"));
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        assert!(cookie.starts_with("__Host-taskloom-login="));
        assert!(
            cookie.contains("; Secure")
                && cookie.contains("; HttpOnly")
                && cookie.contains("SameSite=Lax")
        );
        assert!(!cookie.contains("Domain="));
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        server.abort();
        let _ = server.await;
        login.shutdown().await.unwrap();
        database.close().await;
    }
}
