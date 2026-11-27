# taskloom

[![CI](https://github.com/seriable/taskloom/actions/workflows/ci.yml/badge.svg)](https://github.com/seriable/taskloom/actions/workflows/ci.yml)

Typed, replayable LLM task execution for Rust pipelines, with **token budgets
and call accounting**. Requires Rust 1.86 or newer.

taskloom runs *typed model tasks*: a task says what is its own (a label,
prompts, the models it may use, how its answer becomes a typed value) and
taskloom does the rest the same way for every task: model fallback, a
resilient JSON protocol, deadlines, hedging, circuit breakers, budgets, a
call ledger, and record/replay of every answer.

The default API executes one structured answer per task; your code controls
the pipeline. The optional `runtime` feature adds a bounded native tool-calling
agent loop with explicit permissions, execution events and run-wide limits.
This runtime is unreleased development code and is not in the `v0.1.0` tag.
See [the runtime guide](docs/agent-runtime.md) and the offline
`agent_loop` example. Persistent checkpoints, approval and recovery are
available through [DurableAgent](docs/durable-runs.md). [Sessions and context
compression](docs/sessions-and-context.md) retain full history across bounded
model turns. [Durable workflows](docs/workflows.md) add parallel/conditional
DAGs and bounded planner/executor/writer/reviewer collaboration (`agent_peer`).
[Knowledge retrieval](docs/knowledge.md) supports pluggable ingestion, chunking,
retrieval and exact source citations (`agent_research`). See the
[roadmap](docs/agent-roadmap.md) for acceptance checks and external validation.
The optional [`mcp` integration](docs/mcp.md) binds explicitly configured
Streamable HTTP tools to the same permission, approval and recovery controls.
The optional [`app` host](docs/application.md) adds an installable CLI,
bearer-authenticated local API and operator UI. Install from this development
checkout with `cargo install --path . --locked --features app`, then run
`taskloom init taskloom.json`. These features are not in the `v0.1.0` tag.
The follow-up [preview delivery checklist](docs/preview-delivery-roadmap.md)
tracks actual task evaluation, release delivery and the next application/tool
increments. [Task benchmarks](docs/task-benchmarks.md) provide versioned outcome
checks and campaign budgets with explicit offline/provider provenance.

## What you get

| | |
|---|---|
| **Typed tasks** | `AgentTask` / `PromptTask`: prompts in, a validated Rust value out. Validation is part of decoding: a refused answer moves on to the next model. |
| **Model chains** | `TaskSpec::new("small").then("large")`, with named models or roles (`ModelChoice::role("writer")`) resolved per call. |
| **Resilient JSON** | Structured request → re-seeded retry → plain-text turn whose JSON object is recovered locally. Overflow, auth and content refusals fail at once. |
| **Gateway** | Budget pre-check (requests over the task's token budget are refused *before* sending), per-provider concurrency, per-model circuit breakers with half-open probes, transient retry with backoff (≥ 30 s on HTTP 429), fallback chains, call statistics. |
| **Deadlines & hedging** | A deadline for the whole task; a late copy of a slow request raced against it. |
| **Call ledger** | Where the model time of a stage goes, by task kind, across spawned tasks. |
| **Record / replay** | Record every answer once; re-run your code offline over the same answers. Replay never sends a request. |
| **Step cache** | A validated answer of an opted-in task is served again for the same request, so a resumed job does not pay twice. |
| **Schemas & repair** | `TaskSpec::schema(..)` (or `schema_of::<T>()` with feature `schemars`) is sent to providers that enforce one; `.repair(n)` sends a rejected answer back with the reason, up to *n* times, before the next model. |
| **Streaming & idle timeouts** | A progress callback receives answer text as it streams; an attempt that goes quiet for N seconds fails as transient instead of waiting out a long timeout. |
| **Tokens & cost** | Provider-reported usage on every outcome, in the call ledger per task kind, and priced by your `PriceTable`. |
| **Cancellation** | `CancelToken`: a cancelled run drops its in-flight requests (hedged copies too) at once. Give it to one agent, or make a whole job cancellable with `cancel::scope(token, job)`. |
| **Embeddings** | `Agent::embed` through the same gateway, recorded and cached per input text. |
| **Evaluation** | Re-run recorded requests with a changed prompt or model and judge the answers (`Unchanged`, or a pairwise `ModelJudge`). |
| **Tracing** | `tracing` spans per task and per attempt, named after the OpenTelemetry GenAI conventions (`gen_ai.request.model`, `gen_ai.usage.input_tokens`, ...). |
| **Agent runtime (optional, unreleased)** | Native tool conversations, schema-checked tools, explicit permissions, bounded decisions/tool calls/time/model spending, execution events and reports. |
| **Durable collaboration (optional, unreleased)** | Atomic checkpoints, exact-input approvals, explicit effect reconciliation, sessions/context, parallel/conditional workflows and bounded PEER review. |
| **Knowledge and tools (optional, unreleased)** | Pluggable ingestion/chunking/retrieval, exact source citations, local lexical search and explicitly bound MCP HTTP tools. |
| **Application host (optional, unreleased)** | Installable CLI, authenticated local run/knowledge API and operator UI for events, results, approvals and execution controls. |
| **Typed errors** | `TaskError` / `ModelError` kinds (transient, context overflow, schema mismatch, auth, content rejected, misconfigured, deadline, cancelled). Branch on the kind, never on message text. |

## Quick start

```toml
[dependencies]
taskloom = { git = "https://github.com/seriable/taskloom", tag = "v0.1.0", features = ["openai"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use std::sync::Arc;

use taskloom::openai::OpenAiCompatible;
use taskloom::{
    json_value, Agent, Environment, Gateway, GatewayTransport, PromptTask, Protocol, TaskSpec,
};

#[tokio::main]
async fn main() -> Result<(), taskloom::TaskError> {
    // One gateway per process: breakers and concurrency are per provider.
    let gateway = Arc::new(Gateway::default());
    let provider = OpenAiCompatible::new("https://api.openai.com/v1", std::env::var("OPENAI_API_KEY").unwrap());
    let transport = GatewayTransport::new(gateway, provider);
    let environment = Environment::new();
    let agent = Agent::new(&transport, &environment);

    let task = PromptTask::new(
        "summary",
        TaskSpec::new("gpt-4o-mini", Protocol::ResilientJson, 0.2, 400).then("gpt-4o"),
        r#"Summarize in one sentence. Answer {"summary": "..."}."#,
        "taskloom runs typed LLM tasks with fallbacks and replay.",
        json_value,
    );
    let summary = agent.run(&task).await?;
    println!("{summary}");
    Ok(())
}
```

A task with logic of its own implements `AgentTask` — see
[`examples/basic.rs`](examples/basic.rs) for a typed task whose validation
hands a bad answer to the next model.

## Record and replay

```rust
use taskloom::{Environment, Recording};

// Record: answers come from the provider and are written to the directory.
let recording = Environment::new().with_recording(Recording::Record("answers/".into()));
// Replay: nothing is sent; a request without a recording fails.
let replay = Environment::new().with_recording(Recording::Replay("answers/".into()));
// Or from the environment: TASKLOOM_RECORDING=record|replay, TASKLOOM_RECORDING_DIR=...
let from_env = Environment::new().with_recording(Recording::from_env("TASKLOOM"));
```

A request is identified by its label, model, protocol, options, temperature,
output-token limit, schema, retry salt, prompts and the size and modification
time of every file it sends, so a changed prompt or a rewritten image is a new request. See
[`examples/record_replay.rs`](examples/record_replay.rs).

Recordings and step-cache entries also retain the model that actually served
the answer, including gateway fallback models. `TaskOutcome::model` reports
that model on live calls, cache hits and replay; `models_tried` counts entries
in the task's own model chain, excluding internal gateway retries/fallbacks.
Routing transports can implement `Transport::send_with_metadata` to report
their serving model; existing transports can keep implementing `send` alone.

The request-key and transport-namespace corrections change recording/cache
filenames: entries written before them must be recorded again. Replay fails on a missing entry
and never falls through to a live request. Records without serving-model
metadata use the requested model when read under a current key.

Built-in transports isolate stores by endpoint, provider protocol settings,
model routing and transport settings. Use `.with_cache_namespace("tenant-a")`
for tenant-specific routing or a custom HTTP client whose settings change
answers; never put credentials in the namespace. Custom transports can
override `Transport::cache_namespace` (forwarded through `&T` and `Arc<T>`).
Stores contain prompts and answers; keep their directories out of source control.

Writes serialize cooperating processes per task label using a persistent
`.taskloom-store.lock` file, unique temporary files and atomic replacement, with bounded retries
for temporary Windows file-sharing conflicts. A recording failure returns
`TaskError::Storage` immediately; a failed optional step-cache write is logged
and does not discard a valid answer.

## Budgets and the gateway

```rust
use taskloom::gateway::{ModelProfileOverride, ModelRegistry, TaskEntry, TaskRegistry};
use taskloom::{Gateway, GatewayConfig, Locale};

let gateway = Gateway::new(GatewayConfig {
    // Labels starting with "summary" may send at most 16K input tokens.
    tasks: TaskRegistry::new([TaskEntry::bounded_text("summary", 400, 0.2)]),
    // Model limits and fallback chains (built-in profiles cover the common families).
    models: ModelRegistry::new().with_overrides(vec![ModelProfileOverride {
        id: "gpt-4o-mini".into(),
        fallback_chain: Some(vec!["gpt-4o".into()]),
        ..Default::default()
    }]),
    locale: Locale::English,
    ..GatewayConfig::default()
});
```

The gateway wraps *any* transport through an attempt closure
(`Gateway::execute`), so you can put your own HTTP client behind it.

The registry's `TaskProfile::timeout` is a total task limit (default 120 s),
covering queueing, backoff, protocol retries, repairs and model chains.
`TaskSpec::deadline` can shorten it; the shorter limit wins. `AttemptTimeout`
limits each attempt, including its circuit-breaker and concurrency-permit waits.
An expired total limit stops the task with `TaskError::Deadline` and never
starts another fallback. Direct gateway calls use `ModelError::Deadline`.
For long-running tasks, increase the task entry's `timeout_seconds` explicitly.

## Providers

| Feature | Provider | Notes |
|---|---|---|
| `openai` | `openai::OpenAiCompatible` | OpenAI and compatible gateways: chat completions (JSON mode or `json_schema`), SSE streaming with usage, embeddings. |
| `anthropic` | `anthropic::Anthropic` | Messages API: structured outputs via `output_config.format`, SSE streaming, `anthropic-version: 2023-06-01` (no sampling parameters unless `with_sampling(true)`). |

Both run behind `GatewayTransport`, so budgets, breakers, retries,
fallback chains, costs and spans apply the same way.

OpenAI `message.refusal`, streamed `delta.refusal` and `content_filter`
responses are classified as content rejection while retaining reported usage.
An OpenAI stream must contain `[DONE]` or a nonempty `finish_reason`;
Anthropic requires `message_stop`. EOF before completion is transient and
is never recorded or cached as an answer. Progress deltas remain provisional
until the task succeeds. The stricter protocol checks version provider store
namespaces, so OpenAI/Anthropic recordings made before this fix must be regenerated.

Embeddings retry transient failures on the requested model only: automatic
cross-model fallback is disabled because vector spaces are not interchangeable.
The runner validates vector dimensions and finite values before caching them,
including batches with partial cache hits. Provider API capabilities vary by
model and endpoint; verify schema and sampling support on your deployed models.

## Cost, progress, repair and cancellation

```rust
use taskloom::gateway::{ModelPrice, PriceTable};
use taskloom::ledger::CallLedger;
use taskloom::{CancelToken, GatewayConfig};

let gateway = Gateway::new(GatewayConfig {
    prices: PriceTable::new([ModelPrice::new("claude-opus-5-5", 4.0, 20.0)]),
    ..GatewayConfig::default()
});
// ...
let progress = |_label: &str, _model: &str, delta: &str| print!("{delta}");
let token = CancelToken::new(); // token.cancel() from anywhere stops the run
let agent = Agent::new(&transport, &environment)
    .with_progress(&progress)
    .with_cancel(&token);

// The task's spec asks for one repair turn: TaskSpec::new(..).repair(1)
let ledger = CallLedger::new();
let outcome = ledger.scope(agent.run_traced(&task)).await?;
println!("{} tokens, cost {:.4}", outcome.usage.input_tokens, outcome.usage.cost());
println!("{:?}", ledger.snapshot().total_usage());
```

See [`examples/production.rs`](examples/production.rs) (offline) and
[`examples/anthropic.rs`](examples/anthropic.rs).

Usage reported with a response is counted even if its JSON, refusal status,
or embedding count is rejected. Failed tasks expose that usage through the
call ledger and gateway statistics; successful tasks also include prior
answered attempts in `TaskOutcome::usage`. Missing provider usage, unfinished
streams and cancelled requests cannot be priced reliably. These counters
are accounting, not a job-wide currency spending limit.

## Evaluating a prompt change

Recordings keep each answer with its request. Re-run them with a variant
and judge the new answers against the recorded ones:

```rust
use taskloom::eval::{evaluate, load_cases, Candidate, ModelJudge};

let cases = load_cases("answers/".as_ref());
let judge = ModelJudge::new(judge_agent, "claude-opus-5-5", "Which summary is more faithful?");
let report = evaluate(&agent, &cases, |case| Some(Candidate {
    system_prompt: new_prompt.clone(),
    ..Candidate::from_case(case)
}), &judge, 4).await;
println!("better {} / worse {} / ties {}", report.better, report.worse, report.ties);
```

`Unchanged` instead of a model judge turns this into a regression check.
See [`examples/eval.rs`](examples/eval.rs).

## Tracing

Every task runs in a `gen_ai.task` span and every provider attempt in a
`gen_ai.request` span, with `gen_ai.request.model`, `gen_ai.usage.*`,
`error.type` and taskloom's own fields. Add a `tracing-opentelemetry` layer
to export them to any OpenTelemetry backend.

## Your own transport

Implement `Transport` to send requests your way (another provider SDK, a
queue, a test double), and `ModelResolver` to map roles to models:

```rust
use futures_util::future::BoxFuture;
use taskloom::{Answer, Request, TaskError, Transport};

struct MyTransport;

impl Transport for MyTransport {
    fn send<'a>(&'a self, request: Request<'a>) -> BoxFuture<'a, Result<Answer, TaskError>> {
        Box::pin(async move {
            // request.model, request.system_prompt, request.user_prompt,
            // request.images, request.format, request.options ...
            todo!()
        })
    }
}
```

Transport-specific settings travel as typed options:
`ProtocolSpec::new(Protocol::Json).with_option(MyTimeoutPolicy(30))`, read
back with `request.options.get::<MyTimeoutPolicy>()`. `FnTransport` answers
from a closure — handy for tests.

## Examples

```text
cargo run --example basic          # a typed task with validation and fallback, offline
cargo run --example record_replay  # record once, replay offline
cargo run --features runtime --example agent_loop  # native tool loop, offline
cargo run --features runtime --example agent_resume # checkpoint and approval, offline
cargo run --features runtime --example agent_session # persistent turns and compressed input
cargo run --features runtime --example agent_peer # four durable roles, approvals and review
cargo run --features runtime --example agent_research # local knowledge with verified quotes
cargo run --features app --bin taskloom -- --help # installable API/CLI/UI host
OPENAI_BASE_URL=... OPENAI_API_KEY=... OPENAI_MODEL=... \
  cargo run --features openai --example openai
```

## Status

0.1 is an early GitHub preview: the API may still change. taskloom was extracted from a production
video pipeline that runs some sixty model tasks through it.

This release is distributed on GitHub; the installation example pins its Git
tag. It has not been published to crates.io. The tagged single-task API reports
costs without imposing a whole-job currency cap. The unreleased runtime adds
explicit model-spend reservations and workflow-wide allocations with known
prices; independent external tool/service charges remain outside that cap.
Live-provider interoperability and large-scale load testing are separate from
offline checks.

Local Windows and Linux all-feature tests and Rust 1.86 tests pass. Hosted
GitHub CI has not completed validation; its live status is shown by the badge.

See [the readiness assessment](docs/readiness.md) for verified checks,
external validation and the boundaries of the task API and agent framework.

Contributions are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in taskloom by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
