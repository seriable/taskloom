//! What the gateway knows about models and tasks.
//!
//! A [`ModelProfile`] describes what a model can accept; a [`TaskProfile`]
//! declares what one task is allowed to send. [`ModelRegistry`] resolves a
//! model id to its profile (built-in families by longest prefix, then the
//! caller's overrides); [`TaskRegistry`] resolves a task label to its profile
//! (registered prefixes, longest wins, else the defaults).

use std::{borrow::Cow, time::Duration};

use serde::{Deserialize, Serialize};

/// Default per-call input budget of an unregistered task.
pub const DEFAULT_INPUT_BUDGET_TOKENS: usize = 16_000;
/// Default ceiling of any single task budget.
pub const HARD_INPUT_BUDGET_TOKENS: usize = 32_000;
pub const DEFAULT_OUTPUT_TOKENS: usize = 4_096;
/// Default whole-call limit of a task entry: 0, none. A task opts in with
/// [`TaskEntry::with_timeout`].
pub const DEFAULT_TASK_TIMEOUT_SECONDS: u64 = 0;
pub const DEFAULT_PROVIDER_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    pub id: String,
    /// Total tokens the provider accepts for input plus reserved output.
    pub context_window: usize,
    pub max_output: usize,
    pub supports_json_schema: bool,
    pub supports_images: bool,
    pub max_images: usize,
    pub provider: String,
    pub concurrency: usize,
    #[serde(default)]
    pub fallback_chain: Vec<String>,
}

/// A per-prefix override of model limits, e.g.
/// `{"id":"gpt-5.6","context_window":400000,"fallback_chain":["gpt-5.5"]}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelProfileOverride {
    pub id: String,
    pub context_window: Option<usize>,
    pub max_output: Option<usize>,
    pub supports_json_schema: Option<bool>,
    pub supports_images: Option<bool>,
    pub max_images: Option<usize>,
    pub provider: Option<String>,
    pub concurrency: Option<usize>,
    pub fallback_chain: Option<Vec<String>>,
}

struct BuiltInModel {
    prefix: &'static str,
    context_window: usize,
    max_output: usize,
    supports_json_schema: bool,
    supports_images: bool,
    max_images: usize,
    provider: &'static str,
    concurrency: usize,
}

/// Ordered most-specific first; the longest matching prefix wins anyway.
const BUILT_IN_MODELS: &[BuiltInModel] = &[
    BuiltInModel {
        prefix: "gpt-5.6-luna",
        context_window: 258_400,
        max_output: 128_000,
        supports_json_schema: true,
        supports_images: true,
        max_images: 4,
        provider: "openai",
        concurrency: 4,
    },
    BuiltInModel {
        prefix: "gpt-5",
        context_window: 258_400,
        max_output: 128_000,
        supports_json_schema: true,
        supports_images: true,
        max_images: 24,
        provider: "openai",
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
    },
    BuiltInModel {
        prefix: "gpt-4o",
        context_window: 128_000,
        max_output: 16_384,
        supports_json_schema: true,
        supports_images: true,
        max_images: 24,
        provider: "openai",
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
    },
    BuiltInModel {
        prefix: "mimo-v2.5-asr",
        context_window: 32_768,
        max_output: 8_192,
        supports_json_schema: false,
        supports_images: false,
        max_images: 0,
        provider: "mimo",
        concurrency: 4,
    },
    BuiltInModel {
        prefix: "mimo",
        context_window: 131_072,
        max_output: 32_768,
        supports_json_schema: false,
        supports_images: true,
        max_images: 24,
        provider: "mimo",
        concurrency: 4,
    },
    BuiltInModel {
        prefix: "gemini",
        context_window: 1_048_576,
        max_output: 65_536,
        supports_json_schema: true,
        supports_images: true,
        max_images: 24,
        provider: "gemini",
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
    },
    BuiltInModel {
        prefix: "claude",
        context_window: 200_000,
        max_output: 64_000,
        supports_json_schema: true,
        supports_images: true,
        max_images: 20,
        provider: "anthropic",
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
    },
    BuiltInModel {
        prefix: "qwen",
        context_window: 131_072,
        max_output: 8_192,
        supports_json_schema: false,
        supports_images: true,
        max_images: 24,
        provider: "qwen",
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
    },
    BuiltInModel {
        prefix: "deepseek",
        context_window: 128_000,
        max_output: 8_192,
        supports_json_schema: false,
        supports_images: false,
        max_images: 0,
        provider: "deepseek",
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
    },
];

