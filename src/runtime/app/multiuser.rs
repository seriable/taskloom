//! Optional JSON service boundary for host-registered project Applications.
//! Identities live in SQL; project Applications explicitly choose file or SQL.
//! SQL run writers share project quotas and explicit queue admission. Workers
//! run separately. Optional private-memory CRUD binds ownership to the
//! authenticated caller; shared sessions are never extraction sources.
use super::Application;
use crate::runtime::{database::*, memory::*, RuntimeError};
use axum::{
    body::{to_bytes, Body},
    extract::{Extension, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

/// Trusted host registration. Incoming bodies cannot select an Application,
/// credential, tool grant, storage root or project membership.
pub struct ProjectApplication {
    pub tenant: String,
    pub project: String,
    pub application: Arc<Application>,
}
/// Trusted service policy, separate from single-operator `AppMemory`.
/// Every scope and audit actor is derived from authenticated server context.
#[derive(Debug, Clone, Default)]
pub struct PrivateMemoryPolicy {
    /// Allow Runner/Operator members to curate their own records. Readers
    /// remain read-only, and no role can select another user's namespace.
    pub curate: bool,
    pub recall: MemoryRecallPolicy,
    /// Explicit paid semantic recall only; lexical recall and session turns do
    /// not build vectors or upload memory when this is absent.
    pub semantic: Option<SemanticMemoryPolicy>,
}
impl PrivateMemoryPolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.recall.validate()?;
        if let Some(policy) = &self.semantic {
            policy.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct ServiceOptions {
    pub private_memory: Option<PrivateMemoryPolicy>,
    pub private_execution: Option<super::private_sessions::PrivateSessionPolicy>,
    #[cfg(feature = "oidc")]
    pub oidc: Option<Arc<super::oidc::OidcVerifier>>,
}
#[derive(Clone)]
struct ProjectBoundary {
    identities: SqlIdentityStore,
    namespace: DatabaseNamespace,
    internal: HeaderValue,
    tenant: String,
    project: String,
    queue: Option<SqlQueueStore>,
    application: Arc<Application>,
    private_memory: Option<PrivateMemoryPolicy>,
    private_sessions: Option<Arc<super::private_sessions::PrivateSessionController>>,
    #[cfg(feature = "oidc")]
    oidc: Option<Arc<super::oidc::OidcVerifier>>,
}

/// Mount `/tenants/T/projects/P/api/...` for each explicit project. No root
/// single-operator API, static UI or identity-administration route is mounted.
pub fn router(
    identities: SqlIdentityStore,
    projects: Vec<ProjectApplication>,
) -> Result<Router, RuntimeError> {
    router_with_options(identities, projects, ServiceOptions::default()).map(|(router, _)| router)
}
pub fn router_with_private_memory(
    identities: SqlIdentityStore,
    projects: Vec<ProjectApplication>,
    policy: PrivateMemoryPolicy,
) -> Result<Router, RuntimeError> {
    router_with_options(
        identities,
        projects,
        ServiceOptions {
            private_memory: Some(policy),
            ..Default::default()
        },
    )
    .map(|(router, _)| router)
}
#[cfg(feature = "oidc")]
pub fn router_with_oidc(
    identities: SqlIdentityStore,
    projects: Vec<ProjectApplication>,
    private_memory: Option<PrivateMemoryPolicy>,
    verifier: Arc<super::oidc::OidcVerifier>,
) -> Result<Router, RuntimeError> {
    router_with_options(
        identities,
        projects,
        ServiceOptions {
            private_memory,
            oidc: Some(verifier),
            ..Default::default()
        },
    )
    .map(|(router, _)| router)
}
/// Host-owned capabilities. Retain returned controllers and drain them before
/// stopping the service; default options admit no private generation.
pub fn router_with_options(
    identities: SqlIdentityStore,
    projects: Vec<ProjectApplication>,
    options: ServiceOptions,
) -> Result<
    (
        Router,
        Vec<Arc<super::private_sessions::PrivateSessionController>>,
    ),
    RuntimeError,
> {
    let private_memory = options.private_memory;
    if let Some(policy) = &private_memory {
        policy.validate()?;
    }
    if options.private_execution.is_some() && private_memory.is_none() {
        return Err(config(
            "private execution requires explicit private memory host policy",
        ));
    }
    let mut controllers = Vec::new();
    if projects.is_empty() || projects.len() > 64 {
        return Err(config("multi-user host requires 1..64 explicit projects"));
    }
    let mut scopes = BTreeSet::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut result = Router::new();
    for project in projects {
        if [project.tenant.as_str(), project.project.as_str()]
            .iter()
            .any(|name| matches!(*name, "." | ".."))
        {
            return Err(config(
                "service tenant/project cannot be a dot path segment",
            ));
        }
        let namespace = DatabaseNamespace::new(&project.tenant, &project.project)?;
        if project
            .application
            .config
            .database
            .as_ref()
            .is_some_and(|db| db.tenant != project.tenant || db.project != project.project)
        {
            return Err(config(
                "service scope differs from SQL application namespace",
            ));
        }
        if !scopes.insert((project.tenant.clone(), project.project.clone())) {
            return Err(config("duplicate multi-user project registration"));
        }
        if project.application.config.memory.is_some() {
            return Err(config("fixed single-operator memory configuration cannot be mounted in multi-user service"));
        }
        if private_memory
            .as_ref()
            .is_some_and(|policy| policy.semantic.is_some())
        {
            if project.application.stores.sql.is_none()
                || project.application.config.retrieval.embedding.is_none()
            {
                return Err(config("private semantic memory requires matching SQL storage and explicit metered retrieval.embedding configuration"));
            }
            // Static validation does not resolve credentials or initialize a
            // provider. Upload occurs only on the dedicated authenticated POST.
            project.application.config.retrieval.validate()?;
        }
        // Freeze and check real roots, including aliases/junctions, before
        // accepting requests. The local filesystem administrator is trusted.
        let root = &project.application.config.state_dir;
        if !root.is_absolute() {
            return Err(config("multi-user project state_dir must be absolute"));
        }
        std::fs::create_dir_all(root).map_err(storage)?;
        let root = std::fs::canonicalize(root).map_err(storage)?;
        if roots
            .iter()
            .any(|other| root.starts_with(other) || other.starts_with(&root))
        {
            return Err(config("multi-user project state roots overlap"));
        }
        for store in [
            project.application.runs.file_root(),
            project.application.workflows.file_root(),
            project.application.sessions.file_root(),
            project.application.corpus.file_root(),
            project.application.memory.file_root(),
            project.application.stores.skills.file_root(),
        ]
        .into_iter()
        .flatten()
        {
            std::fs::create_dir_all(store).map_err(storage)?;
            let actual = std::fs::canonicalize(store).map_err(storage)?;
            if actual == root || !actual.starts_with(&root) {
                return Err(config("multi-user store directory escapes project root"));
            }
        }
        if project.application.config.database.is_none() {
            let retrieval = project.application.config.state_dir.join("retrieval");
            std::fs::create_dir_all(&retrieval).map_err(storage)?;
            if !std::fs::canonicalize(&retrieval)
                .map_err(storage)?
                .starts_with(&root)
            {
                return Err(config(
                    "multi-user retrieval directory escapes project root",
                ));
            }
        }
        roots.push(root);
        let internal = format!(
            "internal_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let boundary = ProjectBoundary {
            identities: identities.clone(),
            namespace,
            internal: HeaderValue::from_str(&format!("Bearer {internal}")).map_err(storage)?,
            tenant: project.tenant.clone(),
            project: project.project.clone(),
            queue: project
                .application
                .stores
                .sql
                .as_ref()
                .map(|(db, ns)| db.queue(ns.clone())),
            application: project.application.clone(),
            private_memory: private_memory.clone(),
            private_sessions: if let Some(policy) = &options.private_execution {
                let controller = super::private_sessions::PrivateSessionController::new(
                    project.application.clone(),
                    identities.clone(),
                    project.tenant.clone(),
                    project.project.clone(),
                    private_memory.clone().expect("validated private policy"),
                    policy.clone(),
                )?;
                controllers.push(controller.clone());
                Some(controller)
            } else {
                None
            },
            #[cfg(feature = "oidc")]
            oidc: options.oidc.clone(),
        };
        let prefix = format!("/tenants/{}/projects/{}", project.tenant, project.project);
        let scope_router = super::router(project.application, internal)?
            .route("/api/identity", get(whoami))
            .route(
                "/api/memory/semantic-recall",
                axum::routing::any(|| async {
                    failure(
                        StatusCode::FORBIDDEN,
                        "private semantic recall requires authenticated owner context",
                    )
                }),
            )
            .route(
                "/api/private/{*path}",
                axum::routing::any(|| async {
                    failure(
                        StatusCode::FORBIDDEN,
                        "private operation requires authenticated owner controller",
                    )
                }),
            )
            .layer(middleware::from_fn_with_state(boundary, authorize));
        result = result.nest(&prefix, scope_router);
    }
    Ok((result, controllers))
}
async fn whoami(
    Extension(actor): Extension<AuthenticatedActor>,
    Extension(role): Extension<ProjectRole>,
    Extension(scope): Extension<ProjectScope>,
) -> Json<serde_json::Value> {
    Json(
        json!({"user":actor.user_id(),"tenant":scope.tenant,"project":scope.project,"role":role,"semantic_memory_enabled":scope.semantic_memory_enabled}),
    )
}
#[derive(Clone)]
struct ProjectScope {
    tenant: String,
    project: String,
    semantic_memory_enabled: bool,
}
async fn authorize(
    State(boundary): State<ProjectBoundary>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth_headers = request.headers().get_all(header::AUTHORIZATION);
    if auth_headers.iter().count() != 1 {
        return failure(StatusCode::UNAUTHORIZED, "valid service bearer required");
    }
    let Some(bearer) = auth_headers
        .iter()
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return failure(StatusCode::UNAUTHORIZED, "valid service bearer required");
    };
    let authentication = async {
        #[cfg(feature = "oidc")]
        if let Some(verifier) = &boundary.oidc {
            // Only native opaque tokens enter the native token path. Invalid
            // JWTs never fall through to another issuer/authentication mechanism.
            if !bearer.starts_with("tlm_") {
                return verifier.authenticate(&boundary.identities, bearer).await;
            }
        }
        boundary.identities.authenticate(bearer).await
    }
    .await;
    let actor = match authentication {
        Ok(actor) => actor,
        Err(RuntimeError::Permission(_)) => {
            return failure(StatusCode::UNAUTHORIZED, "valid service bearer required")
        }
        Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "identity store unavailable",
            )
        }
    };
    let Some((permission, bind_actor)) = required(request.method(), request.uri().path()) else {
        return failure(
            StatusCode::FORBIDDEN,
            "operation unavailable in multi-user service",
        );
    };
    let role = match boundary
        .identities
        .authorize(&actor, &boundary.namespace, permission)
        .await
    {
        Ok(role) => role,
        Err(RuntimeError::Permission(_)) => {
            return failure(StatusCode::FORBIDDEN, "project permission required")
        }
        Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "identity store unavailable",
            )
        }
    };
    if request.uri().path().starts_with("/api/private/") {
        return match &boundary.private_sessions {
            Some(controller) => controller.request(actor, role, request).await,
            None => failure(
                StatusCode::FORBIDDEN,
                "private execution is disabled by the host",
            ),
        };
    }
    if request.uri().path().starts_with("/api/memory") {
        return private_memory_request(&boundary, &actor, role, request).await;
    }
    if direct_execution(request.method(), request.uri().path()) {
        if let Some(queue) = &boundary.queue {
            match queue.policy().await {
                Ok(policy) if policy.host_binding.is_some()=>return failure(StatusCode::CONFLICT,"project uses durable queue admission; submit/resume/control the queue job instead"),
                Ok(_)=>(),
                Err(_)=>return failure(StatusCode::SERVICE_UNAVAILABLE,"queue policy unavailable"),
            }
        }
    }
    // The project's single-operator adapter is private to this boundary. An
    // external request can never supply its internal credential or actor.
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, boundary.internal);
    request.extensions_mut().insert(actor.clone());
    request.extensions_mut().insert(role);
    request.extensions_mut().insert(ProjectScope {
        tenant: boundary.tenant,
        project: boundary.project,
        semantic_memory_enabled: boundary
            .private_memory
            .as_ref()
            .is_some_and(|policy| policy.semantic.is_some()),
    });
    if bind_actor {
        let (mut parts, body) = request.into_parts();
        let bytes = match to_bytes(body, 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(_) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds limit"),
        };
        let mut value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(value)) => value,
            _ => return failure(StatusCode::BAD_REQUEST, "JSON object required"),
        };
        value.insert("actor".into(), json!(actor.user_id()));
        let bytes = match serde_json::to_vec(&value) {
            Ok(bytes) => bytes,
            Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid JSON object"),
        };
        parts.headers.remove(header::CONTENT_LENGTH);
        request = Request::from_parts(parts, Body::from(bytes));
    }
    next.run(request).await
}

