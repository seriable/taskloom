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

#[tokio::test]
async fn schema_repairs_receive_constraint_path_without_dumping_the_invalid_answer() {
    struct LongAnswer(AtomicUsize);
    impl Model for LongAnswer {
        fn generate<'a>(
            &'a self,
            request: &'a ModelRequest,
            budget: &'a RunBudget,
        ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
            Box::pin(async move {
                budget.reserve_before_dispatch(0).await?.settle(0);
                let call = self.0.fetch_add(1, Ordering::SeqCst);
                let value = if call == 0 {
                    json!({"text":"PRIVATE_INVALID_VALUE".repeat(180)})
                } else {
                    let feedback = request
                        .messages
                        .iter()
                        .filter_map(|m| {
                            if let Message::User { content } = m {
                                Some(content)
                            } else {
                                None
                            }
                        })
                        .find(|s| s.contains("Host final-output validation feedback"))
                        .unwrap();
                    assert!(feedback.contains("/text"));
                    assert!(feedback.contains("/properties/text/maxLength"));
                    assert!(feedback.contains("2 characters"));
                    assert!(!feedback.contains("PRIVATE_INVALID_VALUE"));
                    json!({"text":"ok"})
                };
                Ok(ModelOutput {
                    model: request.model.clone(),
                    turn: AssistantTurn::text(value.to_string()),
                })
            })
        }
    }
    let model = LongAnswer(AtomicUsize::new(0));
    let tools = ToolRegistry::new();
    let report=AgentLoop::new(&model,&tools,"schema-feedback")
        .with_limits(RunLimits{max_cost_micros:Some(0),..Default::default()})
        .with_output_policy(FinalOutputPolicy::JsonSchema{schema:json!({"type":"object","properties":{"text":{"type":"string","maxLength":2}},"required":["text"]}),max_repairs:1})
        .run("bounded-diagnostic",vec![Message::user("answer")]).await;
    assert_eq!(report.stop_reason, StopReason::Completed);
    assert_eq!(model.0.load(Ordering::SeqCst), 2);
}

