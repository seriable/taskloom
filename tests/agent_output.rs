#![cfg(feature = "runtime")]
//! Contract/control regressions with explicit model fixtures; native quality
//! acceptance is a separate campaign and is never inferred from these tests.
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::{rag::*, workflow::*, *};
use taskloom::CancelToken;

#[derive(Clone, Copy)]
enum Mode {
    Correct,
    Reject,
    Escape,
    RetrieveDuringRepair,
}
struct CitationModel {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    mode: Mode,
}
impl CitationModel {
    fn new(mode: Mode) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            mode,
        }
    }
}
impl Model for CitationModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(10).await?.settle(10);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            let repair = request.messages.iter().any(|m| matches!(m, Message::User { content } if content.starts_with("[Host final-output validation feedback")));
            let evidence = request.messages.iter().find_map(|m| match m {
                Message::Tool { result } => {
                    serde_json::from_value::<SearchResult>(result.output.clone()).ok()
                }
                _ => None,
            });
            let turn = if repair && matches!(self.mode, Mode::Escape) {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "escape".into(),
                        name: "save".into(),
                        arguments: json!({}),
                    }],
                }
            } else if evidence.is_none()
                && (!matches!(self.mode, Mode::RetrieveDuringRepair) || repair)
            {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "lookup".into(),
                        name: "knowledge_search".into(),
                        arguments: json!({"query":"approval"}),
                    }],
                }
            } else if let Some(evidence) = evidence {
                let chunk = &evidence.hits[0].chunk;
                let valid = repair && !matches!(self.mode, Mode::Reject);
                AssistantTurn::text(json!({"text":if valid { format!("Check the operation [[{}]].",chunk.id) } else { "No marker in this invalid draft.".into() },
                    "citations":[{"chunk_id":chunk.id,"quote":chunk.text}]}).to_string())
            } else {
                AssistantTurn::text("not a report")
            };
            Ok(ModelOutput {
                model: request.model.clone(),
                turn,
            })
        })
    }
}
async fn tools(root: &std::path::Path, writes: Arc<AtomicUsize>) -> ToolRegistry {
    let corpus = Arc::new(FileCorpusStore::new(root.join("knowledge")));
    ingest(
        corpus.as_ref(),
        Document {
            id: "approval".into(),
            title: "Approval policy".into(),
            source: "fixture:approval".into(),
            text: "Approval binds the exact operation arguments.".into(),
            metadata: Default::default(),
        },
        &WindowChunker::default(),
        None,
    )
    .await
    .unwrap();
    let mut tools = ToolRegistry::new();
    tools
        .register(RagTool::new(
            "knowledge_search",
            corpus,
            Arc::new(LexicalRetriever),
        ))
        .unwrap();
    tools
        .register(FnTool::new(
            ToolDefinition {
                name: "save".into(),
                description: "effectful fixture".into(),
                effect: ToolEffect::NonIdempotent,
                permissions: BTreeSet::from(["notes:write".into()]),
                input_schema: json!({"type":"object","additionalProperties":false}),
            },
            move |_, _| {
                let writes = writes.clone();
                async move {
                    writes.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"saved":true}))
                }
            },
        ))
        .unwrap();
    tools
}
fn agent<'a>(model: &'a dyn Model, tools: &'a ToolRegistry, repairs: usize) -> AgentLoop<'a> {
    AgentLoop::new(model, tools, "fixture")
        .with_policy(
            ToolPolicy::default()
                .allow("knowledge_search")
                .grant("knowledge:read")
                .allow("save")
                .grant("notes:write"),
        )
        .with_limits(RunLimits {
            max_cost_micros: Some(100),
            ..Default::default()
        })
        .with_context(ContextPolicy::default())
        .with_output_policy(FinalOutputPolicy::cited_report(repairs))
}

#[tokio::test]
async fn invalid_citations_are_repaired_in_the_same_durable_run_and_reentry_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = tools(dir.path(), writes.clone()).await;
    let model = CitationModel::new(Mode::Correct);
    let agent = agent(&model, &tools, 1);
    let store = FileRunStore::new(dir.path().join("runs"));
    let durable = DurableAgent::new(&agent, &store);
    let state = durable
        .start("cited", vec![Message::user("research")])
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::Completed);
    state.validate().unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(state.report.budget.committed_micros, 30);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert_eq!(state.report.tool_calls, 1);
    assert!(state
        .report
        .events
        .iter()
        .any(|e| matches!(e.kind, RunEventKind::OutputRejected { repair: true, .. })));
    {
        let requests = model.requests.lock().unwrap();
        assert!(requests[0].tools.iter().any(|d| d.name == "save"));
        assert!(requests[2]
            .tools
            .iter()
            .all(|d| d.name == "knowledge_search"));
    }
    assert_eq!(
        durable.resume("cited").await.unwrap().revision,
        state.revision
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    let mut corrupt = state.clone();
    corrupt
        .report
        .events
        .retain(|e| !matches!(e.kind, RunEventKind::OutputValidated { .. }));
    assert!(corrupt.validate().is_err());
}

