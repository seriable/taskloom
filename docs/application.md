# Local application: CLI, API and operator UI

The `v0.2.0-alpha.1` preview's `app` feature adds a packaged executable and a shared
durable run controller. It includes `runtime`, both provider adapters and MCP.
Library-only users keep the existing minimal default feature set.

Current development sources also support explicit
[SQL Application persistence](sql-application.md) with `app,database`, including
the `database-migrate` command. File storage remains the default. The separate
[multi-user service](multi-user-service.md) binds authenticated roles to explicit
projects; the ordinary operator UI/API remains a single-operator surface.

New source checkouts also provide pure configuration review and exact workflow
preview/launch APIs. See [workflow design](workflow-design.md) for their additive
paths, migration, deferred checks and current P2 acceptance limits.

## Install and run offline

Use Rust 1.86 or newer. A CLI-only installation uses the pinned preview:

```sh
cargo install --git https://github.com/seriable/taskloom.git --tag v0.2.0-alpha.1 --locked --features app
```

For the supplied offline example, install from a checkout containing its inputs:

```sh
git clone --branch v0.2.0-alpha.1 https://github.com/seriable/taskloom.git
cd taskloom
cargo install --path . --locked --features app
taskloom init taskloom.json
taskloom import approval "Approval guide" examples/knowledge/approval.md
taskloom import recovery "Recovery guide" examples/knowledge/recovery.md
taskloom run research "approval recovery"
taskloom report research
taskloom inspect research
taskloom events research
taskloom list
```

`init` creates a full editable JSON configuration and refuses to overwrite a
file. The default model is an explicitly labelled deterministic offline demo:
it searches local knowledge and returns source excerpts. It does not simulate
general reasoning or claim to be a live model. `report` verifies source identity
and exact quotes against saved tool results. Missing evidence is reported
explicitly. See [knowledge/citations](knowledge.md) for semantic limitations.

The application is absent from `v0.1.0`; use the preview tag above or this source
checkout. No crates.io upload is part of this preview. Public tag/release and
hosted validation status are recorded in the [delivery checklist](preview-delivery-roadmap.md).

Optional web search, webpage ingestion, PDF upload/import and host-injected
hybrid retrieval are described in [research tools](research-tools.md). They
require explicit source/service/extractor configuration; defaults grant none.

All commands accept `--config FILE` before the command. Relative `state_dir`
paths resolve against the configuration file's directory. `inspect`, `events`,
`list`, `report`, `knowledge`, `search` and `import` do not initialize a provider
or contact configured MCP endpoints. They work when provider credentials are
absent. Other commands open the configured host/model and tool bindings.

`run` and `resume` wait for the local worker to reach a durable boundary, then
print its complete state. Exit 0 means the command succeeded; a waiting approval
or recovery boundary is not a completed answer. Inspect `status` and
`report.stop_reason`. A finished failed/limited run exits nonzero. IDs use
1–64 ASCII letters/digits/hyphens/underscores. Reusing an existing ID for a new
run fails; completed resume does not dispatch again.

## Configure a provider

Edit the generated configuration. For OpenAI-compatible HTTP, set:

```json
{
  "provider": {
    "type": "openai",
    "base_url": "https://api.openai.com/v1",
    "key_env": "OPENAI_API_KEY",
    "namespace": "my-provider-tenant-v1"
  },
  "model": "your-model-id",
  "prices": [
    {"prefix": "your-model-id", "input_per_million": 1.0, "output_per_million": 4.0}
  ]
}
```

The prices above illustrate configuration; replace them with your provider's
actual contract. Export `OPENAI_API_KEY` in the host environment. Anthropic uses
`type: "anthropic"`, a base URL such as `https://api.anthropic.com`, and
`key_env: "ANTHROPIC_API_KEY"`. Set the appropriate model ID.
`base_url` must use `https://`. Plain `http://` is accepted only for loopback
hosts (`localhost`, `127.0.0.0/8`, `[::1]`), such as a local model server,
because the API key would otherwise cross the network in cleartext.

The top-level config has defaults, so a short configuration can omit unchanged
fields. Nested `limits` and `context` values use their full generated objects;
edit individual values in the generated file. `output_tokens`, model limits,
tool permissions, context policy and approval policy are frozen for each run.
Provider configuration, tenant namespace, pricing and the system instruction
also contribute to its frozen identity. Changing them rejects execution reentry;
read-only inspection still works. Keys are read from environment names, not
stored in run definitions. Use a distinct namespace for tenant/config changes.

Set `limits.max_cost_micros` to bound model spending, and supply valid prices.
The gateway reserves a conservative maximum before every paid attempt and
counts retries/fallbacks. Unknown pricing fails before dispatch under a spending
cap. `null` disables this optional cost cap. Tools and external MCP/retrieval
services can have separate charges. Step/tool/time/context limits still apply.
The native provider adapters are covered by local HTTP fixtures; run a small
acceptance task on your intended hosted model before relying on it.

