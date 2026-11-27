//! Optional local application host: shared durable control, HTTP API and CLI.
pub mod cli;
mod config;
mod diagnostics;
mod memory;
#[cfg(feature = "database")]
pub mod multiuser;
mod orchestration;
mod planning;
pub mod retrieval;
mod service;
mod skills;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::mcp::{McpClient, McpHttpConfig};
use super::rag::{
    Citation, CitedReport, FileCorpusStore, PdfLoader, RagTool, Retriever, SearchResult, WebClient,
    WebFetchTool, WebSearch, WebSearchTool,
};
use super::*;
use crate::CancelToken;
pub use config::{AppConfig, AppMcpEndpoint, AppProvider, AppResearch, AppRole};
pub use diagnostics::{diagnostic_snapshot, AppObservability, DiagnosticTarget};
pub use memory::{AppMemory, MemoryEdit, MemoryExtract, MemoryForget, MemoryReview};
pub use orchestration::{AppWorkflowLaunch, AppWorkflowPreview, AppWorkflowRequest};
pub use planning::{preview_configuration, AppConfigPreview, DeclaredToolGrant};
pub use retrieval::{AppEmbedding, AppRetrieval, RetrievalMode};
pub use service::{router, serve};
pub use skills::{AppSkill, AppTemplateRequest};

pub struct Application {
    pub config: AppConfig,
    pub runs: FileRunStore,
    pub corpus: Arc<FileCorpusStore>,
    pub workflows: workflow::FileWorkflowStore,
    pub sessions: FileSessionStore,
    pub memory: super::memory::FileMemoryStore,
    web: Option<Arc<WebClient>>,
    search: Option<Arc<WebSearch>>,
    pdf: Option<PdfLoader>,
    retriever: Arc<dyn Retriever>,
    model: Arc<dyn Model>,
    tools: ToolRegistry,
    policy: ToolPolicy,
    clients: Vec<McpClient>,
    jobs: Mutex<BTreeMap<String, Job>>,
    label: String,
    skills: Vec<skills::LoadedSkill>,
    skill_roles: Vec<AppRole>,
    skill_bindings: BTreeMap<String, super::skills::SkillBinding>,
    recovery_roles: std::collections::BTreeSet<String>,
    telemetry: Option<super::observability::OtlpHttpExporter>,
}
#[derive(Clone)]
struct Job {
    cancel: CancelToken,
    cancel_reason: Arc<CancelReason>,
    pause: CancelToken,
    active: bool,
    error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    pub active: bool,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub status: RunStatus,
    pub stop_reason: StopReason,
    pub revision: u64,
    pub steps: usize,
    pub tool_calls: usize,
    pub elapsed_ms: u64,
    pub job: JobStatus,
}

