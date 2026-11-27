use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::rag::{
    self, CorpusStore, Document, LexicalRetriever, RetrievalQuery, WindowChunker,
};
use super::super::{ApprovalDecision, RecoveryDecision, RuntimeError};
use super::{config::config_error, AppWorkflowRequest, Application};

type ApiResult = Result<Json<Value>, ApiError>;
pub struct ApiError(RuntimeError);
impl From<RuntimeError> for ApiError {
    fn from(error: RuntimeError) -> Self {
        Self(error)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            RuntimeError::Configuration(_) | RuntimeError::Protocol(_) => StatusCode::BAD_REQUEST,
            RuntimeError::Permission(_) => StatusCode::FORBIDDEN,
            RuntimeError::Conflict(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({"error":self.0.to_string()}))).into_response()
    }
}
/// Bearer-authenticated single-operator API. The static UI contains no token;
/// it asks the operator for one and retains it only in page memory.
pub fn router(app: Arc<Application>, token: String) -> Result<Router, RuntimeError> {
    if token.len() < 16 || token.len() > 4096 || token.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(config_error(
            "API token must contain 16..=4096 non-whitespace bytes",
        ));
    }
    let api = Router::new()
        .route("/runs", get(list).post(start))
        .route("/runs/{id}", get(inspect))
        .route("/runs/{id}/events", get(events))
        .route("/runs/{id}/resume", post(resume))
        .route("/runs/{id}/pause", post(pause))
        .route("/runs/{id}/cancel", post(cancel))
        .route("/runs/{id}/approvals/{ticket}", post(decide))
        .route("/runs/{id}/reconcile/{call}", post(reconcile))
        .route("/runs/{id}/report", get(report))
        .route("/roles", get(roles))
        .route("/workflows", get(workflows).post(start_workflow))
        .route("/workflows/{id}", get(workflow))
        .route("/workflows/{id}/events", get(workflow_events))
        .route("/workflows/{id}/resume", post(resume_workflow))
        .route("/workflows/{id}/pause", post(pause_workflow))
        .route("/workflows/{id}/cancel", post(cancel_workflow))
        .route(
            "/workflows/{id}/nodes/{node}/approvals/{ticket}",
            post(decide_workflow),
        )
        .route(
            "/workflows/{id}/nodes/{node}/reconcile/{call}",
            post(reconcile_workflow),
        )
        .route("/sessions", get(sessions).post(create_session))
        .route("/sessions/{id}", get(session))
        .route("/sessions/{id}/turns", post(start_turn))
        .route("/sessions/{id}/resume", post(resume_session))
        .route("/sessions/{id}/pause", post(pause_session))
        .route("/sessions/{id}/cancel", post(cancel_session))
        .route("/sessions/{id}/abandon", post(abandon_session))
        .route("/sessions/{id}/approvals/{ticket}", post(decide_session))
        .route("/sessions/{id}/reconcile/{call}", post(reconcile_session))
        .route("/knowledge", get(knowledge).post(import))
        .route("/knowledge/{id}", axum::routing::delete(remove))
        .route("/knowledge/search", post(search))
        .route_layer(middleware::from_fn_with_state(
            Arc::new(token),
            authenticate,
        ));
    Ok(Router::new()
        .route("/", get(|| async { Html(include_str!("ui.html")) }))
        .route(
            "/orchestration.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("orchestration.js"),
                )
            }),
        )
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(app))
}
pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Arc<Application>,
    token: String,
) -> Result<(), RuntimeError> {
    let router = router(app, token)?;
    axum::serve(listener, router)
        .await
        .map_err(super::super::store::storage)
}
async fn authenticate(
    State(expected): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    let actual = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("")
        .as_bytes();
    let bytes = expected.as_bytes();
    let mut difference = actual.len() ^ bytes.len();
    for (index, byte) in bytes.iter().enumerate() {
        difference |= usize::from(byte ^ actual.get(index).copied().unwrap_or(0));
    }
    if difference != 0 {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"valid bearer token required"})),
        )
            .into_response()
    } else {
        next.run(request).await
    }
}
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "referrer-policy",
        header::HeaderValue::from_static("no-referrer"),
    );
    headers.insert("content-security-policy",header::HeaderValue::from_static("default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"));
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    id: String,
    prompt: String,
}
async fn start(
    State(app): State<Arc<Application>>,
    Json(input): Json<Start>,
) -> Result<impl IntoResponse, ApiError> {
    let state = app.start(&input.id, &input.prompt).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":input.id,"revision":state.revision})),
    ))
}
async fn list(State(app): State<Arc<Application>>) -> ApiResult {
    Ok(Json(json!({"runs":app.list().await?,"limit":1000})))
}
async fn inspect(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(
        json!({"state":app.inspect(&id).await?,"job":app.job(&id)?}),
    ))
}
#[derive(Deserialize, Default)]
struct EventQuery {
    #[serde(default)]
    after: usize,
}
async fn events(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
) -> ApiResult {
    let state = app.inspect(&id).await?;
    let events = state
        .report
        .events
        .iter()
        .skip(query.after)
        .take(200)
        .collect::<Vec<_>>();
    let next = query.after.saturating_add(events.len());
    Ok(Json(
        json!({"events":events,"next":next,"revision":state.revision,"job":app.job(&id)?}),
    ))
}
async fn resume(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    app.resume(&id).await?;
    Ok(Json(json!({"accepted":true})))
}
async fn pause(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    app.pause(&id)?;
    Ok(Json(json!({"accepted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Actor {
    actor: String,
    note: String,
}
async fn cancel(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Json(actor): Json<Actor>,
) -> ApiResult {
    app.cancel(&id, &actor.actor, &actor.note).await?;
    Ok(Json(json!({"accepted":true})))
}
async fn decide(
    State(app): State<Arc<Application>>,
    Path((id, ticket)): Path<(String, String)>,
    Json(decision): Json<ApprovalDecision>,
) -> ApiResult {
    Ok(Json(
        json!({"state":app.decide(&id,&ticket,decision).await?}),
    ))
}
async fn reconcile(
    State(app): State<Arc<Application>>,
    Path((id, call)): Path<(String, String)>,
    Json(decision): Json<RecoveryDecision>,
) -> ApiResult {
    Ok(Json(
        json!({"state":app.reconcile(&id,&call,decision).await?}),
    ))
}
async fn report(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(
        serde_json::to_value(app.verified_report(&id).await?)
            .map_err(super::super::store::storage)?,
    ))
}
async fn roles(State(app): State<Arc<Application>>) -> ApiResult {
    Ok(Json(json!({"roles":app.config.roles})))
}
async fn workflows(State(app): State<Arc<Application>>) -> ApiResult {
    Ok(Json(
        json!({"workflows":app.list_workflows().await?,"limit":1000}),
    ))
}
async fn workflow(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    let state = app.workflow(&id).await?;
    Ok(Json(
        json!({"budget":state.budget()?,"state":state,"job":app.workflow_job(&id)?}),
    ))
}
async fn workflow_events(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
) -> ApiResult {
    let state = app.workflow(&id).await?;
    let events = state
        .events
        .iter()
        .skip(query.after)
        .take(200)
        .collect::<Vec<_>>();
    let next = query.after.saturating_add(events.len());
    Ok(Json(
        json!({"events":events,"next":next,"revision":state.revision,"job":app.workflow_job(&id)?}),
    ))
}
async fn start_workflow(
    State(app): State<Arc<Application>>,
    Json(input): Json<AppWorkflowRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let state = app.start_workflow(input).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"workflow_id":state.id,"revision":state.revision})),
    ))
}
async fn resume_workflow(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    app.resume_workflow(&id).await?;
    Ok(Json(json!({"accepted":true})))
}
async fn pause_workflow(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    app.pause_workflow(&id)?;
    Ok(Json(json!({"accepted":true})))
}
async fn cancel_workflow(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Json(actor): Json<Actor>,
) -> ApiResult {
    app.cancel_workflow(&id, &actor.actor, &actor.note).await?;
    Ok(Json(json!({"accepted":true})))
}
async fn decide_workflow(
    State(app): State<Arc<Application>>,
    Path((id, node, ticket)): Path<(String, String, String)>,
    Json(decision): Json<ApprovalDecision>,
) -> ApiResult {
    Ok(Json(
        json!({"state":app.decide_workflow(&id,&node,&ticket,decision).await?}),
    ))
}
async fn reconcile_workflow(
    State(app): State<Arc<Application>>,
    Path((id, node, call)): Path<(String, String, String)>,
    Json(decision): Json<RecoveryDecision>,
) -> ApiResult {
    Ok(Json(
        json!({"state":app.reconcile_workflow(&id,&node,&call,decision).await?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    turn_id: String,
    prompt: String,
}
async fn sessions(State(app): State<Arc<Application>>) -> ApiResult {
    Ok(Json(
        json!({"sessions":app.list_sessions().await?,"limit":1000}),
    ))
}
async fn session(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(
        json!({"session":app.session(&id).await?,"job":app.session_job(&id)?}),
    ))
}
async fn create_session(
    State(app): State<Arc<Application>>,
    Json(input): Json<Identity>,
) -> Result<impl IntoResponse, ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(json!({"session":app.create_session(&input.id).await?})),
    ))
}
async fn start_turn(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Json(input): Json<Turn>,
) -> Result<impl IntoResponse, ApiError> {
    let state = app
        .start_session_turn(&id, &input.turn_id, &input.prompt)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":state.report.run_id,"revision":state.revision})),
    ))
}
async fn resume_session(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    app.resume_session(&id).await?;
    Ok(Json(json!({"accepted":true})))
}
async fn pause_session(State(app): State<Arc<Application>>, Path(id): Path<String>) -> ApiResult {
    app.pause_session(&id)?;
    Ok(Json(json!({"accepted":true})))
}
async fn cancel_session(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Json(actor): Json<Actor>,
) -> ApiResult {
    app.cancel_session(&id, &actor.actor, &actor.note).await?;
    Ok(Json(json!({"accepted":true})))
}
async fn abandon_session(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Json(actor): Json<Actor>,
) -> ApiResult {
    Ok(Json(
        json!({"session":app.abandon_session(&id,&actor.actor,&actor.note).await?}),
    ))
}
async fn decide_session(
    State(app): State<Arc<Application>>,
    Path((id, ticket)): Path<(String, String)>,
    Json(decision): Json<ApprovalDecision>,
) -> ApiResult {
    Ok(Json(
        json!({"state":app.decide_session(&id,&ticket,decision).await?}),
    ))
}
async fn reconcile_session(
    State(app): State<Arc<Application>>,
    Path((id, call)): Path<(String, String)>,
    Json(decision): Json<RecoveryDecision>,
) -> ApiResult {
    Ok(Json(
        json!({"state":app.reconcile_session(&id,&call,decision).await?}),
    ))
}
async fn knowledge(State(app): State<Arc<Application>>) -> ApiResult {
    let snapshot = app.corpus.snapshot().await?;
    let documents = snapshot
        .documents
        .values()
        .map(|d| json!({"id":d.id,"title":d.title,"source":d.source,"bytes":d.text.len()}))
        .collect::<Vec<_>>();
    Ok(Json(
        json!({"revision":snapshot.revision,"documents":documents,"chunks":snapshot.chunks.len()}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    document: Document,
    expected_revision: Option<u64>,
}
async fn import(State(app): State<Arc<Application>>, Json(input): Json<Import>) -> ApiResult {
    let snapshot = rag::ingest(
        app.corpus.as_ref(),
        input.document,
        &WindowChunker::default(),
        input.expected_revision,
    )
    .await?;
    Ok(Json(
        json!({"revision":snapshot.revision,"documents":snapshot.documents.len(),"chunks":snapshot.chunks.len()}),
    ))
}
#[derive(Deserialize)]
struct Revision {
    expected_revision: Option<u64>,
}
async fn remove(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Query(query): Query<Revision>,
) -> ApiResult {
    let snapshot = app.corpus.remove(&id, query.expected_revision).await?;
    Ok(Json(json!({"revision":snapshot.revision})))
}
async fn search(
    State(app): State<Arc<Application>>,
    Json(query): Json<RetrievalQuery>,
) -> ApiResult {
    Ok(Json(
        serde_json::to_value(rag::search(app.corpus.as_ref(), &LexicalRetriever, &query).await?)
            .map_err(super::super::store::storage)?,
    ))
}
