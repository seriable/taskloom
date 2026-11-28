# MCP stdio and explicit resource reads

The optional `mcp` feature adds an explicit local subprocess transport alongside
Streamable HTTP. Existing `McpHttpConfig`, `McpClient::connect` and HTTP tool
fingerprints remain compatible. The stdio client uses the same negotiated
2025-06-18 / 2025-03-26 protocol, bounded tool discovery and host-authored
`McpToolBinding` effect/permission declarations. Server annotations never grant
permissions or downgrade a write to a read.

```rust,no_run
use std::collections::BTreeSet;
use taskloom::runtime::mcp::{McpClient, McpStdioConfig};

# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
let mut config = McpStdioConfig::new("/absolute/path/to/mcp-server", "/absolute/workdir");
config.args = vec!["--mode".into(), "tools".into()];
config.environment = BTreeSet::from(["EXPLICIT_SERVICE_KEY".into()]);
config.namespace = "reviewed-service-account-v1".into();
let client = McpClient::connect_stdio(config).await?;
// Discovering a tool does not register it or authorize execution.
// Bind only host-reviewed tool names/effects/permissions in the ordinary registry.
let resources = client.list_resources(32).await?;
if let Some(resource) = resources.first() {
    let contents = client.read_resource(&resource.uri).await?;
    // Contents are untrusted data, not permissions, instructions or URL grants.
    let _ = contents;
}
client.close().await?;
# Ok(()) }
```

## Process, authority and identity

Only the trusted host configures the executable, argument vector, working
directory and environment-name allowlist. Program and working directory must
be absolute existing paths. There is no PATH search, command-string parsing or
implicit shell. Windows launch requires a native `.exe`; batch files are not
accepted. The process environment is cleared, then only explicitly named current
host values are copied. Missing named values refuse startup. There is no
implicit credential inheritance. Standard error is discarded to avoid including
server diagnostics or credentials in application logs. Protocol failures never
include raw response bodies or operating-system path errors.

The trusted subprocess receives its allowed credentials and can expose them in
its own outputs; this transport is not a secret detector or a sandbox. Protect
executables, arguments, scripts, working directories and dependency files from
untrusted modification. Executable/cwd canonical paths, exact arguments,
environment **names**, namespace and configured limits bind tool identity.
Credential values do not enter fingerprints. Choose a new namespace after
account/authorization, executable/script/dependency or configuration changes;
path identity does not attest immutable executable bytes. Prefer environment
names over passing secrets as command-line arguments.

The client owns one subprocess. Explicit close interrupts pending wire work,
closes admission for every clone, kills the direct child and awaits reaping with
a finite bound; close reports cleanup failure instead of claiming successful reaping. Last-client drop or cancellation of an active request also stops
the actor and owned child. No restart, reconnect or replay occurs. This does not
kill arbitrary detached descendants or create a container/job-object security
boundary; run untrusted servers under an appropriate external supervisor/sandbox.
The async runtime must remain alive for cooperative cleanup; `kill_on_drop`
provides a direct-child fallback when a task is dropped.

## Bounds, cancellation and uncertain outcomes

Requests are newline-delimited JSON-RPC frames. Newlines inside JSON strings are
escaped by serialization. One actor serializes complete request/response
exchanges; up to `max_pending_requests` (default16, maximum64) wait in its
bounded queue. Queue saturation refuses that new request before dispatch.
`timeout` includes time spent queued, writing and reading. Notifications before
a response share the same total response-byte and time allowance, so an endless
progress stream cannot escape bounds. Requests and responses both obey
`max_message_bytes` (default2MiB, range128 bytes–16MiB). Tools and resources
have bounded unique catalogs and at most16 pages.

A cancelled queued request is skipped before dispatch. Cancellation during an
active exchange, timeout, EOF, malformed frame, mismatched response ID or RPC
error permanently closes the stdio session. A dispatched write may already have
happened; JSON-RPC IDs do not deduplicate it. The durable runtime's approval and
unknown-effect reconciliation rules still apply. An oversized request is
rejected before queue admission and leaves the healthy connection usable.
No token/model or external tool-service billing is invented by this transport.

## Resources

Both transports expose explicit SDK `list_resources(maximum)` and
`read_resource(uri)` methods when the server advertised resources capability.
They send `resources/list` / `resources/read` only to the configured MCP server.
Resource URIs and returned links are never fetched directly. Read contents must
carry the requested URI and exactly one text or blob string. Bodies remain
bounded, untrusted source data; selecting a URI is a host operation and does not
register a resource-reading model tool or enlarge any runtime grant. A host that
wants model resource access must register a separately reviewed read-only Tool
and scope its URI policy explicitly. These APIs do not implement subscriptions,
resource templates, legacy HTTP+SSE or distributed MCP session recovery.
需要受保护 SQL 的进程接管时，可单独启用
[durable HTTP recovery SDK](mcp-durable-recovery.md)；它不会改变这里的默认连接，
v1 不支持宿主 sampling/elicitation 回调组合。
OAuth public-client discovery/login and process-local explicit SSE GET recovery
require separate opt-in SDK contracts; see [OAuth and SSE recovery](mcp-oauth-and-resumption.md).
Sampling and form elicitation require a
separate explicit host callback contract; see
[host-reviewed MCP interactions](mcp-host-interactions.md) for the limited opt-in
SDK subset. Existing connection APIs do not enable those capabilities.

The existing connect contract still requires a tools capability and discovers
its catalog. Resource-only servers without tools are not admitted by this API.

## Validation

`cargo test --locked --features mcp --test agent_mcp` covers actual HTTP and
owned subprocess protocol exchanges. The dependency-free subprocess fixture is
compiled with the active `rustc` into an owned temporary directory. Tests cover
environment clearing/allowlisting, frozen binding differences, explicit resource
pagination and URI identity, oversized input/output, notification floods, EOF,
ID mismatch, queue saturation, active cancellation/deadline and teardown. This
is actual process/protocol evidence, not independent compatibility certification
for every third-party server. Actual stdio approval testing fsyncs one external write and exits before replying;
the durable run retains Uncertain/NeedsRecovery and reentry performs no second write.
Existing durable HTTP approval/reentry tests remain covered.