## Start the operator console

Set a local API token of at least 16 non-whitespace characters. For PowerShell:

```powershell
$env:TASKLOOM_API_TOKEN = 'replace-with-your-local-operator-token'
taskloom serve 127.0.0.1:8080
```

Open `http://127.0.0.1:8080/`, enter the same token and connect. The UI provides
document upload, run creation/list/status, final text, source verification,
ordered events, full saved state, pause/resume/cancel and exact-input approval.
The token stays in page memory, is cleared from the input after connection,
and is lost on refresh. There is no browser storage of credentials, external
asset/CDN dependency or CORS grant. Run content is rendered as text rather than
executing source HTML. Small screens use a single-column layout.

The CLI binds loopback only. The library exports `runtime::app::router` and
`serve` for an application-specific deployment. This host is a basic single
operator console, not a multi-tenant production control plane: it does not
provide OAuth/RBAC, tenant isolation, TLS termination, distributed scheduling,
database migrations, automatic retention or a visual workflow editor. Supply
those at the deployment boundary when needed. The console also operates
persisted role DAGs, PEER collaboration and conversation sessions as described
below. There is no automatic resume after restarting the server.

All `/api` routes require `Authorization: Bearer TOKEN`, including read access.
The static UI shell contains no secret. Responses disable caching. Request
bodies are limited to 1 MiB; UI uploads accept up to 750 KB. CLI file ingestion
supports the RAG loader's 8 MiB limit. A file store is intended for small local
collections, and listing returns at most 1,000 run summaries (scanning at most
20,000 directory entries). Status inspection is the last atomic checkpoint,
not speculative in-memory progress.

## API contract

JSON responses carry data or `{ "error": "..." }`. Bad configuration/protocol
input returns 400, permission failures 403, missing/conflicting/owned state 409,
and storage/internal failures 500. Invalid JSON/extractor input may return
Axum's 400/422 response; an oversized body returns 413. Unauthorized access
returns 401 before run inspection.

| Method and path | Input / behavior |
|---|---|
| `POST /api/runs` | `{ "id": "research", "prompt": "approval recovery" }`; saves a new run before returning 202, then schedules its worker |
| `GET /api/runs` | Bounded summaries and limit; each includes local worker activity/error |
| `GET /api/runs/{id}` | `{ state, job }`, including messages, result, usage, budget and pending approval |
| `GET /api/runs/{id}/events?after=0` | Up to 200 ordered events; `next` is the next zero-based offset, with checkpoint revision and job status |
| `POST /api/runs/{id}/resume` | Explicitly schedules reentry; finished nodes/runs do not replay |
| `POST /api/runs/{id}/pause` | Signal an active local worker; finish its current operation before stopping |
| `POST /api/runs/{id}/cancel` | `{ "actor": "operator", "note": "reason" }`; signal a local worker or cancel an unowned saved run without dispatch |
| `POST /api/runs/{id}/approvals/{ticket}` | `{ "approved": true, "actor": "operator", "note": "reviewed exact arguments" }`; records a decision only |
| `POST /api/runs/{id}/reconcile/{call}` | A `RecoveryDecision` JSON; records observed output, safe retry eligibility or abort, without implicitly running tools |
| `GET /api/runs/{id}/report` | Verify final `CitedReport` against all saved `knowledge_search` outputs, preserving distinct corpus revisions |
| `GET /api/roles` | Host-configured role contracts; requests cannot grant new tools or replace role system prompts |
| `POST /api/workflows` | `{id,input,definition}` for a role DAG or `{id,input,peer}` for PEER; optional full `WorkflowLimits`; validates and saves before 202 |
| `GET /api/workflows` / `GET /api/workflows/{id}` | Bounded parent summaries / complete saved graph, handoffs, events, child IDs, budget and local job |
| `GET /api/workflows/{id}/events?after=0` | Up to 200 ordered parent events with next offset and revision |
| `POST /api/workflows/{id}/resume`, `/pause`, `/cancel` | Explicit parent controls; cancellation takes `{actor,note}`; no completed executor replay |
| `POST /api/workflows/{id}/nodes/{node}/approvals/{ticket}` | ApprovalDecision handled by the node's original role controller; separately resume the parent |
| `POST /api/workflows/{id}/nodes/{node}/reconcile/{call}` | RecoveryDecision handled by the original role; inspect the actual external effect first |
| `POST /api/sessions` | `{id}`; stores the host system prefix and returns 201 |
| `GET /api/sessions` / `GET /api/sessions/{id}` | Bounded summaries / complete append-only conversation, committed turns, active binding and local job |
| `POST /api/sessions/{id}/turns` | `{turn_id,prompt}`; persist binding and durable child before 202; another active turn conflicts |
| `POST /api/sessions/{id}/resume`, `/pause`, `/cancel` | Operate the active turn; cancellation takes `{actor,note}` |
| `POST /api/sessions/{id}/approvals/{ticket}`, `/reconcile/{call}` | Decision for the active turn only; separately resume the session |
| `POST /api/sessions/{id}/abandon` | `{actor,note}` acknowledges a failed terminal turn after uncertain effects are reconciled; then permits another turn |
| `GET /api/knowledge` | Revision, document summaries and chunk count |
| `POST /api/knowledge` | `{ "document": { "id", "title", "source", "text", "metadata": {} }, "expected_revision": 0 }`; atomic import/replacement |
| `DELETE /api/knowledge/{id}?expected_revision=1` | Atomic document/chunk removal with optional CAS |
| `POST /api/knowledge/search` | `{ "text": "query", "top_k": 5, "max_bytes": 24576 }`; bounded source-bearing results |

