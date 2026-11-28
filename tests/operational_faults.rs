#![cfg(feature = "database")]
//! Single-host process/SQL fault drills. Model decisions are explicit fixtures.
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use taskloom::runtime::{database::*, *};

const BINDING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn options() -> DatabaseOptions {
    DatabaseOptions {
        lease_duration: Duration::from_secs(2),
        ..Default::default()
    }
}
fn append(path: &Path, value: &Value) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{}", value).unwrap();
    file.sync_all().unwrap();
}
fn save(path: &Path, value: &Value) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(serde_json::to_string_pretty(value).unwrap().as_bytes())
        .unwrap();
    file.sync_all().unwrap();
}
fn count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .count()
}
struct Fixture {
    root: PathBuf,
    kind: String,
    hold: bool,
}
impl Model for Fixture {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let reservation = budget.reserve_before_dispatch(0).await?;
            append(
                &self.root.join("model-dispatch.jsonl"),
                &json!({"kind":self.kind,"fixture":true}),
            );
            if self.kind == "model" && self.hold {
                save(
                    &self.root.join("model-held.json"),
                    &json!({"reservation_durable_before_fixture_dispatch":true}),
                );
                futures_util::future::pending::<()>().await;
            }
            reservation.settle(0);
            let turn = if self.kind == "effect"
                && !request
                    .messages
                    .iter()
                    .any(|message| matches!(message, Message::Tool { .. }))
            {
                AssistantTurn {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "append-once".into(),
                        name: "append".into(),
                        arguments: json!({"line":"one exact operational fixture append"}),
                    }],
                }
            } else {
                AssistantTurn::text("fixture completed")
            };
            Ok(ModelOutput {
                model: request.model.clone(),
                turn,
            })
        })
    }
}
fn tools(root: &Path, hold: bool) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    let root = root.to_path_buf();
    tools.register(FnTool::new(ToolDefinition {name:"append".into(),description:"Actual single local fsynced fixture append".into(),effect:ToolEffect::NonIdempotent,permissions:BTreeSet::from(["fixture:write".into()]),input_schema:json!({"type":"object","properties":{"line":{"type":"string"}},"required":["line"],"additionalProperties":false})},move |input:Value,_context:ToolContext| {
        let root=root.clone();async move {
            append(&root.join("actual-effect.jsonl"),&input);
            if hold {save(&root.join("effect-held.json"),&json!({"fsync_finished":true,"reply_not_returned":true}));futures_util::future::pending::<()>().await;}
            Ok(json!({"saved":true,"entries":count(&root.join("actual-effect.jsonl"))}))
        }
    })).unwrap();
    tools
}
fn agent<'a>(model: &'a Fixture, tools: &'a ToolRegistry) -> AgentLoop<'a> {
    AgentLoop::new(model, tools, "operational-fixture")
        .with_label("operational-fault-fixture-v1")
        .with_policy(ToolPolicy::default().allow("append").grant("fixture:write"))
        .with_limits(RunLimits {
            max_cost_micros: Some(0),
            max_duration: Duration::from_secs(120),
            tool_timeout: Duration::from_secs(90),
            ..Default::default()
        })
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn child(url: &str, project: &str, kind: &str, root: &Path) -> OwnedChild {
    let stdout = std::fs::File::create(root.join("killed-worker.log")).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "operational_fault_worker",
            "--ignored",
            "--nocapture",
        ])
        .env("TASKLOOM_FAULT_DATABASE_URL", url)
        .env("TASKLOOM_FAULT_PROJECT", project)
        .env("TASKLOOM_FAULT_KIND", kind)
        .env("TASKLOOM_FAULT_ROOT", root)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    OwnedChild(command.spawn().unwrap())
}
async fn wait_file(path: &Path, child: &mut OwnedChild) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "fixture worker ended before held durable operation"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("held fixture deadline exceeded");
}
async fn recover(queue: &SqlQueueStore) -> SqlQueueLease {
    for _ in 0..400 {
        if let Some(owner) = queue.claim(BINDING, "successor").await.unwrap() {
            return owner;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("expired queue and runtime ownership never became recoverable");
}

#[tokio::test]
#[ignore = "internal owned subprocess fixture; invoked only by the parent operational drill"]
async fn operational_fault_worker() {
    let url = std::env::var("TASKLOOM_FAULT_DATABASE_URL").unwrap();
    let project = std::env::var("TASKLOOM_FAULT_PROJECT").unwrap();
    let kind = std::env::var("TASKLOOM_FAULT_KIND").unwrap();
    let root = PathBuf::from(std::env::var_os("TASKLOOM_FAULT_ROOT").unwrap());
    let db = SqlDatabase::connect(&url, options()).await.unwrap();
    let ns = DatabaseNamespace::new("operational-faults", &project).unwrap();
    let owner = db
        .queue(ns.clone())
        .claim(BINDING, "doomed-worker")
        .await
        .unwrap()
        .unwrap();
    let runs = db.runs(ns).with_queue_lease(&owner).unwrap();
    let model = Fixture {
        root: root.clone(),
        kind: kind.clone(),
        hold: true,
    };
    let registry = tools(&root, true);
    let configured = agent(&model, &registry);
    let result = DurableAgent::new(&configured, &runs)
        .start_or_resume(
            &kind,
            vec![Message::user(format!("explicit {kind} fixture"))],
        )
        .await;
    panic!(
        "fault fixture returned before parent kill: {:?}",
        result.map(|state| state.status)
    );
}

async fn model_death(
    db: &SqlDatabase,
    url: &str,
    root: &Path,
    checks: &mut BTreeMap<String, bool>,
) -> Value {
    std::fs::create_dir(root).unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    let ns = DatabaseNamespace::new("operational-faults", &project).unwrap();
    let queue = db.queue(ns.clone());
    let quota = db.generation_quota(ns.clone());
    quota
        .configure(
            Some(GenerationQuota {
                currency: "USD".into(),
                max_cost_micros: 0,
                max_outstanding_attempts: 1,
            }),
            0,
            "drill-operator",
            "one zero-price fixture attempt",
        )
        .await
        .unwrap();
    queue
        .configure(
            Some(QueueLimits {
                max_open_jobs: 4,
                max_running: 1,
            }),
            BINDING,
            0,
            "drill-operator",
            "explicit local drill",
        )
        .await
        .unwrap();
    queue
        .enqueue(
            "model",
            "run:model",
            json!({"fixture":"model"}),
            BINDING,
            "drill-runner",
        )
        .await
        .unwrap();
    let mut process = child(url, &project, "model", root);
    wait_file(&root.join("model-held.json"), &mut process).await;
    checks.insert(
        "live_model_owner_is_not_stolen".into(),
        queue.claim(BINDING, "premature").await.unwrap().is_none(),
    );
    let pid = process.0.id();
    process.0.kill().unwrap();
    let exit = process.0.wait().unwrap();
    let held = db
        .runs(ns.clone())
        .inspect_budget("model")
        .await
        .unwrap()
        .unwrap();
    checks.insert(
        "kill_retains_durable_pre_dispatch_reservation".into(),
        held.in_flight_attempts == 1 && held.in_flight_micros == 0,
    );
    let owner = recover(&queue).await;
    let runs = db.runs(ns.clone()).with_queue_lease(&owner).unwrap();
    let model = Fixture {
        root: root.into(),
        kind: "model".into(),
        hold: false,
    };
    let registry = tools(root, false);
    let configured = agent(&model, &registry);
    let durable = DurableAgent::new(&configured, &runs);
    let state = durable.resume("model").await.unwrap();
    checks.insert(
        "unknown_zero_price_slot_blocks_recovery_dispatch".into(),
        state.report.stop_reason == StopReason::Budget
            && state.report.budget.uncertain_attempts == 1
            && count(&root.join("model-dispatch.jsonl")) == 1,
    );
    let before = serde_json::to_value(&state).unwrap();
    let terminal = durable.resume("model").await.unwrap();
    checks.insert(
        "terminal_model_reentry_is_unchanged".into(),
        serde_json::to_value(&terminal).unwrap() == before
            && count(&root.join("model-dispatch.jsonl")) == 1,
    );
    owner
        .finish(
            QueueState::Stopped,
            "unknown generation slot retained; explicit review required",
        )
        .await
        .unwrap();
    drop(durable);
    drop(runs);
    drop(owner);
    let ledger = quota.snapshot().await.unwrap();
    checks.insert(
        "generation_uncertainty_is_not_refunded".into(),
        ledger.usage.as_ref().unwrap().uncertain_attempts == 1
            && ledger.usage.as_ref().unwrap().in_flight_attempts == 0,
    );
    save(
        &root.join("restored-run.json"),
        &serde_json::to_value(&state).unwrap(),
    );
    save(
        &root.join("project-quota.json"),
        &serde_json::to_value(&ledger).unwrap(),
    );
    json!({"tenant":"operational-faults","project":project,"killed_owned_pid":pid,"killed_success_exit":exit.success(),"model_fixture_dispatches":count(&root.join("model-dispatch.jsonl")),"run":state,"quota":ledger})
}

async fn effect_death(
    db: &SqlDatabase,
    url: &str,
    root: &Path,
    checks: &mut BTreeMap<String, bool>,
    reconcile: bool,
) -> Value {
    std::fs::create_dir(root).unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    let ns = DatabaseNamespace::new("operational-faults", &project).unwrap();
    let queue = db.queue(ns.clone());
    let quota = db.generation_quota(ns.clone());
    quota
        .configure(
            Some(GenerationQuota {
                currency: "USD".into(),
                max_cost_micros: 0,
                max_outstanding_attempts: 4,
            }),
            0,
            "drill-operator",
            "bounded zero-price fixture",
        )
        .await
        .unwrap();
    queue
        .configure(
            Some(QueueLimits {
                max_open_jobs: 4,
                max_running: 1,
            }),
            BINDING,
            0,
            "drill-operator",
            "explicit local drill",
        )
        .await
        .unwrap();
    queue
        .enqueue(
            "effect",
            "run:effect",
            json!({"fixture":"effect"}),
            BINDING,
            "drill-runner",
        )
        .await
        .unwrap();
    let model = Fixture {
        root: root.into(),
        kind: "effect".into(),
        hold: false,
    };
    let registry = tools(root, false);
    let configured = agent(&model, &registry);
    let owner = queue
        .claim(BINDING, "prepare-approval")
        .await
        .unwrap()
        .unwrap();
    let guarded = db.runs(ns.clone()).with_queue_lease(&owner).unwrap();
    let state = DurableAgent::new(&configured, &guarded)
        .start("effect", vec![Message::user("explicit effect fixture")])
        .await
        .unwrap();
    let ticket = state.pending_approval().unwrap().clone();
    checks.insert(
        "non_idempotent_effect_waits_for_exact_approval".into(),
        state.status == RunStatus::WaitingApproval
            && count(&root.join("actual-effect.jsonl")) == 0
            && ticket.call.arguments == json!({"line":"one exact operational fixture append"}),
    );
    owner
        .finish(QueueState::Waiting, "exact approval required")
        .await
        .unwrap();
    drop(guarded);
    drop(owner);
    let bare = db.runs(ns.clone());
    DurableAgent::new(&configured, &bare)
        .decide(
            "effect",
            &ticket.request_id,
            ApprovalDecision {
                approved: true,
                actor: "drill-operator".into(),
                note: "approve exact one-line local fixture append".into(),
            },
        )
        .await
        .unwrap();
    checks.insert(
        "approval_records_without_dispatch".into(),
        count(&root.join("actual-effect.jsonl")) == 0,
    );
    let waiting = queue.inspect("effect").await.unwrap().unwrap();
    queue
        .resume(
            "effect",
            waiting.revision,
            BINDING,
            "drill-runner",
            "explicit approved continuation",
        )
        .await
        .unwrap();
    let mut process = child(url, &project, "effect", root);
    wait_file(&root.join("effect-held.json"), &mut process).await;
    checks.insert(
        "effect_is_fsynced_before_lost_reply".into(),
        count(&root.join("actual-effect.jsonl")) == 1,
    );
    checks.insert(
        "live_effect_owner_is_not_stolen".into(),
        queue.claim(BINDING, "premature").await.unwrap().is_none(),
    );
    let pid = process.0.id();
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    let owner = recover(&queue).await;
    let guarded = db.runs(ns.clone()).with_queue_lease(&owner).unwrap();
    let durable = DurableAgent::new(&configured, &guarded);
    let uncertain = durable.resume("effect").await.unwrap();
    let steps = &uncertain.tool_steps;
    checks.insert(
        "new_owner_retains_same_uncertain_call_without_replay".into(),
        uncertain.status == RunStatus::NeedsRecovery
            && steps[0].call == ticket.call
            && steps[0].attempts == 1
            && matches!(steps[0].progress, ToolProgress::Uncertain { .. })
            && count(&root.join("actual-effect.jsonl")) == 1
            && count(&root.join("model-dispatch.jsonl")) == 1,
    );
    let again = durable.resume("effect").await.unwrap();
    checks.insert(
        "uncertain_reentry_never_replays_effect_or_model".into(),
        again.status == RunStatus::NeedsRecovery
            && count(&root.join("actual-effect.jsonl")) == 1
            && count(&root.join("model-dispatch.jsonl")) == 1,
    );
    owner
        .finish(
            QueueState::Waiting,
            "explicit external observation required",
        )
        .await
        .unwrap();
    drop(durable);
    drop(guarded);
    drop(owner);
    if !reconcile {
        let retained = bare.inspect("effect").await.unwrap().unwrap();
        let waiting = queue.inspect("effect").await.unwrap().unwrap();
        checks.insert(
            "unknown_effect_retained_for_cold_restore".into(),
            serde_json::to_value(&retained).unwrap() == serde_json::to_value(&uncertain).unwrap()
                && waiting.state == QueueState::Waiting,
        );
        save(
            &root.join("uncertain-run.json"),
            &serde_json::to_value(&retained).unwrap(),
        );
        return json!({"tenant":"operational-faults","project":project,"killed_owned_pid":pid,"actual_effect_entries":count(&root.join("actual-effect.jsonl")),"fixture_model_dispatches":count(&root.join("model-dispatch.jsonl")),"uncertain_run":retained,"queue":waiting,"reconciled":false});
    }
    // The operator actually reads the nontransactional effect; it is never
    // deduced from a queue fence or a missing response.
    let observed: Vec<Value> = std::fs::read_to_string(root.join("actual-effect.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(observed, vec![ticket.call.arguments.clone()]);
    DurableAgent::new(&configured, &bare)
        .reconcile(
            "effect",
            &ticket.call.id,
            RecoveryDecision::RecordResult {
                output: json!({"saved":true,"entries":observed.len()}),
                is_error: false,
                actor: "drill-operator".into(),
                note: "read exact fsynced local effect; record observation without replay".into(),
            },
        )
        .await
        .unwrap();
    let waiting = queue.inspect("effect").await.unwrap().unwrap();
    queue
        .resume(
            "effect",
            waiting.revision,
            BINDING,
            "drill-runner",
            "observed and explicitly reconciled",
        )
        .await
        .unwrap();
    let owner = queue
        .claim(BINDING, "after-observation")
        .await
        .unwrap()
        .unwrap();
    let guarded = db.runs(ns.clone()).with_queue_lease(&owner).unwrap();
    let completed = DurableAgent::new(&configured, &guarded)
        .resume("effect")
        .await
        .unwrap();
    owner
        .finish(
            QueueState::Completed,
            "observed effect and final fixture answer complete",
        )
        .await
        .unwrap();
    drop(guarded);
    drop(owner);
    checks.insert(
        "observed_reconciliation_completes_with_exactly_one_fixture_effect".into(),
        completed.report.stop_reason == StopReason::Completed
            && count(&root.join("actual-effect.jsonl")) == 1
            && count(&root.join("model-dispatch.jsonl")) == 2,
    );
    checks.insert(
        "terminal_queue_is_not_reclaimed".into(),
        queue.claim(BINDING, "terminal").await.unwrap().is_none(),
    );
    save(
        &root.join("uncertain-run.json"),
        &serde_json::to_value(&uncertain).unwrap(),
    );
    save(
        &root.join("completed-run.json"),
        &serde_json::to_value(&completed).unwrap(),
    );
    json!({"tenant":"operational-faults","project":project,"killed_owned_pid":pid,"observed_effect":observed,"fixture_model_dispatches":count(&root.join("model-dispatch.jsonl")),"uncertain_run":uncertain,"completed_run":completed})
}

async fn fence(
    db: &SqlDatabase,
    url: &str,
    root: &Path,
    checks: &mut BTreeMap<String, bool>,
) -> Value {
    let project = uuid::Uuid::new_v4().to_string();
    let ns = DatabaseNamespace::new("operational-faults", &project).unwrap();
    let queue = db.queue(ns.clone());
    queue
        .configure(
            Some(QueueLimits {
                max_open_jobs: 4,
                max_running: 1,
            }),
            BINDING,
            0,
            "drill-operator",
            "epoch fault injection",
        )
        .await
        .unwrap();
    queue
        .enqueue(
            "fence",
            "run:fence",
            json!({"fixture":"fence"}),
            BINDING,
            "drill-runner",
        )
        .await
        .unwrap();
    let owner = queue.claim(BINDING, "stale-worker").await.unwrap().unwrap();
    let guarded = db.runs(ns.clone()).with_queue_lease(&owner).unwrap();
    let model = Fixture {
        root: root.into(),
        kind: "fence".into(),
        hold: false,
    };
    let registry = tools(root, false);
    let configured = agent(&model, &registry);
    let original = DurableAgent::new(&configured, &guarded)
        .create("fence", vec![Message::user("fixture")])
        .await
        .unwrap();
    let lease = guarded.acquire("fence").await.unwrap();
    let held = BudgetSnapshot {
        generation: 1,
        limit_micros: Some(0),
        in_flight_attempts: 1,
        ..Default::default()
    };
    lease.save_budget(&held).await.unwrap();
    let sql = sqlx::AnyPool::connect(url).await.unwrap();
    sqlx::query("UPDATE taskloom_leases SET owner='explicit-fixture-fence',epoch=epoch+1,expires_ms=0 WHERE namespace=$1 AND family='queue_job' AND object_id='fence'").bind(serde_json::to_string(&("operational-faults",&project)).unwrap()).execute(&sql).await.unwrap();
    checks.insert(
        "stale_owner_fails_pre_dispatch_verification".into(),
        lease.verify_ownership().await.is_err(),
    );
    let mut changed = original.clone();
    changed.revision += 1;
    checks.insert(
        "stale_owner_cannot_commit_checkpoint".into(),
        matches!(
            lease.save(&changed).await,
            Err(RuntimeError::Conflict(_) | RuntimeError::Permission(_))
        ),
    );
    let next = BudgetSnapshot {
        generation: 2,
        in_flight_attempts: 2,
        ..held.clone()
    };
    checks.insert(
        "stale_owner_cannot_reserve_or_erase_journal".into(),
        lease.save_budget(&next).await.is_err() && lease.load_budget().await.unwrap() == Some(held),
    );
    checks.insert(
        "live_runtime_blocks_queue_takeover_after_injected_queue_expiry".into(),
        queue
            .claim(BINDING, "too-early-successor")
            .await
            .unwrap()
            .is_none(),
    );
    drop(lease);
    drop(guarded);
    drop(owner);
    let successor = recover(&queue).await;
    successor
        .finish(
            QueueState::Waiting,
            "retained old journal requires explicit review",
        )
        .await
        .unwrap();
    drop(successor);
    sql.close().await;
    json!({"tenant":"operational-faults","project":project,"fault":"explicit local SQL epoch/owner injection; not a real network partition"})
}
async fn run_backend(url: &str, backend: &str) {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("TASKLOOM_FAULT_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned())
        .join(backend);
    std::fs::create_dir(&root).unwrap();
    let db = SqlDatabase::connect(url, options()).await.unwrap();
    db.migrate().await.unwrap();
    let mut checks = BTreeMap::new();
    let model = model_death(&db, url, &root.join("model"), &mut checks).await;
    let effect = effect_death(&db, url, &root.join("effect"), &mut checks, true).await;
    let mut pending_checks = BTreeMap::new();
    let pending_effect = effect_death(
        &db,
        url,
        &root.join("pending-effect"),
        &mut pending_checks,
        false,
    )
    .await;
    checks.extend(
        pending_checks
            .into_iter()
            .map(|(name, passed)| (format!("unreconciled_effect_{name}"), passed)),
    );
    let fenced = fence(&db, url, &root, &mut checks).await;
    db.close().await;
    let record = json!({"scope":"actual local SQL/owned-process fault controls; scripted fixture model, real fsynced filesystem effect; not model quality, multihost failover or SLO","backend":backend,"checks":checks,"model_death":model,"effect_death":effect,"pending_effect":pending_effect,"owner_fence":fenced,"status":if checks.values().all(|passed|*passed){"passed"}else{"failed"}});
    save(&root.join("report.json"), &record);
    assert!(checks.values().all(|passed| *passed), "{record}");
}
#[tokio::test]
async fn sqlite_operational_process_and_owner_faults() {
    let temporary = tempfile::tempdir().unwrap();
    let url = std::env::var("TASKLOOM_FAULT_SQLITE_URL").unwrap_or_else(|_| {
        SqlDatabase::sqlite_url(temporary.path().join("faults.sqlite")).unwrap()
    });
    run_backend(&url, "sqlite").await;
}
#[tokio::test]
#[ignore = "requires explicitly owned TASKLOOM_TEST_POSTGRES_URL; hard-kills only owned fixture children"]
async fn postgres_operational_process_and_owner_faults() {
    run_backend(
        &std::env::var("TASKLOOM_TEST_POSTGRES_URL").expect("owned PostgreSQL required"),
        "postgres",
    )
    .await;
}