// Explicit allowlist: future app routes inherit no service authority. Reject
// encoded/ambiguous path forms before policy matching, including GET bypasses.
fn required(method: &Method, path: &str) -> Option<(ProjectPermission, bool)> {
    use ProjectPermission::*;
    if path.contains('%') || path.contains('\\') || path.contains("//") {
        return None;
    }
    if path.starts_with("/api/private/") {
        // The controller validates every exact subroute and current owner.
        // No private operation falls through to the shared Application router.
        return if *method == Method::GET || *method == Method::HEAD {
            Some((Read, false))
        } else if *method == Method::POST {
            Some((Submit, false))
        } else {
            None
        };
    }
    let path = path.strip_prefix("/api/")?;
    let segments: Vec<_> = path.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || matches!(*s, "." | ".."))
    {
        return None;
    }
    let segments = segments.as_slice();
    if *method == Method::GET || *method == Method::HEAD {
        return match segments {
            ["identity" | "operations" | "runs" | "roles" | "setup" | "templates" | "workflows"
            | "sessions" | "knowledge" | "retrieval"]
            | ["runs" | "workflows" | "sessions", _]
            | ["runs", _, "events" | "report"]
            | ["workflows", _, "events"]
            | ["diagnostics", _, _]
            | ["research", "capabilities"] => Some((Read, false)),
            ["memory"] => Some((Read, false)),
            ["operations", "metrics"] => Some((Read, false)),
            ["queue"] | ["queue", "jobs"] | ["queue", "jobs", _] => Some((Read, false)),
            _ => None,
        };
    }
    if *method == Method::DELETE {
        return matches!(segments, ["knowledge", _]).then_some((ManageKnowledge, false));
    }
    if *method != Method::POST {
        return None;
    }
    match segments {
        ["memory", "recall"] => Some((Read, false)),
        ["memory", "semantic-recall"] => Some((Submit, false)),
        ["memory", "review" | "edit" | "forget" | "extract"] => Some((Submit, false)),
        ["queue", "jobs"] | ["queue", "jobs", _, "resume"] => Some((Submit, true)),
        ["queue", "jobs", _, "control"] => Some((Control, true)),
        ["setup" | "retrieval", "validate"]
        | ["workflow-design", "preview"]
        | ["templates", _, _, "preview"] => Some((Read, false)),
        ["runs" | "workflows" | "sessions"]
        | ["workflow-design", "launch"]
        | ["templates", _, _, "runs"]
        | ["sessions", _, "turns"]
        | ["runs" | "workflows" | "sessions", _, "resume"]
        | ["knowledge", "search"]
        | ["retrieval", "explain"] => Some((Submit, false)),
        ["runs" | "sessions", _, "approvals", _] | ["workflows", _, "nodes", _, "approvals", _] => {
            Some((Approve, true))
        }
        ["runs" | "workflows" | "sessions", _, "pause"] => Some((Control, false)),
        ["runs" | "workflows" | "sessions", _, "cancel"]
        | ["sessions", _, "abandon"]
        | ["runs" | "sessions", _, "reconcile", _]
        | ["workflows", _, "nodes", _, "reconcile", _] => Some((Control, true)),
        ["knowledge"]
        | ["knowledge", "web" | "pdf"]
        | ["retrieval", "index"]
        | ["research", "search"] => Some((ManageKnowledge, false)),
        ["diagnostics", _, _, "export"] => Some((ExportTelemetry, false)),
        _ => None,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateMemoryRecall {
    text: String,
}

async fn private_memory_body<T: DeserializeOwned>(request: Request) -> Result<T, Box<Response>> {
    let bytes = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| {
            Box::new(failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds limit",
            ))
        })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        Box::new(failure(
            StatusCode::BAD_REQUEST,
            "invalid private memory request",
        ))
    })
}

