# Changelog

## 0.2.0-alpha.1 — 2026-11-27

- Optional `sandbox` worker contract and Docker Python tool: immutable audited
  image/local engine, no host mounts/network/credentials, bounded resources and
  a container PID 1 deadline/output supervisor with an unprivileged code child.
  Exact-code approvals freeze the worker identity; there is no host execution
  fallback. Command/control checks are separate from the ignored real-container
  acceptance suite, which needs an available engine and built audited image.

- Opt-in `research` sources: origin-granted DNS-pinned web fetch, configured
  Brave-compatible search and bounded trusted Poppler PDF extraction. Added
  CLI/API/UI imports with source hashes, UTF-8 extraction provenance and
  explicit unsupported-format behavior. Search credentials never follow redirects.
- Snapshot-bound vector indexes, provider embedding adapter and weighted
  reciprocal-rank hybrid retrieval. Stale/invalid indexes fail before query
  dispatch; custom application retrieval is shared by tools and operator search.
  Local HTTP/citation tests and actual Linux Poppler acceptance are documented
  separately from unexecuted hosted search/embedding/model quality validation.

- Workflow/session operator APIs and UI: host-defined roles, persisted DAG or
  PEER creation, exact child-controller approvals/recovery, session turn binding
  and explicit failed-turn acknowledgement. Shared typed admission bounds all
  parent workers. Role handoffs, child runs, spend and review revisions are
  inspectable; completed executors do not replay during draft revision. Added
  restart/effect/concurrency integration coverage and a multi-role UI fixture.

- Parse persisted decimal floating-point values with round-trip precision.
  Multi-source retrieval exposed a score changing precision on reload and
  invalidating a context fingerprint; full-suite CLI regression covers the
  original failure while retaining strict context/input integrity checks.

- Versioned task-acceptance campaigns with 12 supplied research/tool/approval/
  recovery cases, strict outcome checks, retained trajectories, shared campaign
  spending ceilings and explicit fixture/provider provenance. Incremental
  reports distinguish active, failed and not-run work, preserve unsettled cost
  and never equate fixture success with hosted-model effectiveness. Added
  `taskloom benchmark` and documented controlled fixture approval/recovery.

- Optional `app` host with an installable CLI, authenticated local run/knowledge
  API and browser operator console for trajectories, results, citations,
  approvals and execution controls. Persist before scheduling; explicit reentry
  and bounded local admission retain existing durable recovery guarantees.
- Direct persisted cancellation handles waiting and abandoned runs without
  further dispatch, auditing pending rejection and preserving unknown effects
  and model spending. Host worker panics become inspectable errors rather than
  silent replays. Read-only CLI commands require no provider/MCP connection.
- Multi-retrieval report verification retains distinct corpus revisions;
  local HTTP, real CLI subprocess and browser acceptance cover application
  delivery without paid model requests. Added installation/operator/API and
  host extension documentation and an offline approval console example.

- Optional `mcp` integration: explicit HTTP session initialization, bounded
  paginated catalog discovery, JSON/SSE tool responses, host-assigned aliases,
  effect/permission contracts and frozen endpoint/tenant binding identities.
  Calls reuse durable approvals/recovery and never retry remote effects or
  reinitialize expired sessions implicitly. Local HTTP fixtures verify protocol
  errors, limits, split SSE, auth/session headers and approved native tool use.

- Pluggable document loading/chunking/corpus/retrieval, bounded UTF-8 import,
  source-preserving overlap chunks, atomic local corpus replacement with CAS,
  deterministic lexical retrieval and a permissioned read-only RAG tool.
- Citation identity/quotation/marker validation against archived tool evidence
  and an offline cited research example. Source validation does not imply
  semantic support or truth; embedding/vector retrieval remains pluggable.

- Checkpointed sequential/parallel/conditional workflows with exact output
  handoff, stable durable agent children, global spending allocations,
  cancellation draining and explicit recovery contracts for custom handlers.
- Bounded planner/executor/writer/reviewer collaboration with validated plans
  and reviews, retained executor effects across rewrites and failure on review
  exhaustion. Offline `agent_peer` demonstrates native roles, two approved
  real file effects and a review-driven rewrite. Process-death/storage-failure
  tests verify parent/child recovery without repeating completed effects.

- Persistent sessions with active-turn binding, revision CAS, idempotent run
  re-entry, exact initial-input checks and explicit failed-turn cleanup.
- Optional bounded context projections with pluggable compression, complete
  tool-call/result blocks, unchanged raw archives and persisted model inputs.
- Run-state format 2 adds initial-input/context metadata. Development format-1
  checkpoints require the earlier checkout; single-task recordings are unchanged.

- Durable agent execution: versioned checkpoints and budget journals, per-run
  filesystem ownership, graceful pause/resume, exact-input approval tickets,
  auditable reconciliation of uncertain tool effects, and a restart example.
  Crash tests terminate child processes after writes/reservations; interrupted
  non-idempotent tools are never automatically repeated.

