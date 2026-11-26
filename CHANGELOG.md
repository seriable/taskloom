# Changelog

## 0.1.0 (unreleased)

First release, extracted from a production video pipeline.

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
