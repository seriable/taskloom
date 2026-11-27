use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ledger::Usage;
use crate::runtime::{
    rag::Document, ContextPolicy, RunLimits, RunStatus, RuntimeError, StopReason,
};

pub const BENCHMARK_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuite {
    pub version: u32,
    pub name: String,
    pub cases: Vec<BenchmarkCase>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkCase {
    pub id: String,
    pub category: CaseCategory,
    pub prompt: String,
    #[serde(default)]
    pub documents: Vec<Document>,
    pub checks: Vec<OutcomeCheck>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseCategory {
    Research,
    ToolUse,
    Approval,
    Recovery,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutcomeCheck {
    FinalContains {
        text: String,
    },
    JsonEquals {
        pointer: String,
        value: Value,
    },
    ToolCalls {
        name: String,
        min: usize,
        max: usize,
    },
    ToolArguments {
        name: String,
        arguments: Value,
    },
    Citations {
        min_sources: usize,
        required_documents: Vec<String>,
    },
}
impl BenchmarkSuite {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.version != BENCHMARK_VERSION
            || self.name.trim().is_empty()
            || self.name.len() > 128
            || self.cases.is_empty()
            || self.cases.len() > 200
        {
            return Err(config("unsupported or oversized benchmark suite"));
        }
        let mut ids = BTreeSet::new();
        for case in &self.cases {
            if !valid_id(&case.id)
                || !ids.insert(&case.id)
                || case.prompt.trim().is_empty()
                || case.prompt.len() > 64 * 1024
                || case.checks.is_empty()
                || case.checks.len() > 32
                || case.documents.len() > 20
            {
                return Err(config("invalid/duplicate case identity, prompt or checks"));
            }
            let mut documents = BTreeSet::new();
            for document in &case.documents {
                document.validate()?;
                if !documents.insert(&document.id) {
                    return Err(config("duplicate case document"));
                }
            }
            for check in &case.checks {
                let valid = match check {
                    OutcomeCheck::FinalContains { text } => !text.is_empty() && text.len() <= 8192,
                    OutcomeCheck::JsonEquals { pointer, .. } => {
                        pointer.is_empty() || pointer.starts_with('/')
                    }
                    OutcomeCheck::ToolCalls { name, min, max } => {
                        valid_id(name) && min <= max && *max <= 128
                    }
                    OutcomeCheck::ToolArguments { name, arguments } => {
                        valid_id(name) && arguments.is_object()
                    }
                    OutcomeCheck::Citations {
                        min_sources,
                        required_documents,
                    } => {
                        *min_sources > 0
                            && *min_sources <= 20
                            && required_documents.len() <= 20
                            && required_documents.iter().all(|id| documents.contains(id))
                    }
                };
                if !valid {
                    return Err(config("invalid benchmark outcome assertion"));
                }
            }
            let has_required_check=match case.category {
                CaseCategory::Research=>!case.documents.is_empty()&&case.checks.iter().any(|check|matches!(check,OutcomeCheck::Citations {..})),
                CaseCategory::ToolUse=>case.checks.iter().any(|check|matches!(check,OutcomeCheck::ToolCalls {min,..} if *min>0)),
                CaseCategory::Approval|CaseCategory::Recovery=>case.checks.iter().any(|check|matches!(check,OutcomeCheck::ToolArguments {name,..} if name=="record_note")),
            };
            if !has_required_check {
                return Err(config("case category lacks its required outcome assertion"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    OfflineDemo,
    Fixture,
    ConfiguredProvider,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkOrigin {
    pub kind: ExecutionKind,
    pub model: String,
    /// Nonsecret identity, not credentials or an endpoint containing tokens.
    pub configuration_fingerprint: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkConfig {
    pub origin: BenchmarkOrigin,
    pub limits: RunLimits,
    pub context: Option<ContextPolicy>,
    pub output_tokens: u32,
    pub max_total_cost_micros: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Running,
    Passed,
    Failed,
    NotRun,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub id: String,
    pub category: CaseCategory,
    pub status: CaseStatus,
    pub failures: Vec<String>,
    pub wall_elapsed_ms: u64,
    pub run_status: Option<RunStatus>,
    pub stop_reason: Option<StopReason>,
    pub model_steps: usize,
    /// Actual serving identities reported by completed model decisions.
    pub models_served: Vec<String>,
    pub tool_attempts: usize,
    pub effects: usize,
    pub usage: Usage,
    /// Committed plus unobserved journal reservations. Active cases reserve
    /// their ceiling conservatively until an outcome is observed.
    pub charged_model_cost_micros: u64,
    pub uncertain_model_cost_micros: u64,
    pub cost_unknown: bool,
    pub approval_exercised: bool,
    pub controller_interruption_exercised: bool,
    pub replay_unchanged: bool,
    pub final_text: Option<String>,
    /// Relative to the campaign directory. Includes complete private trajectory.
    pub trajectory_path: String,
}
impl CaseResult {
    pub(super) fn pending(case: &BenchmarkCase) -> Self {
        Self {
            id: case.id.clone(),
            category: case.category,
            status: CaseStatus::NotRun,
            failures: vec!["not executed".into()],
            wall_elapsed_ms: 0,
            run_status: None,
            stop_reason: None,
            model_steps: 0,
            models_served: Vec::new(),
            tool_attempts: 0,
            effects: 0,
            usage: Usage::default(),
            charged_model_cost_micros: 0,
            uncertain_model_cost_micros: 0,
            cost_unknown: false,
            approval_exercised: false,
            controller_interruption_exercised: false,
            replay_unchanged: false,
            final_text: None,
            trajectory_path: format!(
                "cases/{}/runs/{}.json",
                case.id,
                hex::encode(sha2::Sha256::digest(case.id.as_bytes()))
            ),
        }
    }
}
use sha2::Digest;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub version: u32,
    pub suite_name: String,
    pub suite_fingerprint: String,
    pub origin: BenchmarkOrigin,
    pub configuration: BenchmarkConfig,
    pub success_definition: String,
    pub completed: bool,
    pub active_case: Option<String>,
    pub requested: usize,
    pub executed: usize,
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
    pub not_run: usize,
    /// Passed/requested, including not-run cases in the denominator.
    pub success_rate: f64,
    pub wall_elapsed_ms: u64,
    pub charged_model_cost_micros: u64,
    pub uncertain_model_cost_micros: u64,
    pub unknown_cost_cases: usize,
    pub cases: Vec<CaseResult>,
}
impl BenchmarkReport {
    pub(super) fn summarize(&mut self) -> Result<(), RuntimeError> {
        self.requested = self.cases.len();
        self.passed = 0;
        self.failed = 0;
        self.running = 0;
        self.not_run = 0;
        self.charged_model_cost_micros = 0;
        self.uncertain_model_cost_micros = 0;
        self.unknown_cost_cases = 0;
        for case in &self.cases {
            match case.status {
                CaseStatus::Running => self.running += 1,
                CaseStatus::Passed => self.passed += 1,
                CaseStatus::Failed => self.failed += 1,
                CaseStatus::NotRun => self.not_run += 1,
            }
            self.charged_model_cost_micros = self
                .charged_model_cost_micros
                .checked_add(case.charged_model_cost_micros)
                .ok_or_else(|| config("campaign cost overflow"))?;
            self.uncertain_model_cost_micros = self
                .uncertain_model_cost_micros
                .checked_add(case.uncertain_model_cost_micros)
                .ok_or_else(|| config("campaign uncertainty overflow"))?;
            self.unknown_cost_cases += usize::from(case.cost_unknown);
        }
        self.executed = self.passed + self.failed + self.running;
        self.success_rate = self.passed as f64 / self.requested.max(1) as f64;
        Ok(())
    }
}
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(super) fn config(detail: &str) -> RuntimeError {
    RuntimeError::Configuration(format!("benchmark: {detail}"))
}
