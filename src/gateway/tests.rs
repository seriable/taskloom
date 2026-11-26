use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use std::sync::LazyLock;

use super::*;

/// The gateway of these tests: a few registered labels, waits divided by
/// 100 so retry and breaker tests stay fast.
fn gw() -> &'static Gateway {
    static GATEWAY: LazyLock<Gateway> = LazyLock::new(|| {
        Gateway::new(GatewayConfig {
            tasks: TaskRegistry::new([
                TaskEntry::bounded_text("critical_interpretation", 5_000, 0.25),
                TaskEntry::observed_visual("compact_visual_observation", 2_048),
            ]),
            locale: Locale::Chinese,
            time_divisor: 100,
            ..GatewayConfig::default()
        })
    });
    &GATEWAY
}

fn unique_provider(name: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "https://{name}-{}-{}.test/v1",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn model_profile(model: &str) -> ModelProfile {
    gw().model_profile(model)
}

fn install_provider_control(
    provider: &str,
    permits: usize,
    breaker: CircuitBreaker,
) -> Arc<ProviderControl> {
    gw().install_provider_control(provider, permits, breaker)
}

/// The gateway's chain walk; the tests' mode is the default one.
async fn execute_models<T, F, Fut>(
    ctx: &CallContext,
    models: &[String],
    _mode: BudgetMode,
    attempt: &mut F,
) -> Result<T, ModelError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<AttemptOutput<T>, ModelError>>,
{
    gw().execute_models(ctx, models, attempt).await
}

fn check_budget(
    ctx: &CallContext,
    profile: &ModelProfile,
    mode: BudgetMode,
) -> Result<BudgetCheck, ModelError> {
    super::check_budget(ctx, profile, mode, Locale::Chinese)
}

fn context(task_label: &str, provider: &str, model: &str, user: &str) -> CallContext {
    let mut context = gw().call_context(
        task_label,
        provider,
        model,
        "系统提示",
        user,
        0,
        2_000,
        TokenEstimator::default(),
    );
    context.transient_attempts = 2;
    context
}

#[test]
fn context_too_large_in_a_400_body_is_an_overflow() {
    let body = r#"{"error":{"message":"context_too_large: input requires about 30000 tokens; model limit is 16000 effective tokens"}}"#;
    let error = ModelError::from_http_status(
        400,
        body,
        format!("agent failed with status 400 Bad Request: {body}"),
    );
    assert!(error.is_context_overflow());
    let openai = "This model's maximum context length is 128000 tokens. However, your messages resulted in 130123 tokens.";
    match ModelError::from_http_status(400, openai, openai) {
        ModelError::ContextOverflow {
            required_tokens,
            limit_tokens,
            ..
        } => {
            assert_eq!(required_tokens, Some(130_123));
            assert_eq!(limit_tokens, Some(128_000));
        }
        other => panic!("expected context overflow, got {other:?}"),
    }
}

// ------------------------------------------------------------- estimator

#[test]
fn estimator_counts_cjk_per_character_and_ascii_per_three_and_a_half() {
    assert_eq!(TokenEstimator::estimate(""), 0);
    // 4 CJK characters × 1.15 = 4.6 → 5
    assert_eq!(TokenEstimator::estimate("你好世界"), 5);
    // 35 ASCII characters / 3.5 = 10 × 1.15 = 11.5 → 12
    assert_eq!(TokenEstimator::estimate(&"a".repeat(35)), 12);
    // Mixed: 2 CJK + 7 ASCII = 2 + 2 = 4 × 1.15 = 4.6 → 5
    assert_eq!(TokenEstimator::estimate("剧情abcdefg"), 5);
    // Full-width punctuation counts as CJK.
    assert_eq!(TokenEstimator::estimate("，。"), 3);
}

#[test]
fn estimator_adds_a_fixed_cost_per_image() {
    let estimator = TokenEstimator::with_tokens_per_image(1_000);
    let text_only = estimator.estimate_request("系统", "用户", 0);
    assert_eq!(
        estimator.estimate_request("系统", "用户", 3),
        text_only + 3_000
    );
    assert_eq!(
        TokenEstimator::for_vision_detail("low").tokens_per_image,
        LOW_DETAIL_TOKENS_PER_IMAGE
    );
    assert_eq!(
        TokenEstimator::for_vision_detail("auto").tokens_per_image,
        DEFAULT_TOKENS_PER_IMAGE
    );
}

// -------------------------------------------------------------- profiles

#[test]
fn model_profiles_resolve_by_longest_prefix_with_generic_fallback() {
    assert_eq!(model_profile("gpt-5.6-luna").max_images, 4);
    let sol = model_profile("gpt-5.6-sol");
    assert_eq!(sol.max_images, 24);
    assert_eq!(sol.context_window, 258_400);
    assert_eq!(model_profile("MiMo-V2.5").provider, "mimo");
    assert!(!model_profile("mimo-v2.5-asr").supports_images);
    assert_eq!(model_profile("gemini-2.5-pro").context_window, 1_048_576);
    let unknown = model_profile("some-local-model");
    assert_eq!(unknown.provider, "generic");
    assert_eq!(unknown.context_window, 128_000);
}

#[test]
fn model_profile_env_json_overrides_selected_fields() {
    fn model_profile_with_overrides(model: &str, json: &str) -> ModelProfile {
        ModelRegistry::new()
            .with_overrides(parse_model_overrides(json, "test"))
            .profile(model)
    }
    let profile = model_profile_with_overrides(
        "gpt-5.6-sol",
        r#"[{"id":"gpt-5.6","context_window":400000,"fallback_chain":["gpt-5.5"]},{"id":"gpt","context_window":1}]"#,
    );
    assert_eq!(profile.context_window, 400_000);
    assert_eq!(profile.max_images, 24);
    assert_eq!(profile.fallback_chain, vec!["gpt-5.5".to_string()]);
    // Malformed JSON is ignored instead of breaking every call.
    assert_eq!(
        model_profile_with_overrides("gpt-5.6-sol", "not json").context_window,
        258_400
    );
}

#[test]
fn fallback_chain_skips_primary_duplicates_and_image_incapable_models() {
    let chain_of = |primary: &str, images: bool, profile_chain: &[&str], default: &str| {
        let models = ModelRegistry::new()
            .with_default_fallback_chain(ModelRegistry::parse_model_list(default))
            .with_overrides(if profile_chain.is_empty() {
                Vec::new()
            } else {
                vec![ModelProfileOverride {
                    id: primary.to_string(),
                    fallback_chain: Some(profile_chain.iter().map(|m| m.to_string()).collect()),
                    ..ModelProfileOverride::default()
                }]
            });
        models.fallback_models(primary, images)
    };
    assert_eq!(
        chain_of(
            "gpt-5.6-sol",
            false,
            &[],
            " gpt-5.5, deepseek-v3,GPT-5.6-SOL,gpt-5.5 "
        ),
        vec!["gpt-5.5".to_string(), "deepseek-v3".to_string()]
    );
    assert_eq!(
        chain_of("gpt-5.6-sol", true, &[], "gpt-5.5,deepseek-v3"),
        vec!["gpt-5.5".to_string()]
    );
    assert_eq!(
        chain_of("gpt-5.6-sol", false, &["gemini-2.5-pro"], "gpt-5.5"),
        vec!["gemini-2.5-pro".to_string()]
    );
    assert!(chain_of("gpt-5.6-sol", false, &[], "").is_empty());
    assert!(resolve_fallback_chain("gpt-5.6-sol", false, &[], model_profile).is_empty());
}

#[test]
fn budget_mode_parses_operator_values() {
    assert_eq!(BudgetMode::parse("enforce"), BudgetMode::EnforceAll);
    assert_eq!(BudgetMode::parse("OBSERVE_ALL"), BudgetMode::ObserveAll);
    assert_eq!(BudgetMode::parse(""), BudgetMode::Registry);
}

// ---------------------------------------------------------------- budget

#[test]
fn budget_is_min_of_task_budget_and_window_minus_output() {
    let provider = unique_provider("budget");
    let mut ctx = context("critical_interpretation", &provider, "gpt-5.6-sol", "短");
    let profile = model_profile("gpt-5.6-sol");
    let check = check_budget(&ctx, &profile, BudgetMode::Registry).unwrap();
    assert_eq!(check.limit_tokens, DEFAULT_INPUT_BUDGET_TOKENS);

    // A tiny window makes the window the binding limit.
    let mut small = profile.clone();
    small.context_window = 10_000;
    assert_eq!(
        check_budget(&ctx, &small, BudgetMode::Registry)
            .unwrap()
            .limit_tokens,
        8_000
    );

    ctx.estimated_input_tokens = 20_000;
    match check_budget(&ctx, &profile, BudgetMode::Registry) {
        Err(ModelError::ContextOverflow {
            required_tokens,
            limit_tokens,
            ..
        }) => {
            assert_eq!(required_tokens, Some(20_000));
            assert_eq!(limit_tokens, Some(16_000));
        }
        other => panic!("expected overflow, got {other:?}"),
    }
    // Observe mode only enforces the model window.
    let observed = check_budget(&ctx, &profile, BudgetMode::ObserveAll).unwrap();
    assert!(observed.over_task_budget);
    assert_eq!(observed.limit_tokens, 258_400 - 2_000);
}

// ------------------------------------------------------- retry / fallback

#[tokio::test]
async fn non_transient_failures_are_not_retried_or_rerouted() {
    for error in [
        ModelError::schema_mismatch("bad json"),
        ModelError::Auth {
            detail: "401".to_string(),
        },
        ModelError::ContextOverflow {
            required_tokens: None,
            limit_tokens: None,
            detail: "413".to_string(),
        },
    ] {
        let provider = unique_provider("no-retry");
        let ctx = context(
            "critical_interpretation",
            &provider,
            "gpt-5.6-sol",
            "短提示",
        );
        let sent = AtomicUsize::new(0);
        let expected = error.clone();
        let result: Result<(), ModelError> = execute_models(
            &ctx,
            &["gpt-5.6-sol".to_string(), "gpt-5.5".to_string()],
            BudgetMode::Registry,
            &mut |_model: String| {
                sent.fetch_add(1, Ordering::SeqCst);
                let error = error.clone();
                async move { Err(error) }
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(sent.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn exponential_backoff_is_bounded() {
    assert_eq!(exponential_backoff(0), std::time::Duration::from_secs(1));
    assert_eq!(exponential_backoff(4), std::time::Duration::from_secs(16));
    assert_eq!(exponential_backoff(99), std::time::Duration::from_secs(256));
}

#[test]
fn transient_backoff_uses_the_task_policy_schedule() {
    let bad_gateway = ModelError::transient(Some(502), "x");
    assert_eq!(
        transient_backoff(&bad_gateway, 0),
        std::time::Duration::from_secs(1)
    );
    assert_eq!(
        transient_backoff(&bad_gateway, 1),
        std::time::Duration::from_secs(2)
    );
    assert_eq!(
        transient_backoff(&bad_gateway, 3),
        std::time::Duration::from_secs(8)
    );
    let rate_limited = ModelError::transient(Some(429), "x");
    assert_eq!(
        transient_backoff(&rate_limited, 0),
        std::time::Duration::from_secs(30)
    );
    assert_eq!(
        transient_backoff(&rate_limited, 1),
        std::time::Duration::from_secs(60)
    );
}

// --------------------------------------------------------------- breaker

#[test]
fn breaker_opens_after_consecutive_transients_and_half_opens_after_cooldown() {
    let cooldown = std::time::Duration::from_secs(30);
    let mut breaker = CircuitBreaker::new(3, cooldown);
    let start = std::time::Instant::now();
    assert_eq!(breaker.admit(start), Admission::Allowed { probe: false });

    breaker.record_transient(start);
    breaker.record_transient(start);
    assert_eq!(breaker.state(), BreakerState::Closed);
    // A reachable answer resets the streak.
    breaker.record_reachable();
    breaker.record_transient(start);
    breaker.record_transient(start);
    assert_eq!(breaker.state(), BreakerState::Closed);
    breaker.record_transient(start);
    assert!(matches!(breaker.state(), BreakerState::Open { .. }));
    assert!(matches!(breaker.admit(start), Admission::Wait(delay) if delay == cooldown));

    let later = start + cooldown + std::time::Duration::from_secs(1);
    assert_eq!(breaker.admit(later), Admission::Allowed { probe: true });
    // Only one probe at a time; the others wait for its outcome.
    assert_eq!(breaker.admit(later), Admission::AwaitProbe);
    // A failed probe re-opens immediately (no need for N more failures).
    breaker.record_transient(later);
    assert!(matches!(breaker.state(), BreakerState::Open { .. }));

    let much_later = later + cooldown + std::time::Duration::from_secs(1);
    assert_eq!(
        breaker.admit(much_later),
        Admission::Allowed { probe: true }
    );
    breaker.record_reachable();
    assert_eq!(breaker.state(), BreakerState::Closed);
    assert_eq!(
        breaker.admit(much_later),
        Admission::Allowed { probe: false }
    );
}

#[test]
fn abandoned_probe_is_released_for_the_next_caller() {
    let mut breaker = CircuitBreaker::new(1, std::time::Duration::from_secs(1));
    let start = std::time::Instant::now();
    breaker.record_transient(start);
    let later = start + std::time::Duration::from_secs(2);
    assert_eq!(breaker.admit(later), Admission::Allowed { probe: true });
    breaker.release_probe();
    assert_eq!(breaker.admit(later), Admission::Allowed { probe: true });
}

#[tokio::test]
async fn open_breaker_stops_sending_and_reports_circuit_open() {
    let provider = unique_provider("breaker");
    let control = install_provider_control(
        &provider,
        4,
        CircuitBreaker::new(2, std::time::Duration::from_secs(60)),
    );
    let mut ctx = context(
        "critical_interpretation",
        &provider,
        "gpt-5.6-sol",
        "短提示",
    );
    ctx.transient_attempts = 2;
    let sent = AtomicUsize::new(0);
    let failing = |sent: &AtomicUsize| {
        sent.fetch_add(1, Ordering::SeqCst);
        async { Err::<AttemptOutput<()>, _>(ModelError::transient(Some(503), "status 503")) }
    };
    let first = execute_models(
        &ctx,
        &["gpt-5.6-sol".to_string()],
        BudgetMode::Registry,
        &mut |_model: String| failing(&sent),
    )
    .await;
    assert!(first.unwrap_err().is_transient());
    assert_eq!(sent.load(Ordering::SeqCst), 2);
    assert!(matches!(
        control.model_breaker_state("gpt-5.6-sol"),
        BreakerState::Open { .. }
    ));
    // The breaker belongs to the failing model, not to the whole provider.
    assert_eq!(control.breaker_state(), BreakerState::Closed);
    assert_eq!(control.model_breaker_state("gpt-5.4"), BreakerState::Closed);
    assert!(gw()
        .controls
        .provider_breaker_blocks(&provider, std::time::Instant::now()));

    let second = execute_models(
        &ctx,
        &["gpt-5.6-sol".to_string()],
        BudgetMode::Registry,
        &mut |_model: String| failing(&sent),
    )
    .await;
    let error = second.unwrap_err();
    assert!(error.is_transient());
    assert!(error.to_string().contains(CIRCUIT_OPEN_MARKER), "{error}");
    assert!(error.to_string().contains("gateway failed"), "{error}");
    assert_eq!(
        sent.load(Ordering::SeqCst),
        2,
        "an open breaker must not send"
    );
}

#[tokio::test]
async fn an_open_model_breaker_hands_the_call_to_the_next_model_of_the_chain() {
    let provider = unique_provider("breaker-fallback");
    let control = install_provider_control(
        &provider,
        4,
        CircuitBreaker::new(1, std::time::Duration::from_secs(600)),
    );
    control
        .model_breaker("gpt-5.6-sol")
        .record_transient(std::time::Instant::now());
    let mut ctx = context(
        "critical_interpretation",
        &provider,
        "gpt-5.6-sol",
        "短提示",
    );
    ctx.transient_attempts = 2;
    let sent_to = std::sync::Mutex::new(Vec::new());
    let started = std::time::Instant::now();
    let served = execute_models(
        &ctx,
        &["gpt-5.6-sol".to_string(), "gpt-5.4".to_string()],
        BudgetMode::Registry,
        &mut |model: String| {
            sent_to.lock().unwrap().push(model.clone());
            async move { Ok(AttemptOutput::new(model, None)) }
        },
    )
    .await
    .expect("the fallback model is not behind the primary model's breaker");
    assert_eq!(served, "gpt-5.4");
    assert_eq!(*sent_to.lock().unwrap(), vec!["gpt-5.4".to_string()]);
    // The open primary is skipped at once, not waited on.
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn provider_semaphore_bounds_concurrency() {
    let provider = unique_provider("semaphore");
    install_provider_control(
        &provider,
        1,
        CircuitBreaker::new(5, std::time::Duration::from_secs(30)),
    );
    let ctx = Arc::new(context(
        "critical_interpretation",
        &provider,
        "gpt-5.6-sol",
        "短提示",
    ));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let ctx = ctx.clone();
        let active = active.clone();
        let peak = peak.clone();
        handles.push(tokio::spawn(async move {
            execute_models(
                &ctx,
                &["gpt-5.6-sol".to_string()],
                BudgetMode::Registry,
                &mut |_model: String| {
                    let active = active.clone();
                    let peak = peak.clone();
                    async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(AttemptOutput::new((), None))
                    }
                },
            )
            .await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

/// Opens the breaker so its cool-down has already elapsed, starts one slow
/// half-open probe, then queues `waiters` callers behind it. Returns the
/// waiters' results, how many of them sent, and how long they took.
async fn slow_probe_with_waiters(
    probe_succeeds: bool,
    waiters: usize,
) -> (Vec<Result<(), ModelError>>, usize, std::time::Duration) {
    let provider = unique_provider("half-open");
    let cooldown = std::time::Duration::from_secs(60);
    let control = install_provider_control(&provider, 8, CircuitBreaker::new(1, cooldown));
    let expired = std::time::Instant::now()
        .checked_sub(cooldown + std::time::Duration::from_secs(1))
        .expect("monotonic clock is older than one minute");
    control
        .model_breaker("gpt-5.6-sol")
        .record_transient(expired);
    let mut ctx = context(
        "critical_interpretation",
        &provider,
        "gpt-5.6-sol",
        "短提示",
    );
    ctx.transient_attempts = 1;
    let ctx = Arc::new(ctx);
    let models = ["gpt-5.6-sol".to_string()];

    let probe = {
        let ctx = ctx.clone();
        let models = models.clone();
        tokio::spawn(async move {
            execute_models(
                &ctx,
                &models,
                BudgetMode::Registry,
                &mut |_model: String| async move {
                    // Far longer than the old fixed "cool-down + 5 s" (scaled to
                    // 650 ms in tests) half-open poll budget.
                    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                    if probe_succeeds {
                        Ok(AttemptOutput::new((), None))
                    } else {
                        Err(ModelError::transient(Some(503), "status 503"))
                    }
                },
            )
            .await
        })
    };
    while control.model_breaker_state("gpt-5.6-sol")
        != (BreakerState::HalfOpen {
            probe_in_flight: true,
        })
    {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }

    let sent = Arc::new(AtomicUsize::new(0));
    let started = std::time::Instant::now();
    let handles: Vec<_> = (0..waiters)
        .map(|_| {
            let ctx = ctx.clone();
            let models = models.clone();
            let sent = sent.clone();
            tokio::spawn(async move {
                execute_models(
                    &ctx,
                    &models,
                    BudgetMode::Registry,
                    &mut |_model: String| {
                        sent.fetch_add(1, Ordering::SeqCst);
                        async { Ok(AttemptOutput::new((), None)) }
                    },
                )
                .await
            })
        })
        .collect();
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let elapsed = started.elapsed();
    assert_eq!(probe.await.unwrap().is_ok(), probe_succeeds);
    (results, sent.load(Ordering::SeqCst), elapsed)
}

#[tokio::test]
async fn queued_callers_wait_for_a_slow_half_open_probe_that_recovers() {
    let (results, sent, _) = slow_probe_with_waiters(true, 7).await;
    for result in &results {
        assert!(result.is_ok(), "{result:?}");
    }
    assert_eq!(sent, 7, "every queued caller sends once the probe closes");
}

#[tokio::test]
async fn queued_callers_fail_fast_when_the_half_open_probe_reopens() {
    let (results, sent, elapsed) = slow_probe_with_waiters(false, 7).await;
    for result in &results {
        let error = result.as_ref().unwrap_err();
        assert!(error.to_string().contains(CIRCUIT_OPEN_MARKER), "{error}");
    }
    assert_eq!(sent, 0, "a re-opened breaker must not send");
    // They wake on the probe's failure instead of sleeping through another
    // (scaled 650 ms) cool-down after it.
    assert!(
        elapsed < std::time::Duration::from_millis(1_400),
        "{elapsed:?}"
    );
}
