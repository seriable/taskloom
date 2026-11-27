# Journal diagnostics and explicit OpenTelemetry export

`runtime::observability` projects **validated saved journals** into a correlated
trace. It performs no model, embedding or tool calls. There is no process-global
subscriber, automatic uploader, implicit `OTEL_*` environment discovery or
send-on-drop behavior. Network export requires the optional `telemetry` feature;
the application feature includes it, while default features remain empty.

## Trace and time semantics

`TraceSnapshot::run`, `workflow` and `session` build run or parent traces.
Workflow/node/durable-run and session/turn/durable-run relationships come from
persisted references and exact initial-input bindings. Closed session children
also match the recorded report fingerprint. Unreferenced, duplicate or mismatched
children fail; absent children are counted. Active durable children can be found
through their frozen node context before an outcome returns to the parent.
Reads of separate records are individually atomic, **not** a cross-record
transaction or a guarantee that a live parent and child were read simultaneously.

Stable SHA-256-derived IDs use a trusted host namespace and logical identities.
Revisions change the source revision key, not existing trace/span identities.
Changing the namespace changes IDs. The application defaults to a namespace
derived from its absolute state root; set an explicit namespace before moving
journals if continuity across deployment paths matters. These hashes provide
correlation and pseudonymization, not cryptographic storage authentication.

Spans cover workflows, nodes, individual dispatches, durable runs, model
decisions, tool attempts, retrieval, approval waits, operator reconciliation and
output validation. A model decision can contain gateway HTTP retries; the native
usage request count is a different quantity. A node can dispatch repeatedly to
the **same** durable child without repeating an uncertain external effect.
Child usage is counted once, not again as parent workflow usage.

New managed children persist a host-owned `TraceBinding` before their first
checkpoint. Independently inspecting such a child uses the same trace, global
root, run and descendant span IDs as inspecting its workflow/session. The
binding is outside model input and execution configuration, changes no grants,
and is immutable across checkpoint revisions, including cancellation. A model
cannot create it through a prompt. SDK callers may attach an exact managed
identity with `DurableAgent::with_trace_binding`; invalid child IDs or workflow
input bindings fail before the initial budget journal is created.

Child-only views have `selected_span_id` pointing at the real run and two
`context_only` ancestor references. References have unknown outcome, no clocks
and no asserted completion: they are omitted from OTLP as `omitted_context`,
separately from actually open or missing-clock observations. Inspect the owning
parent for its current observed state. These projections do not install a live
subscriber or inject W3C headers into external services.

New `RunEvent` and `WorkflowEvent` records include optional `unix_ms`. Relative
elapsed time still drives the existing execution limits; wall time never changes
approval, budget or recovery admission. The OTLP projection uses actual recorded
Unix times, including downtime between parent/run start and completion. Active
elapsed time is a separate attribute and may exclude operator/offline waits.
Missing, backwards or overflowing wall clocks stay visible and are omitted from
trace export; they are never synthesized from relative durations. Open spans are
not exported as completed. Sessions have no close operation, so their root stays
open; completed turns/runs can be exported with that still-open parent ID.

A stopped uncertain tool span retains its historical uncertainty even after
successful reconciliation; the separate recovery span records the bounded
`record_result`, `retry_safe` or `abort` action. It does not export the actor,
note or observed result. Output-rejection/validation spans retain repair history.
Span status and safe stage codes distinguish model, tool, output-contract,
authorization, context, storage, generation-budget and recovery outcomes.

## Metrics, retrieval and billing

Metrics describe the selected scope's saved journal: model decisions, native
usage requests/tokens, tool attempts/errors, recovered models, output checks,
omitted context messages, retrievals, empty recalls and skipped oversized chunks.
OTLP exports up to eighteen **gauges**, not process-wide totals, deltas or a cross-run
latency ranking. Re-exporting snapshots can resend the same completed span IDs;
the receipt/revision key permits host-side tracking, but exporter delivery is not
exactly once. No automatic retry hides an uncertain receiver outcome.
Gauges bind both `taskloom.trace_id` and `taskloom.selected_span_id`. A parent
scope includes its children; a child scope contains only that run. Do not sum
overlapping scopes or treat repeated snapshot observations as incremental work.
Completeness and usage-reporting flags accompany the observations. Missing child
records suppress generation gauges; unreported provider usage suppresses token
and request gauges. Missing data does not become a zero-valued measurement.
Local JSON retains recorded subtotals with those explicit validity flags.

Generation usage and spending come from saved run checkpoints. While a model
request is live, its independently persisted spending-reservation journal can
be newer; these metrics are **not a live invoice** or a replacement for admission
accounting. The existing budget controller still persists reservations before
paid dispatch. Generation micro-units use the configured price-table currency.
Committed generation amounts already include uncertain-attempt ceilings; do not
add the uncertainty field again. In-flight reservations remain separate.
Embedding remains in its independent host ledger (`index-status` / `/retrieval`);
per-run embedding allocation is unavailable here. Generic tools do not report
service billing. Those two attributed cost fields are `null`, never assumed zero.
An explicit zero-priced connector example does not make arbitrary tools free.

Saved retrieval diagnostics expose revision, returned/skipped counts and scores.
Local inspection also shows exact document/chunk IDs, titles and byte ranges to
identify problematic recall without repeating a paid query. Body quotes, source
URIs and tool arguments are not added to this view. The exporter **never** sends
these local hit records: its separate allowlist admits only static names,
pseudonymous IDs, validated statuses, booleans and numerical attributes. Tests
check that prompts, model/tool names, arguments, answers and source contents do
not enter OTLP. An SDK caller adding an unrecognized span attribute gets an error.

## Application configuration and operations

