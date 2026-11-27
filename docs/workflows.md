# Durable workflows and PEER collaboration

Requires the unreleased `runtime` feature and Rust 1.86+. Run the complete
offline example with:

```sh
cargo run --locked --features runtime --example agent_peer
```

It runs four native durable model roles, plans two tasks, waits for explicit
approval of two real file writes, resumes, rejects the first draft and approves
the rewrite. It asserts two file effects, one planner call, two writer calls,
two reviewer calls, and no further dispatch on completed reentry. The model is
deterministic and local; this verifies orchestration, not model quality.

## DAGs and exact handoffs

`runtime::workflow` exposes `WorkflowDefinition`, `WorkflowNode`,
`WorkflowTasks`, `WorkflowRunner` and `FileWorkflowStore`. A node names a
registered `WorkflowTask`. Dependencies establish sequential ordering;
independent ready nodes run concurrently up to `max_parallel`. `Condition`
supports existence, equality, conjunction and negation over declared
dependencies. Missing handlers, cycles, undeclared condition/selector inputs
and duplicate identifiers fail before dispatch.

```rust,no_run
use taskloom::runtime::workflow::*;
use serde_json::json;

let graph = WorkflowDefinition::new([
    WorkflowNode::new("left", "research"),
    WorkflowNode::new("right", "research"),
    WorkflowNode::new("report", "writer")
        .after(["left", "right"])
        .select("evidence", "left", "/findings")
        .select("counterpoints", "right", "/findings"),
    WorkflowNode::new("publish", "publisher")
        .after(["report"])
        .when(Condition::Equals {
            node: "report".into(), pointer: "/approved".into(), value: json!(true),
        }),
]);
```

`NodeContext` contains the workflow/node IDs, a stable child run ID, original
workflow input, node parameters and dependency outputs. Selectors use JSON
Pointers. Without selectors, all declared dependencies are passed by name;
with selectors, only the selected aliases are passed. Skipped dependencies
skip successors unless `allow_skipped()` is explicit. A present JSON `null`
is a valid output and survives storage. Exact handoffs are checkpointed and
validated on load, rather than reconstructed from partial model text.

`AgentWorkflowTask` delegates to `DurableAgent::start_or_resume` with that
stable child ID. Its output can be text or locally schema-validated JSON.
Approval policy, role system prompt, model/run configuration and output
schema are frozen. Changed registration/configuration/limits reject resume.
Custom handlers must version their implementation contract explicitly;
configuration fingerprints cannot detect changed Rust code.

## Recovery and effects

The workflow saves an intent and spending allocation before polling a handler.
Completed nodes are never invoked again. Each handler declares one contract:

| Recovery | Implementer obligation | After process interruption |
|---|---|---|
| `Pure` | No effects, paid I/O or usage; zero cost ceiling | Safe recomputation |
| `DurableChild` | Reenter the exact stable child ID/input; child owns effects and spending journal | Reenter child and observe its state |
| `Manual` | Any other effectful implementation | Mark uncertain; operator must record observed output or abort |

These are host implementation contracts, not a sandbox. Effects must occur
inside the returned future. Cancellation and dropping a future do not undo
external operations. Tools keep their original read-only/idempotent/
non-idempotent metadata and exact-input approval rules.

Use `WorkflowStore::inspect` for atomic read-only status. It does not take
worker ownership or dispatch work. A file lease excludes concurrent workflow
controllers across processes. Failed checkpoint writes stop execution. If a
child completed but the parent's result checkpoint failed, reentry observes
the completed child without repeating its tools. Real process-death tests
also terminate after an external write and require child reconciliation.

For a native agent node waiting on approval, inspect `node.child.run_id`,
read its `RunState::pending_approval()`, then call the original role's
`DurableAgent::decide` with that ticket and actor/note. Resume the workflow
after decisions; merely approving does not dispatch tools. An uncertain tool
requires `DurableAgent::reconcile` on the child. Reentering the parent then
continues the existing child. A busy child remains waiting with unobserved
status; the workflow does not invent a successful outcome or release its
budget allocation.

Manual uncertain nodes use `WorkflowRunner::reconcile` with `RecordOutput`
(observed result/cost/usage) or `Abort`. `RetryPure` is restricted to pure
nodes in nonterminal workflows. Reconciliation records actor, note and time.
Durable-child effects cannot be overridden through the parent's manual
reconciliation API.

## Limits, pause and cancellation

`WorkflowLimits` bounds nodes, parallelism, total dispatches (including child
resume probes), active duration, shutdown grace, output bytes and optional
model spending. Waiting for an operator between invocations does not consume
active duration. An abandoned running invocation conservatively charges wall
time until recovery; a backward wall clock fails closed. Passive inspection
does not spend dispatches. Repeated explicit resumes do, even if approval is
still pending, so poll status with `inspect`.

A global spending cap requires a per-task ceiling. The scheduler reserves
each admitted node's full ceiling, waits for concurrent settlements when
needed, and releases unused funds on completion. Native agent costs are
cumulative across child resumes and counted once. Unknown costs remain
charged at their ceiling and marked uncertain. Custom implementations must
enforce their declared paid-attempt ceilings; the runner cannot preempt an
arbitrary external client that ignores its contract. This cap covers model
spending, not independent external service/tool charges.

`with_pause` requests a graceful pause: finish admitted nodes, admit no new
ones, then checkpoint. `with_cancel` or a deadline signals child tokens and
drains up to `shutdown_grace`. Uncooperative manual tasks become uncertain;
durable tasks keep a child reference and allocation for direct inspection.
Cancellation, deadline, dispatch/budget exhaustion and failure are terminal;
`resume` does not reset their limits. Outstanding effects in a terminal
workflow must still be reconciled through their owning controller. A terminal
label is not proof that an external operation was undone.

Workflow state format is `WORKFLOW_STATE_VERSION = 1`. Run checkpoints retain
their separate format 2. Both reject unsupported versions and corrupt state.
File roots are captured absolutely at construction. Persisted messages,
outputs, exact inputs and events may contain private data; host access control
and retention remain the application's responsibility.

## Bounded planner/executor/writer/reviewer flow

Register four roles and call `PeerSpec::install(&mut tasks)` to construct a
bounded static graph. `PeerPlan::schema(max_tasks)` and `PeerReview::schema()`
provide JSON output contracts for model roles. The plan validator rejects
empty/oversized plans, duplicate task IDs and empty instructions.

1. Planner produces a bounded list of tasks.
2. Executors consume distinct task selectors concurrently, exactly once per
   completed child. Extra worker slots skip.
3. Writer consumes the plan and executor outputs.
4. Reviewer returns `{ "approved": bool, "feedback": string }`.
5. A rejected draft is rewritten with previous draft/feedback, within
   `max_revisions`; executor effects are retained. Approval skips later rounds.
6. `result` selects the first approved draft. Exhausted revisions fail rather
   than treating an unapproved draft as success.

Planner/writer/reviewer must be pure transforms or native durable agent roles
with only read-only tools. Put side effects in executors. This prevents a
review rewrite from implicitly repeating a publishing/write role. Tool
implementations still must tell the truth about effects. PEER provides basic
bounded coordination; distributed workers, arbitrary cyclic graphs, dynamic
team formation and semantic report quality evaluation are not implemented.
