//! Pure configuration review. Never resolves a secret or opens a service/store.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{config::config_error, AppConfig, AppProvider};
use crate::gateway::PriceTable;
use crate::runtime::{
    state::fingerprint, workflow::WorkflowOutput, FinalOutputPolicy, RuntimeError, ToolEffect,
};

#[derive(Debug, Clone, Serialize)]
pub struct DeclaredToolGrant {
    pub name: String,
    pub effect: ToolEffect,
    pub permissions: BTreeSet<String>,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppConfigPreview {
    pub version: u32,
    pub configuration: AppConfig,
    pub configuration_key: String,
    pub credential_names: BTreeSet<String>,
    pub declared_tools: Vec<DeclaredToolGrant>,
    pub deferred_checks: Vec<String>,
    pub services_contacted: bool,
    pub configuration_applied: bool,
}

/// Static validation for a new host configuration. Remote schemas, credentials,
/// skill content, frozen indexes and local executables require startup checks;
/// this result never grants or activates a proposed capability in a live host.
pub fn preview_configuration(config: AppConfig) -> Result<AppConfigPreview, RuntimeError> {
    config.validate()?;
    config.limits.validate()?;
    config.context.validate()?;
    if config.state_dir.as_os_str().is_empty() {
        return Err(config_error("state directory must be explicit"));
    }
    let mut credentials = BTreeSet::new();
    if let Some(database) = &config.database {
        credential(&database.url_env, &mut credentials)?;
    }
    provider(&config.provider, &mut credentials)?;
    if let Some(rerank) = &config.retrieval.http_rerank {
        credential(&rerank.provider.key_env, &mut credentials)?;
    }
    if !matches!(config.provider, AppProvider::Offline)
        && (config.limits.max_cost_micros.is_none()
            || PriceTable::new(config.prices.clone())
                .price(&config.model)
                .is_none())
    {
        return Err(config_error(
            "new model configuration requires a matching price and explicit generation spending cap",
        ));
    }
    let mut tools = BTreeMap::new();
    let mut declare =
        |name: &str, effect: ToolEffect, permissions: BTreeSet<String>, origin: &str| {
            super::valid_id(name)?;
            if permissions.len() > 128
                || permissions
                    .iter()
                    .any(|p| p.trim().is_empty() || p.len() > 512)
            {
                return Err(config_error("invalid declared tool permissions"));
            }
            if tools
                .insert(
                    name.to_string(),
                    DeclaredToolGrant {
                        name: name.into(),
                        effect,
                        permissions,
                        origin: origin.into(),
                    },
                )
                .is_some()
            {
                return Err(config_error("duplicate configured tool name"));
            }
            Ok(())
        };
    if config.knowledge {
        declare(
            "knowledge_search",
            ToolEffect::ReadOnly,
            BTreeSet::from(["knowledge:read".into()]),
            "local knowledge",
        )?;
    }
    if config.research.web.is_some() {
        declare(
            "web_fetch",
            ToolEffect::ReadOnly,
            BTreeSet::from(["web:fetch".into()]),
            "configured web origins",
        )?;
    }
    if let Some(search) = &config.research.search {
        credential(&search.key_env, &mut credentials)?;
        declare(
            "web_search",
            ToolEffect::ReadOnly,
            BTreeSet::from(["web:search".into()]),
            "configured search service",
        )?;
    }
    if config.sandbox.is_some() {
        declare(
            "python_sandbox",
            ToolEffect::NonIdempotent,
            BTreeSet::from(["sandbox:python".into()]),
            "configured isolated sandbox",
        )?;
    }
    for endpoint in &config.mcp {
        safe_endpoint(&endpoint.endpoint)?;
        if endpoint.namespace.trim().is_empty()
            || endpoint.namespace.len() > 256
            || endpoint.bindings.len() > 256
        {
            return Err(config_error("invalid MCP namespace or binding count"));
        }
        if let Some(name) = &endpoint.token_env {
            credential(name, &mut credentials)?;
        }
        for binding in &endpoint.bindings {
            if binding.remote_name.trim().is_empty()
                || binding.remote_name.len() > 256
                || binding
                    .description
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty() || s.len() > 16 * 1024)
            {
                return Err(config_error("invalid MCP remote name or host description"));
            }
            declare(
                &binding.name,
                binding.effect,
                binding.permissions.clone(),
                "MCP declaration; discovery deferred",
            )?;
        }
    }
    for process in &config.mcp_stdio {
        process.validate()?;
        for name in &process.environment {
            credential(name, &mut credentials)?;
        }
        for binding in &process.bindings {
            if binding.remote_name.trim().is_empty() || binding.remote_name.len() > 256 {
                return Err(config_error("invalid subprocess MCP remote tool name"));
            }
            declare(
                &binding.name,
                binding.effect,
                binding.permissions.clone(),
                "explicit MCP subprocess; startup deferred",
            )?;
        }
    }
    if tools.len() > 256 {
        return Err(config_error(
            "at most 256 tool declarations may be configured",
        ));
    }
    for skill in &config.skills {
        for name in &skill.grant.credentials {
            credential(name, &mut credentials)?;
        }
    }
    if let Some(embedding) = &config.retrieval.embedding {
        provider(&embedding.provider, &mut credentials)?;
    }
    #[cfg(feature = "qdrant")]
    if let Some(name) = config
        .retrieval
        .qdrant
        .as_ref()
        .and_then(|target| target.api_key_env.as_ref())
    {
        credential(name, &mut credentials)?;
    }
    if let Some(name) = config
        .observability
        .as_ref()
        .and_then(|o| o.otlp.as_ref())
        .and_then(|o| o.bearer_env.as_ref())
    {
        credential(name, &mut credentials)?;
    }
    for role in &config.roles {
        if role.tools.iter().any(|name| !tools.contains_key(name)) {
            return Err(config_error(&format!(
                "role {} requests an undeclared tool",
                role.name
            )));
        }
        match &role.output {
            WorkflowOutput::Json { schema } => {
                if serde_json::to_vec(schema)
                    .map_err(crate::runtime::store::storage)?
                    .len()
                    > 128 * 1024
                {
                    return Err(config_error("role output schema exceeds 128 KiB"));
                }
                jsonschema::validator_for(schema)
                    .map_err(|e| config_error(&format!("invalid role schema: {e}")))?;
            }
            WorkflowOutput::Validated { policy } => {
                output_policy(policy, &role.tools, &tools, &config)?
            }
            WorkflowOutput::Text => {}
        }
    }
    if let Some(policy) = &config.final_output {
        let allowed = tools
            .keys()
            .filter(|name| config.sandbox_top_level || name.as_str() != "python_sandbox")
            .cloned()
            .collect();
        output_policy(policy, &allowed, &tools, &config)?;
    }
    Ok(AppConfigPreview {
        version:1, configuration_key:fingerprint(&config)?, configuration:config,
        credential_names:credentials, declared_tools:tools.into_values().collect(),
        deferred_checks:vec![
            "Credential availability is not read during static review; configure the named environments on the host.".into(),
            "Model/MCP/search/embedding endpoints, remote schemas, indexes, installed skill content and local executables are not contacted or loaded.".into(),
            "Save the reviewed configuration and restart explicitly. Live roles, grants, tools and frozen runs are unchanged by review.".into(),
            "Generation prices/caps exclude embedding and external tool-service charges; configure those boundaries separately.".into(),
        ], services_contacted:false, configuration_applied:false,
    })
}

