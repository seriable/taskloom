#![cfg(feature = "runtime")]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use taskloom::runtime::*;

#[derive(Default)]
struct TestModel {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
    tools: bool,
    fail: bool,
}
impl Model for TestModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            if self.fail {
                return Err(RuntimeError::Protocol("injected model failure".into()));
            }
            let last_user = request
                .messages
                .iter()
                .rposition(|m| matches!(m, Message::User { .. }))
                .unwrap();
            let observed = request.messages[last_user + 1..]
                .iter()
                .any(|m| matches!(m, Message::Tool { .. }));
            let turn = if self.tools && !observed {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: format!("write-{}", request.messages.len()),
                        name: "write".into(),
                        arguments: json!({"n":3}),
                    }],
                }
            } else {
                let Message::User { content } = &request.messages[last_user] else {
                    unreachable!()
                };
                AssistantTurn::text(format!("reply: {content}"))
            };
            Ok(ModelOutput {
                turn,
                model: request.model.clone(),
            })
        })
    }
}

fn registry(count: Arc<AtomicUsize>, uncertain: bool) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register(FnTool::new(ToolDefinition {name:"write".into(),description:"Test effect".into(),input_schema:json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false}),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::new()},move |_:Value,_:ToolContext| {
        let count=Arc::clone(&count);
        async move {count.fetch_add(1,Ordering::SeqCst);if uncertain {Err(ToolError("remote result is unknown".into()))} else {Ok(json!(6))}}
    })).unwrap();
    tools
}
fn approve() -> ApprovalDecision {
    ApprovalDecision {
        approved: true,
        actor: "operator".into(),
        note: "reviewed".into(),
    }
}

#[tokio::test]
async fn successive_session_turns_keep_full_history_while_model_context_is_compressed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager
        .create("chat", vec!["host instructions".into()])
        .await
        .unwrap();
    let model = TestModel::default();
    let tools = ToolRegistry::new();
    let context = ContextPolicy {
        max_input_bytes: 2200,
        keep_recent_blocks: 1,
        max_summary_bytes: 100,
    };
    let agent = AgentLoop::new(&model, &tools, "test").with_context(context);
    let durable = DurableAgent::new(&agent, &runs);
    let first = "第一轮输入 ".repeat(100);
    let second = "second question";
    let third = "third question";
    manager
        .run_turn(&durable, "chat", "first", &first)
        .await
        .unwrap();
    manager
        .run_turn(&durable, "chat", "second", second)
        .await
        .unwrap();
    let complete = manager
        .run_turn(&durable, "chat", "third", third)
        .await
        .unwrap();
    assert_eq!(complete.session.messages.len(), 7);
    assert_eq!(complete.session.turns.len(), 3);
    assert_eq!(complete.session.revision, 7);
    assert_eq!(complete.session.messages[1], Message::user(&first));
    assert!(complete.run.report.contexts[0].omitted_messages() > 0);
    let calls = model.calls.load(Ordering::SeqCst);
    let replay = manager
        .run_turn(&durable, "chat", "first", &first)
        .await
        .unwrap();
    assert_eq!(replay.session.revision, 7);
    assert_eq!(model.calls.load(Ordering::SeqCst), calls);
    assert!(manager
        .run_turn(&durable, "chat", "first", "different input")
        .await
        .is_err());
    let loaded = FileSessionStore::new(dir.path().join("sessions"))
        .load("chat")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded, complete.session);
}