/// Unknown models are treated as a conservative 128K OpenAI-compatible model.
fn generic_model_profile(id: &str) -> ModelProfile {
    ModelProfile {
        id: id.to_string(),
        context_window: 128_000,
        max_output: 16_384,
        supports_json_schema: true,
        supports_images: true,
        max_images: 24,
        provider: "generic".to_string(),
        concurrency: DEFAULT_PROVIDER_CONCURRENCY,
        fallback_chain: Vec::new(),
    }
}

fn built_in_model_profile(id: &str) -> ModelProfile {
    let lower = id.trim().to_ascii_lowercase();
    BUILT_IN_MODELS
        .iter()
        .filter(|model| lower.starts_with(model.prefix))
        .max_by_key(|model| model.prefix.len())
        .map(|model| ModelProfile {
            id: id.trim().to_string(),
            context_window: model.context_window,
            max_output: model.max_output,
            supports_json_schema: model.supports_json_schema,
            supports_images: model.supports_images,
            max_images: model.max_images,
            provider: model.provider.to_string(),
            concurrency: model.concurrency,
            fallback_chain: Vec::new(),
        })
        .unwrap_or_else(|| generic_model_profile(id.trim()))
}

/// Parses a JSON array of [`ModelProfileOverride`]s; malformed JSON is
/// logged under `source` and ignored.
pub fn parse_model_overrides(raw: &str, source: &str) -> Vec<ModelProfileOverride> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<ModelProfileOverride>>(raw) {
        Ok(overrides) => overrides
            .into_iter()
            .filter(|item| !item.id.trim().is_empty())
            .collect(),
        Err(error) => {
            tracing::warn!(
                source,
                %error,
                "ignoring malformed model profile override JSON"
            );
            Vec::new()
        }
    }
}

fn apply_model_overrides(
    mut profile: ModelProfile,
    overrides: &[ModelProfileOverride],
) -> ModelProfile {
    let lower = profile.id.to_ascii_lowercase();
    let Some(item) = overrides
        .iter()
        .filter(|item| lower.starts_with(&item.id.trim().to_ascii_lowercase()))
        .max_by_key(|item| item.id.trim().len())
    else {
        return profile;
    };
    if let Some(value) = item.context_window.filter(|value| *value > 0) {
        profile.context_window = value;
    }
    if let Some(value) = item.max_output.filter(|value| *value > 0) {
        profile.max_output = value;
    }
    if let Some(value) = item.supports_json_schema {
        profile.supports_json_schema = value;
    }
    if let Some(value) = item.supports_images {
        profile.supports_images = value;
    }
    if let Some(value) = item.max_images {
        profile.max_images = value;
    }
    if let Some(value) = item
        .provider
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        profile.provider = value.trim().to_string();
    }
    if let Some(value) = item.concurrency.filter(|value| *value > 0) {
        profile.concurrency = value;
    }
    if let Some(value) = &item.fallback_chain {
        profile.fallback_chain = value.clone();
    }
    profile
}

/// Model profiles: built-in families by longest prefix (unknown models get
/// a conservative 128K profile), then the overrides, plus the fallback chain
/// used for models whose profile has none.
#[derive(Debug, Clone, Default)]
pub struct ModelRegistry {
    overrides: Vec<ModelProfileOverride>,
    default_fallback_chain: Vec<String>,
}

