//! Optional JSON service boundary for host-registered project Applications.
//! Identities live in SQL; project Applications explicitly choose file or SQL.
//! SQL run writers share project quotas and explicit queue admission. Workers
//! run separately; user-specific memory routes remain unavailable.
use super::Application;
use crate::runtime::{database::*, RuntimeError};
use axum::{
    body::{to_bytes, Body},
    extract::{Extension, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

/// Trusted host registration. Incoming bodies cannot select an Application,
/// credential, tool grant, storage root or project membership.
pub struct ProjectApplication {
    pub tenant: String,
    pub project: String,
    pub application: Arc<Application>,
}
#[derive(Clone)]
struct ProjectBoundary {
    identities: SqlIdentityStore,
    namespace: DatabaseNamespace,
    internal: HeaderValue,
    tenant: String,
    project: String,
    queue: Option<SqlQueueStore>,
}

/// Mount `/tenants/T/projects/P/api/...` for each explicit project. No root
/// single-operator API, static UI or identity-administration route is mounted.
pub fn router(
    identities: SqlIdentityStore,
    projects: Vec<ProjectApplication>,
) -> Result<Router, RuntimeError> {
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
        };
        let prefix = format!("/tenants/{}/projects/{}", project.tenant, project.project);
        let scope_router = super::router(project.application, internal)?
            .route("/api/identity", get(whoami))
            .layer(middleware::from_fn_with_state(boundary, authorize));
        result = result.nest(&prefix, scope_router);
    }
    Ok(result)
}
async fn whoami(
    Extension(actor): Extension<AuthenticatedActor>,
    Extension(role): Extension<ProjectRole>,
    Extension(scope): Extension<ProjectScope>,
) -> Json<serde_json::Value> {
    Json(json!({"user":actor.user_id(),"tenant":scope.tenant,"project":scope.project,"role":role}))
}
#[derive(Clone)]
struct ProjectScope {
    tenant: String,
    project: String,
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
    let actor = match boundary.identities.authenticate(bearer).await {
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
            ["identity" | "runs" | "roles" | "setup" | "templates" | "workflows" | "sessions"
            | "knowledge" | "retrieval"]
            | ["runs" | "workflows" | "sessions", _]
            | ["runs", _, "events" | "report"]
            | ["workflows", _, "events"]
            | ["diagnostics", _, _]
            | ["research", "capabilities"] => Some((Read, false)),
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
