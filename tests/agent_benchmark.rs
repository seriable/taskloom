#![cfg(feature = "runtime")]
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::future::BoxFuture;
use serde_json::json;
use taskloom::runtime::benchmark::*;
use taskloom::runtime::rag::{Citation, CitedReport, Document, SearchResult};
use taskloom::runtime::*;

#[derive(Default)]
struct FixtureModel {
    calls: AtomicUsize,
    ignore_tools: bool,
}
impl Model for FixtureModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(1).await?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            let tool = request.messages.iter().rev().find_map(|m| match m {
                Message::Tool { result } => Some(result),
                _ => None,
            });
            let prompt = request
                .messages
                .iter()
                .find_map(|m| match m {
                    Message::User { content } => Some(content.as_str()),
                    _ => None,
                })
                .unwrap();
            let turn = if self.ignore_tools {
                AssistantTurn::text("not executed")
            } else if let Some(result) = tool {
                assert!(!result.is_error);
                if prompt == "research" {
                    let result: SearchResult =
                        serde_json::from_value(result.output.clone()).unwrap();
                    let source = &result.hits[0].chunk;
                    AssistantTurn::text(
                        serde_json::to_string(&CitedReport {
                            text: format!("Approval records exact input [[{}]].", source.id),
                            citations: vec![Citation {
                                chunk_id: source.id.clone(),
                                quote: source.text.clone(),
                            }],
                        })
                        .unwrap(),
                    )
                } else {
                    AssistantTurn::text(result.output.to_string())
                }
            } else {
                let (name, arguments) = match prompt {
                    "research" => ("knowledge_search", json!({"query":"approval"})),
                    "calculate" => ("calculate", json!({"operation":"multiply","a":6,"b":7})),
                    _ => ("record_note", json!({"note":"approved fixture"})),
                };
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "fixture-call".into(),
                        name: name.into(),
                        arguments,
                    }],
                }
            };
            reservation.settle(1);
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}
fn configuration(cap: Option<u64>) -> BenchmarkConfig {
    BenchmarkConfig {
        origin: BenchmarkOrigin {
            kind: ExecutionKind::Fixture,
            model: "fixture".into(),
            configuration_fingerprint: "fixture-v1".into(),
        },
        limits: RunLimits {
            max_cost_micros: Some(10),
            ..Default::default()
        },
        context: None,
        output_tokens: 1024,
        max_total_cost_micros: cap,
    }
}
fn case(id: &str, category: CaseCategory) -> BenchmarkCase {
    let (prompt, checks) = match category {
        CaseCategory::Research => (
            "research",
            vec![OutcomeCheck::Citations {
                min_sources: 1,
                required_documents: vec!["approval".into()],
            }],
        ),
        CaseCategory::ToolUse => (
            "calculate",
            vec![
                OutcomeCheck::JsonEquals {
                    pointer: "/value".into(),
                    value: json!(42),
                },
                OutcomeCheck::ToolCalls {
                    name: "calculate".into(),
                    min: 1,
                    max: 1,
                },
            ],
        ),
        _ => (
            "write",
            vec![
                OutcomeCheck::JsonEquals {
                    pointer: "/saved".into(),
                    value: json!(true),
                },
                OutcomeCheck::ToolCalls {
                    name: "record_note".into(),
                    min: 1,
                    max: 1,
                },
                OutcomeCheck::ToolArguments {
                    name: "record_note".into(),
                    arguments: json!({"note":"approved fixture"}),
                },
            ],
        ),
    };
    BenchmarkCase {
        id: id.into(),
        category,
        prompt: prompt.into(),
        documents: if category == CaseCategory::Research {
            vec![Document {
                id: "approval".into(),
                title: "Approval".into(),
                source: "fixture:approval".into(),
                text: "Approval records exact input.".into(),
                metadata: Default::default(),
            }]
        } else {
            Vec::new()
        },
        checks,
    }
}
fn suite(cases: Vec<BenchmarkCase>) -> BenchmarkSuite {
    BenchmarkSuite {
        version: BENCHMARK_VERSION,
        name: "fixture acceptance".into(),
        cases,
    }
}

