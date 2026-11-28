# Changelog

## Unreleased

- Optional host lexical stop words and exact long alphanumeric identifier
  constraints prevent function-word and absent-record false recalls without
  rewriting citable source text. Empty/disabled settings preserve legacy
  fingerprints. A fixed configured Qwen profile measured hybrid recall@5
  46/48 and 22/22 empty negative queries; the first profile failure and two
  remaining positive misses are retained. This small visible development set
  does not establish general default retrieval or semantic/factual quality.
  See `docs/retrieval-relevance.md`.

- Explicit SQLite/PostgreSQL durable project queues persist exact input before
  independent worker processes execute existing run/workflow/PEER/session
  controllers. Queue/runtime fences, approval/recovery occupancy, authenticated
  submit/control actors, cooperative cross-process cancellation and strict
  capability bindings prevent stale dispatch and uncertain-write replay.
  Schema v3 explicitly upgrades quiescent v1/v2 datasets and refuses old binaries.
  CLI/API, real native worker-death/reconciliation, finite four-process load and
  full queue backup/migration evidence are documented in `durable-queue.md`.
  Source-only delivery and broader P0 quality gates remain separate.

- SQL project generation quotas atomically admit conservative monetary ceilings
  and outstanding attempts across pools/processes; unknown zero-priced attempts
  retain capacity through crashes. Explicit CAS administration imports prior
  journals, audits policy changes and continues accounting while disabled.
  Known refusal no longer invents uncertain spending; already incurred overruns
  persist and require explicit review. Schema-v2 migration requires quiescent
  hosts and fences old v1 binaries. All SQL generation requires known prices,
  including runs without an individual cap. File stores, minimal features,
  AgentTask and Rust 1.86 remain compatible. Queue/workers and broader quality
  gates remain open. See `docs/generation-quota.md`.

- Native collaboration V5 adds explicit submitted-draft review and corrected
  bounded role guidance, with a pinned optional Qwen3-4B download profile. Real
  local results remain 1/3, not general quality acceptance. An actual upstream
  agentUniverse PEER/parallel-executor adapter records a qualified 0/1 shared
  task result with native traffic and protocol differences. Separate non-blind
  session-assisted claim annotations distinguish citation support from
  corpus-relative factuality. Failed harness/dependency probes remain visible;
  see `docs/collaboration-comparison.md`.

- Journal diagnostics project validated run/workflow/session records into parent
  traces with model/tool/retrieval, approval, reconciliation and output checks.
  Explicit OTLP/HTTP JSON export sends bounded allowlisted spans and gauges;
  nothing uploads by default. CLI/API/UI expose billing boundaries and local
  evaluation comparison without inferring rankings. Actual native-model and
  official Collector evidence is retained. New managed children persist immutable
  host trace bindings; independent views retain parent identity and qualify
  context-only ancestors. Gauges include the selected scope to distinguish
  overlapping observations. Legacy unbound runs resume without backfill.
  New optional event timestamps preserve old wire
  hashes; direct Rust event literals need `unix_ms`. See `docs/observability.md`.
- Opt-in exact-locator citation contracts retain model JSON and resolve only
  unambiguous retrieved document/revision/UTF-8-byte-offset references. Workflow
  handoffs receive verified canonical citations; report API/CLI expose original
  text and explicit mappings. Legacy full-ID citations remain strict. The
  versioned research-brief 1.1.0 source example uses the new format; historical
  failures remain recorded. See `docs/final-output.md` for migration and limits.
- Data-only versioned skills: bounded USTAR validation, immutable content pins,
  exact dependencies, tool/credential declarations and separate host grants.
  CLI install/inspect/retire and mounted API templates preserve durable package
  identities; explicit recovery-only mounts cannot create new workflows. Source
  MCP examples read GitHub metadata and reconcile a real response-lost note
  append. Native citation failure remains visible. See `docs/skill-packages.md`.
- Workflow child approval/reconciliation now uses the same role-selected final
  output contract as child execution, including validated packaged roles, instead
  of inheriting a top-level session contract or omitting the role validator.
- Opt-in cross-session memory: scoped file snapshots, source-linked native
  extraction candidates, operator review/conflict replacement, versioned edits,
  export and erasure with anti-resurrection bindings. SDK/API/CLI and explicit
  memory-aware session turns retain ordinary history and host-owned tool grants.
  Small local Qwen campaigns passed their declared lifecycle checks; this
  is not general semantic-recall quality or multi-user service authorization.
  See `docs/long-term-memory.md` for limits and migration.
- Opt-in host final-output contracts validate exact JSON/schema or citations
  before a durable run can complete. Bounded repair freezes the policy, keeps
  the original budget and narrows tools to permitted read-only retrieval (none
  for JSON). Workflow roles can return validated typed JSON. Existing default
  behavior and serialized configs without a policy are preserved; exhaustive
  Rust matches need the new output/event variants. Exact citation matching does
  not establish factual correctness. See `docs/final-output.md`.
- Native evaluation examples now exercise verified local Qwen weights, real
  embedding index lifecycle, actual DAG/PEER roles and an audited LangGraph
  comparison. Failures remain in the reports: basic trials scored 5/12 for
  both frameworks, and all three initial collaboration tasks failed final
  citation/review checks despite observed parallelism and reviewer rewrites.
  No broader superiority or P0-P2 completion is claimed.