The event API uses polling and cursor offsets. It is not a websocket or push
event stream. Poll inspection rather than repeatedly calling resume while a
task awaits a human. Worker errors are retained in the local controller until
evicted/restarted (up to 1,024 job records); saved run state remains authoritative.
After server restart, a saved `running` state with no worker is interrupted
work, not an automatically restarted job. Explicit resume applies durable
recovery rules. Active worker ownership is local to this host instance; use
its API/UI to signal cancellation rather than a separate CLI process.

## Workflow roles and conversation sessions

The host configuration's optional `roles` array defines each role's `name`,
`system`, `output` (`{"type":"text"}` or `{"type":"json","schema":...}`)
and explicit `tools` allowlist. All tools must already be permitted by the host;
roles cannot expand permissions. Empty tools means no tools. Role schemas are
validated before the server starts. Example role configurations are in
[`examples/app/roles.json`](../examples/app/roles.json); merge the `roles` field
into a provider configuration and choose model prices/ceilings before paid use.
The normal offline model returns excerpts, so it cannot perform a general PEER
plan. This is reported as an output-contract failure rather than invented work.

`definition` uses the serialized [workflow DAG](workflows.md) types; node `task`
values name configured roles. `peer` uses `PeerSpec` (planner/executor/writer/
reviewer names, `max_tasks`, `max_revisions`). Planning, writing and review tools
must be read-only; effects belong to executors. The console provides a JSON DAG
input and a bounded PEER preset. The saved graph shows dependency handoffs,
per-node spend, reserved/uncertain parent budget, child runs and review revisions.
The internal application input envelope retains the user `input` and optional
PEER specification so generated pure validation tasks can be restored exactly.

`WorkflowRunner::create` saves a Ready workflow without executing a task. API
workers resume this checkpoint using saved limits and current role contracts;
changed provider/tool/prompt/schema settings reject reentry. Parent creation
IDs and turn IDs use 1–64 ASCII letters/digits/hyphens/underscores. Generated
workflow/session child IDs are longer: `GET /api/runs/{id}`, events and report
inspection accept up to 128 characters. Generic run mutation routes reject
these managed child IDs. Use the parent-specific decision/control route so an
approval always reaches its owning configuration. In the UI choose **Inspect
child** or **Inspect turn**, review the exact call, decide, then **Resume**.

Sessions preserve the raw archive and one active turn through approval, pause,
recovery and process restart. Successful turns commit once; failed terminal
turns stay active until explicit acknowledgement. Their uncertain tools must
first be reconciled. Archived turns remain inspectable and read-only in the
console. Subsequent turns reuse the saved conversation and the configured
context projection policy; source/tool messages are not reinterpreted as grants.

`max_active_runs` is a shared admission ceiling across standalone runs,
workflows and sessions. Typed job keys keep identical parent IDs independent.
Within each workflow, `max_parallel` limits child concurrency. The API additionally
caps requested graph size at 256 nodes, parallelism at 32, dispatches at 4096,
outputs at 1 MiB, elapsed duration at one day and shutdown grace at 30 seconds.
These are resource bounds, not tenant isolation or a global billing account.

For a reproducible zero-model-cost UI acceptance fixture:

```powershell
$env:TASKLOOM_API_TOKEN = 'replace-with-your-local-operator-token'
cargo run --locked --features app --example app_approval -- .fixture-state 127.0.0.1:8081
```

Connect to the console, select PEER, create a workflow, inspect `worker-0`, approve
its one local note and resume. Review 0 rejects, draft 1 is accepted, and the
executor write remains at one. Create a session and submit a turn to exercise
the same approval path with conversation history. The fixture's local writes
are real and confined to the supplied state directory; its planner/reviewer
are deterministic. This verifies controls and handoffs, not model quality.

