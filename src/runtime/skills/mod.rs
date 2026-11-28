//! Data-only skill packages with exact content/dependency pins. Neither a
//! package declaration nor its text is an authorization grant.
mod activation;
mod archive;
mod store;

use crate::runtime::{
    state::fingerprint,
    workflow::{PeerSpec, WorkflowDefinition, WorkflowOutput},
    RuntimeError, ToolEffect,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub use activation::{ActivatedSkill, SkillActivationMode, SkillBinding, SkillGrant, SkillHost};
pub use archive::read_skill_archive;
pub use store::{FileSkillStore, SkillCatalog, SkillCatalogEntry, SkillCatalogEvent};

pub const SKILL_FORMAT: u32 = 1;
const MAX_FILES: usize = 128;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRef {
    pub name: String,
    pub version: String,
    pub digest: String,
}
impl SkillRef {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        name(&self.name)?;
        version(&self.version)?;
        hash(&self.digest)
    }
    pub fn catalog_key(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillToolRequirement {
    pub effect: ToolEffect,
    pub permissions: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCredential {
    pub name: String,
    pub purpose: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillInclude {
    pub dependency: String,
    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRole {
    pub name: String,
    pub instructions: String,
    #[serde(default)]
    pub includes: Vec<SkillInclude>,
    pub tools: BTreeSet<String>,
    pub output: WorkflowOutput,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkillWorkflow {
    Dag { definition: WorkflowDefinition },
    Peer { spec: PeerSpec },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillTemplate {
    pub id: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    pub workflow: SkillWorkflow,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillConnector {
    pub id: String,
    /// Documentation/config example only. Never interpreted as an endpoint or
    /// command by the installer; host service configuration remains separate.
    pub configuration_example: String,
    pub tools: BTreeSet<String>,
    pub credentials: BTreeSet<String>,
    pub description: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillManifest {
    pub format: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: Vec<SkillRef>,
    #[serde(default)]
    pub tools: BTreeMap<String, SkillToolRequirement>,
    #[serde(default)]
    pub credentials: Vec<SkillCredential>,
    #[serde(default)]
    pub roles: Vec<SkillRole>,
    #[serde(default)]
    pub templates: Vec<SkillTemplate>,
    #[serde(default)]
    pub connectors: Vec<SkillConnector>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackage {
    pub manifest: SkillManifest,
    pub files: BTreeMap<String, String>,
}
impl SkillPackage {
    pub fn reference(&self) -> Result<SkillRef, RuntimeError> {
        self.validate()?;
        Ok(SkillRef {
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            digest: fingerprint(&("taskloom-skill-v1", self))?,
        })
    }
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let m = &self.manifest;
        name(&m.name)?;
        version(&m.version)?;
        text(&m.description, 4096)?;
        let manifest_bytes = serde_json::to_vec(m).map_err(json_error)?.len();
        if manifest_bytes > 128 * 1024 {
            return Err(invalid("manifest exceeds 128 KiB"));
        }
        if m.format != SKILL_FORMAT
            || self.files.len() > MAX_FILES
            || self.files.len() != m.files.len()
            || self
                .files
                .values()
                .map(String::len)
                .sum::<usize>()
                .saturating_add(manifest_bytes)
                > MAX_CONTENT_BYTES
            || m.roles.len() > 16
            || m.templates.len() > 16
            || m.connectors.len() > 16
            || m.dependencies.len() > 16
            || m.tools.len() > 64
            || m.credentials.len() > 32
        {
            return Err(invalid("unsupported or oversized skill package"));
        }
        for (path, value) in &self.files {
            asset_path(path)?;
            if path
                .split('/')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case("skill.json"))
                || value.len() > MAX_FILE_BYTES
                || value
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
            {
                return Err(invalid("invalid skill asset content"));
            }
            use sha2::{Digest, Sha256};
            if m.files.get(path) != Some(&hex::encode(Sha256::digest(value.as_bytes()))) {
                return Err(invalid("skill asset hash does not match manifest"));
            }
        }
        let mut paths = BTreeSet::new();
        for path in self.files.keys() {
            if !paths.insert(path.to_ascii_lowercase()) {
                return Err(invalid("case-colliding asset paths"));
            }
            let mut prefix = String::new();
            for part in path.split('/').take(path.split('/').count() - 1) {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if self.files.keys().any(|p| p.eq_ignore_ascii_case(&prefix)) {
                    return Err(invalid("asset file/directory collision"));
                }
            }
        }
        let mut deps = BTreeSet::new();
        for dep in &m.dependencies {
            dep.validate()?;
            if dep.name == m.name || !deps.insert(&dep.name) {
                return Err(invalid("self or duplicate dependency name"));
            }
        }
        let mut credentials = BTreeSet::new();
        for credential in &m.credentials {
            env_name(&credential.name)?;
            text(&credential.purpose, 1024)?;
            if !credentials.insert(&credential.name) {
                return Err(invalid("duplicate credential declaration"));
            }
        }
        for (tool, requirement) in &m.tools {
            tool_name(tool)?;
            if requirement.permissions.len() > 32 {
                return Err(invalid("too many tool permissions"));
            }
            for p in &requirement.permissions {
                text(p, 128)?;
            }
        }
        let mut roles = BTreeSet::new();
        for role in &m.roles {
            name(&role.name)?;
            if !roles.insert(&role.name)
                || !self.files.contains_key(&role.instructions)
                || role.includes.len() > 8
                || role.tools.iter().any(|t| !m.tools.contains_key(t))
            {
                return Err(invalid(
                    "duplicate role, missing instructions or undeclared tool",
                ));
            }
            for include in &role.includes {
                asset_path(&include.path)?;
                if !deps.contains(&include.dependency) {
                    return Err(invalid("role include has no declared dependency"));
                }
            }
            validate_output(&role.output)?;
        }
        let mut templates = BTreeSet::new();
        for template in &m.templates {
            name(&template.id)?;
            text(&template.title, 256)?;
            text(&template.description, 4096)?;
            if !templates.insert(&template.id) {
                return Err(invalid("duplicate template"));
            }
            validate_schema(&template.input_schema)?;
            match &template.workflow {
                SkillWorkflow::Dag { definition } => {
                    definition.validate(&crate::runtime::workflow::WorkflowLimits {
                        max_nodes: 64,
                        ..Default::default()
                    })?;
                    if definition.nodes.is_empty()
                        || definition.nodes.len() > 64
                        || definition.nodes.iter().any(|n| !roles.contains(&n.task))
                    {
                        return Err(invalid("template refers to an unavailable role"));
                    }
                    // Structural graph/schema validation also occurs when the
                    // compiled template is admitted to WorkflowRunner.
                }
                SkillWorkflow::Peer { spec } => {
                    for role in [&spec.planner, &spec.executor, &spec.writer, &spec.reviewer] {
                        if !roles.contains(role) {
                            return Err(invalid("PEER template has an unavailable role"));
                        }
                    }
                    if !(1..=16).contains(&spec.max_tasks) || spec.max_revisions > 3 {
                        return Err(invalid("unbounded PEER template"));
                    }
                }
            }
        }
        let mut connectors = BTreeSet::new();
        for connector in &m.connectors {
            name(&connector.id)?;
            text(&connector.description, 4096)?;
            if !connectors.insert(&connector.id)
                || !self.files.contains_key(&connector.configuration_example)
                || connector.tools.iter().any(|t| !m.tools.contains_key(t))
                || connector
                    .credentials
                    .iter()
                    .any(|c| !credentials.contains(c))
            {
                return Err(invalid(
                    "connector example has undeclared files, tools or credentials",
                ));
            }
        }
        Ok(())
    }
}
fn validate_output(output: &WorkflowOutput) -> Result<(), RuntimeError> {
    match output {
        WorkflowOutput::Text => Ok(()),
        WorkflowOutput::Json { schema } => validate_schema(schema),
        WorkflowOutput::Validated { policy } => policy.validate(),
    }
}
fn validate_schema(schema: &Value) -> Result<(), RuntimeError> {
    if serde_json::to_vec(schema).map_err(json_error)?.len() > 128 * 1024 {
        return Err(invalid("skill schema exceeds 128 KiB"));
    }
    jsonschema::validator_for(schema)
        .map_err(|e| invalid(&format!("invalid skill schema: {e}")))?;
    Ok(())
}
fn name(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > 48
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(invalid(
            "names must be 1..=48 lowercase ASCII letters/digits/hyphens",
        ));
    }
    Ok(())
}
fn tool_name(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid("invalid tool name"));
    }
    Ok(())
}
fn version(value: &str) -> Result<(), RuntimeError> {
    let parts = value.split('.').collect::<Vec<_>>();
    if value.len() > 32
        || parts.len() != 3
        || parts
            .iter()
            .any(|p| p.parse::<u32>().ok().is_none_or(|n| n.to_string() != *p))
    {
        return Err(invalid(
            "skill version must be an exact canonical major.minor.patch triple",
        ));
    }
    Ok(())
}
fn hash(value: &str) -> Result<(), RuntimeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid content digest"));
    }
    Ok(())
}
fn env_name(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        || value.as_bytes()[0].is_ascii_digit()
    {
        return Err(invalid("invalid credential environment name"));
    }
    Ok(())
}
fn text(value: &str, max: usize) -> Result<(), RuntimeError> {
    if value.trim().is_empty()
        || value.len() > max
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err(invalid("empty, oversized or control-bearing text"));
    }
    Ok(())
}
fn asset_path(value: &str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > 240
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-/".contains(&b))
    {
        return Err(invalid("invalid portable asset path"));
    }
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid("unsafe or platform-reserved asset path"));
        }
    }
    Ok(())
}
fn invalid(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(format!("skill: {detail}"))
}
fn json_error(e: serde_json::Error) -> RuntimeError {
    invalid(&e.to_string())
}