#[tokio::test]
async fn plain_loop_applies_the_same_rejection_bound_and_does_not_execute_escape_tools() {
    let dir = tempfile::tempdir().unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = tools(dir.path(), writes.clone()).await;
    let model = CitationModel::new(Mode::Reject);
    let report = agent(&model, &tools, 1)
        .run("bad", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    assert!(report.final_text().is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        report
            .events
            .iter()
            .filter(|e| matches!(e.kind, RunEventKind::OutputRejected { .. }))
            .count(),
        2
    );
    let model = CitationModel::new(Mode::Escape);
    let store = FileRunStore::new(dir.path().join("runs"));
    let agent = agent(&model, &tools, 1);
    let state = DurableAgent::new(&agent, &store)
        .start("escape", vec![Message::user("research")])
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::PermissionDenied);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    assert!(state.tool_steps.iter().all(|step| step.call.name != "save"));
}

#[tokio::test]
async fn persisted_feedback_can_resume_with_narrowed_context_and_changed_contract_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let model = CitationModel::new(Mode::Correct);
    let pause = CancelToken::new();
    let sink = |event: &RunEvent| {
        if matches!(
            event.kind,
            RunEventKind::OutputRejected { repair: true, .. }
        ) {
            pause.cancel();
        }
    };
    let base = agent(&model, &tools, 1);
    let paused = base.clone().with_events(&sink);
    let store = FileRunStore::new(dir.path().join("runs"));
    let state = DurableAgent::new(&paused, &store)
        .with_pause(&pause)
        .start("paused", vec![Message::user("research")])
        .await
        .unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    state.validate().unwrap();
    let changed = base
        .clone()
        .with_output_policy(FinalOutputPolicy::cited_report(0));
    assert!(matches!(
        DurableAgent::new(&changed, &store).resume("paused").await,
        Err(RuntimeError::Configuration(_))
    ));
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    let done = DurableAgent::new(&base, &store)
        .resume("paused")
        .await
        .unwrap();
    assert_eq!(done.report.stop_reason, StopReason::Completed);
    done.validate().unwrap();
    let mut corrupt = done;
    if let Some(RunEvent {
        kind: RunEventKind::OutputRejected { attempt, .. },
        ..
    }) = corrupt
        .report
        .events
        .iter_mut()
        .find(|e| matches!(e.kind, RunEventKind::OutputRejected { .. }))
    {
        *attempt = 99;
    }
    assert!(corrupt.validate().is_err());
}

#[tokio::test]
async fn removed_retrieval_tool_does_not_block_administrative_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let model = CitationModel::new(Mode::Correct);
    let pause = CancelToken::new();
    let sink = |event: &RunEvent| {
        if matches!(event.kind, RunEventKind::OutputRejected { .. }) {
            pause.cancel();
        }
    };
    let original = agent(&model, &tools, 1).with_events(&sink);
    let store = FileRunStore::new(dir.path().join("runs"));
    DurableAgent::new(&original, &store)
        .with_pause(&pause)
        .start("cancel", vec![Message::user("research")])
        .await
        .unwrap();
    let empty = ToolRegistry::new();
    let removed = AgentLoop::new(&model, &empty, "fixture")
        .with_output_policy(FinalOutputPolicy::cited_report(1));
    let state = DurableAgent::new(&removed, &store)
        .cancel("cancel", "operator", "stop")
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::Cancelled);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_repair_can_retrieve_fresh_evidence_but_history_without_execution_is_not_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let model = CitationModel::new(Mode::RetrieveDuringRepair);
    let report = agent(&model, &tools, 1)
        .run("lookup", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::Completed);
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    let found = report
        .messages
        .iter()
        .find_map(|m| match m {
            Message::Tool { result } => Some(result.clone()),
            _ => None,
        })
        .unwrap();
    let model = CitationModel::new(Mode::Correct);
    let call = ToolCall {
        id: found.call_id.clone(),
        name: "knowledge_search".into(),
        arguments: json!({"query":"approval"}),
    };
    let history = vec![
        Message::user("old"),
        Message::Assistant {
            turn: AssistantTurn {
                content: String::new(),
                tool_calls: vec![call],
            },
        },
        Message::Tool { result: found },
        Message::user("new"),
    ];
    let report = agent(&model, &tools, 0).run("history", history).await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    assert!(report
        .detail
        .unwrap()
        .contains("no citation evidence was retrieved"));
}

struct JsonModel(AtomicUsize);
impl Model for JsonModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(10).await?.settle(10);
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelOutput {
                model: request.model.clone(),
                turn: AssistantTurn::text(if n == 0 {
                    "not JSON"
                } else {
                    "{\"answer\":true}"
                }),
            })
        })
    }
}
fn json_policy(repairs: usize) -> FinalOutputPolicy {
    FinalOutputPolicy::JsonSchema {
        schema: json!({"type":"object","properties":{"answer":{"type":"boolean"}},"required":["answer"],"additionalProperties":false}),
        max_repairs: repairs,
    }
}

