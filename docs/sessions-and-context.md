# Sessions and bounded model context

Enable `runtime`. Sessions retain full conversations across turns; context
projections bound the data sent to a model without rewriting that archive.
Use either capability independently or combine them with `DurableAgent`.

## Session lifecycle

`SessionManager` works with the `SessionStore` compare-and-swap contract.
`FileSessionStore` uses versioned JSON, hashed ids, per-document OS locks,
flushed atomic replacement and append-only history. Both file store types
resolve relative roots at construction, keeping their identity stable if a
host tool later changes process cwd. Store data in a trusted directory.

```rust,no_run
use taskloom::runtime::{AgentLoop, DurableAgent, FileRunStore, FileSessionStore, SessionManager};
# async fn example(agent: &AgentLoop<'_>) -> Result<(), taskloom::runtime::RuntimeError> {
let runs = FileRunStore::new("state/runs");
let sessions = FileSessionStore::new("state/sessions");
let manager = SessionManager::new(&sessions);
manager.create("chat", vec!["Follow the host's tool policy.".into()]).await?;
let durable = DurableAgent::new(agent, &runs);
let result = manager.run_turn(&durable, "chat", "turn-1", "Research this topic").await?;
// Reconstruct the same configured agent after restart:
let result = manager.resume_turn(&durable, "chat", "turn-1").await?;
println!("{:?}", result.run.status);
# Ok(()) }
```

Before any model request, `begin_turn` commits one active binding containing
the exact user input and a run id namespaced by session and turn id. Another
turn cannot enter that session while a job is running, paused or awaiting
approval/reconciliation. Distinct sessions can execute independently.

`run_turn` and `resume_turn` use `DurableAgent::start_or_resume`. They recover
both a crash before run creation and a completed run whose session commit was
interrupted. A duplicate turn id with the same input reuses its run; different
input is rejected. Committing the same final report twice does not append
duplicate messages. Concurrent metadata updates use revision CAS; `Conflict`
can be retried, but must never be treated as permission to start another job.

Only completed runs commit automatically. A failed terminal turn remains
bound until `abandon_turn` records an authenticated operator/application actor
and note. Uncertain/in-flight tools must first be reconciled through the
durable controller. Known observations stay in conversation history; calls
that were never dispatched receive explicit host error results. This includes
a call whose earlier attempt was reconciled with `RetrySafe` when the retry
itself never started; a call whose current attempt may have been dispatched
is never closed this way. An `Abort` reconciliation resolves the call with an
error result, after which the turn can be closed. A host note
records the failed outcome. This prevents an incomplete tool protocol from
leaking into the following model request, or a new question from silently
forgetting an uncertain write.

`commit_turn`/`abandon_turn` accept trusted controller state, not arbitrary
client-supplied JSON. The host application authenticates session access and
operator identity. A session is conversation state, not a cross-turn money
budget; each durable run keeps its own configured limits.

## Context projection

Opt in using `AgentLoop::with_context(ContextPolicy { .. })`. The default
single-task API and loops without a context policy keep their existing input
behavior. The byte ceiling includes taskloom's normalized serialized messages
and tool definitions, rather than a provider's entire HTTP body;
it is not a tokenizer. Gateway token/context-window checks still apply.

The projection keeps all leading system instructions, the current user
question and the latest complete block. An assistant tool request plus all
its results is an indivisible block, including batches with multiple calls.
Recent blocks are retained up to `keep_recent_blocks`; if necessary, fewer
older blocks are kept to fit the limit. Tool calls/results are never split.

Omitted history is compressed into a bounded, explicitly labeled **user-role
data** message. It is not elevated into a system instruction. The built-in
`ExtractiveCompressor` preserves bounded excerpts of recent omitted entries,
including tool ids/results. It is lossy: it does not guarantee retention of
every fact or infer a semantic summary. Keep critical task state in explicit
application data or retrieve it from the full session archive.

Implement `ContextCompressor` for domain-specific/precomputed summaries, and
select it with `with_context_compressor`. It must be deterministic, return
promptly and perform no paid I/O or effects. Version its stable id whenever
its contract changes. Model-generated summaries should be computed/accounted
as explicit application work, not hidden inside this synchronous hook.

The full transcript stays in `RunReport.messages`. `ContextRecord` saves the
source fingerprint, retained indices, exact summary, byte sizes and step id.
It reconstructs the actual input for inspection and is persisted before a
model dispatch. Recovery of an interrupted model reuses the saved projection
without calling the compressor again. Altered sources, split tool blocks,
changed compressor identity or over-budget projections are rejected.

If protected instructions/question, schemas and the latest tool block cannot
fit even with older material compressed, the run stops with `ContextLimit`
before a model request. It does not silently truncate a tool observation.

## Format and examples

Run checkpoints now use **format 2**, adding the exact initial-input boundary
and context projections. Development format-1 checkpoints are not silently
upgraded or reset: finish them with the prior checkout, or start a new run.
Single-task answer recording/cache formats and `AgentTask` are unchanged.
Sessions have their own format version 1.

```text
cargo run --locked --features runtime --example agent_session
```

The deterministic offline demo opens two turns, reconstructs the session store,
uses a compressed preference excerpt, preserves the original messages, then
repeats a turn without another model request. It makes no paid calls.

Tests in `agent_session.rs` cover restart windows, approval occupancy, exact
input binding, stale/concurrent revisions, failed-turn cleanup and uncertain
effects. `agent_context.rs` covers native tool correlations, Unicode/JSON byte
limits, frozen projections and compressor-free recovery after interruption.
