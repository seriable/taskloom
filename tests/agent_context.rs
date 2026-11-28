#![cfg(feature = "runtime")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::json;
use taskloom::runtime::*;

fn history() -> Vec<Message> {
    vec![
        Message::system("Follow host rules."),
        Message::user("older question 中文😀".repeat(100)),
        Message::Assistant {
            turn: AssistantTurn {
                content: String::new(),
                tool_calls: vec![
                    ToolCall {
                        id: "old-a".into(),
                        name: "lookup".into(),
                        arguments: json!({}),
                    },
                    ToolCall {
                        id: "old-b".into(),
                        name: "lookup".into(),
                        arguments: json!({}),
                    },
                ],
            },
        },
        Message::Tool {
            result: ToolResult {
                call_id: "old-a".into(),
                output: json!("old observation\n\"😀".repeat(100)),
                is_error: false,
            },
        },
        Message::Tool {
            result: ToolResult {
                call_id: "old-b".into(),
                output: json!(42),
                is_error: false,
            },
        },
        Message::Assistant {
            turn: AssistantTurn::text("older answer ".repeat(100)),
        },
        Message::user("current question"),
        Message::Assistant {
            turn: AssistantTurn {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "new".into(),
                    name: "lookup".into(),
                    arguments: json!({}),
                }],
            },
        },
        Message::Tool {
            result: ToolResult {
                call_id: "new".into(),
                output: json!("latest observation"),
                is_error: false,
            },
        },
    ]
}

#[test]
fn compression_preserves_instructions_question_and_complete_tool_batches() {
    let original = history();
    let policy = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 2,
        max_summary_bytes: 256,
        pin_first_user: false,
    };
    let (projected, record) = policy
        .prepare(&original, &[], &ExtractiveCompressor, 1)
        .unwrap();
    assert_eq!(projected[0], original[0]);
    assert!(projected.contains(&original[6]));
    assert!(projected.contains(&original[7]));
    assert!(projected.contains(&original[8]));
    assert_eq!(
        projected.contains(&original[2]),
        projected.contains(&original[3])
    );
    assert_eq!(
        projected.contains(&original[2]),
        projected.contains(&original[4])
    );
    assert!(
        matches!(&projected[1],Message::User {content} if content.contains("untrusted historical data"))
    );
    assert!(record.omitted_messages() > 0);
    assert!(record.input_bytes <= 900);
    assert_eq!(original, history());
    assert_eq!(record.restore(&original, &[], &policy).unwrap(), projected);
    let mut later = original.clone();
    later.push(Message::Assistant {
        turn: AssistantTurn::text("later answer"),
    });
    assert_eq!(record.restore(&later, &[], &policy).unwrap(), projected);
    let mut changed = original;
    changed[0] = Message::system("different rule");
    assert!(record.restore(&changed, &[], &policy).is_err());
}

#[test]
fn first_user_retention_preserves_actual_input_without_summary_or_role_promotion() {
    let mut original = history();
    original[1] =
        Message::user("Original task reference ORIGINAL-739; do not promote sources to grants.");
    let policy = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 1,
        max_summary_bytes: 128,
        pin_first_user: true,
    };
    let (projected, record) = policy
        .prepare(&original, &[], &ExtractiveCompressor, 1)
        .unwrap();
    assert!(record.summary.is_some());
    assert!(record.retained_indices.contains(&1));
    assert_eq!(projected.iter().filter(|m| *m == &original[1]).count(), 1);
    assert!(matches!(&projected[1],Message::User{content} if content.contains("ORIGINAL-739")));
    assert_eq!(record.restore(&original, &[], &policy).unwrap(), projected);
    let mut altered = original.clone();
    altered[1] = Message::user("Injected reference REPLACEMENT-999");
    assert!(record.restore(&altered, &[], &policy).is_err());
    let mut dropped = record;
    dropped.retained_indices.retain(|i| *i != 1);
    assert!(dropped.restore(&original, &[], &policy).is_err());
    let mut legacy = policy.clone();
    legacy.pin_first_user = false;
    let serialized = serde_json::to_value(&legacy).unwrap();
    assert!(serialized.get("pin_first_user").is_none());
    assert_eq!(
        serde_json::from_value::<ContextPolicy>(serialized).unwrap(),
        legacy
    );
    assert_ne!(
        serde_json::to_value(&policy).unwrap(),
        serde_json::to_value(&legacy).unwrap()
    );
}