#[derive(Clone, Copy)]
enum Mode {
    Correct,
    Reject,
    Escape,
    RetrieveDuringRepair,
    SectionRepair,
}
struct CitationModel {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    mode: Mode,
    located: bool,
    selected: bool,
}
impl CitationModel {
    fn new(mode: Mode) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            mode,
            located: false,
            selected: false,
        }
    }
    fn located(mode: Mode) -> Self {
        Self {
            located: true,
            ..Self::new(mode)
        }
    }
    fn selected(mode: Mode) -> Self {
        Self {
            selected: true,
            ..Self::new(mode)
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
                let valid = (repair || matches!(self.mode, Mode::SectionRepair))
                    && !matches!(self.mode, Mode::Reject);
                let text = if valid {
                    format!(
                        "{} [[{}]].",
                        if repair && matches!(self.mode, Mode::SectionRepair) {
                            "Approval checklist: inspect the exact arguments"
                        } else {
                            "Check the operation"
                        },
                        if self.located { "1" } else { &chunk.id }
                    )
                } else {
                    "No marker in this invalid draft.".into()
                };
                let citation = if self.located {
                    json!({"document_id":chunk.document_id,"corpus_revision":evidence.corpus_revision,"start":chunk.start,"quote":chunk.text})
                } else {
                    json!({"chunk_id":chunk.id,"quote":chunk.text})
                };
                if self.selected {
                    AssistantTurn::text(json!({"text":text,"sources":[chunk.id]}).to_string())
                } else {
                    AssistantTurn::text(json!({"text":text,"citations":[citation]}).to_string())
                }
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

#[tokio::test]
async fn cited_answer_schema_repairs_structure_in_same_run_and_binds_original_format() {
    for kind in 0..4 {
        let located = kind == 1;
        let selected = kind >= 2;
        let dir = tempfile::tempdir().unwrap();
        let writes = Arc::new(AtomicUsize::new(0));
        let tools = tools(dir.path(), writes.clone()).await;
        let model = if selected {
            CitationModel::selected(Mode::SectionRepair)
        } else if located {
            CitationModel::located(Mode::SectionRepair)
        } else {
            CitationModel::new(Mode::SectionRepair)
        };
        let base = if kind == 3 {
            FinalOutputPolicy::selected_sources(1)
                .with_host_bibliography()
                .unwrap()
        } else if selected {
            FinalOutputPolicy::selected_sources(1)
        } else if located {
            FinalOutputPolicy::located_cited_report(1)
        } else {
            FinalOutputPolicy::cited_report(1)
        };
        let legacy = serde_json::to_value(&base).unwrap();
        assert!(legacy.get("answer_schema").is_none());
        assert_eq!(
            serde_json::from_value::<FinalOutputPolicy>(legacy)
                .unwrap()
                .identity()
                .unwrap(),
            base.identity().unwrap()
        );
        let schema = json!({"type":"object","required":["text",if selected{"sources"}else{"citations"}],"properties":{
            "text":{"type":"string","pattern":if located {"^Approval checklist:.*\\[\\[1\\]\\]"} else {"^Approval checklist:"}},
            (if selected{"sources"}else{"citations"}):{"type":"array","minItems":1,"items":if selected {json!({"type":"string"})}else{json!({"type":"object","required":if located {vec!["document_id","corpus_revision","start","quote"]} else {vec!["chunk_id","quote"]}})}}
        }});
        let policy = base.clone().with_answer_schema(schema).unwrap();
        assert_ne!(base.identity().unwrap(), policy.identity().unwrap());
        let agent = agent(&model, &tools, 1).with_output_policy(policy.clone());
        let store = FileRunStore::new(dir.path().join("runs"));
        let durable = DurableAgent::new(&agent, &store);
        let done = durable
            .start(
                "sections",
                vec![Message::user("write an approval checklist")],
            )
            .await
            .unwrap();
        assert_eq!(done.report.stop_reason, StopReason::Completed);
        assert_eq!(done.report.budget.committed_micros, 30);
        let rejections = done
            .report
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::OutputRejected { detail, .. } => Some(detail),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(rejections.len(), 1);
        assert!(rejections[0].contains("cited answer schema rejected"));
        assert_eq!(done.report.tool_calls, 1);
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert!(model.requests.lock().unwrap()[2]
            .tools
            .iter()
            .all(|t| t.name == "knowledge_search"));
        assert_eq!(
            durable.resume("sections").await.unwrap().revision,
            done.revision
        );
        let changed = agent.with_output_policy(base);
        assert!(matches!(
            DurableAgent::new(&changed, &store).resume("sections").await,
            Err(RuntimeError::Configuration(_))
        ));
        assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn additional_citation_schema_is_validated_and_old_none_is_identity_compatible() {
    let base = FinalOutputPolicy::cited_report(1);
    assert!(base
        .clone()
        .with_answer_schema(json!({"type":"invented-type"}))
        .is_err());
    assert!(base
        .clone()
        .with_answer_schema(json!({"description":"a".repeat(128*1024)}))
        .is_err());
    let old: FinalOutputPolicy =
        serde_json::from_value(json!({"type":"cited_report","max_repairs":1})).unwrap();
    assert_eq!(base.identity().unwrap(), old.identity().unwrap());
    let explicit: FinalOutputPolicy =
        serde_json::from_value(json!({"type":"cited_report","max_repairs":1,"answer_schema":null}))
            .unwrap();
    assert_eq!(base.identity().unwrap(), explicit.identity().unwrap());
}

#[tokio::test]
async fn selected_sources_have_run_local_evidence_bounded_repairs_and_explicit_host_rendering() {
    let dir = tempfile::tempdir().unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = tools(dir.path(), writes.clone()).await;
    let model = CitationModel::selected(Mode::Correct);
    let policy=FinalOutputPolicy::selected_sources(1).with_answer_schema(json!({"type":"object","required":["text","sources"],"properties":{"sources":{"type":"array","minItems":1,"items":{"type":"string","pattern":"^[a-f0-9]{64}$"}}}})).unwrap();
    let selected_agent = agent(&model, &tools, 1).with_output_policy(policy.clone());
    let store = FileRunStore::new(dir.path().join("runs"));
    let durable = DurableAgent::new(&selected_agent, &store);
    let done = durable
        .start("selected", vec![Message::user("research")])
        .await
        .unwrap();
    assert_eq!(done.report.stop_reason, StopReason::Completed);
    let original: SourceSelectionReport =
        serde_json::from_str(done.report.final_text().unwrap()).unwrap();
    let verified = policy.verified_citations(&done.report).unwrap();
    assert_eq!(verified.report.citations[0].chunk_id, original.sources[0]);
    assert_eq!(
        verified.report.citations[0].quote,
        "Approval binds the exact operation arguments."
    );
    assert!(serde_json::from_str::<CitedReport>(done.report.final_text().unwrap()).is_err());
    assert_eq!(
        policy.validated_value(&done.report).unwrap(),
        serde_json::to_value(&verified.report).unwrap()
    );
    assert!(model.requests.lock().unwrap()[2]
        .tools
        .iter()
        .all(|t| t.name == "knowledge_search"));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    let mut forged = done.report.clone();
    forged.events.clear();
    assert!(policy
        .verified_citations(&forged)
        .unwrap_err()
        .contains("no citation evidence"));
    assert_eq!(
        durable.resume("selected").await.unwrap().revision,
        done.revision
    );
    let incompatible = selected_agent.with_output_policy(FinalOutputPolicy::cited_report(1));
    assert!(matches!(
        DurableAgent::new(&incompatible, &store)
            .resume("selected")
            .await,
        Err(RuntimeError::Configuration(_))
    ));
    #[cfg(feature = "app")]
    {
        let view = taskloom::runtime::app::verify_report_with_provenance(&done).unwrap();
        assert_eq!(view["rendering"]["type"], "host_selected_source_bodies");
        assert_eq!(view["rendering"]["quotations_generated_by_host"], true);
        assert_eq!(
            view["rendering"]["original"],
            serde_json::to_value(original).unwrap()
        );
    }
    let escape = CitationModel::selected(Mode::Escape);
    let escaped = agent(&escape, &tools, 1)
        .with_output_policy(FinalOutputPolicy::selected_sources(1))
        .run("selected-escape", vec![Message::user("research")])
        .await;
    assert_eq!(escaped.stop_reason, StopReason::PermissionDenied);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[test]
fn bibliography_is_explicit_and_identity_bound() {
    let inline = FinalOutputPolicy::selected_sources(1);
    let bibliography = inline.clone().with_host_bibliography().unwrap();
    assert_ne!(inline.identity().unwrap(), bibliography.identity().unwrap());
    assert!(serde_json::to_value(&inline)
        .unwrap()
        .get("bibliography")
        .is_none());
    assert_eq!(
        serde_json::to_value(&bibliography).unwrap()["bibliography"],
        true
    );
    assert!(FinalOutputPolicy::cited_report(1)
        .with_host_bibliography()
        .is_err());
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
        answer_schema: None,
    };
    let report = agent(&model, &tools, 1)
        .with_output_policy(contract)
        .run("coverage", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    let detail = report.detail.unwrap();
    assert!(detail.contains("citation coverage"));
    assert!(detail.contains("absent-document"));
    assert!(detail.contains("1 missing documents"));
    let before = model.calls.load(Ordering::SeqCst);
    let contract = FinalOutputPolicy::CitedReport {
        retrieval_tools: BTreeSet::from(["save".into()]),
        min_sources: 1,
        required_documents: BTreeSet::new(),
        max_repairs: 1,
        answer_schema: None,
    };
    let report = agent(&model, &tools, 1)
        .with_output_policy(contract)
        .run("bad-reader", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ConfigurationError);
    assert_eq!(model.calls.load(Ordering::SeqCst), before);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn located_contract_survives_pause_and_restart_without_mutating_model_text() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let model = CitationModel::located(Mode::Correct);
    let policy = FinalOutputPolicy::located_cited_report(1);
    let base = agent(&model, &tools, 1).with_output_policy(policy.clone());
    let pause = CancelToken::new();
    let sink = |event: &RunEvent| {
        if matches!(
            event.kind,
            RunEventKind::OutputRejected { repair: true, .. }
        ) {
            pause.cancel();
        }
    };
    let paused = base.clone().with_events(&sink);
    let store = FileRunStore::new(dir.path().join("runs"));
    let state = DurableAgent::new(&paused, &store)
        .with_pause(&pause)
        .start("located", vec![Message::user("research")])
        .await
        .unwrap();
    assert_eq!(state.status, RunStatus::Paused);
    state.validate().unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    // A format switch changes the frozen contract, even with equal limits.
    let legacy = base
        .clone()
        .with_output_policy(FinalOutputPolicy::cited_report(1));
    assert!(matches!(
        DurableAgent::new(&legacy, &store).resume("located").await,
        Err(RuntimeError::Configuration(_))
    ));
    let done = DurableAgent::new(&base, &store)
        .resume("located")
        .await
        .unwrap();
    done.validate().unwrap();
    assert_eq!(done.report.stop_reason, StopReason::Completed);
    assert_eq!(done.report.budget.committed_micros, 30);
    let raw: LocatedReport = serde_json::from_str(done.report.final_text().unwrap()).unwrap();
    assert_eq!(raw.text, "Check the operation [[1]].");
    let resolved = policy.resolve_located(&done.report).unwrap();
    assert_eq!(resolved.original, raw);
    assert!(resolved
        .verified
        .report
        .text
        .contains(&resolved.mappings[0].chunk_id));
    assert_eq!(model.requests.lock().unwrap()[2].tools.len(), 1);
    assert_eq!(
        DurableAgent::new(&base, &store)
            .resume("located")
            .await
            .unwrap()
            .revision,
        done.revision
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    #[cfg(feature = "app")]
    {
        let view = taskloom::runtime::app::verify_report_with_provenance(&done).unwrap();
        assert_eq!(
            view["rendering"]["original"],
            serde_json::to_value(raw).unwrap()
        );
        assert_eq!(view["rendering"]["model_transcript_unchanged"], true);
        assert_eq!(
            view["report"],
            policy.validated_value(&done.report).unwrap()
        );
        let mut corrupt = done.clone();
        corrupt
            .report
            .events
            .retain(|e| !matches!(e.kind, RunEventKind::OutputValidated { .. }));
        assert!(taskloom::runtime::app::verify_report_with_provenance(&corrupt).is_err());
    }
}

#[tokio::test]
async fn located_contract_requires_actual_tool_execution_and_denies_repair_writes() {
    let dir = tempfile::tempdir().unwrap();
    let writes = Arc::new(AtomicUsize::new(0));
    let tools = tools(dir.path(), writes.clone()).await;
    let model = CitationModel::located(Mode::Correct);
    let policy = FinalOutputPolicy::located_cited_report(1);
    let report = agent(&model, &tools, 1)
        .with_output_policy(policy.clone())
        .run("actual", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::Completed);
    let mut history = report.clone();
    history.events.clear();
    assert!(policy
        .resolve_located(&history)
        .unwrap_err()
        .contains("no citation evidence"));
    let mut failed = report.clone();
    for event in &mut failed.events {
        if let RunEventKind::ToolFinished { is_error, .. } = &mut event.kind {
            *is_error = true;
        }
    }
    assert!(policy.resolve_located(&failed).is_err());
    let escape = CitationModel::located(Mode::Escape);
    let denied = agent(&escape, &tools, 1)
        .with_output_policy(policy.clone())
        .run("escape-located", vec![Message::user("research")])
        .await;
    assert_eq!(denied.stop_reason, StopReason::PermissionDenied);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    let capped = agent(&model, &tools, 1)
        .with_output_policy(policy)
        .with_limits(RunLimits {
            max_cost_micros: Some(20),
            ..Default::default()
        })
        .run("located-cap", vec![Message::user("research")])
        .await;
    assert_eq!(capped.stop_reason, StopReason::Budget);
    assert_eq!(capped.budget.committed_micros, 20);
}

#[tokio::test]
async fn located_workflow_exposes_canonical_output_and_retains_raw_child_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let model = CitationModel::located(Mode::Correct);
    let agent = agent(&model, &tools, 1);
    let runs = FileRunStore::new(dir.path().join("runs"));
    let workflows = FileWorkflowStore::new(dir.path().join("workflows"));
    let policy = FinalOutputPolicy::located_cited_report(1);
    let mut tasks = WorkflowTasks::new();
    tasks
        .register(
            AgentWorkflowTask::new(
                "analyst",
                &agent,
                &runs,
                "located JSON",
                WorkflowOutput::Validated {
                    policy: policy.clone(),
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
            "located-wf",
            WorkflowDefinition::new(vec![WorkflowNode::new("draft", "analyst")]),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    let child = runs
        .inspect(&done.node("draft").unwrap().child.as_ref().unwrap().run_id)
        .await
        .unwrap()
        .unwrap();
    child.validate().unwrap();
    let resolved = policy.resolve_located(&child.report).unwrap();
    assert_eq!(
        done.output("draft"),
        Some(&serde_json::to_value(&resolved.verified.report).unwrap())
    );
    assert_eq!(resolved.original.text, "Check the operation [[1]].");
    assert_ne!(
        serde_json::to_value(resolved.original).unwrap(),
        done.output("draft").unwrap().clone()
    );
    assert_eq!(
        runner.resume("located-wf").await.unwrap().revision,
        done.revision
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn located_coverage_and_reader_effects_remain_host_owned() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(dir.path(), Arc::new(AtomicUsize::new(0))).await;
    let model = CitationModel::located(Mode::Correct);
    let policy = FinalOutputPolicy::LocatedCitedReport {
        retrieval_tools: BTreeSet::from(["knowledge_search".into()]),
        min_sources: 1,
        required_documents: BTreeSet::from(["missing".into()]),
        max_repairs: 1,
        answer_schema: None,
    };
    let report = agent(&model, &tools, 1)
        .with_output_policy(policy)
        .run("located-coverage", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ProtocolError);
    let detail = report.detail.unwrap();
    assert!(detail.contains("citation coverage"));
    assert!(detail.contains("missing"));
    let before = model.calls.load(Ordering::SeqCst);
    let policy = FinalOutputPolicy::LocatedCitedReport {
        retrieval_tools: BTreeSet::from(["save".into()]),
        min_sources: 1,
        required_documents: Default::default(),
        max_repairs: 1,
        answer_schema: None,
    };
    let report = agent(&model, &tools, 1)
        .with_output_policy(policy)
        .run("located-effect", vec![Message::user("research")])
        .await;
    assert_eq!(report.stop_reason, StopReason::ConfigurationError);
    assert_eq!(model.calls.load(Ordering::SeqCst), before);
}
