# Agent framework development

The existing typed task API remains supported. New agent capabilities live
above that execution core and are optional. An unchecked item is unfinished;
the presence of a type or a passing unit test alone does not complete a phase.

## Phase 1: bounded tool execution

- [x] Serializable messages, assistant tool calls and correlated results.
- [x] Tool registry, JSON Schema validation, permission scopes, execution
  timeouts, cancellation and explicit side-effect metadata.
- [x] An agent loop with step, tool-call, elapsed-time and model-spend limits.
- [x] Native OpenAI-compatible and Anthropic tool messages through the gateway.
- [x] Ordered execution events and an offline multi-step example.
- [x] Regression coverage of protocol errors, limits, cancellation, usage on
  invalid responses and side-effect boundaries; docs and feature/MSRV checks.

Acceptance: a goal causes successive model/tool turns, returns a final answer,
and has an inspectable trajectory. Limits prevent further dispatch. Legacy
single-task consumers continue to compile and pass their existing checks.

Local acceptance (2026-11-27): 125 all-feature tests pass on Windows,
Rust 1.86 and independent WSL Linux; native tool round trips are exercised
over local HTTP for both providers. Feature combinations, warning-free Clippy
and docs, and five offline examples are verified. The offline loop makes
three model decisions and two tool calls, producing 12 from doubling 3 twice.
Hosted CI and paid provider validation are separate external checks.

## Phase 2: state, recovery and collaboration

- [x] Versioned run/step state, atomic persistent checkpoints, ownership of
  concurrent runs and crash recovery semantics.
- [x] Pause/resume and human approval, with approval bound to exact tool input.
- [x] Explicit handling of uncertain tool side effects; no automatic retry of
  non-idempotent operations after an interruption.
- [x] Sessions, context limits and compression with preserved tool correlation.
- [x] Sequential/parallel/conditional workflows and planner/executor/reviewer
  collaboration, with bounded revision and clear result handoff.
- [x] Restart and approval examples, including injected failures and tests.

Acceptance: restart an interrupted job, continue from recorded state, and
demonstrate review-driven bounded revision without silently duplicating effects.

Recovery increment: `DurableAgent`, `RunState`, `RunStore`/`FileRunStore`,
versioned budget journals, exact-input approval and operator reconciliation
are implemented. Actual child-process termination tests cover write/model
interruption, and injected storage failures prevent further dispatch. A lock
release regression is covered with a duplicate descriptor and repeated Linux
recovery runs. See [the recovery guide](durable-runs.md).

Session/context increment: persistent turn binding and compare-and-swap
session commits, idempotent durable re-entry, bounded context projections,
pluggable extractive compression and exact projection recovery are implemented.
The original transcript remains intact. See [sessions and context](sessions-and-context.md).
Workflow/collaboration acceptance: checkpointed DAGs, exact selectors,
parallel admission, stable durable child runs, global spending allocations
and bounded PEER draft/review loops are implemented. The offline `agent_peer`
example runs four native roles, two approved file effects and one rewrite.
Actual process termination after an external write and injected parent-store
failures verify recovery without silently repeating effects. See
[workflows and collaboration](workflows.md).

Phase 2 local acceptance (2026-11-27): 173 all-feature tests pass on Windows,
Rust 1.86 and independent WSL Linux. Hosted CI and paid-provider acceptance
remain external checks. Phase 3 acceptance is recorded below.

## Phase 3: knowledge and application delivery

- [x] Pluggable document ingestion, chunking, retrieval and source citations.
- [x] Service API for run creation, status, events, cancellation and approvals.
- [x] CLI for local execution and state inspection.
- [x] Basic UI for run trajectories, results and approvals.
- [x] Necessary ecosystem integrations (including MCP tool access), documented
  extension contracts and a cited research/report end-to-end example.
- [x] Installation, configuration, API and operator documentation.
- [x] Relevant automated checks, package verification and local service/UI
  acceptance. External real-provider/hosted acceptance is reported separately.

Acceptance: a new user can install the project, connect a provider and tools,
run a cited knowledge task, and inspect/control it through CLI and UI.

Knowledge increment: pluggable loader/chunker/corpus/retriever contracts,
bounded UTF-8 import and overlap windows, atomic corpus replacement/CAS,
local lexical retrieval and a permissioned search tool are implemented.
The offline `agent_research` imports two files and verifies exact quotes and
source markers against archived evidence. Citation checks establish provenance,
not semantic truth. See [knowledge and citations](knowledge.md).
The application gathers all saved knowledge-search outputs for report
verification and retains distinct corpus revisions rather than hiding updates.

MCP increment: explicit Streamable HTTP initialization, bounded paginated tool
discovery, JSON/SSE calls and host-authored effect/permission bindings reuse
native durable approval/recovery. Endpoint/tenant binding changes reject reentry.
Local fixtures exercise the supported protocol surface without external service
credentials. See [MCP integration](mcp.md).

Application acceptance (2026-11-27): the optional `app` host provides an
installable executable, authenticated local API, run controls and a browser
operator console. API creation persists before dispatch; worker panics and
server restart retain explicit durable recovery. Read-only CLI commands do
not need provider/MCP connections. See [installation, API and operations](application.md).

All 195 all-feature tests pass on Windows, Rust 1.86 and independent WSL Linux.
Warning-free Clippy/docs, feature combinations, nine offline examples and
package verification pass. A real local debug-profile install executes and
verifies a cited knowledge task. Browser acceptance creates/inspects a run,
verifies sources, reloads persisted results after restart, and approves/resumes
one real file write. A narrow-screen check finds no horizontal overflow.

The three development phases have completed local acceptance. These remain
optional preview features targeted by `v0.2.0-alpha.1`; they are not in `v0.1.0`.
External provider/MCP deployment compatibility, hosted CI and production-scale
load testing have not been established by this local acceptance.

## Verification constraints

Provider tests use local HTTP servers and require no credentials. Offline
tests do not establish compatibility with every hosted model. GitHub CI and
real-provider checks are reported separately from local checks. Changes are
not considered published merely because they exist in the working tree.