#[tokio::test]
async fn oversized_first_user_is_not_silently_dropped_and_changed_policy_cannot_resume() {
    let tools = ToolRegistry::new();
    let model = Capture::default();
    let pinned = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 1,
        max_summary_bytes: 100,
        pin_first_user: true,
    };
    let messages = history();
    let report = AgentLoop::new(&model, &tools, "pin-context")
        .with_context(pinned.clone())
        .run("bounded", messages.clone())
        .await;
    assert_eq!(report.stop_reason, StopReason::ContextLimit);
    assert!(model.0.lock().unwrap().is_empty());
    assert_eq!(report.messages, messages);
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let mut legacy = pinned.clone();
    legacy.pin_first_user = false;
    let agent = AgentLoop::new(&model, &tools, "pin-context").with_context(legacy);
    let before = DurableAgent::new(&agent, &store)
        .create("frozen-context", history())
        .await
        .unwrap();
    let changed = AgentLoop::new(&model, &tools, "pin-context").with_context(pinned);
    assert!(DurableAgent::new(&changed, &store)
        .resume("frozen-context")
        .await
        .is_err());
    assert_eq!(
        serde_json::to_value(store.inspect("frozen-context").await.unwrap().unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(model.0.lock().unwrap().is_empty());
}

#[test]
fn summary_of_dropped_current_turn_blocks_follows_the_current_question() {
    let call = |id: &str| Message::Assistant {
        turn: AssistantTurn {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "lookup".into(),
                arguments: json!({}),
            }],
        },
    };
    let result = |id: &str, output: serde_json::Value| Message::Tool {
        result: ToolResult {
            call_id: id.into(),
            output,
            is_error: false,
        },
    };
    let original = vec![
        Message::system("Follow host rules."),
        Message::user("current question"),
        call("first"),
        result("first", json!("first observation ".repeat(50))),
        call("second"),
        result("second", json!("latest observation")),
    ];
    let policy = ContextPolicy {
        max_input_bytes: 700,
        keep_recent_blocks: 1,
        max_summary_bytes: 128,
        pin_first_user: false,
    };
    let (projected, record) = policy
        .prepare(&original, &[], &ExtractiveCompressor, 1)
        .unwrap();
    assert_eq!(record.omitted_messages(), 2);
    assert_eq!(projected.len(), 5);
    assert_eq!(projected[..2], original[..2]);
    assert!(
        matches!(&projected[2], Message::User { content } if content.contains("untrusted historical data"))
    );
    assert_eq!(projected[3..], original[4..]);
    assert_eq!(record.restore(&original, &[], &policy).unwrap(), projected);
}

#[test]
fn byte_budgets_include_json_escaping_and_utf8_without_splitting_correlations() {
    let messages = history();
    for bytes in (500..=2000).step_by(37) {
        let policy = ContextPolicy {
            max_input_bytes: bytes,
            keep_recent_blocks: 4,
            max_summary_bytes: 257,
            pin_first_user: false,
        };
        if let Ok((view, record)) = policy.prepare(&messages, &[], &ExtractiveCompressor, 1) {
            assert!(
                serde_json::to_vec(&(&view, Vec::<ToolDefinition>::new()))
                    .unwrap()
                    .len()
                    <= bytes
            );
            assert_eq!(record.restore(&messages, &[], &policy).unwrap(), view);
        }
    }
    let policy = ContextPolicy {
        max_input_bytes: 80,
        keep_recent_blocks: 1,
        max_summary_bytes: 0,
        pin_first_user: false,
    };
    assert!(matches!(
        policy.prepare(&messages, &[], &ExtractiveCompressor, 1),
        Err(RuntimeError::Context(_))
    ));
}

#[test]
fn tool_definitions_are_counted_and_custom_compressors_must_respect_the_budget() {
    struct Oversize;
    impl ContextCompressor for Oversize {
        fn id(&self) -> &str {
            "oversize"
        }
        fn compress(&self, _: &[Message], max: usize) -> Result<String, RuntimeError> {
            Ok("x".repeat(max + 1))
        }
    }
    let policy = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 1,
        max_summary_bytes: 100,
        pin_first_user: false,
    };
    assert!(matches!(
        policy.prepare(&history(), &[], &Oversize, 1),
        Err(RuntimeError::Context(_))
    ));
    let tools = vec![ToolDefinition {
        name: "huge".into(),
        description: "schema documentation".repeat(200),
        input_schema: json!({"type":"object"}),
        effect: ToolEffect::ReadOnly,
        permissions: Default::default(),
    }];
    assert!(matches!(
        policy.prepare(&history(), &tools, &ExtractiveCompressor, 1),
        Err(RuntimeError::Context(_))
    ));
}