#[tokio::test]
async fn campaign_records_real_effects_approval_recovery_and_unchanged_reentry() {
    let model = FixtureModel::default();
    let root = tempfile::tempdir().unwrap();
    let suite = suite(vec![
        case("research", CaseCategory::Research),
        case("tools", CaseCategory::ToolUse),
        case("approval", CaseCategory::Approval),
        case("recovery", CaseCategory::Recovery),
    ]);
    let directory = root.path().join("campaign");
    let report = BenchmarkRunner::new(&model, configuration(Some(20)))
        .run(&suite, &directory)
        .await
        .unwrap();
    assert_eq!(report.origin.kind, ExecutionKind::Fixture);
    assert_eq!(report.passed, 4);
    assert_eq!(report.failed, 0);
    assert_eq!(report.success_rate, 1.0);
    assert!(report.completed);
    assert_eq!(report.charged_model_cost_micros, 8);
    assert_eq!(model.calls.load(Ordering::SeqCst), 8);
    assert_eq!(report.unknown_cost_cases, 0);
    assert!(report.cases[2].approval_exercised);
    assert!(report.cases[3].controller_interruption_exercised);
    assert_eq!(report.cases[2].effects, 1);
    assert_eq!(report.cases[3].effects, 1);
    assert!(report.cases.iter().all(|case| case.replay_unchanged));
    let saved: BenchmarkReport =
        serde_json::from_slice(&std::fs::read(directory.join("report.json")).unwrap()).unwrap();
    assert_eq!(saved.passed, 4);
    for result in &report.cases {
        assert!(directory.join(&result.trajectory_path).is_file());
    }
    assert!(BenchmarkRunner::new(&model, configuration(Some(20)))
        .run(&suite, &directory)
        .await
        .is_err());
    assert_eq!(model.calls.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn shared_budget_prevents_further_paid_dispatch_and_not_run_cases_stay_in_denominator() {
    let model = FixtureModel::default();
    let root = tempfile::tempdir().unwrap();
    let report = BenchmarkRunner::new(&model, configuration(Some(3)))
        .run(
            &suite(vec![
                case("a", CaseCategory::ToolUse),
                case("b", CaseCategory::ToolUse),
                case("c", CaseCategory::ToolUse),
            ]),
            root.path().join("campaign"),
        )
        .await
        .unwrap();
    assert_eq!(report.passed, 1);
    assert_eq!(report.failed, 1);
    assert_eq!(report.not_run, 1);
    assert_eq!(report.charged_model_cost_micros, 3);
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(report.success_rate, 1.0 / 3.0);
    assert_eq!(report.cases[2].status, CaseStatus::NotRun);
    assert!(report.cases[1]
        .failures
        .iter()
        .any(|failure| failure.contains("complete")));
}

#[tokio::test]
async fn incomplete_lifecycle_and_wrong_answers_cannot_be_reported_as_passed() {
    let model = FixtureModel {
        ignore_tools: true,
        ..Default::default()
    };
    let root = tempfile::tempdir().unwrap();
    let report = BenchmarkRunner::new(&model, configuration(Some(20)))
        .run(
            &suite(vec![
                case("approval", CaseCategory::Approval),
                case("recovery", CaseCategory::Recovery),
            ]),
            root.path().join("campaign"),
        )
        .await
        .unwrap();
    assert_eq!(report.passed, 0);
    assert_eq!(report.failed, 2);
    assert_eq!(report.success_rate, 0.0);
    assert_eq!(report.cases[0].effects, 0);
    assert!(!report.cases[0].approval_exercised);
    assert!(!report.cases[1].controller_interruption_exercised);
}

#[tokio::test]
async fn invalid_suites_and_missing_provider_campaign_cap_fail_before_dispatch() {
    let model = FixtureModel::default();
    let root = tempfile::tempdir().unwrap();
    let runner = BenchmarkRunner::new(&model, configuration(Some(20)));
    assert!(runner
        .run(
            &suite(vec![
                case("duplicate", CaseCategory::ToolUse),
                case("duplicate", CaseCategory::ToolUse)
            ]),
            root.path().join("duplicate")
        )
        .await
        .is_err());
    let mut config = configuration(None);
    config.origin.kind = ExecutionKind::ConfiguredProvider;
    assert!(BenchmarkRunner::new(&model, config)
        .run(
            &suite(vec![case("a", CaseCategory::ToolUse)]),
            root.path().join("uncapped")
        )
        .await
        .is_err());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert!(!root.path().join("uncapped").exists());
}

#[tokio::test]
async fn zero_budget_configured_provider_campaign_is_not_mislabeled_as_successful() {
    let model = FixtureModel::default();
    let root = tempfile::tempdir().unwrap();
    let mut config = configuration(Some(0));
    config.origin.kind = ExecutionKind::ConfiguredProvider;
    let report = BenchmarkRunner::new(&model, config)
        .run(
            &suite(vec![case("a", CaseCategory::ToolUse)]),
            root.path().join("zero"),
        )
        .await
        .unwrap();
    assert_eq!(report.not_run, 1);
    assert_eq!(report.passed, 0);
    assert_eq!(report.executed, 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn public_suite_is_valid_and_covers_all_required_categories() {
    let suite: BenchmarkSuite =
        serde_json::from_str(include_str!("../examples/benchmarks/acceptance-v1.json")).unwrap();
    suite.validate().unwrap();
    assert_eq!(suite.cases.len(), 12);
    for category in [
        CaseCategory::Research,
        CaseCategory::ToolUse,
        CaseCategory::Approval,
        CaseCategory::Recovery,
    ] {
        assert!(suite.cases.iter().any(|case| case.category == category));
    }
}

#[cfg(feature = "app")]
#[test]
fn cli_writes_an_offline_labeled_report_and_refuses_output_reuse() {
    let root = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_taskloom");
    let init = std::process::Command::new(bin)
        .current_dir(root.path())
        .args(["init", "taskloom.json"])
        .output()
        .unwrap();
    assert!(init.status.success());
    let mut manifest = suite(vec![case("research", CaseCategory::Research)]);
    manifest.cases[0].prompt = "approval".into();
    std::fs::write(
        root.path().join("suite.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let run = || {
        std::process::Command::new(bin)
            .current_dir(root.path())
            .args(["benchmark", "suite.json", "campaign", "0"])
            .output()
            .unwrap()
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: BenchmarkReport =
        serde_json::from_slice(&std::fs::read(root.path().join("campaign/report.json")).unwrap())
            .unwrap();
    assert_eq!(report.origin.kind, ExecutionKind::OfflineDemo);
    assert_eq!(report.passed, 1);
    assert_eq!(report.charged_model_cost_micros, 0);
    assert!(!run().status.success());
}

#[cfg(feature = "app")]
#[test]
fn full_offline_suite_preserves_multi_source_float_scores_and_reports_unsupported_tasks() {
    let root = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_taskloom");
    let init = std::process::Command::new(bin)
        .current_dir(root.path())
        .args(["init", "taskloom.json"])
        .output()
        .unwrap();
    assert!(init.status.success());
    std::fs::write(
        root.path().join("suite.json"),
        include_str!("../examples/benchmarks/acceptance-v1.json"),
    )
    .unwrap();
    let output = std::process::Command::new(bin)
        .current_dir(root.path())
        .args(["benchmark", "suite.json", "campaign", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: BenchmarkReport =
        serde_json::from_slice(&std::fs::read(root.path().join("campaign/report.json")).unwrap())
            .unwrap();
    assert_eq!(
        report.passed,
        3,
        "{:?}",
        report
            .cases
            .iter()
            .filter(|case| case.status == CaseStatus::Failed)
            .map(|case| (&case.id, &case.failures))
            .collect::<Vec<_>>()
    );
    assert_eq!(report.failed, 9);
    assert_eq!(report.not_run, 0);
    assert_eq!(report.unknown_cost_cases, 0);
    assert_eq!(report.charged_model_cost_micros, 0);
    assert!(report.completed);
    assert_eq!(report.cases[2].status, CaseStatus::Passed);
}

struct UnsettledFailure(AtomicUsize);
impl Model for UnsettledFailure {
    fn generate<'a>(
        &'a self,
        _: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let _reservation = budget.reserve_before_dispatch(3).await?;
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(RuntimeError::Protocol(
                "fixture loses the paid response before settlement".into(),
            ))
        })
    }
}
#[tokio::test]
async fn failed_unsettled_attempts_remain_charged_and_stop_the_campaign() {
    let model = UnsettledFailure(AtomicUsize::new(0));
    let root = tempfile::tempdir().unwrap();
    let report = BenchmarkRunner::new(&model, configuration(Some(3)))
        .run(
            &suite(vec![
                case("failed", CaseCategory::ToolUse),
                case("later", CaseCategory::ToolUse),
            ]),
            root.path().join("campaign"),
        )
        .await
        .unwrap();
    assert_eq!(report.charged_model_cost_micros, 3);
    assert_eq!(report.uncertain_model_cost_micros, 3);
    assert_eq!(report.failed, 1);
    assert_eq!(report.not_run, 1);
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
}

struct WaitingModel {
    started: tokio::sync::Notify,
}
impl Model for WaitingModel {
    fn generate<'a>(
        &'a self,
        _: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let _reservation = budget.reserve_before_dispatch(1).await?;
            self.started.notify_one();
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn interrupted_campaign_report_marks_the_active_case_and_reserves_unknown_spending() {
    let model = std::sync::Arc::new(WaitingModel {
        started: tokio::sync::Notify::new(),
    });
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("campaign");
    let worker_model = std::sync::Arc::clone(&model);
    let worker_dir = directory.clone();
    let worker = tokio::spawn(async move {
        BenchmarkRunner::new(worker_model.as_ref(), configuration(Some(20)))
            .run(
                &suite(vec![case("waiting", CaseCategory::ToolUse)]),
                worker_dir,
            )
            .await
    });
    model.started.notified().await;
    let report: BenchmarkReport =
        serde_json::from_slice(&std::fs::read(directory.join("report.json")).unwrap()).unwrap();
    assert!(!report.completed);
    assert_eq!(report.active_case.as_deref(), Some("waiting"));
    assert_eq!(report.running, 1);
    assert_eq!(report.cases[0].status, CaseStatus::Running);
    assert!(report.cases[0].cost_unknown);
    assert_eq!(report.charged_model_cost_micros, 10);
    assert_eq!(report.passed, 0);
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
}

struct PanickingModel;
impl Model for PanickingModel {
    fn generate<'a>(
        &'a self,
        _: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let _reservation = budget.reserve_before_dispatch(3).await?;
            panic!("injected benchmark model panic")
        })
    }
}
#[tokio::test]
async fn model_panic_retains_a_failed_result_and_observes_the_latest_reservation_journal() {
    let root = tempfile::tempdir().unwrap();
    let report = BenchmarkRunner::new(&PanickingModel, configuration(Some(3)))
        .run(
            &suite(vec![
                case("panic", CaseCategory::ToolUse),
                case("later", CaseCategory::ToolUse),
            ]),
            root.path().join("campaign"),
        )
        .await
        .unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.not_run, 1);
    assert_eq!(report.charged_model_cost_micros, 3);
    assert_eq!(report.uncertain_model_cost_micros, 3);
    assert!(report.cases[0]
        .failures
        .iter()
        .any(|failure| failure.contains("panicked")));
    assert!(report.completed);
    assert_eq!(report.running, 0);
    assert!(report.active_case.is_none());
}