impl Application {
    pub async fn open(config: AppConfig) -> Result<Arc<Self>, RuntimeError> {
        config.validate()?;
        let model = config.model()?;
        Self::with_model(config, model, ToolRegistry::new(), ToolPolicy::default()).await
    }
    /// Host extension point: custom model/tools still use the same controller.
    pub async fn with_model(
        config: AppConfig,
        model: Arc<dyn Model>,
        tools: ToolRegistry,
        policy: ToolPolicy,
    ) -> Result<Arc<Self>, RuntimeError> {
        config.validate()?;
        let retriever = retrieval::configured(&config).await?;
        Self::with_retriever(config, model, tools, policy, retriever).await
    }
    /// The operator search API and model knowledge tool share this retriever.
    /// Build/load paid embedding indexes explicitly before opening the host.
    pub async fn with_retriever(
        config: AppConfig,
        model: Arc<dyn Model>,
        mut tools: ToolRegistry,
        mut policy: ToolPolicy,
        retriever: Arc<dyn Retriever>,
    ) -> Result<Arc<Self>, RuntimeError> {
        config.validate()?;
        let label = config.label()?;
        let corpus = Arc::new(FileCorpusStore::new(config.state_dir.join("knowledge")));
        let runs = FileRunStore::new(config.state_dir.join("runs"));
        let workflows = workflow::FileWorkflowStore::new(config.state_dir.join("workflows"));
        let sessions = FileSessionStore::new(config.state_dir.join("sessions"));
        let memory = super::memory::FileMemoryStore::new(config.state_dir.join("memory"));
        if config.knowledge {
            tools.register(RagTool::new(
                "knowledge_search",
                corpus.clone(),
                retriever.clone(),
            ))?;
            policy = policy.allow("knowledge_search").grant("knowledge:read");
        }
        let web = config
            .research
            .web
            .clone()
            .map(WebClient::new)
            .transpose()?
            .map(Arc::new);
        let search = config
            .research
            .search
            .clone()
            .map(|cfg| {
                let key = config::secret(&cfg.key_env)?;
                WebSearch::new(cfg, &key).map(Arc::new)
            })
            .transpose()?;
        let pdf = config
            .research
            .pdf
            .clone()
            .map(PdfLoader::new)
            .transpose()?;
        if let Some(config) = &config.sandbox {
            let sandbox = super::sandbox::DockerSandbox::new(config.clone())?;
            tools.register(super::sandbox::SandboxTool(Arc::new(sandbox)))?;
            // Allowed here so workflow roles can list it; `agent()` withholds
            // it from top-level runs and sessions unless sandbox_top_level.
            policy = policy.allow("python_sandbox").grant("sandbox:python");
        }
        if let Some(web) = &web {
            tools.register(WebFetchTool(web.clone()))?;
            policy = policy.allow("web_fetch").grant("web:fetch");
        }
        if let Some(search) = &search {
            tools.register(WebSearchTool(search.clone()))?;
            policy = policy.allow("web_search").grant("web:search");
        }
        let mut clients = Vec::new();
        for endpoint in &config.mcp {
            let mut mcp = McpHttpConfig::new(&endpoint.endpoint);
            mcp.namespace = endpoint.namespace.clone();
            mcp.bearer_token = endpoint
                .token_env
                .as_deref()
                .map(config::secret)
                .transpose()?;
            let client = McpClient::connect(mcp).await?;
            for binding in &endpoint.bindings {
                tools.register(client.bind(binding.clone())?)?;
                policy = policy.allow(&binding.name);
                for permission in &binding.permissions {
                    policy = policy.grant(permission);
                }
            }
            clients.push(client);
        }
        for role in &config.roles {
            for name in &role.tools {
                if !tools.definition(name).is_some_and(|d| policy.permits(d)) {
                    return Err(RuntimeError::Permission(format!(
                        "role {} requests unavailable tool {name}",
                        role.name
                    )));
                }
            }
        }
        let compiled = skills::load(&config, &tools, &policy).await?;
        let telemetry = config
            .observability
            .as_ref()
            .and_then(|c| c.otlp.clone())
            .map(super::observability::OtlpHttpExporter::new)
            .transpose()?;
        let app = Arc::new(Self {
            config,
            runs,
            corpus,
            workflows,
            sessions,
            memory,
            web,
            search,
            pdf,
            retriever,
            model,
            tools,
            policy,
            clients,
            jobs: Mutex::new(BTreeMap::new()),
            label,
            skills: compiled.skills,
            skill_roles: compiled.roles,
            skill_bindings: compiled.bindings,
            recovery_roles: compiled.recovery_roles,
            telemetry,
        });
        // Validate output schemas before the server begins accepting requests.
        let roles = app.role_agents();
        app.agent().validate_output_policy()?;
        roles.tasks(&app.runs)?;
        Ok(app)
    }
    /// Top-level runs and session turns. Workflow roles replace this policy
    /// with their own role-filtered copy of the full application policy.
    fn agent(&self) -> AgentLoop<'_> {
        let mut policy = self.policy.clone();
        if self.config.sandbox.is_some() && !self.config.sandbox_top_level {
            policy.allowed_tools.remove("python_sandbox");
        }
        let agent = AgentLoop::new(self.model.as_ref(), &self.tools, &self.config.model)
            .with_policy(policy)
            .with_limits(self.config.limits.clone())
            .with_context(self.config.context.clone())
            .with_output_tokens(self.config.output_tokens)
            .with_label(&self.label);
        if let Some(policy) = &self.config.final_output {
            agent.with_output_policy(policy.clone())
        } else {
            agent
        }
    }
    fn jobs(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Job>>, RuntimeError> {
        self.jobs
            .lock()
            .map_err(|_| RuntimeError::Storage("application job controller panicked".into()))
    }
    pub fn job(&self, id: &str) -> Result<JobStatus, RuntimeError> {
        self.job_key(&format!("run:{id}"))
    }
    fn job_key(&self, key: &str) -> Result<JobStatus, RuntimeError> {
        Ok(self.jobs()?.get(key).map_or(
            JobStatus {
                active: false,
                error: None,
            },
            |job| JobStatus {
                active: job.active,
                error: job.error.clone(),
            },
        ))
    }
    fn admit(&self, id: &str) -> Result<Job, RuntimeError> {
        valid_id(id)?;
        self.admit_key(&format!("run:{id}"))
    }
    fn admit_key(&self, id: &str) -> Result<Job, RuntimeError> {
        let mut jobs = self.jobs()?;
        if jobs.get(id).is_some_and(|job| job.active) {
            return Err(RuntimeError::Conflict(
                "run has an active local worker".into(),
            ));
        }
        if jobs.values().filter(|job| job.active).count() >= self.config.max_active_runs {
            return Err(RuntimeError::Conflict(
                "application run capacity reached".into(),
            ));
        }
        if jobs.len() >= 1024 {
            let old = jobs
                .iter()
                .find(|(_, job)| !job.active)
                .map(|(id, _)| id.clone());
            if let Some(id) = old {
                jobs.remove(&id);
            }
        }
        let job = Job {
            cancel: CancelToken::new(),
            cancel_reason: Arc::new(CancelReason::new()),
            pause: CancelToken::new(),
            active: true,
            error: None,
        };
        jobs.insert(id.into(), job.clone());
        Ok(job)
    }
    /// Persists a new run before returning; execution is asynchronous.
    pub async fn start(self: &Arc<Self>, id: &str, prompt: &str) -> Result<RunState, RuntimeError> {
        if prompt.trim().is_empty() || prompt.len() > 128 * 1024 {
            return Err(config::config_error("prompt must be 1..=131072 bytes"));
        }
        let job = self.admit(id)?;
        let created = DurableAgent::new(&self.agent(), &self.runs)
            .create(
                id,
                vec![Message::system(&self.config.system), Message::user(prompt)],
            )
            .await;
        match created {
            Ok(state) => {
                self.spawn(id, job);
                Ok(state)
            }
            Err(error) => {
                self.jobs()?.remove(&format!("run:{id}"));
                Err(error)
            }
        }
    }
    pub async fn resume(self: &Arc<Self>, id: &str) -> Result<(), RuntimeError> {
        valid_id(id)?;
        if self.runs.inspect(id).await?.is_none() {
            return Err(RuntimeError::Conflict("run does not exist".into()));
        }
        let job = self.admit(id)?;
        self.spawn(id, job);
        Ok(())
    }
    fn spawn(self: &Arc<Self>, id: &str, job: Job) {
        let app = Arc::clone(self);
        let id = id.to_string();
        tokio::spawn(async move {
            let agent = app.agent().with_cancel(&job.cancel);
            let durable = DurableAgent::new(&agent, &app.runs)
                .with_pause(&job.pause)
                .with_cancel_reason(&job.cancel_reason);
            let result = std::panic::AssertUnwindSafe(durable.resume(&id))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(RuntimeError::Storage(
                        "worker panicked; inspect/recover its persisted state".into(),
                    ))
                });
            if let Ok(mut jobs) = app.jobs() {
                if let Some(job) = jobs.get_mut(&format!("run:{id}")) {
                    job.active = false;
                    job.error = result.err().map(|e| e.to_string());
                }
            }
        });
    }
    pub async fn inspect(&self, id: &str) -> Result<RunState, RuntimeError> {
        valid_read_id(id)?;
        self.runs
            .inspect(id)
            .await?
            .ok_or_else(|| RuntimeError::Conflict("run does not exist".into()))
    }
    pub fn pause(&self, id: &str) -> Result<(), RuntimeError> {
        let jobs = self.jobs()?;
        let job = jobs
            .get(&format!("run:{id}"))
            .filter(|job| job.active)
            .ok_or_else(|| RuntimeError::Conflict("run has no active local worker".into()))?;
        job.pause.cancel();
        Ok(())
    }
    pub async fn cancel(&self, id: &str, actor: &str, note: &str) -> Result<(), RuntimeError> {
        valid_id(id)?;
        if actor.trim().is_empty() || actor.len() > 256 || note.len() > 4096 {
            return Err(config::config_error("invalid cancellation actor or note"));
        }
        {
            let jobs = self.jobs()?;
            if let Some(job) = jobs.get(&format!("run:{id}")).filter(|job| job.active) {
                // Record who and why before the worker observes the token.
                job.cancel_reason.record(actor, note)?;
                job.cancel.cancel();
                return Ok(());
            }
        }
        DurableAgent::new(&self.agent(), &self.runs)
            .cancel(id, actor, note)
            .await?;
        Ok(())
    }
    pub async fn decide(
        &self,
        id: &str,
        ticket: &str,
        decision: ApprovalDecision,
    ) -> Result<RunState, RuntimeError> {
        valid_id(id)?;
        DurableAgent::new(&self.agent(), &self.runs)
            .decide(id, ticket, decision)
            .await
    }
    pub async fn reconcile(
        &self,
        id: &str,
        call_id: &str,
        decision: RecoveryDecision,
    ) -> Result<RunState, RuntimeError> {
        valid_id(id)?;
        DurableAgent::new(&self.agent(), &self.runs)
            .reconcile(id, call_id, decision)
            .await
    }
    /// Reconcile reports against their saved tool evidence, not today's corpus.
    pub async fn verified_report(
        &self,
        id: &str,
    ) -> Result<super::rag::VerifiedReport, RuntimeError> {
        verify_report(&self.inspect(id).await?)
    }
    pub async fn list(&self) -> Result<Vec<RunSummary>, RuntimeError> {
        let mut summaries = list_persisted(&self.runs).await?;
        for summary in &mut summaries {
            summary.job = self.job(&summary.id)?;
        }
        Ok(summaries)
    }
    pub async fn shutdown(&self, grace: Duration) -> Result<(), RuntimeError> {
        {
            for job in self.jobs()?.values().filter(|job| job.active) {
                job.cancel.cancel();
            }
        }
        let until = tokio::time::Instant::now() + grace;
        while self.jobs()?.values().any(|job| job.active) && tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for client in &self.clients {
            client.close().await?;
        }
        if let Some(exporter) = &self.telemetry {
            exporter.shutdown().await?;
        }
        Ok(())
    }
}

