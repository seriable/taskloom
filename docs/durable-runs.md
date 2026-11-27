# Checkpoints, approval and recovery

Enable `runtime`. `DurableAgent` adds persistent execution to an `AgentLoop`
without changing the single-task API. `FileRunStore` is a local filesystem
backend; `RunStore`/`RunLease` are extension points for other stores.

```rust,no_run
use taskloom::runtime::{AgentLoop, DurableAgent, FileRunStore, Message};
# async fn example(agent: &AgentLoop<'_>) -> Result<(), taskloom::runtime::RuntimeError> {
let store = FileRunStore::new("run-state");
let durable = DurableAgent::new(agent, &store);
let state = durable.start("research-1", vec![Message::user("Research this topic")]).await?;
// In another invocation, reconstruct the same agent configuration:
let state = durable.resume("research-1").await?;
println!("{:?}", state.status);
# Ok(()) }
```

`create` only saves a runnable job. `start` saves and executes it. `resume`
continues paused/abandoned work; it returns an already finished, waiting for
approval or waiting for reconciliation state without dispatching more work.
`RunStatus` is authoritative for the current lifecycle; `RunReport` contains
the last stop reason, cumulative usage/counters and the conversation/events.

## Checkpoint contract

`RunState` contains a format version, monotonic revision, frozen configuration,
execution cursor, model/tool counters, budget snapshot, full conversation,
events, tool attempts, approvals and operator recovery records. Definitions,
schemas, permissions, model settings and limits must match on resume. Bump a
tool's advertised definition when its implementation contract changes; the
library cannot detect a code change behind identical metadata.
After such a bump, `resume`, approval and `RetrySafe` are rejected for older
runs, because they lead to dispatch under the frozen contract. Operations that
only terminate or record remain available: `cancel`, an approval rejection,
and the `RecordResult`/`Abort` reconciliations. Use them to finish paused,
approval-waiting or recovery runs (and their session turns) after an upgrade.
Format 2 also binds the exact initial-message boundary and persists context
projections. Development format-1 jobs require the earlier checkout; they are
not silently migrated or reset. See [sessions and context](sessions-and-context.md).
Provider credentials/endpoints, model profiles and prices belong to the host
application. Keep their intended contract and currency unit consistent when
reconstructing a run; they are not authenticated by `RunConfig`.

The file backend uses hashed run ids, a per-run OS lock, flushed temporary
files, atomic replacement and monotonic revision checks. The lock is held
throughout an invocation, including background file writes after an async
future is dropped. Other workers receive `Conflict`; they cannot execute the
same run concurrently. Persistent lock files are never unlinked. `inspect`
does an atomic read of the last committed checkpoint without acquiring the
execution lock, so status can be viewed while a worker is active.
The final lease owner explicitly unlocks the file, including when another
process briefly inherited a duplicate descriptor; active background writes
keep ownership until they finish.

Checkpoints are saved before model calls, before tool dispatch and after each
result. If persistence fails, no subsequent operation is dispatched. A result
that exists only in memory is not treated as durably complete. Malformed,
inconsistent or future-version checkpoints are rejected rather than reset.
The default maximum file size is 64 MiB, configurable on `FileRunStore`.

This provides process-crash recovery on a local filesystem, not a distributed
lease or an atomic transaction with an external service. Power-loss durability
depends on filesystem/OS guarantees; Unix directory metadata is flushed after
replacement. Store directories contain sensitive conversation/tool data and
must be trusted and access controlled by the host application.

## Graceful pause and cancellation

`DurableAgent::with_pause(&CancelToken)` requests a pause at the next completed
operation/checkpoint, before the following dispatch. It lets the active model
or tool finish. Resume using a fresh controller/pause token.

The agent's explicit or ambient cancellation token still interrupts active
work. Cancellation and exhausted limits are terminal; `resume` does not
silently reactivate them. In-flight tools remain marked uncertain for audit.
Recording a known result afterward does not reactivate a cancelled run.