impl ModelRegistry {
    /// Only protocol and model-routing settings belong in answer-store keys.
    /// Keep every prefix (including operational-only overrides), since a more
    /// specific override shadows all fields of a less specific one.
    pub(crate) fn answer_settings(&self) -> String {
        let overrides = self
            .overrides
            .iter()
            .map(|item| {
                (
                    item.id.trim().to_ascii_lowercase(),
                    item.supports_json_schema,
                    item.supports_images,
                    item.max_images,
                    &item.fallback_chain,
                )
            })
            .collect::<Vec<_>>();
        format!("{:?}|{:?}", overrides, self.default_fallback_chain)
    }

    /// The built-in profiles only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds per-prefix overrides (the longest matching id wins).
    pub fn with_overrides(mut self, overrides: Vec<ModelProfileOverride>) -> Self {
        self.overrides = overrides;
        self
    }

    /// The fallback chain of models whose profile declares none.
    pub fn with_default_fallback_chain(mut self, chain: Vec<String>) -> Self {
        self.default_fallback_chain = chain;
        self
    }

    /// Parses a comma or semicolon separated model list.
    pub fn parse_model_list(raw: &str) -> Vec<String> {
        raw.split([',', ';'])
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    /// The profile of `model`.
    pub fn profile(&self, model: &str) -> ModelProfile {
        apply_model_overrides(built_in_model_profile(model), &self.overrides)
    }

    /// Ordered fallback models for `primary`: its profile's own chain, or
    /// the default chain. The primary model is never repeated and models
    /// that cannot take images are skipped for an image-bearing request. An
    /// empty chain is valid.
    pub fn fallback_models(&self, primary: &str, needs_images: bool) -> Vec<String> {
        let profile = self.profile(primary);
        let candidates = if profile.fallback_chain.is_empty() {
            &self.default_fallback_chain
        } else {
            &profile.fallback_chain
        };
        resolve_fallback_chain(primary, needs_images, candidates, |model| {
            self.profile(model)
        })
    }
}

/// The deduplicated, image-capable chain of `candidates` after `primary`.
pub fn resolve_fallback_chain(
    primary: &str,
    needs_images: bool,
    candidates: &[String],
    profile_for: impl Fn(&str) -> ModelProfile,
) -> Vec<String> {
    let mut seen = vec![primary.trim().to_ascii_lowercase()];
    let mut chain = Vec::new();
    for model in candidates
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
    {
        let key = model.to_ascii_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        if needs_images && !profile_for(model).supports_images {
            continue;
        }
        chain.push(model.to_string());
    }
    chain
}

/// How strictly the per-task input budget is applied. The model window is
/// always enforced regardless of mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetMode {
    /// Use each task's registered `enforce_input_budget` flag (default).
    #[default]
    Registry,
    /// Enforce every task budget.
    EnforceAll,
    /// Only log and count task-budget overruns.
    ObserveAll,
}

impl BudgetMode {
    /// `enforce`/`enforce_all`, `observe`/`observe_all`, else `Registry`.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "enforce" | "enforce_all" => Self::EnforceAll,
            "observe" | "observe_all" => Self::ObserveAll,
            _ => Self::Registry,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskProfile {
    pub label: String,
    pub input_budget_tokens: usize,
    pub output_tokens: usize,
    /// Whole-call limit, including queueing, retries and model fallbacks;
    /// `None` (the default) sets none, so only the attempt timeout and the
    /// task's own deadline apply.
    pub timeout: Option<Duration>,
    pub allow_fallback: bool,
    /// When false, an over-budget request is logged and counted but still
    /// sent as long as it fits the model window.
    pub enforce_input_budget: bool,
    pub temperature: f32,
    /// Whether the label matched the registry.
    pub registered: bool,
}

impl TaskProfile {
    pub fn budget_enforced(&self, mode: BudgetMode) -> bool {
        match mode {
            BudgetMode::EnforceAll => true,
            BudgetMode::ObserveAll => false,
            BudgetMode::Registry => self.enforce_input_budget,
        }
    }
}

/// The declared limits of every label starting with `prefix`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskEntry {
    pub prefix: Cow<'static, str>,
    pub input_budget_tokens: usize,
    pub output_tokens: usize,
    /// Whole-call limit in seconds; 0 (the default) sets none.
    pub timeout_seconds: u64,
    pub allow_fallback: bool,
    pub enforce_input_budget: bool,
    pub temperature: f32,
}