/// Verify archived source evidence without opening provider/MCP connections.
pub fn verify_report(state: &RunState) -> Result<super::rag::VerifiedReport, RuntimeError> {
    state.validate()?;
    let text = state
        .report
        .final_text()
        .ok_or_else(|| config::config_error("run has no completed report"))?;
    if let Some(policy) = &state.config.output_policy {
        if policy.citation_tools().is_some() {
            return policy
                .verified_citations(&state.report)
                .map_err(|e| config::config_error(&e));
        }
    }
    let report: CitedReport = serde_json::from_str(text)
        .map_err(|_| config::config_error("final answer is not a CitedReport"))?;
    let eligible = state
        .tool_steps
        .iter()
        .filter(|step| step.definition.name == "knowledge_search")
        .map(|step| step.call.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let evidence = state
        .report
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool { result }
                if !result.is_error && eligible.contains(result.call_id.as_str()) =>
            {
                serde_json::from_value::<SearchResult>(result.output.clone()).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        return Err(config::config_error("run has no saved knowledge evidence"));
    }
    SearchResult::verify_all(&evidence, report)
}
/// Add explicit rendering provenance for located reports. Legacy reports keep
/// their existing JSON shape; model text is never replaced in the run journal.
pub fn verify_report_with_provenance(state: &RunState) -> Result<serde_json::Value, RuntimeError> {
    let mut value = serde_json::to_value(verify_report(state)?).map_err(super::store::storage)?;
    if let Some(policy) = &state.config.output_policy {
        if matches!(policy, FinalOutputPolicy::LocatedCitedReport { .. }) {
            let resolved = policy
                .resolve_located(&state.report)
                .map_err(|e| config::config_error(&e))?;
            value["rendering"] = json!({"type":"verified_source_locators","original":resolved.original,"mappings":resolved.mappings,"model_transcript_unchanged":true});
        }
    }
    Ok(value)
}
/// Bounded local listing without opening provider/MCP connections.
pub async fn list_persisted(runs: &FileRunStore) -> Result<Vec<RunSummary>, RuntimeError> {
    let root = runs.root().to_path_buf();
    let mut summaries = super::store::blocking(move || {
        let mut states = Vec::new();
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(states),
            Err(e) => return Err(super::store::storage(e)),
        };
        for entry in entries.take(20_000) {
            let path = entry.map_err(super::store::storage)?.path();
            let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if name.len() != 69
                || !name.ends_with(".json")
                || !name.as_bytes()[..64].iter().all(u8::is_ascii_hexdigit)
            {
                continue;
            }
            if let Some(state) = super::store::read_json::<RunState>(&path, 64 * 1024 * 1024)? {
                state.validate()?;
                use sha2::{Digest, Sha256};
                if hex::encode(Sha256::digest(state.report.run_id.as_bytes())) != name[..64] {
                    return Err(super::store::storage(
                        "listed checkpoint identity differs from filename",
                    ));
                }
                states.push(RunSummary {
                    id: state.report.run_id,
                    status: state.status,
                    stop_reason: state.report.stop_reason,
                    revision: state.revision,
                    steps: state.report.steps,
                    tool_calls: state.report.tool_calls,
                    elapsed_ms: state.report.elapsed_ms,
                    job: JobStatus {
                        active: false,
                        error: None,
                    },
                });
            }
            if states.len() >= 1000 {
                break;
            }
        }
        Ok(states)
    })
    .await?;
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(summaries)
}
fn valid_id(id: &str) -> Result<(), RuntimeError> {
    valid_identifier(id, 64)
}
fn valid_read_id(id: &str) -> Result<(), RuntimeError> {
    valid_identifier(id, 128)
}
fn valid_identifier(id: &str, max: usize) -> Result<(), RuntimeError> {
    if id.is_empty()
        || id.len() > max
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Err(config::config_error(&format!(
            "ID must contain 1..={max} ASCII letters, digits, hyphens or underscores"
        )))
    } else {
        Ok(())
    }
}