#[tokio::test]
async fn repair_requires_a_spend_cap_and_an_exhausted_cap_prevents_redispatch() {
    let tools = ToolRegistry::new();
    let model = JsonModel(AtomicUsize::new(0));
    let report = AgentLoop::new(&model, &tools, "fixture")
        .with_output_policy(json_policy(1))
        .run("uncapped", vec![Message::user("x")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ConfigurationError);
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_limits(RunLimits {
            max_cost_micros: Some(10),
            ..Default::default()
        })
        .with_output_policy(json_policy(1));
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let state = DurableAgent::new(&agent, &store)
        .start("capped", vec![Message::user("x")])
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::Budget);
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
    assert_eq!(state.report.budget.committed_micros, 10);
}

#[tokio::test]
async fn output_feedback_capacity_is_reserved_before_any_model_dispatch() {
    let tools = ToolRegistry::new();
    let model = JsonModel(AtomicUsize::new(0));
    let agent = AgentLoop::new(&model, &tools, "fixture")
        .with_limits(RunLimits {
            max_cost_micros: Some(100),
            max_model_result_bytes: 256,
            max_tool_calls: 0,
            ..Default::default()
        })
        .with_output_policy(json_policy(1));
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path()).with_max_bytes(30 * 1024);
    let state = DurableAgent::new(&agent, &store)
        .start("space", vec![Message::user("x")])
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::StorageLimit);
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn validated_workflow_child_returns_typed_json_only_after_success_and_never_replays() {
    let dir = tempfile::tempdir().unwrap();
    let tools = ToolRegistry::new();
    let model = JsonModel(AtomicUsize::new(0));
    let agent = AgentLoop::new(&model, &tools, "fixture").with_limits(RunLimits {
        max_cost_micros: Some(100),
        ..Default::default()
    });
    let runs = FileRunStore::new(dir.path().join("runs"));
    let workflows = FileWorkflowStore::new(dir.path().join("workflows"));
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(
            AgentWorkflowTask::new(
                "writer",
                &agent,
                &runs,
                "write JSON",
                WorkflowOutput::Validated {
                    policy: json_policy(1),
                },
            )
            .unwrap(),
        )
        .unwrap();
    let runner = WorkflowRunner::new(&tasks, &workflows).with_limits(WorkflowLimits {
        max_cost_micros: Some(100),
        ..Default::default()
    });
    let done = runner
        .start(
            "validated",
            WorkflowDefinition::new(vec![WorkflowNode::new("draft", "writer")]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!(done.output("draft"), Some(&json!({"answer":true})));
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
    let child = runs
        .inspect(&done.node("draft").unwrap().child.as_ref().unwrap().run_id)
        .await
        .unwrap()
        .unwrap();
    assert!(child.config.output_policy.is_some());
    child.validate().unwrap();
    assert_eq!(
        runner.resume("validated").await.unwrap().revision,
        done.revision
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn legacy_checkpoints_without_an_output_policy_keep_their_configuration_and_reentry() {
    let dir = tempfile::tempdir().unwrap();
    let tools = ToolRegistry::new();
    let model = JsonModel(AtomicUsize::new(1));
    let agent = AgentLoop::new(&model, &tools, "fixture").with_limits(RunLimits {
        max_cost_micros: Some(100),
        ..Default::default()
    });
    let store = FileRunStore::new(dir.path());
    let durable = DurableAgent::new(&agent, &store);
    let state = durable
        .start("legacy", vec![Message::user("x")])
        .await
        .unwrap();
    let wire = serde_json::to_value(&state).unwrap();
    assert!(wire["config"].get("output_policy").is_none());
    let restored: RunState = serde_json::from_value(wire.clone()).unwrap();
    restored.validate().unwrap();
    assert_eq!(serde_json::to_value(&restored).unwrap(), wire);
    assert_eq!(
        durable.resume("legacy").await.unwrap().revision,
        state.revision
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn citation_contract_rejects_missing_required_documents_and_effectful_readers() {
    let dir = tempfile::tempdir().unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = tools(dir.path(), writes.clone()).await;
    let model = CitationModel::new(Mode::Correct);
    let contract = FinalOutputPolicy::CitedReport {
        retrieval_tools: BTreeSet::from(["knowledge_search".into()]),
        min_sources: 1,
        required_documents: BTreeSet::from(["absent-document".into()]),
        max_repairs: 1,
    };
    let report = agent(&model, &tools, 1)
        .with_output_policy(contract)
        .run("coverage", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    assert!(report.detail.unwrap().contains("citation coverage"));
    let before = model.calls.load(Ordering::SeqCst);
    let contract = FinalOutputPolicy::CitedReport {
        retrieval_tools: BTreeSet::from(["save".into()]),
        min_sources: 1,
        required_documents: BTreeSet::new(),
        max_repairs: 1,
    };
    let report = agent(&model, &tools, 1)
        .with_output_policy(contract)
        .run("bad-reader", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ConfigurationError);
    assert_eq!(model.calls.load(Ordering::SeqCst), before);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}