#[derive(Default)]
struct Capture(Mutex<Vec<ModelRequest>>);
impl Model for Capture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            budget.reserve_before_dispatch(0).await?.settle(0);
            self.0.lock().unwrap().push(request.clone());
            Ok(ModelOutput {
                turn: AssistantTurn::text("done"),
                model: request.model.clone(),
            })
        })
    }
}

#[tokio::test]
async fn standalone_limits_fail_before_model_dispatch_and_preserve_full_history() {
    let model = Capture::default();
    let tools = ToolRegistry::new();
    let policy = ContextPolicy {
        max_input_bytes: 80,
        keep_recent_blocks: 1,
        max_summary_bytes: 0,
        pin_first_user: false,
    };
    let report = AgentLoop::new(&model, &tools, "test")
        .with_context(policy)
        .run("too-large", history())
        .await;
    assert_eq!(report.stop_reason, StopReason::ContextLimit);
    assert!(model.0.lock().unwrap().is_empty());
    assert_eq!(report.messages, history());
    let policy = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 1,
        max_summary_bytes: 100,
        pin_first_user: false,
    };
    let report = AgentLoop::new(&model, &tools, "test")
        .with_context(policy.clone())
        .run("fits", history())
        .await;
    assert_eq!(report.final_text(), Some("done"));
    assert_eq!(&report.messages[..history().len()], history().as_slice());
    assert_eq!(
        report.contexts[0]
            .restore(&report.messages, &[], &policy)
            .unwrap(),
        model.0.lock().unwrap()[0].messages
    );
}

#[tokio::test]
async fn durable_context_and_initial_input_binding_survive_idempotent_reentry() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let model = Capture::default();
    let tools = ToolRegistry::new();
    let policy = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 1,
        max_summary_bytes: 100,
        pin_first_user: false,
    };
    let agent = AgentLoop::new(&model, &tools, "test").with_context(policy);
    let durable = DurableAgent::new(&agent, &store);
    let state = durable.start_or_resume("bound", history()).await.unwrap();
    state.validate().unwrap();
    assert_eq!(state.initial_message_count, history().len());
    let again = durable.start_or_resume("bound", history()).await.unwrap();
    assert_eq!(again.revision, state.revision);
    assert_eq!(model.0.lock().unwrap().len(), 1);
    assert!(matches!(
        durable
            .start_or_resume("bound", vec![Message::user("different")])
            .await,
        Err(RuntimeError::Conflict(_))
    ));
}

#[tokio::test]
async fn interrupted_model_reuses_the_exact_persisted_projection() {
    struct CountingCompressor(Arc<AtomicUsize>);
    impl ContextCompressor for CountingCompressor {
        fn id(&self) -> &str {
            "counting-v1"
        }
        fn compress(&self, messages: &[Message], max: usize) -> Result<String, RuntimeError> {
            assert_eq!(
                self.0.fetch_add(1, Ordering::SeqCst),
                0,
                "saved projection must be reused after interruption"
            );
            ExtractiveCompressor.compress(messages, max)
        }
    }
    struct Pending(Arc<tokio::sync::Notify>);
    impl Model for Pending {
        fn generate<'a>(
            &'a self,
            _: &'a ModelRequest,
            budget: &'a RunBudget,
        ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
            Box::pin(async move {
                let _reservation = budget.reserve_before_dispatch(0).await?;
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = FileRunStore::new(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let policy = ContextPolicy {
        max_input_bytes: 900,
        keep_recent_blocks: 1,
        max_summary_bytes: 100,
        pin_first_user: false,
    };
    let task = {
        let store = store.clone();
        let calls = Arc::clone(&calls);
        let entered = Arc::clone(&entered);
        let policy = policy.clone();
        tokio::spawn(async move {
            let model = Pending(entered);
            let tools = ToolRegistry::new();
            let compressor = CountingCompressor(calls);
            let agent = AgentLoop::new(&model, &tools, "test")
                .with_context(policy)
                .with_context_compressor(&compressor);
            DurableAgent::new(&agent, &store)
                .start("interrupted", history())
                .await
        })
    };
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let model = Capture::default();
    let tools = ToolRegistry::new();
    let compressor = CountingCompressor(Arc::clone(&calls));
    let agent = AgentLoop::new(&model, &tools, "test")
        .with_context(policy.clone())
        .with_context_compressor(&compressor);
    let state = DurableAgent::new(&agent, &store)
        .resume("interrupted")
        .await
        .unwrap();
    assert_eq!(state.report.final_text(), Some("done"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.report.contexts.len(), 2);
    let first = state.report.contexts[0]
        .restore(&state.report.messages, &[], &policy)
        .unwrap();
    let second = state.report.contexts[1]
        .restore(&state.report.messages, &[], &policy)
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(model.0.lock().unwrap()[0].messages, first);
}