- Optional `runtime` feature: typed native tool conversations, JSON Schema
  tool validation, explicit permission grants, bounded sequential agent loops,
  run events/reports and model-spend reservations across retries/fallbacks.
- OpenAI-compatible and Anthropic native tool adapters share the gateway;
  existing single-task APIs remain available without the runtime dependency.
- Offline multi-step and live OpenAI-compatible examples, runtime guide and
  three-phase agent framework roadmap with explicit acceptance boundaries.

- Normalize OpenAI `message.refusal`, streamed `delta.refusal`, refusal
  content parts and `content_filter` into typed content rejection, preserving
  reported usage and avoiding resilient-JSON retries and repair turns.
- Require `[DONE]` or a nonempty `finish_reason` for OpenAI streaming
  completion; keep reading after `finish_reason` to collect trailing usage.
  Anthropic streams require `message_stop`. Premature EOF is transient and
  cannot populate recordings or step caches, even when the text is valid JSON.
- Version the built-in provider namespaces to invalidate answers stored
  before these protocol checks. Existing OpenAI/Anthropic recordings must be
  regenerated; their old keys are never used as an offline fallback.

## 0.1.0 (2026-11-27)

First release, extracted from a production video pipeline.

### Correctness fixes

- Chat recording and step-cache keys now include request format, exact
  temperature and output-token limit. Existing chat entries need to be
  recorded again because their filenames change.
- Response usage survives JSON validation, refusal and embedding-count
  failures, preserving both typed errors and gateway/ledger accounting.
- `Response` and the backward-compatible `Transport::send_with_metadata`
  preserve the serving model through gateway fallback, repair, hedging,
  cache hits and replay. `TaskOutcome::models_tried` counts the outer task
  model chain only.
- Default-feature API documentation builds without broken optional-provider
  links; CI checks documentation for each feature selection.
- Minimum Rust version and CI aligned to 1.86, matching the current dependency
  graph. Transport options use explicit `as_any` downcasting.
- Embeddings never cross model boundaries on fallback, and reject invalid,
  non-finite or dimensionally inconsistent stored/returned vectors.
- Store writes use cross-process directory locks, unique files and atomic replacement, with bounded retries
  for Windows sharing conflicts. Recording failures return `TaskError::Storage`
  and stop without spending another model request.
- Registry task timeouts now bound the full execution, including queueing,
  retry backoff, protocol turns, repairs and model chains. Attempt timeouts
  include admission waits; expired task deadlines do not trigger fallback.
- Built-in providers isolate recordings and caches by endpoint, protocol and
  routing settings; tenant/custom-client configurations have explicit namespaces.
  Existing built-in transport recordings and embedding caches need rebuilding.
- Anthropic uses the stable `2023-06-01` version header by default.
- CI covers Windows/Linux, Rust 1.86, feature combinations, examples and packaging.

### Features

- Typed tasks: `AgentTask`, `PromptTask`, `TaskSpec` (model chains, roles,
  protocols, deadlines, hedging, ledger, step cache).
- `Agent`: model resolution, the resilient JSON protocol, fallback on failure
  or rejection, deadlines, hedging, ledger, recording, replay, step cache.
- `Transport` and `ModelResolver` traits; `FnTransport` for tests.
- `gateway`: budget pre-checks, per-provider concurrency, per-model circuit
  breakers, transient retry and backoff, fallback chains, statistics; built-in
  profiles for common model families.
- `GatewayTransport` over a `Provider`; `openai::OpenAiCompatible`
  (feature `openai`).
- `ledger`, `hedge`, `json_text`, `retry`, `many`, `prompts`, `registry`.
- Messages in English or Chinese (`Locale`).
- Usage and cost: provider-reported tokens on `TaskOutcome::usage`, in the
  call ledger per kind and in gateway statistics; `PriceTable` prices them.
- Cancellation: `CancelToken`, `Agent::with_cancel`, `TaskError::Cancelled`;
  an ambient token for a whole job (`cancel::scope`), carried onto spawned
  tasks by `cancel::propagate` and `ledger::propagate`.
- JSON Schemas (`TaskSpec::schema`, `schema_of` with feature `schemars`) sent
  to providers whose model profile supports them; repair turns
  (`TaskSpec::repair`, `Phase::Repair`).
- Streaming: `Agent::with_progress`, `Request::on_delta`, `Streaming` and the
  `IdleTimeout` option of `GatewayTransport`; SSE streaming in both
  providers.
- `anthropic::Anthropic` (feature `anthropic`).
- Embeddings: `Agent::embed`, `Transport::embed`, `Provider::embed`, the
  OpenAI-compatible `/embeddings`; recorded and cached per input.
- `eval`: recorded requests (`RecordedRequest`), `load_cases`, `evaluate`,
  `Unchanged`, `ModelJudge`.
- `tracing` spans after the OpenTelemetry GenAI conventions.