fn output_policy(
    policy: &FinalOutputPolicy,
    allowed: &BTreeSet<String>,
    tools: &BTreeMap<String, DeclaredToolGrant>,
    config: &AppConfig,
) -> Result<(), RuntimeError> {
    policy.validate()?;
    if policy.max_repairs() > 0 && config.limits.max_cost_micros.is_none() {
        return Err(config_error(
            "output repairs require an explicit generation spending cap",
        ));
    }
    if policy.citation_tools().is_some_and(|names| {
        names.iter().any(|name| {
            !allowed.contains(name)
                || tools
                    .get(name)
                    .is_none_or(|tool| tool.effect != ToolEffect::ReadOnly)
        })
    }) {
        return Err(config_error(
            "citation validation requires permitted read-only retrieval declarations",
        ));
    }
    Ok(())
}
fn credential(name: &str, names: &mut BTreeSet<String>) -> Result<(), RuntimeError> {
    if name.is_empty()
        || name.len() > 128
        || !name.as_bytes()[0].is_ascii_alphabetic() && name.as_bytes()[0] != b'_'
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(config_error(
            "use a credential environment variable name, never a key value",
        ));
    }
    names.insert(name.into());
    Ok(())
}
fn provider(provider: &AppProvider, names: &mut BTreeSet<String>) -> Result<(), RuntimeError> {
    match provider {
        AppProvider::Offline => Ok(()),
        AppProvider::Native {
            protocol,
            base_url,
            key_env,
            namespace,
        } => {
            crate::native_providers::NativeProvider::validate_endpoint(*protocol, base_url)
                .map_err(|e| config_error(e.detail()))?;
            if namespace.trim().is_empty() || namespace.len() > 256 {
                return Err(config_error("invalid provider namespace"));
            }
            credential(key_env, names)
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
            safe_endpoint(base_url)?;
            if namespace.trim().is_empty() || namespace.len() > 256 {
                return Err(config_error("invalid provider namespace"));
            }
            credential(key_env, names)
        }
    }
}
fn safe_endpoint(value: &str) -> Result<(), RuntimeError> {
    let url =
        reqwest::Url::parse(value).map_err(|_| config_error("invalid configured endpoint"))?;
    let local = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.host_str().is_none()
        || !matches!(url.scheme(), "http" | "https")
        || url.scheme() == "http" && !local
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(config_error("new endpoints require HTTPS or loopback HTTP, without URL credentials, queries or fragments"));
    }
    Ok(())
}