impl TaskEntry {
    /// A text task whose input budget is enforced before sending.
    pub const fn bounded_text(
        prefix: &'static str,
        output_tokens: usize,
        temperature: f32,
    ) -> Self {
        Self {
            prefix: Cow::Borrowed(prefix),
            input_budget_tokens: DEFAULT_INPUT_BUDGET_TOKENS,
            output_tokens,
            timeout_seconds: DEFAULT_TASK_TIMEOUT_SECONDS,
            allow_fallback: true,
            enforce_input_budget: true,
            temperature,
        }
    }

    /// The same entry with a whole-call limit of `seconds` (0: none). The
    /// limit covers queueing, backoff, retries and the fallback chain, so set
    /// it above the attempt timeout times the attempts you expect.
    pub const fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = seconds;
        self
    }

    /// A text task whose over-budget requests are only observed.
    pub const fn observed_text(
        prefix: &'static str,
        output_tokens: usize,
        temperature: f32,
    ) -> Self {
        let mut entry = Self::bounded_text(prefix, output_tokens, temperature);
        entry.enforce_input_budget = false;
        entry
    }

    /// An image-bearing task: the hard budget ceiling, observed.
    pub const fn observed_visual(prefix: &'static str, output_tokens: usize) -> Self {
        let mut entry = Self::bounded_text(prefix, output_tokens, 0.1);
        entry.input_budget_tokens = HARD_INPUT_BUDGET_TOKENS;
        entry.enforce_input_budget = false;
        entry
    }
}

/// Task profiles by label prefix (case-insensitive, the longest wins).
/// Unregistered labels get the default budget in observe mode, so only the
/// model window is enforced for them.
#[derive(Debug, Clone)]
pub struct TaskRegistry {
    entries: Vec<TaskEntry>,
    hard_input_budget_tokens: usize,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            hard_input_budget_tokens: HARD_INPUT_BUDGET_TOKENS,
        }
    }
}

impl TaskRegistry {
    pub fn new(entries: impl IntoIterator<Item = TaskEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
            ..Self::default()
        }
    }

    /// Registers one more task prefix.
    pub fn register(&mut self, entry: TaskEntry) {
        self.entries.push(entry);
    }

    /// Caps every registered budget (default 32K).
    pub fn with_hard_input_budget(mut self, tokens: usize) -> Self {
        self.hard_input_budget_tokens = tokens;
        self
    }

    /// The profile of `label`.
    pub fn profile(&self, label: &str) -> TaskProfile {
        let label = label.trim();
        let lower = label.to_ascii_lowercase();
        match self
            .entries
            .iter()
            .filter(|entry| lower.starts_with(entry.prefix.as_ref()))
            .max_by_key(|entry| entry.prefix.len())
        {
            Some(entry) => TaskProfile {
                label: label.to_string(),
                input_budget_tokens: entry.input_budget_tokens.min(self.hard_input_budget_tokens),
                output_tokens: entry.output_tokens,
                timeout: (entry.timeout_seconds > 0)
                    .then(|| Duration::from_secs(entry.timeout_seconds)),
                allow_fallback: entry.allow_fallback,
                enforce_input_budget: entry.enforce_input_budget,
                temperature: entry.temperature,
                registered: true,
            },
            None => TaskProfile {
                label: label.to_string(),
                input_budget_tokens: DEFAULT_INPUT_BUDGET_TOKENS,
                output_tokens: DEFAULT_OUTPUT_TOKENS,
                timeout: None,
                allow_fallback: true,
                enforce_input_budget: false,
                temperature: 0.2,
                registered: false,
            },
        }
    }
}