#[tokio::test]
async fn active_approval_turn_blocks_other_turns_and_resumes_after_store_reconstruction() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec!["host".into()]).await.unwrap();
    let model = TestModel {
        tools: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&count), false);
    let agent =
        AgentLoop::new(&model, &tools, "test").with_policy(ToolPolicy::default().allow("write"));
    let durable = DurableAgent::new(&agent, &runs);
    let waiting = manager
        .run_turn(&durable, "chat", "one", "write once")
        .await
        .unwrap();
    assert_eq!(waiting.run.status, RunStatus::WaitingApproval);
    assert_eq!(waiting.session.active.as_ref().unwrap().turn_id, "one");
    assert!(matches!(
        manager.begin_turn("chat", "two", "new question").await,
        Err(RuntimeError::Conflict(_))
    ));
    let duplicate = manager.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(duplicate.run.revision, waiting.run.revision);
    let ticket = waiting.run.pending_approval().unwrap();
    durable
        .decide(&ticket.run_id, &ticket.request_id, approve())
        .await
        .unwrap();
    let fresh_store = FileSessionStore::new(dir.path().join("sessions"));
    let fresh = SessionManager::new(&fresh_store);
    let completed = fresh.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(completed.run.report.stop_reason, StopReason::Completed);
    assert!(completed.session.active.is_none());
    assert_eq!(completed.session.turns.len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    fresh.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recovery_covers_run_creation_and_completion_before_session_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel::default();
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test");
    let durable = DurableAgent::new(&agent, &runs);
    manager
        .begin_turn("chat", "before-create", "question")
        .await
        .unwrap();
    manager
        .resume_turn(&durable, "chat", "before-create")
        .await
        .unwrap();
    let input = manager
        .begin_turn("chat", "before-commit", "another question")
        .await
        .unwrap();
    let state = durable
        .start_or_resume(&input.run_id, input.messages)
        .await
        .unwrap();
    assert_eq!(state.report.stop_reason, StopReason::Completed);
    assert!(manager.load("chat").await.unwrap().active.is_some());
    let committed = SessionManager::new(&store)
        .resume_turn(&durable, "chat", "before-commit")
        .await
        .unwrap();
    assert_eq!(committed.session.turns.len(), 2);
    assert!(committed.session.active.is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn run_ids_are_session_scoped_and_wrong_session_results_cannot_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    for id in ["a", "b"] {
        manager.create(id, vec![]).await.unwrap();
    }
    let a = manager.begin_turn("a", "same", "same input").await.unwrap();
    let b = manager.begin_turn("b", "same", "same input").await.unwrap();
    assert_ne!(a.run_id, b.run_id);
    let model = TestModel::default();
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test");
    let durable = DurableAgent::new(&agent, &runs);
    let run = durable
        .start_or_resume(&a.run_id, a.messages)
        .await
        .unwrap();
    assert!(manager.commit_turn("b", "same", &run).await.is_err());
    assert!(manager.load("b").await.unwrap().active.is_some());
    manager.commit_turn("a", "same", &run).await.unwrap();
}

#[tokio::test]
async fn failed_terminal_turn_needs_operator_acknowledgement_and_keeps_known_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel {
        fail: true,
        ..Default::default()
    };
    let tools = ToolRegistry::new();
    let agent = AgentLoop::new(&model, &tools, "test");
    let durable = DurableAgent::new(&agent, &runs);
    let failed = manager
        .run_turn(&durable, "chat", "failed", "first question")
        .await
        .unwrap();
    assert_eq!(failed.run.report.stop_reason, StopReason::ProtocolError);
    assert!(manager
        .commit_turn("chat", "failed", &failed.run)
        .await
        .is_err());
    assert!(manager
        .begin_turn("chat", "next", "next question")
        .await
        .is_err());
    let closed = manager
        .abandon_turn(
            "chat",
            "failed",
            &failed.run,
            "operator",
            "acknowledged the model failure",
        )
        .await
        .unwrap();
    assert!(closed.active.is_none());
    assert_eq!(closed.turns[0].outcome, StopReason::ProtocolError);
    assert!(
        matches!(&closed.messages[1],Message::User {content} if content.contains("Host record"))
    );
    let healthy = TestModel::default();
    let agent = AgentLoop::new(&healthy, &tools, "test");
    let done = manager
        .run_turn(
            &DurableAgent::new(&agent, &runs),
            "chat",
            "next",
            "next question",
        )
        .await
        .unwrap();
    assert_eq!(done.session.turns.len(), 2);
    assert_eq!(
        &done.session.messages[..closed.messages.len()],
        closed.messages.as_slice()
    );
}