No `observability` setting is needed for local inspection. To explicitly enable
export operations, add this host-owned config fragment:

```json
{
  "observability": {
    "namespace": "my-project-development",
    "otlp": {
      "endpoint": "http://127.0.0.1:4318",
      "service_name": "taskloom",
      "bearer_env": null,
      "timeout_ms": 10000,
      "max_in_flight": 2,
      "max_payload_bytes": 2097152
    }
  }
}
```

Configuring a destination constructs an inert HTTP client; **only an explicit
export operation sends data**. `bearer_env` names an optional host environment
containing a bearer token, resolved when constructing the exporter. Never put
the token value in configuration or model input. The receiver URL cannot contain
credentials, query parameters or a fragment. HTTP is restricted to loopback;
other destinations require HTTPS. No ambient proxy or redirect is followed.
The service name is an explicitly exported host label; choose a nonsecret name.

```powershell
taskloom --config taskloom.json diagnostics workflow research-job
taskloom --config taskloom.json trace-export workflow research-job REVISION_KEY EXPORT_BINDING
```

`diagnostics` accepts `run`, `workflow` or `session` and executes before model,
MCP and exporter initialization. It works with service credentials removed.
Use the displayed source revision key and export binding for `trace-export`.
They bind the read journals and configured destination/options; changed inputs
require inspecting again. These read-only commands never rewind live stores.

Authenticated API equivalents are `GET /api/diagnostics/{kind}/{id}` and
`POST /api/diagnostics/{kind}/{id}/export` with exactly
`{source_revision_key,export_binding}`. The request cannot supply a destination,
headers or a model/tool implementation. Changed revisions or export settings
return 409 before sending. The UI shows parent IDs, clocks, outcomes, metrics,
error stages and retrieval details, and displays the configured destination
before enabling its explicit export button. Ordinary viewing does not export.
This is the existing single-operator bearer host, not P2 multi-user authorization.

The separate comparison view reads two version-1 Taskloom `BenchmarkReport`
files **locally**. It shows case outcomes, failures, recorded ledgers and declared
condition differences. Fixture/offline ledgers do not measure actual API/chat
fees, and their wall time is integration time. Imported content is not attested;
matching labels do not establish equal prompts, prices, hardware or independent
claim support. The view remains descriptive and does not infer leadership or
latency rankings. Other campaign schemas, including the current standalone
LangGraph adapter report, need an explicit adapter and are rejected visibly.
Each selected file is bounded to 4 MiB and 256 cases; select a narrower campaign
when a report exceeds those UI limits.

## Bounds, shutdown and error behavior

SDK snapshots accept at most 4,096 spans, 32,768 events per record and 256 child
runs. Application inspection caps a record at 16 MiB and child records at 64 MiB
combined. Larger histories require selecting narrower runs. Export defaults to
two in-flight calls and 2 MiB per signal payload. A full semaphore rejects the
operation before allocating payload buffers; there is no unbounded queue.
Responses are capped at 16 KiB, each HTTP attempt has a timeout, and both trace
and metric outcomes appear separately in the receipt. HTTP errors, OTLP partial
acceptance, invalid responses and acceptance-unknown timeouts are not successes.
Oversized payloads fail before either signal is sent. No automatic retry occurs.

`OtlpHttpExporter::shutdown` refuses new work and waits up to its configured
timeout for admitted operations. On timeout it reports remaining work; callers
still own those futures, which retain their own request deadlines. There are no
exporter-created background workers to leak. Application shutdown uses this
method. A dropped/cancelled network future may have been accepted remotely;
repeat export is an explicit host decision, not an exactly-once guarantee.

## Migration and reproducible acceptance

Missing `unix_ms` deserializes to `None` and serializes without the new field,
preserving old journal/report hashes and old closed-session bindings. No config,
approval input or model transcript is migrated or changed. Old records remain
locally diagnosable but have no fabricated OTLP trace timing. Rust struct literals
for `RunEvent`/`WorkflowEvent` must add `unix_ms`; direct `AppConfig` literals need
the new optional `observability` field or a default update. `AgentTask` API,
minimal default features and Rust 1.86 remain supported. There are no new crates.

Missing `RunState.trace_binding` remains absent when serialized. Old unbound
children resume without backfill and retain their original input/report hashes;
their standalone view still has a standalone trace. Parent inspection continues
to join them from its verified persisted references. No implicit scan, parent
inference from model text or historical ID rewrite is performed. New direct
`RunState` literals must supply `trace_binding: None` or an exact host binding.
Custom `RunLease` backends must preserve the original binding, including legacy
`None`, as the file store does. A strict resume with conflicting existing host
binding fails; administrative cancellation can still resolve the original run.

```powershell
cargo test --locked --all-features --test agent_observability
python examples/observability/download_collector.py target/collector-pinned
target/collector-pinned/otelcol.exe --config target/collector-loopback.yaml
python examples/benchmarks/native_skills.py target/native-observability --config local-native-otlp.json --taskloom target/debug/taskloom.exe --research-package research-located
python examples/observability/accept_native.py target/native-observability --taskloom target/debug/taskloom.exe --collector-log target/collector.log
python examples/observability/accept_sessions.py target/native-session-traces --config local-native-otlp.json --taskloom target/debug/taskloom.exe --collector-log target/collector.log
```

See `examples/observability/README.md` for loopback receiver setup and versions,
and `acceptance/p1-observability-20261128.json` for raw native evidence, actual
Collector output, browser observations, failures and check boundaries. The later
`acceptance/p1-trace-binding-20261128.json` records fresh native workflow/session
parent binding, cold CLI/API continuity, actual receiver acceptance and failed
diagnostic-ID probes before the fix. This
increment does not complete P0 research/collaboration quality or P2 service/UI.