Step/tool limits and model spending accumulate across invocations. The elapsed
limit counts cumulative active execution, rounded up to milliseconds; known
pause/approval downtime is excluded. When a process dies during an operation,
the interval since its persisted wall-clock start is charged conservatively,
including unknown downtime. A backwards wall clock at recovery is rejected.

## Approval of exact input

Persistent runs require approval for idempotent/non-idempotent effects by
default. `ApprovalPolicy` can additionally require reads/named tools, or let
the host explicitly preauthorize a different policy at creation. The tool
must still pass its ordinary allowlist, scope and schema checks.

`state.pending_approval()` exposes a request containing the run id, complete
tool call, frozen definition, attempt number and a SHA-256 ticket over those
values. `decide(run_id, ticket, ApprovalDecision)` saves an approval or rejection
with actor/note/time. It does not execute anything. An approved job becomes
paused and needs an explicit `resume`. A rejection is terminal. Another attempt
requires a new ticket; earlier approvals stay in the audit history.

The application must authenticate actors before accepting decisions. Ticket
hashes bind the decision to input; they are not authentication signatures and
do not protect against someone who can rewrite the entire trusted store.

## Interrupted tools and model attempts

A tool that was in flight when a worker stopped becomes `Uncertain`; the run
stops as `NeedsRecovery`. No tool is automatically retried, including reads.
Failed/timed-out/unusable results from a tool with effects also require
reconciliation. Use `reconcile` with a matching call id and an operator record:

- `RecordResult`: after inspecting the external system, supply the known
  result. It completes that exact call without invoking the tool again.
- `RetrySafe`: only read-only/idempotent tools are eligible. The same run/call
  ids are reused for tool-side deduplication. It requires fresh approval where
  the policy demands it and consumes another tool attempt. If the run
  finishes before the retry is dispatched (for example, the fresh approval
  is rejected, a limit is reached or the run is cancelled), the call stays
  pending and a session turn can still be closed: the earlier attempt was
  already reconciled, and the host result records that its outcome is unknown.
- `Abort`: terminate the run without continuing the uncertain operation. The
  call is resolved with an explicit error result recording the actor, note
  and an unknown external outcome, so the run can be closed and the abort
  cannot be repeated.

Non-idempotent operations cannot use `RetrySafe`. The library cannot determine
whether an email was sent, a remote write committed or spawned work is still
running. Verify the external effect before recording a result; dropping a
future cannot roll it back. Tool implementations must uphold their declared
effect/idempotency contract.

An abandoned model call can be retried as a new bounded model step. Its
reservation is persisted **before** dispatch in a separate versioned journal
bound to the run id. Recovery selects the newer journal/checkpoint generation
and charges unobserved in-flight reservations conservatively. A crash cannot
reset earlier cost or step counts. Missing reported usage remains unknown;
the ledger does not invent tokens for interrupted attempts.

Custom `Model` implementations must await `budget.reserve_before_dispatch`
for every paid attempt. Synchronous `reserve` is rejected with persistent
journaling. The built-in `GatewayTransport` follows this contract for retries
and fallback models. A journal failure prevents the provider request.

## Reproducible offline example

```text
cargo run --locked --features runtime --example agent_resume
```

The default demo creates a temporary workspace, waits for exact write approval,
reconstructs the controller, and creates a note once. It uses a deterministic
offline model. For separate process invocations, choose a new workspace:

```text
cargo run --locked --features runtime --example agent_resume -- start ./demo-state
cargo run --locked --features runtime --example agent_resume -- show ./demo-state
cargo run --locked --features runtime --example agent_resume -- approve ./demo-state
cargo run --locked --features runtime --example agent_resume -- resume ./demo-state
```

Review the printed request before running `approve`. The `save_note` tool uses
`create_new` and never overwrites a preexisting note. The example deliberately
does not auto-reconcile an interrupted non-idempotent write.

`tests/agent_durable.rs` also terminates actual child processes after an
external write/budget reservation, then verifies recovery, preserved costs,
fresh approvals, no duplicated effects, failed writes, exclusion and atomic
inspection. It uses no provider credentials.
