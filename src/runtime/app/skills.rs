use super::{
    config::config_error, AppConfig, AppProvider, AppRole, AppWorkflowRequest, Application,
};
use crate::runtime::{skills::*, workflow::*, *};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSkill {
    pub package: SkillRef,
    pub mount: String,
    pub grant: SkillGrant,
    #[serde(default)]
    pub mode: SkillActivationMode,
}
impl AppSkill {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.package.validate()?;
        self.grant.validate()?;
        if self.mount.is_empty()
            || self.mount.len() > 14
            || !self
                .mount
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.grant.tools.len() > 64
            || self.grant.permissions.len() > 128
            || self.grant.credentials.len() > 32
        {
            return Err(config_error("invalid skill mount or grant bounds"));
        }
        Ok(())
    }
}
pub(super) struct LoadedSkill {
    pub mount: String,
    pub activated: ActivatedSkill,
}
pub(super) struct CompiledSkills {
    pub skills: Vec<LoadedSkill>,
    pub roles: Vec<AppRole>,
    pub bindings: BTreeMap<String, SkillBinding>,
    pub recovery_roles: BTreeSet<String>,
}
pub(super) async fn load(
    config: &AppConfig,
    tools: &ToolRegistry,
    policy: &ToolPolicy,
    store: &super::AppSkillStore,
) -> Result<CompiledSkills, RuntimeError> {
    let mut compiled = CompiledSkills {
        skills: Vec::new(),
        roles: Vec::new(),
        bindings: BTreeMap::new(),
        recovery_roles: BTreeSet::new(),
    };
    let mut names = config
        .roles
        .iter()
        .map(|r| r.name.clone())
        .collect::<BTreeSet<_>>();
    let mut credentials = BTreeSet::new();
    provider_names(&config.provider, &mut credentials);
    if let Some(rerank) = &config.retrieval.http_rerank {
        credentials.insert(rerank.provider.key_env.clone());
    }
    #[cfg(feature = "qdrant")]
    if let Some(name) = config
        .retrieval
        .qdrant
        .as_ref()
        .and_then(|target| target.api_key_env.as_ref())
    {
        credentials.insert(name.clone());
    }
    for process in &config.mcp_stdio {
        credentials.extend(process.environment.iter().cloned());
    }
    for endpoint in &config.mcp {
        if let Some(name) = &endpoint.token_env {
            credentials.insert(name.clone());
        }
    }
    if let Some(search) = &config.research.search {
        credentials.insert(search.key_env.clone());
    }
    if let Some(embedding) = &config.retrieval.embedding {
        provider_names(&embedding.provider, &mut credentials);
    }
    for entry in &config.skills {
        let host = SkillHost {
            tools: tools.definitions(policy),
            policy: policy.clone(),
            credential_names: credentials.clone(),
        };
        #[cfg(feature = "skill-registry")]
        let activated = if let Some(root) = &config.skill_registry {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| config_error("system clock cannot validate signed release expiry"))?
                .as_secs();
            FileSkillRegistry::new(root.clone())?
                .activate(
                    store,
                    &entry.package,
                    entry.grant.clone(),
                    host,
                    entry.mode,
                    now,
                )
                .await?
        } else {
            store
                .activate(&entry.package, entry.grant.clone(), host, entry.mode)
                .await?
        };
        #[cfg(not(feature = "skill-registry"))]
        let activated = store
            .activate(&entry.package, entry.grant.clone(), host, entry.mode)
            .await?;
        for role in &activated.manifest().roles {
            let mounted = format!("{}-{}", entry.mount, role.name);
            if !names.insert(mounted.clone()) || names.len() > 64 {
                return Err(config_error(
                    "skill role names collide or exceed the combined role limit",
                ));
            }
            if entry.mode == SkillActivationMode::RecoveryOnly {
                compiled.recovery_roles.insert(mounted.clone());
            }
            compiled
                .bindings
                .insert(mounted.clone(), activated.binding(&role.name)?);
            compiled.roles.push(AppRole {
                name: mounted,
                system: activated.instructions(&role.name)?,
                tools: role.tools.clone(),
                output: role.output.clone(),
            });
        }
        compiled.skills.push(LoadedSkill {
            mount: entry.mount.clone(),
            activated,
        });
    }
    Ok(compiled)
}
fn provider_names(provider: &AppProvider, names: &mut BTreeSet<String>) {
    match provider {
        AppProvider::Offline => {}
        AppProvider::Openai { key_env, .. }
        | AppProvider::Anthropic { key_env, .. }
        | AppProvider::Native { key_env, .. } => {
            names.insert(key_env.clone());
        }
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppTemplateRequest {
    pub id: String,
    pub input: Value,
    #[serde(default)]
    pub limits: WorkflowLimits,
}
impl Application {
    pub fn registered_roles(&self) -> impl Iterator<Item = &AppRole> {
        self.config.roles.iter().chain(&self.skill_roles)
    }
    pub(super) fn agent_for_role(&self, role: &AppRole) -> AgentLoop<'_> {
        let mut policy = self.policy.clone();
        policy.allowed_tools.retain(|t| role.tools.contains(t));
        if let Some(binding) = self.skill_bindings.get(&role.name) {
            if let Some(skill) = self
                .skills
                .iter()
                .find(|s| s.activated.reference == binding.package)
            {
                policy
                    .grants
                    .retain(|p| skill.activated.grant().permissions.contains(p));
            }
        }
        let mut agent = self.agent().without_output_policy().with_policy(policy);
        if let Some(binding) = self.skill_bindings.get(&role.name) {
            agent = agent.with_skill_binding(binding.clone());
        }
        if let WorkflowOutput::Validated { policy } = &role.output {
            agent = agent.with_output_policy(policy.clone());
        }
        agent
    }
    pub fn skill_templates(&self) -> Value {
        json!({"templates":self.skills.iter().filter(|s|s.activated.mode==SkillActivationMode::Active).flat_map(|s|s.activated.manifest().templates.iter().map(move|t|json!({"mount":s.mount,"package":s.activated.reference,"id":t.id,"title":t.title,"description":t.description,"input_schema":t.input_schema,"workflow":t.workflow}))).collect::<Vec<_>>()})
    }
    pub async fn start_skill_template(
        self: &Arc<Self>,
        mount: &str,
        template: &str,
        input: AppTemplateRequest,
    ) -> Result<WorkflowState, RuntimeError> {
        let request = self.compile_skill_template(mount, template, input)?;
        self.start_workflow(request).await
    }
    /// Resolve an active pinned template and its input schema without starting
    /// work. The ordinary workflow preview then binds its mounted role contracts.
    pub fn preview_skill_template(
        &self,
        mount: &str,
        template: &str,
        input: AppTemplateRequest,
    ) -> Result<super::AppWorkflowPreview, RuntimeError> {
        self.preview_workflow(self.compile_skill_template(mount, template, input)?)
    }
    fn compile_skill_template(
        &self,
        mount: &str,
        template: &str,
        input: AppTemplateRequest,
    ) -> Result<AppWorkflowRequest, RuntimeError> {
        let skill = self
            .skills
            .iter()
            .find(|s| s.mount == mount)
            .ok_or_else(|| config_error("skill mount is not active"))?;
        let template = skill.activated.template(template, &input.input)?;
        let task = |name: &str| format!("{mount}-{name}");
        let (definition, peer) = match template.workflow {
            SkillWorkflow::Dag { mut definition } => {
                for node in &mut definition.nodes {
                    node.task = task(&node.task);
                }
                (Some(definition), None)
            }
            SkillWorkflow::Peer { mut spec } => {
                spec.planner = task(&spec.planner);
                spec.executor = task(&spec.executor);
                spec.writer = task(&spec.writer);
                spec.reviewer = task(&spec.reviewer);
                (None, Some(spec))
            }
        };
        Ok(AppWorkflowRequest {
            id: input.id,
            input: input.input,
            definition,
            peer,
            limits: input.limits,
        })
    }
}
