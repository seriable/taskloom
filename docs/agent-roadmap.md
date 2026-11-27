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
- [ ] Sessions, context limits and compression with preserved tool correlation.
- [ ] Sequential/parallel/conditional workflows and planner/executor/reviewer
  collaboration, with bounded revision and clear result handoff.
- [x] Restart and approval examples, including injected failures and tests.

Acceptance: restart an interrupted job, continue from recorded state, and
demonstrate review-driven bounded revision without silently duplicating effects.

Recovery increment: `DurableAgent`, `RunState`, `RunStore`/`FileRunStore`,
versioned budget journals, exact-input approval and operator reconciliation
are implemented. Actual child-process termination tests cover write/model
interruption, and injected storage failures prevent further dispatch. A lock
release regression is covered with a duplicate descriptor and repeated Linux
recovery runs. See [the recovery guide](durable-runs.md). Session/context and
workflow/collaboration acceptance is still outstanding; phase 2 is not complete.

## Phase 3: knowledge and application delivery

- [ ] Pluggable document ingestion, chunking, retrieval and source citations.
- [ ] Service API for run creation, status, events, cancellation and approvals.
- [ ] CLI for local execution and state inspection.
- [ ] Basic UI for run trajectories, results and approvals.
- [ ] Necessary ecosystem integrations (including MCP tool access), documented
  extension contracts and a cited research/report end-to-end example.
- [ ] Installation, configuration, API and operator documentation.
- [ ] Relevant automated checks, package verification and local service/UI
  acceptance; real-provider acceptance when credentials are available.

Acceptance: a new user can install the project, connect a provider and tools,
run a cited knowledge task, and inspect/control it through CLI and UI.

## Verification constraints

Provider tests use local HTTP servers and require no credentials. Offline
tests do not establish compatibility with every hosted model. GitHub CI and
real-provider checks are reported separately from local checks. Changes are
not considered published merely because they exist in the working tree.