- Vector retrieval accepts an explicit, identity-bound minimum cosine score
  (`retrieval.semantic_min_score` in the app). Zero preserves positive-only
  behavior; thresholds require model/corpus calibration and do not prove
  relevance or claim support. See `docs/native-evaluation.md`.

- P0 retrieval increment: field-aware lexical recall (body/title/selected
  metadata), bounded host-owned query aliases, explicit application hybrid
  configuration and CLI/API/UI index lifecycle. Embedding requests use a
  separate durable spending ledger; unknown usage retains reservations.
  See `docs/research-tools.md` for pricing boundaries and migration. Real
  embedding quality and native-model/competitor effectiveness remain unverified.
- **(durable tool binding change)** Default lexical ranking is now `lexical-v2`;
  suspended runs using the old search definition must finish with the original
  binary or restart as a new run. `LexicalRetriever` remains a public unit struct.

Fixes from a full review of 0.2.0-alpha.1. Breaking changes are marked **(breaking)**.

### Core

- **(breaking)** The registry's whole-call timeout is off by default:
  `TaskProfile::timeout` is `Option<Duration>`, `timeout_seconds = 0` means
  none, `TaskEntry::with_timeout` opts in. The previous 120 s default cut off
  long requests and the fallback chain for every unregistered label.
- Breaker and concurrency-permit waits are no longer part of the attempt
  timeout. A batch queueing behind the provider limit is not failed for
  queueing, and a caller waiting on an open breaker gets the circuit-open error
  (with `CIRCUIT_OPEN_MARKER`) again. A declared whole-call timeout still covers
  queueing.
- **(breaking for stores)** The transport cache namespace keeps only
  answer-affecting settings (endpoint, explicit namespace, seeds, schema/image
  capabilities and fallback routing; overrides are keyed by prefix in sorted
  order, so reordering them keeps recordings valid). Tuning
  timeouts, retries, concurrency or budgets no longer invalidates recordings
  and cached steps; recordings made with 0.2.0-alpha.1 must be regenerated once.
- `RequestKey::for_transport` builds the key the runner stores a request
  under; `Environment::write_answer` documents it.
- **(breaking)** `ModelError` and `TaskError` are `#[non_exhaustive]`.
- Temporary files of atomic writes have a recognisable name; stale ones are
  pruned with the step cache.

### Runtime, sessions and workflows

- A session turn whose RetrySafe retry never started (approval rejected,
  limit hit, cancelled) can be closed; a call whose current attempt may have
  been dispatched is still refused.
- `cancel`, approval rejection, `RecordResult` and `Abort` work after a tool
  definition bump; `resume`, approval and `RetrySafe` stay strict.
- `Abort` resolves the uncertain call (error result: aborted by operator,
  outcome unknown), so the turn can be closed.
- **(breaking)** New `RuntimeError::StorageLimit` / `StopReason::StorageLimit`
  (HTTP 507): a turn or dispatch that could not be committed under the store
  size cap is refused before dispatch. Stores report their cap through
  `max_bytes()`; writes, including operator notes, always obey it. Every
  admission keeps about 21 KiB free for an operator's terminal record, so a
  run admitted to work can always still be cancelled with the longest note;
  stores that bound runs therefore need at least that much headroom.
- **(breaking)** Decision actors and notes (cancel, approve, reconcile) may not
  contain control characters other than newline and tab, which bounds their
  JSON size for that reservation.
- **(breaking for Rust RunLimits literals)** `max_model_result_bytes` bounds
  serialized native replies (64 KiB by default, also defaulted in old JSON).
  Admission reserves declared reply limits, duplicated tool state and actual
  session commit metadata. Oversized custom replies leave a terminal record
  with observed charges instead of an in-flight checkpoint eligible for replay.
  Oversized session input is refused before binding a turn.
- A stale checkpoint or workflow save returns `RuntimeError::Conflict` instead
  of reporting success.
- The context summary of dropped current-turn blocks follows the current
  question.
- Cancelling a live job records the operator's actor and note
  (`CancelReason`, `DurableAgent::with_cancel_reason`).
- Workflows: crash recovery runs on every entry point, `reconcile` included;
  a non-running workflow with `Running` nodes fails validation; a finished
  child without final text fails its node; a resume that changes nothing adds
  no events.

### Retrieval, MCP, sandbox and app

- `SearchResult::skipped_oversize` counts ranked hits left out for size.
- Citations need a minimum quote (`QuotePolicy`: 12 non-space characters,
  3 words or 6 CJK characters by default); the docs state what a self-hashed
  chunk id does and does not prove.
- MCP: remote tool descriptions are framed as untrusted and capped at 2 KiB,
  or replaced by a host-authored `McpToolBinding::description`; SSE parsing is
  linear and accepts bare CR; proxies are disabled; a session id first sent
  after `initialize` is rejected.
- Web: bounded incremental HTML5 tokenization/tree building checks deadlines
  and caps live parser references at 512, including quoted attributes and raw
  text; four workers cap concurrent CPU work. Extraction traverses once.
- Sandbox: the supervisor verifies the code's byte length and SHA-256 before
  running it (the image digest changes); Docker's own error wins over a broken
  stdin pipe; `python_sandbox` is offered to top-level runs and sessions only
  with `sandbox_top_level = true` (workflow roles unchanged).
- App: `http://` provider URLs only for loopback hosts; Ctrl-C stops accepting
  requests before shutting workers down; a missing workflow node entry is an
  error, not a panic. CI builds `app_approval` and runs the offline CLI flow.

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
