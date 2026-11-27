# Configuration review and workflow design

This increment adds code/API admission primitives for P2. It does not yet
accept the visual editor, configuration wizard, multi-user service or queue.
The existing run, workflow, session and typed `AgentTask` entry points remain.

## Review a proposed host configuration

`taskloom config-check proposed.json` loads the file without constructing an
application, resolving environment values, creating a store, reading skill
packages or contacting a provider/MCP/index. Even a deliberately absent
`--config` main file is unnecessary for this command. Relative paths follow the
same config-file-relative rules as normal startup.

Authenticated `GET /api/setup` lists defaults and the current model, tools,
roles and installed templates. `POST /api/setup/validate` takes an `AppConfig`
and returns normalized `configuration`, its `configuration_key`, required
credential **names**, declared tools/effects/permissions and `deferred_checks`.
It never applies the configuration. Write the reviewed configuration to a new
file and explicitly restart when ready. Existing frozen runs still require
their original host contracts to resume.

New configuration review requires matching generation prices and an explicit
generation cap for remote/local native providers. Endpoint URLs must use HTTPS
or loopback HTTP, with no inline credentials, query or fragment. Credential
fields take environment names; do not paste a key value. Static review checks
limits, role schemas, duplicate tool declarations, requested role tools and
read-only citation-tool contracts. It cannot establish credential availability,
remote schemas, installed-package contents, index freshness, executable
availability or service readiness. Those checks remain explicit at startup or
execution. Embedding/tool-service costs are separate from generation prices.
These stricter review checks do not change legacy `AppConfig::validate()`
startup behavior globally.

## Preview, then launch the exact workflow

`POST /api/workflow-design/preview` takes the same `AppWorkflowRequest` as
`POST /api/workflows`: `id`, `input`, exactly one of `definition`/`peer`, and
optional `limits`. It uses the actual role registration, PEER validation and
workflow compiler, returning the initial `plan`, canonical `request` and
`preview_key`. No lease, workflow/checkpoint, event, generation, tool call or
budget reservation is created. An available ID or working storage/service is
not promised. The SDK equivalents are `Application::preview_workflow` and
`WorkflowRunner::preview`.

`POST /api/workflow-design/launch` takes:

```json
{"request":{"id":"example","input":"task","definition":{"nodes":[{"id":"work","task":"executor","depends_on":[],"condition":{"type":"always"},"allow_skipped_dependencies":false,"select":{},"params":null}]},"peer":null},"preview_key":"copy from preview"}
```

The SDK `Application::launch_workflow` recomputes the plan and rejects a stale
key with conflict before dispatch. The key binds exact input, ID, limits,
compiled graph and frozen host role contracts. It is an integrity/usability
binding, not authentication or evidence that a human approved a tool call.
Normal exact tool approvals and spending journals remain authoritative.
Concurrent launches of an occupied ID still conflict through the normal store.
The original workflow-start API remains available. Dedicated design paths avoid
shadowing workflows whose valid IDs happen to be `preview` or `launch`.

Keep arbitrary JSON lossless in clients: parsing and reserializing through a
JavaScript `Number` can change `9007199254740993` or convert `1.0` to integer
`1`. Rust/API regression tests verify those values reach the actual model input
unchanged. Do not silently replace unknown roles, conditions, selectors or
parameters while editing.

## Migration and acceptance

There is no checkpoint migration or new default feature. The new review API is
additive; existing callers may retain their start paths. Proposed configs are
not automatically written or loaded. Preview creates no saved run to migrate.

```powershell
cargo test --locked --all-features --test app_planning
cargo build --locked --features app --bin taskloom
# Use an explicitly configured zero-priced local model with the named key env.
python examples/planning/accept_native.py target/native-planning --config target/native-generation-4b-config.json --taskloom target/debug/taskloom.exe
```

`acceptance/p2-planning-20261128.json` retains the native Qwen3-4B report: all
nine controls passed, including a static proposal with an unresolved MCP
credential, unchanged live setup, no dispatch/storage on preview, conflict on
edited input, actual generation, exact frozen input/definition/limits, large
integer/float preservation, host trace binding and cold CLI review without a
model credential. This is one short native integration check, not effectiveness,
UI, database, authorization or load acceptance. P2 roadmap boxes remain open.
