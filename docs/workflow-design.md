# Configuration review and workflow design

The operator console includes configuration review and visual workflow
authoring over the same SDK/API admission primitives. It remains a
single-operator local host. Multi-user service, transactional database and
cross-process queue acceptance are separate P2 requirements. Existing run,
workflow, session and typed `AgentTask` entry points remain.

## Browser authoring

Connect with the host token, then choose **Load host capabilities** in
**Configure & design**. The configuration wizard starts from offline defaults
or a complete imported configuration. Its model/budget fields, MCP declarations,
role contracts and retrieval settings update the complete JSON explicitly.
Review shows the credential names, declared effects/permissions and deferred
checks before enabling export. Editing any field disables export until review
passes again. Export saves a JSON file; it does not write server files or
activate grants in a running host. Configure secrets in host environment values,
then explicitly start/restart with the exported file. A partial configuration
can first be normalized with `config-check`; browser form imports expect the
complete normalized configuration.

Choose a blank DAG, PEER, or an installed active template. Template selection
shows the pinned package identity and input schema; its preview endpoint
validates that schema before compiling mounted roles. Modify task input and
limits, then add/remove node cards and choose registered roles, dependencies,
conditions and selected handoffs. An arrow diagram updates with the graph.
Parameters and equality values accept arbitrary JSON. Composite All/Not
conditions remain visible as editable advanced JSON. Unknown roles remain in
the editor with a warning and fail preview; they are never silently replaced.
Renaming/removing a node leaves references for explicit correction. Editing a
template's graph contracts makes a standalone role workflow, while task input,
ID and limits can retain template validation.

The code representation and file import/export preserve numeric tokens,
including large integers and decimal types. Duplicate object keys, excessive
nesting and ambiguous JSON are refused. Visible field edits are committed before
preview even if an input method has not emitted blur/change. Invalid edits block
launch. Any edit clears the prior preview; the server also recomputes its key.
After launch, the existing parent/child console exposes progress, handoffs,
review feedback, exact tool decisions, pause, cancellation and recovery. A
paused workflow is selected from saved parents after restart and resumed there;
finished children are retained. Reconciliation still requires observing the
external outcome before recording it; pause/cancel do not undo external effects.

The codec is also used when submitting the original textual DAG form and
operator-observed reconciliation JSON. API/code entry points stay available;
the visual editor does not replace exact tool approval or billing contracts.

## Review a proposed host configuration

`taskloom config-check proposed.json` loads the file without constructing an
application, resolving environment values, creating a store, reading skill
packages or contacting a provider/MCP/index. Even a deliberately absent
`--config` main file is unnecessary for this command. Relative paths follow the
same config-file-relative rules as normal startup.

Authenticated `GET /api/setup` lists defaults and the current model, tools,
roles, workflow defaults and installed templates. `POST /api/setup/validate` takes an `AppConfig`
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

`POST /api/templates/{mount}/{template}/preview` accepts `AppTemplateRequest`
(`id`, `input`, optional `limits`), validates the active package's input schema
and returns the same workflow preview. `Application::preview_skill_template`
is the SDK equivalent. Caller-provided grants or roles are refused. Normal
template start now shares that same resolver; package identity, granted tools
and exact approval behavior are preserved.

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

The subsequent UI increment is recorded separately in
`acceptance/p2-design-ui-20261128.json`. Real browser operations covered a
native two-node DAG, an installed native research template, and a separate
native two-node composite-condition workflow paused after its first node.
After host restart, browser resume completed the second node with the first
child unchanged and one generation per child. Model fee declarations were zero,
with cap zero and the credential name `TASKLOOM_LOCAL_API_KEY`; hardware,
electricity and this chat are excluded. These short visible tasks test product
integration, not independent task quality or competitor performance.

An explicitly separate offline fixture exercised actual local file approval,
one reviewer rejection/rewrite and cancellation. Exactly one note file was
created across the accepted PEER and cancelled second workflow. Fixture role
decisions/usage are not native inference evidence. Static configuration review
with an unresolved MCP key passed without applying the proposal; export created
the actual file, a later edit disabled it, and an inline-key-shaped value failed.
The browser retained an unknown imported role until explicit correction and
preserved advanced conditions, selectors, `9007199254740993` and `1.0`.

At 390 pixels, the final page measured client/scroll width 375/375 with the
wizard open and no wizard field beyond the viewport; the graph has its own
bounded overflow region. Early browser probes found an uncommitted-input issue,
malformed provider option tags and a long parent-budget string causing page
overflow. All were repaired and the affected flows rerun. An intermediate
viewport measurement targeted the other active tab and is not narrow-screen
evidence. No final page errors were observed. New storage backends, multi-user
authorization, a queue, load acceptance and broad task quality remain open.

For a reproducible browser check, build `--features app --bin taskloom`, start
an explicitly configured local host, follow the authoring flow above with a
registered role and two nodes, preview, change a field to invalidate, restore and
launch. Pause during the first child, stop/restart the host and resume through
the saved parent list. Compare child IDs, dispatch and generation counts.
Install `examples/skills/research-located` and its pinned `evidence-base`
dependency as described in `skill-packages.md` to repeat template selection.
`examples/app_approval.rs` supplies the isolated offline PEER approval fixture:

```powershell
node tests/planning-json.cjs
cargo test --locked --all-features --test app_planning --test app_skills
cargo run --locked --features app --example app_approval -- target/design-demo 127.0.0.1:8081
```

Set `TASKLOOM_API_TOKEN` to an explicit local operator token first. Only the
fixture's own state directory receives note writes. Stop the owned hosts before
rebuilding their executable on Windows. No release asset/tag is replaced.
