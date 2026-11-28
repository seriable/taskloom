use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::workflow::WorkflowOutput;
use super::super::{mcp::McpToolBinding, ContextPolicy, Model, RunLimits, RuntimeError};
use crate::gateway::{ModelPrice, PriceTable};
use crate::{Gateway, GatewayConfig, GatewayTransport};

/// Public configuration stores environment variable names, never key values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppProvider {
    Offline,
    Openai {
        base_url: String,
        key_env: String,
        namespace: String,
    },
    Anthropic {
        base_url: String,
        key_env: String,
        namespace: String,
    },
    Native {
        protocol: crate::native_providers::NativeProtocol,
        base_url: String,
        key_env: String,
        namespace: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppMcpEndpoint {
    pub endpoint: String,
    pub token_env: Option<String>,
    pub namespace: String,
    pub bindings: Vec<McpToolBinding>,
}
/// Explicit host-owned subprocess declaration. Values of inherited environment
/// variables are resolved only at startup and never serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppMcpStdio {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: std::collections::BTreeSet<String>,
    pub namespace: String,
    pub bindings: Vec<McpToolBinding>,
}
impl AppMcpStdio {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if !self.program.is_absolute()
            || !self.cwd.is_absolute()
            || self.namespace.trim().is_empty()
            || self.namespace.len() > 256
            || self.args.len() > 64
            || self.args.iter().any(|a| a.len() > 8192 || a.contains('\0'))
            || self.environment.len() > 64
            || self.bindings.len() > 256
            || self.environment.iter().any(|n| {
                n.is_empty()
                    || n.len() > 256
                    || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
        {
            return Err(config_error(
                "invalid explicit MCP subprocess paths, namespace, arguments or environment names",
            ));
        }
        Ok(())
    }
    pub(super) fn process_config(&self) -> crate::runtime::mcp::McpStdioConfig {
        let mut config = crate::runtime::mcp::McpStdioConfig::new(&self.program, &self.cwd);
        config.args = self.args.clone();
        config.environment = self.environment.clone();
        config.namespace = self.namespace.clone();
        config
    }
}
/// Host-owned role contract. Requests can select registered roles but cannot
/// supply tools, system instructions or grants for a role.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRole {
    pub name: String,
    pub system: String,
    pub output: WorkflowOutput,
    #[serde(default)]
    pub tools: std::collections::BTreeSet<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppResearch {
    pub web: Option<super::super::rag::WebAccess>,
    pub search: Option<super::super::rag::WebSearchConfig>,
    pub pdf: Option<super::super::rag::PdfConfig>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub state_dir: PathBuf,
    /// Explicit SQL persistence; credentials remain in the named host env.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<super::AppDatabase>,
    pub provider: AppProvider,
    pub model: String,
    pub system: String,
    pub output_tokens: u32,
    /// Explicit OpenAI-compatible extension; absent keeps the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,
    pub limits: RunLimits,
    pub context: ContextPolicy,
    /// Optional top-level/session output contract. Workflow roles declare
    /// their own contracts through `WorkflowOutput::Validated`.
    pub final_output: Option<super::super::FinalOutputPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observability: Option<super::AppObservability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<super::AppMemory>,
    pub prices: Vec<ModelPrice>,
    pub knowledge: bool,
    pub mcp: Vec<AppMcpEndpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_stdio: Vec<AppMcpStdio>,
    pub max_active_runs: usize,
    pub roles: Vec<AppRole>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<super::AppSkill>,
    /// Explicit host-owned signed publisher/rollback ledger; absent retains
    /// legacy unsigned local package installation and activation behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_registry: Option<PathBuf>,
    pub research: AppResearch,
    pub retrieval: super::retrieval::AppRetrieval,
    pub sandbox: Option<super::super::sandbox::DockerSandboxConfig>,
    /// Also offer `python_sandbox` to top-level runs and session turns.
    /// Default false: only workflow roles that list the tool receive it.
    pub sandbox_top_level: bool,
}
impl Default for AppConfig {
    fn default() -> Self {
        Self {
        state_dir:".taskloom".into(),database:None,provider:AppProvider::Offline,model:"offline-demo".into(),
        system:format!("{}\nReturn a JSON CitedReport with text and citations [{{chunk_id,quote}}]. Use [[full_chunk_id]] references for source assertions and exact source quotes. If evidence is missing, say so within the scope of the supplied evidence.", include_str!("research-guidance.txt")),
        output_tokens:4096,enable_thinking:None,limits:RunLimits::default(),context:ContextPolicy::default(),final_output:None,observability:None,memory:None,prices:Vec::new(),knowledge:true,mcp:Vec::new(),mcp_stdio:Vec::new(),max_active_runs:4,roles:Vec::new(),skills:Vec::new(),skill_registry:None,research:AppResearch::default(),retrieval:super::retrieval::AppRetrieval::default(),sandbox:None,sandbox_top_level:false,
    }
    }
}
impl AppConfig {
    /// Resolve state_dir relative to this config file, not the caller's cwd.
    pub async fn load(path: &Path) -> Result<Self, RuntimeError> {
        let path = std::fs::canonicalize(path).map_err(super::super::store::storage)?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(super::super::store::storage)?;
        if bytes.len() > 1024 * 1024 {
            return Err(config_error("configuration exceeds 1 MiB"));
        }
        let mut config: Self =
            serde_json::from_slice(&bytes).map_err(|e| config_error(&e.to_string()))?;
        if config.state_dir.is_relative() {
            config.state_dir = path.parent().unwrap().join(&config.state_dir);
        }
        if let Some(root) = &mut config.skill_registry {
            if root.is_relative() {
                *root = path.parent().unwrap().join(&*root);
            }
        }
        if let Some(pdf) = &mut config.research.pdf {
            if pdf.executable.is_relative() {
                pdf.executable = path.parent().unwrap().join(&pdf.executable);
            }
        }
        if let Some(sandbox) = &mut config.sandbox {
            if sandbox.executable.is_relative() {
                sandbox.executable = path.parent().unwrap().join(&sandbox.executable);
            }
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if let Some(root) = &self.skill_registry {
            if !cfg!(feature = "skill-registry")
                || !root.is_absolute()
                || root.as_os_str().is_empty()
            {
                return Err(config_error("signed skill activation requires skill-registry feature and an explicit absolute ledger root"));
            }
        }
        if self.mcp_stdio.len() > 16 {
            return Err(config_error("at most sixteen MCP subprocesses"));
        }
        for process in &self.mcp_stdio {
            process.validate()?;
        }
        if self.enable_thinking.is_some() && !matches!(self.provider, AppProvider::Openai { .. }) {
            return Err(config_error(
                "enable_thinking requires an explicitly configured OpenAI-compatible provider",
            ));
        }
        if let Some(database) = &self.database {
            database.validate()?;
            if self.memory.as_ref().is_some_and(|m| {
                m.scope.tenant != database.tenant || m.scope.project != database.project
            }) {
                return Err(config_error(
                    "memory tenant/project must match configured SQL namespace",
                ));
            }
        }
        if let Some(config) = &self.observability {
            config.validate()?;
        }
        if self.skills.len() > 8 {
            return Err(config_error("at most eight skill mounts may be configured"));
        }
        let mut mounts = std::collections::BTreeSet::new();
        for skill in &self.skills {
            skill.validate()?;
            if !mounts.insert(&skill.mount) {
                return Err(config_error("duplicate skill mount"));
            }
        }
        if let Some(memory) = &self.memory {
            memory.validate()?;
        }
        if let Some(policy) = &self.final_output {
            policy.validate()?;
            if policy.max_repairs() > 0 && self.limits.max_cost_micros.is_none() {
                return Err(config_error(
                    "final-output repair requires an explicit model-spend cap",
                ));
            }
        }
        self.retrieval.validate()?;
        if let Some(sandbox) = &self.sandbox {
            sandbox.validate()?;
        } else if self.sandbox_top_level {
            return Err(config_error(
                "sandbox_top_level requires a sandbox configuration",
            ));
        }
        if let Some(web) = &self.research.web {
            web.validate()?;
        }
        if let Some(search) = &self.research.search {
            search.access.validate()?;
        }
        if let Some(pdf) = &self.research.pdf {
            pdf.validate()?;
        }
        let mut names = std::collections::BTreeSet::new();
        if self.roles.len() > 32 {
            return Err(config_error("at most 32 workflow roles may be configured"));
        }
        for role in &self.roles {
            super::valid_id(&role.name)?;
            if !names.insert(&role.name) || role.system.len() > 128 * 1024 || role.tools.len() > 256
            {
                return Err(config_error("invalid or duplicate workflow role"));
            }
        }
        if self.model.trim().is_empty()
            || self.model.len() > 256
            || self.system.len() > 128 * 1024
            || self.output_tokens == 0
            || self.max_active_runs == 0
            || self.max_active_runs > 128
            || self.mcp.len() > 16
            || self.prices.len() > 1024
            || self.prices.iter().any(|p| {
                p.prefix.is_empty()
                    || !p.input_per_million.is_finite()
                    || !p.output_per_million.is_finite()
                    || p.input_per_million < 0.0
                    || p.output_per_million < 0.0
            })
        {
            return Err(config_error("invalid model, prompts, prices or capacity"));
        }
        self.validate_provider()
    }
    pub(super) fn validate_provider(&self) -> Result<(), RuntimeError> {
        match &self.provider {
            AppProvider::Offline => {}
            AppProvider::Native {
                protocol,
                base_url,
                key_env,
                namespace,
            } => {
                crate::native_providers::NativeProvider::validate_model_endpoint(
                    *protocol,
                    base_url,
                    &self.model,
                )
                .map_err(|e| config_error(e.detail()))?;
                if key_env.is_empty() || namespace.trim().is_empty() {
                    return Err(config_error(
                        "native provider requires a credential environment name and namespace",
                    ));
                }
            }
            AppProvider::Openai {
                base_url,
                key_env,
                namespace,
            }
            | AppProvider::Anthropic {
                base_url,
                key_env,
                namespace,
            } => {
                let endpoint = reqwest::Url::parse(base_url)
                    .map_err(|_| config_error("invalid provider endpoint"))?;
                if !matches!(endpoint.scheme(), "http" | "https")
                    || !endpoint.username().is_empty()
                    || endpoint.password().is_some()
                    || endpoint.fragment().is_some()
                    || key_env.is_empty()
                    || namespace.trim().is_empty()
                {
                    return Err(config_error(
                        "invalid provider endpoint/key environment/namespace",
                    ));
                }
                if endpoint.scheme() == "http" && !loopback(&endpoint) {
                    return Err(config_error(
                        "provider base_url must use https; http is allowed only for localhost, 127.0.0.0/8 or ::1 because the API key would be sent in cleartext",
                    ));
                }
            }
        }
        Ok(())
    }
    pub(super) fn label(&self) -> Result<String, RuntimeError> {
        // Bind provider, tenant, pricing and system contract to durable reentry.
        let bytes = serde_json::to_vec(&(&self.provider, &self.prices, &self.system))
            .map_err(super::super::store::storage)?;
        let legacy = format!("app-{}", hex::encode(Sha256::digest(bytes)));
        Ok(match self.enable_thinking {
            Some(enabled) => format!("{legacy}|enable_thinking={enabled}"),
            None => legacy,
        })
    }
    pub(super) fn model(&self) -> Result<Arc<dyn Model>, RuntimeError> {
        let gateway = Arc::new(Gateway::new(GatewayConfig {
            prices: PriceTable::new(self.prices.clone()),
            ..Default::default()
        }));
        Ok(match &self.provider {
            AppProvider::Offline => Arc::new(super::OfflineModel),
            AppProvider::Native {
                protocol,
                base_url,
                key_env,
                namespace,
            } => Arc::new(
                GatewayTransport::new(
                    gateway,
                    crate::native_providers::NativeProvider::new(
                        *protocol,
                        base_url,
                        secret(key_env)?,
                    )
                    .map_err(|e| config_error(e.detail()))?,
                )
                .with_cache_namespace(namespace),
            ),
            AppProvider::Openai {
                base_url,
                key_env,
                namespace,
            } => Arc::new(
                GatewayTransport::new(
                    gateway,
                    crate::openai::OpenAiCompatible::new(base_url, secret(key_env)?)
                        .with_enable_thinking(self.enable_thinking),
                )
                .with_cache_namespace(namespace),
            ),
            AppProvider::Anthropic {
                base_url,
                key_env,
                namespace,
            } => Arc::new(
                GatewayTransport::new(
                    gateway,
                    crate::anthropic::Anthropic::with_base_url(base_url, secret(key_env)?),
                )
                .with_cache_namespace(namespace),
            ),
        })
    }
}
/// Loopback hosts where a cleartext provider URL cannot leave the machine.
fn loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}
pub(super) fn secret(name: &str) -> Result<String, RuntimeError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error(&format!("missing environment variable {name}")))
}
pub(super) fn config_error(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(detail.into())
}
