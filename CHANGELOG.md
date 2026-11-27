# Changelog

## Unreleased

- Optional `runtime` feature: typed native tool conversations, JSON Schema
  tool validation, explicit permission grants, bounded sequential agent loops,
  run events/reports and model-spend reservations across retries/fallbacks.
- OpenAI-compatible and Anthropic native tool adapters share the gateway;
  existing single-task APIs remain available without the runtime dependency.
- Offline multi-step and live OpenAI-compatible examples, runtime guide and
  three-phase agent framework roadmap. Durable state, workflows and application
  delivery are still in development.

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