2026-11-27 local browser acceptance completed PEER approval plus one revision
and a two-turn session without console errors. Integration tests separately
cover restart, role changes, global admission, cancellation and uncertain write
reconciliation. Hosted/model validation is tracked in the
[delivery roadmap](preview-delivery-roadmap.md).

## Approvals, cancellation and recovery

Review the pending ticket's complete tool, input, effect and permissions.
Approval records exact input, actor and note; click **Resume** separately.
Wrong/old tickets and decisions after cancellation fail. Approval never grants
future changed arguments. CLI equivalents:

```sh
taskloom approve RUN_ID TICKET_ID operator "reviewed exact input"
taskloom resume RUN_ID
taskloom reject RUN_ID TICKET_ID operator "not authorized"
taskloom cancel RUN_ID operator "stop unowned work"
taskloom reconcile RUN_ID CALL_ID decision.json
```

An example `decision.json` after verifying an uncertain external operation:

```json
{
  "type": "record_result",
  "output": {"saved": true},
  "is_error": false,
  "actor": "operator",
  "note": "confirmed the original operation in the external audit log"
}
```

Never record an invented result to unblock a task. Safe retry is restricted
to declared read-only/idempotent tools in nonterminal runs. Cancellation and
process exit cannot undo remote effects. Cancelling an abandoned tool preserves
uncertainty; a terminal cancelled run can still record its observed outcome,
but cannot dispatch more work. Pending approvals are rejected and audited on
direct persisted cancellation. Cancelling a crashed model retains uncertain
reserved spending without sending another request.

`serve` handles Ctrl+C by first stopping the listener: no new connections
are accepted, and keep-alive connections close after their current request.
Only then does it signal local workers, wait up to five seconds and close
configured MCP sessions. A request that was already being handled when Ctrl+C
arrived can still finish. Interrupted effects remain subject to
reconciliation. A direct CLI process killed during execution leaves its existing
checkpoints for explicit recovery. Worker panics release local scheduling state;
their last durable state is recovered explicitly rather than silently retried.

For an offline approval UI with a real local file effect:

```sh
cargo run --locked --features app --example app_approval -- target/approval-demo
```

It uses `TASKLOOM_API_TOKEN`, binds `127.0.0.1:8081` by default, and demonstrates
`Application::with_model` with a custom model, registry and policy. Create a run,
review the known note creation, approve, then resume. A new note file is created
once per run. The example's explicit state directory retains its evidence.

## MCP and host extensions

Add entries to `mcp` in configuration:

```json
[
  {
    "endpoint": "http://127.0.0.1:8082/mcp",
    "token_env": "MY_MCP_TOKEN",
    "namespace": "my-tools-tenant-v1",
    "bindings": [
      { "name": "lookup", "remote_name": "knowledge/search",
        "effect": "read_only", "permissions": ["mcp:knowledge"] }
    ]
  }
]
```

These explicit host bindings populate the registry/policy. Discovery alone
does not register other remote tools. Read [MCP's contracts and supported
protocols](mcp.md) before assigning effect metadata. Use `non_idempotent` for
unverified effects. A binding may set `"description"` to replace the remote
server's tool description with host-authored text; otherwise the remote text
is shown to the model framed as untrusted and capped at 2 KiB. The offline demo
only searches local knowledge; select a
real provider or a custom model host to use arbitrary MCP tools.

A configured `sandbox` registers `python_sandbox` for workflow roles that list
it in `tools`. Top-level runs and session turns receive it only with the
explicit opt-in `"sandbox_top_level": true` (default `false`; it requires a
`sandbox` section). See [sandbox configuration](sandbox.md).

Custom hosts can inject a `Model`, `ToolRegistry` and `ToolPolicy` using
`Application::with_model`; version their model/tool contracts and configuration
identity explicitly. RAG loaders, stores/retrievers, context compressors and
workflow tasks have separate documented Rust interfaces. API imports take text,
not arbitrary server filesystem paths or executable plugin code.

Keep state directories private. They contain prompts, raw tools/results,
approvals, sources and complete reports. The default `.taskloom/` directory and
root `taskloom.json` are ignored by this checkout; custom paths need the host's
own access control, backup and retention policy.

## Local verification

`tests/agent_app.rs` exercises real HTTP auth/headers, create/status/events,
archived source verification after corpus deletion, explicit approvals,
cancellation, graceful pause, capacity, worker panic recovery, persistence after
restart, and actual CLI subprocesses. Crash-process tests also cover cancelling
abandoned model/tool operations without further dispatch. Browser acceptance
checks create/view/report citation verification and approve/resume a real note
write. Offline checks require no external keys and do not establish compatibility
with every hosted provider or MCP service. Hosted CI remains a separate check.