/// Deliberately deterministic demonstration, never represented as a live LLM.
struct OfflineModel;
impl Model for OfflineModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            let evidence = request
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::Tool { result } => Some(result),
                    _ => None,
                });
            let turn = if let Some(evidence) = evidence {
                let found: SearchResult =
                    serde_json::from_value(evidence.output.clone()).map_err(|_| {
                        RuntimeError::Protocol("offline demo expected knowledge results".into())
                    })?;
                if found.hits.is_empty() {
                    AssistantTurn::text("Offline demo: no matching knowledge evidence. Import a text document and ask using its terms.")
                } else {
                    let mut lines = Vec::new();
                    let mut citations = Vec::new();
                    for hit in found.hits {
                        lines.push(format!("{} [[{}]]", hit.chunk.text, hit.chunk.id));
                        citations.push(Citation {
                            chunk_id: hit.chunk.id,
                            quote: hit.chunk.text,
                        });
                    }
                    AssistantTurn::text(
                        serde_json::to_string(&CitedReport {
                            text: format!("Offline evidence excerpts:\n{}", lines.join("\n")),
                            citations,
                        })
                        .map_err(super::store::storage)?,
                    )
                }
            } else if request
                .tools
                .iter()
                .any(|tool| tool.name == "knowledge_search")
            {
                let query = request
                    .messages
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        Message::User { content } => Some(content.as_str()),
                        _ => None,
                    })
                    .unwrap_or("knowledge");
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "knowledge".into(),
                        name: "knowledge_search".into(),
                        arguments: json!({"query":query.chars().take(512).collect::<String>()}),
                    }],
                }
            } else {
                AssistantTurn::text(
                    "Offline demo completed. Configure a provider for real model answers.",
                )
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