async fn private_memory_request(
    boundary: &ProjectBoundary,
    actor: &AuthenticatedActor,
    role: ProjectRole,
    request: Request,
) -> Response {
    let Some(policy) = &boundary.private_memory else {
        return failure(
            StatusCode::FORBIDDEN,
            "private memory is not enabled by the host",
        );
    };
    if request.uri().query().is_some() {
        return failure(
            StatusCode::BAD_REQUEST,
            "private memory scope is server-selected",
        );
    }
    let path = request.uri().path().to_owned();
    if path == "/api/memory/extract" {
        return failure(StatusCode::FORBIDDEN, "private extraction requires isolated private sessions and project-quota-bound run storage; shared sessions are not eligible");
    }
    let scope = MemoryScope {
        tenant: boundary.tenant.clone(),
        project: boundary.project.clone(),
        owner: actor.user_id().to_owned(),
    };
    let is_read = *request.method() == Method::GET
        || *request.method() == Method::HEAD
        || matches!(
            path.as_str(),
            "/api/memory/recall" | "/api/memory/semantic-recall"
        );
    let access = if is_read {
        MemoryAccess::reader(scope, actor.user_id())
    } else if policy.curate && matches!(role, ProjectRole::Runner | ProjectRole::Operator) {
        MemoryAccess::curator(scope, actor.user_id())
    } else {
        return failure(
            StatusCode::FORBIDDEN,
            "host policy and project submission authority are required to curate private memory",
        );
    };
    let access = match access {
        Ok(access) => access,
        Err(error) => return super::service::ApiError::from(error).into_response(),
    };
    let manager = MemoryManager::new(&boundary.application.memory);
    let method = request.method().clone();
    let result: Result<serde_json::Value, RuntimeError> = match path.as_str() {
        "/api/memory" => manager.snapshot(&access).await.and_then(to_private_json),
        "/api/memory/recall" => {
            let input: PrivateMemoryRecall = match private_memory_body(request).await {
                Ok(input) => input,
                Err(response) => return *response,
            };
            manager
                .recall(&access, &input.text, &policy.recall)
                .await
                .and_then(to_private_json)
        }
        "/api/memory/semantic-recall" => {
            let Some(semantic) = &policy.semantic else {
                return failure(
                    StatusCode::FORBIDDEN,
                    "private semantic recall is disabled by the host",
                );
            };
            let input: PrivateMemoryRecall = match private_memory_body(request).await {
                Ok(input) => input,
                Err(response) => return *response,
            };
            private_semantic_recall(boundary, actor, &access, semantic, &input.text)
                .await
                .and_then(to_private_json)
        }
        "/api/memory/review" => {
            let input: super::MemoryReview = match private_memory_body(request).await {
                Ok(input) => input,
                Err(response) => return *response,
            };
            manager
                .review(
                    &access,
                    input.expected_revision,
                    &input.id,
                    MemoryReviewDecision {
                        accept: input.accept,
                        replace: input.replace,
                        note: input.note,
                    },
                )
                .await
                .and_then(to_private_json)
        }
        "/api/memory/edit" => {
            let input: super::MemoryEdit = match private_memory_body(request).await {
                Ok(input) => input,
                Err(response) => return *response,
            };
            manager
                .edit(
                    &access,
                    input.expected_revision,
                    &input.id,
                    &input.value,
                    &input.note,
                )
                .await
                .and_then(to_private_json)
        }
        "/api/memory/forget" => {
            let input: super::MemoryForget = match private_memory_body(request).await {
                Ok(input) => input,
                Err(response) => return *response,
            };
            manager
                .forget(&access, input.expected_revision, input.kind, &input.key)
                .await
                .and_then(to_private_json)
        }
        _ => {
            return failure(
                StatusCode::FORBIDDEN,
                "private memory operation unavailable",
            )
        }
    };
    let mut response = match result {
        Ok(_) if method == Method::HEAD => StatusCode::OK.into_response(),
        Ok(value) => Json(value).into_response(),
        Err(error) => super::service::ApiError::from(error).into_response(),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

struct AuthorizedPrivateEmbedding {
    inner: Arc<dyn crate::runtime::rag::EmbeddingModel>,
    identities: SqlIdentityStore,
    namespace: DatabaseNamespace,
    actor: AuthenticatedActor,
}
impl AuthorizedPrivateEmbedding {
    async fn verify(&self) -> Result<(), RuntimeError> {
        self.identities
            .authorize(&self.actor, &self.namespace, ProjectPermission::Submit)
            .await?;
        Ok(())
    }
}
impl crate::runtime::rag::EmbeddingModel for AuthorizedPrivateEmbedding {
    fn identity(&self) -> String {
        self.inner.identity()
    }
    fn embed<'a>(
        &'a self,
        texts: &'a [String],
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<crate::runtime::rag::EmbeddingBatch, RuntimeError>,
    > {
        Box::pin(async move {
            // Live SQL authority includes token expiry/revocation, enabled user
            // and membership. A paid result is discarded after authority loss;
            // the shared metered provider retains settled/unknown charges.
            self.verify().await?;
            let result = self.inner.embed(texts).await?;
            self.verify().await?;
            Ok(result)
        })
    }
}
async fn private_semantic_recall(
    boundary: &ProjectBoundary,
    actor: &AuthenticatedActor,
    access: &MemoryAccess,
    policy: &SemanticMemoryPolicy,
    text: &str,
) -> Result<SemanticMemoryRecall, RuntimeError> {
    if text.trim().is_empty()
        || text.len() > 4096
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(config("semantic query requires 1..4096 bytes without unsupported control characters; no dispatch"));
    }
    let manager = MemoryManager::new(&boundary.application.memory);
    let before = manager.snapshot(access).await?;
    policy.validate_query_envelope(&before.scope, before.revision, text)?;
    let model = AuthorizedPrivateEmbedding {
        inner: super::retrieval::semantic_embedding(
            &boundary.application.config,
            &boundary.application.stores,
        )?,
        identities: boundary.identities.clone(),
        namespace: boundary.namespace.clone(),
        actor: actor.clone(),
    };
    model.verify().await?;
    let index = manager
        .semantic_index(access, &model, policy.clone())
        .await?;
    let result = manager
        .semantic_recall(access, &index, &model, text)
        .await?;
    if manager.snapshot(access).await? != before {
        return Err(RuntimeError::Conflict(
            "private memory changed during semantic recall; result discarded".into(),
        ));
    }
    model.verify().await?;
    Ok(result)
}
fn to_private_json<T: serde::Serialize>(value: T) -> Result<serde_json::Value, RuntimeError> {
    serde_json::to_value(value).map_err(storage)
}
fn direct_execution(method: &Method, path: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    let Some(path) = path.strip_prefix("/api/") else {
        return false;
    };
    let parts = path.split('/').collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["runs" | "workflows"]
            | ["workflow-design", "launch"]
            | ["templates", _, _, "runs"]
            | ["sessions", _, "turns" | "memory-turns"]
            | [
                "runs" | "workflows" | "sessions",
                _,
                "resume" | "pause" | "cancel"
            ]
    )
}
fn failure(status: StatusCode, message: &str) -> Response {
    let mut result = (status, Json(json!({"error":message}))).into_response();
    result
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    result
}
fn storage(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Storage(error.to_string())
}
fn config(message: &str) -> RuntimeError {
    RuntimeError::Configuration(message.into())
}
