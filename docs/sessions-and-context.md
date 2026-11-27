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

## Session size limit

Every turn's run starts from the full session history, and each checkpoint
rewrites the whole run state, so a long session eventually approaches the
store's byte limit (64 MiB by default for both `FileSessionStore` and
`FileRunStore`; keep the session limit at least as large as the run limit).
To avoid a run that completes but can no longer be saved or committed, work is
refused **before** it starts:

- `begin_turn` (and so `run_turn`) rejects a new turn with
  `RuntimeError::StorageLimit` once the serialized session exceeds half of the
  session store's limit. Nothing is bound or dispatched.
- The new input and the metadata needed to close its turn must also fit;
  oversized input is rejected before the active binding is written.
- Managed execution subtracts the complete session commit metadata from its
  byte allowance. A durable run reserves the largest declared model/tool
  response, duplicated tool state, context projection and a terminal-record
  margin before dispatch. Insufficient space produces `StorageLimit` without
  a model/tool call. Close that turn with `abandon_turn` as for any failed run.

`RunLimits::max_model_result_bytes` caps the serialized native `ModelOutput`
(turn plus serving model) at 64 KiB by default. Small custom stores should
configure a smaller ceiling, for example 512 bytes for bounded short answers;
`max_tool_result_bytes` also contributes to tool admission. Existing limit JSON
without the new field receives the default. A custom model that violates its
byte ceiling receives a persisted terminal `StorageLimit`: its reply is not
retained, observed cost stays charged, and reentry does not call it again.

Cancellation, approval rejection, `RecordResult`/`Abort` reconciliation and
`abandon_turn` do not run dispatch-admission checks. Actual writes still obey
store byte limits, including operator-supplied notes; use a bounded note that
fits, or explicitly enlarge the trusted store's cap. When the limit is reached, start a new session
(optionally seeding it with a summary from the old one); the existing session
and its history are kept unchanged. Custom `SessionStore`/`RunLease`
implementations opt in by returning their limit from `max_bytes`.

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
data** message. It is not elevated into a system instruction. The summary
takes the position of the newest omitted message: when older tool blocks of
the current turn are dropped, it follows the current question and precedes
the retained recent blocks, so the chronology stays intact. The built-in
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