#[tokio::test]
async fn uncertain_effects_must_be_reconciled_before_abandoning_the_session_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel {
        tools: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&count), true);
    let agent =
        AgentLoop::new(&model, &tools, "test").with_policy(ToolPolicy::default().allow("write"));
    let durable = DurableAgent::new(&agent, &runs);
    let waiting = manager
        .run_turn(&durable, "chat", "one", "write")
        .await
        .unwrap();
    let ticket = waiting.run.pending_approval().unwrap();
    durable
        .decide(&ticket.run_id, &ticket.request_id, approve())
        .await
        .unwrap();
    let unknown = manager.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(unknown.run.status, RunStatus::NeedsRecovery);
    let run_id = unknown.run.report.run_id.clone();
    let call_id = unknown.run.tool_steps[0].call.id.clone();
    let cancelled = durable
        .cancel(&run_id, "operator", "stop pending verification")
        .await
        .unwrap();
    assert!(manager
        .abandon_turn(
            "chat",
            "one",
            &cancelled,
            "operator",
            "cannot ignore uncertainty"
        )
        .await
        .is_err());
    let reconciled = durable
        .reconcile(
            &run_id,
            &call_id,
            RecoveryDecision::RecordResult {
                output: json!("confirmed saved"),
                is_error: false,
                actor: "operator".into(),
                note: "verified remote state".into(),
            },
        )
        .await
        .unwrap();
    let session = manager
        .abandon_turn(
            "chat",
            "one",
            &reconciled,
            "operator",
            "known effects retained",
        )
        .await
        .unwrap();
    assert!(session
        .messages
        .iter()
        .any(|m| matches!(m,Message::Tool {result} if result.output==json!("confirmed saved"))));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abort_resolves_the_uncertain_tool_so_the_turn_can_be_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel {
        tools: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&count), true);
    let agent =
        AgentLoop::new(&model, &tools, "test").with_policy(ToolPolicy::default().allow("write"));
    let durable = DurableAgent::new(&agent, &runs);
    let waiting = manager
        .run_turn(&durable, "chat", "one", "write")
        .await
        .unwrap();
    let ticket = waiting.run.pending_approval().unwrap();
    durable
        .decide(&ticket.run_id, &ticket.request_id, approve())
        .await
        .unwrap();
    let unknown = manager.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(unknown.run.status, RunStatus::NeedsRecovery);
    let run_id = unknown.run.report.run_id.clone();
    let call_id = unknown.run.tool_steps[0].call.id.clone();
    let abort = || RecoveryDecision::Abort {
        actor: "operator".into(),
        note: "stop pending verification".into(),
    };
    let aborted = durable.reconcile(&run_id, &call_id, abort()).await.unwrap();
    assert_eq!(aborted.status, RunStatus::Finished);
    assert_eq!(aborted.report.stop_reason, StopReason::ToolError);
    assert!(matches!(
        aborted.tool_steps[0].progress,
        ToolProgress::Complete { ref result } if result.is_error
    ));
    assert!(matches!(
        durable.reconcile(&run_id, &call_id, abort()).await,
        Err(RuntimeError::Conflict(_))
    ));
    let session = manager
        .abandon_turn("chat", "one", &aborted, "operator", "outcome unknown")
        .await
        .unwrap();
    session.validate().unwrap();
    assert!(session.messages.iter().any(|m| matches!(m, Message::Tool { result }
        if result.is_error && result.output["host_error"].as_str().unwrap().contains("aborted"))));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rejected_retry_after_retry_safe_leaves_a_closable_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel {
        tools: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    let counter = Arc::clone(&count);
    tools.register(FnTool::new(ToolDefinition {name:"write".into(),description:"Idempotent effect".into(),input_schema:json!({"type":"object"}),effect:ToolEffect::Idempotent,permissions:BTreeSet::new()},move |_:Value,_:ToolContext| {
        let counter=Arc::clone(&counter);
        async move {counter.fetch_add(1,Ordering::SeqCst);Err::<Value,_>(ToolError("remote result is unknown".into()))}
    })).unwrap();
    let agent =
        AgentLoop::new(&model, &tools, "test").with_policy(ToolPolicy::default().allow("write"));
    let durable = DurableAgent::new(&agent, &runs);
    let waiting = manager
        .run_turn(&durable, "chat", "one", "write")
        .await
        .unwrap();
    let ticket = waiting.run.pending_approval().unwrap();
    durable
        .decide(&ticket.run_id, &ticket.request_id, approve())
        .await
        .unwrap();
    let unknown = manager.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(unknown.run.status, RunStatus::NeedsRecovery);
    let run_id = unknown.run.report.run_id.clone();
    let call_id = unknown.run.tool_steps[0].call.id.clone();
    durable
        .reconcile(
            &run_id,
            &call_id,
            RecoveryDecision::RetrySafe {
                actor: "operator".into(),
                note: "idempotent".into(),
            },
        )
        .await
        .unwrap();
    let retry = manager.resume_turn(&durable, "chat", "one").await.unwrap();
    assert_eq!(retry.run.status, RunStatus::WaitingApproval);
    let ticket = retry.run.pending_approval().unwrap();
    let rejected = durable
        .decide(
            &ticket.run_id,
            &ticket.request_id,
            ApprovalDecision {
                approved: false,
                actor: "operator".into(),
                note: "do not retry".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, RunStatus::Finished);
    assert_eq!(rejected.tool_steps[0].progress, ToolProgress::Pending);
    let session = manager
        .abandon_turn("chat", "one", &rejected, "operator", "retry rejected")
        .await
        .unwrap();
    session.validate().unwrap();
    assert!(session.active.is_none());
    assert!(session.messages.iter().any(|m| matches!(m, Message::Tool { result }
        if result.is_error && result.output["host_error"].as_str().unwrap().contains("retry not executed"))));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runs_can_be_cancelled_or_aborted_after_a_tool_definition_bump() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel {
        tools: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let old_tools = registry(Arc::clone(&count), true);
    let policy = ToolPolicy::default().allow("write");
    let old_agent = AgentLoop::new(&model, &old_tools, "test").with_policy(policy.clone());
    let old = DurableAgent::new(&old_agent, &runs);
    // A host upgrade bumps the advertised definition of the same tool.
    let mut new_tools = ToolRegistry::new();
    new_tools.register(FnTool::new(ToolDefinition {name:"write".into(),description:"Test effect v2".into(),input_schema:json!({"type":"object"}),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::new()},|_:Value,_:ToolContext| async {Ok(json!(0))})).unwrap();
    let new_agent = AgentLoop::new(&model, &new_tools, "test").with_policy(policy);
    let upgraded = DurableAgent::new(&new_agent, &runs);

    // A run waiting for approval: approval and resume stay strict, while
    // rejection and cancellation can still terminate it.
    let waiting = manager.run_turn(&old, "chat", "one", "write").await.unwrap();
    let ticket = waiting.run.pending_approval().unwrap().clone();
    assert!(matches!(
        upgraded.decide(&ticket.run_id, &ticket.request_id, approve()).await,
        Err(RuntimeError::Configuration(_))
    ));
    assert!(matches!(
        upgraded.resume(&ticket.run_id).await,
        Err(RuntimeError::Configuration(_))
    ));
    let cancelled = upgraded
        .cancel(&ticket.run_id, "operator", "host upgraded")
        .await
        .unwrap();
    assert_eq!(cancelled.status, RunStatus::Finished);
    assert_eq!(cancelled.report.stop_reason, StopReason::Cancelled);
    manager
        .abandon_turn("chat", "one", &cancelled, "operator", "host upgraded")
        .await
        .unwrap();

    // A run needing recovery: RetrySafe stays strict; Abort terminates it.
    let waiting = manager.run_turn(&old, "chat", "two", "write").await.unwrap();
    let ticket = waiting.run.pending_approval().unwrap().clone();
    old.decide(&ticket.run_id, &ticket.request_id, approve())
        .await
        .unwrap();
    let unknown = manager.resume_turn(&old, "chat", "two").await.unwrap();
    assert_eq!(unknown.run.status, RunStatus::NeedsRecovery);
    let call_id = unknown.run.tool_steps[0].call.id.clone();
    assert!(matches!(
        upgraded
            .reconcile(
                &ticket.run_id,
                &call_id,
                RecoveryDecision::RetrySafe {
                    actor: "operator".into(),
                    note: "retry".into(),
                },
            )
            .await,
        Err(RuntimeError::Configuration(_))
    ));
    let aborted = upgraded
        .reconcile(
            &ticket.run_id,
            &call_id,
            RecoveryDecision::Abort {
                actor: "operator".into(),
                note: "host upgraded".into(),
            },
        )
        .await
        .unwrap();
    manager
        .abandon_turn("chat", "two", &aborted, "operator", "host upgraded")
        .await
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn never_dispatched_calls_get_explicit_error_results_when_a_failed_turn_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("sessions"));
    let runs = FileRunStore::new(dir.path().join("runs"));
    let manager = SessionManager::new(&store);
    manager.create("chat", vec![]).await.unwrap();
    let model = TestModel {
        tools: true,
        ..Default::default()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let tools = registry(Arc::clone(&count), false);
    let agent = AgentLoop::new(&model, &tools, "test")
        .with_policy(ToolPolicy::default().allow("write"))
        .with_limits(RunLimits {
            max_tool_calls: 0,
            ..Default::default()
        });
    let failed = manager
        .run_turn(
            &DurableAgent::new(&agent, &runs),
            "chat",
            "limited",
            "write",
        )
        .await
        .unwrap();
    assert_eq!(failed.run.report.stop_reason, StopReason::ToolLimit);
    let closed = manager
        .abandon_turn(
            "chat",
            "limited",
            &failed.run,
            "operator",
            "no call was dispatched",
        )
        .await
        .unwrap();
    assert!(closed.messages.iter().any(|m|matches!(m,Message::Tool {result} if result.is_error && result.output.get("host_error").is_some())));
    closed.validate().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn metadata_cas_prevents_lost_updates_and_corrupt_sessions_are_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path());
    let manager = SessionManager::new(&store);
    let initial = manager
        .create("../聊天", vec!["host".into()])
        .await
        .unwrap();
    let input = manager
        .begin_turn("../聊天", "one", "question")
        .await
        .unwrap();
    assert_eq!(input.messages.len(), 2);
    assert!(matches!(
        store.save(&initial, Some(1)).await,
        Err(RuntimeError::Conflict(_))
    ));
    assert_eq!(manager.load("../聊天").await.unwrap().revision, 2);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".session.json"))
        .unwrap();
    let mut value = serde_json::to_value(manager.load("../聊天").await.unwrap()).unwrap();
    value["version"] = json!(SESSION_VERSION + 1);
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&file, &bytes).unwrap();
    assert!(manager.load("../聊天").await.is_err());
    assert_eq!(std::fs::read(file).unwrap(), bytes);
    assert!(FileSessionStore::new("relative-sessions")
        .root()
        .is_absolute());
    assert!(FileRunStore::new("relative-runs").root().is_absolute());
}

#[tokio::test]
async fn competing_workers_cannot_bind_two_active_turns() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path());
    let manager = SessionManager::new(&store);
    manager.create("race", vec![]).await.unwrap();
    let (a, b) = tokio::join!(
        manager.begin_turn("race", "a", "question a"),
        manager.begin_turn("race", "b", "question b")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let winner = a.ok().or_else(|| b.ok()).unwrap();
    let session = manager.load("race").await.unwrap();
    assert_eq!(session.revision, 2);
    assert_eq!(session.active.as_ref().unwrap().turn_id, winner.turn_id);
    let duplicate = manager
        .begin_turn(
            "race",
            &winner.turn_id,
            if winner.turn_id == "a" {
                "question a"
            } else {
                "question b"
            },
        )
        .await
        .unwrap();
    assert_eq!(duplicate, winner);
    assert_eq!(manager.load("race").await.unwrap().revision, 2);
}
