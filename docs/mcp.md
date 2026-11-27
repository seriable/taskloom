# MCP tools over Streamable HTTP

The unreleased optional `mcp` feature includes `runtime` and the HTTP client.
It implements tool discovery/calls over explicitly configured MCP Streamable
HTTP endpoints, negotiating **2025-06-18** or **2025-03-26** protocol versions.
It supports JSON responses and response SSE, including notifications before
the matching JSON-RPC response. Unsupported negotiated versions fail closed.
See the pinned [transport specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports).

```rust,no_run
use std::collections::BTreeSet;
use taskloom::runtime::mcp::*;
use taskloom::runtime::{ToolRegistry, ToolEffect, ToolPolicy};

# async fn setup() -> Result<(), taskloom::runtime::RuntimeError> {
let mut config = McpHttpConfig::new("http://127.0.0.1:8081/mcp");
config.namespace = "my-local-service-v1".into();
// For authenticated endpoints, load secrets from the host environment.
config.bearer_token = std::env::var("MY_MCP_TOKEN").ok();
let client = McpClient::connect(config).await?;
let mut tools = ToolRegistry::new();
tools.register(client.bind(McpToolBinding {
    name: "lookup".into(),             // runtime/model-facing alias
    remote_name: "knowledge/search".into(),
    effect: ToolEffect::ReadOnly,      // host verifies this contract
    permissions: BTreeSet::from(["mcp:knowledge".into()]),
})?)?;
let policy = ToolPolicy::default().allow("lookup").grant("mcp:knowledge");
// Construct AgentLoop with this registry/policy; DurableAgent supplies
// exact-input approval and effect reconciliation as for any other tool.
# Ok(())
# }
```

Connecting initializes the session and discovers the catalog. It does not
execute or automatically register advertised tools. Bind only intended names;
the registry validates schemas and freezes definitions, while `ToolPolicy`
independently grants capabilities. MCP `readOnlyHint`/`idempotentHint` and other
server annotations never determine permissions or side-effect metadata.

Use `NonIdempotent` unless you have verified a stronger remote contract.
Default durable policy requires approval for effects. The client does not
send runtime context as a deduplication key: JSON-RPC IDs correlate requests,
they do not make remote writes idempotent. Declare `Idempotent` only when the
remote tool/arguments provide actual deduplication. A timeout, dropped HTTP
request, process interruption or lost response does not prove the server
cancelled its work. Reconcile uncertain effects through `DurableAgent`; there
are no automatic tools/call retries, redirects, session reinitializations or
replays after session expiry.

The endpoint/protocol/remote name and a host-authored nonsecret `namespace`
are fingerprinted in the frozen tool description. Changed identities reject
durable reentry. Set a distinct namespace for different tenants or configuration
contracts when using the same endpoint. Credentials are not a namespace, are
not placed in tool definitions, and can be rotated within the same tenant.
Tools discovered from changed schemas/descriptions also produce changed frozen
definitions. Existing in-flight work still belongs to its original service.

Defaults bound each HTTP exchange to 30 seconds, each response to 2 MiB and
the catalog to 256 tools/16 pages. Limits are configurable within hard upper
bounds. Request IDs and session headers are validated, pagination cycles fail,
and JSON-RPC/tool errors cannot become successful results. `content`,
`structuredContent` and resource links are preserved as data; the client does
not fetch links or execute returned instructions. Remote HTTP errors omit
request URLs; application logs and remote tool content can still contain
private data. External MCP service charges are outside model-spend limits.

`McpClient::close` explicitly deletes stateful sessions (HTTP 405 is tolerated
when teardown is unsupported) and prevents further calls from all its clones.
The client does not implicitly perform async network work in `Drop`.

This integration does not implement stdio process launch, legacy HTTP+SSE,
OAuth discovery/interactive login, elicitation, server-initiated sampling,
resource fetching, resumable SSE or distributed session recovery. Supply bearer
credentials from your host. A supported test server is not proof of compatibility
with every MCP deployment; test the endpoint and protocol you intend to use.

`tests/agent_mcp.rs` uses local HTTP fixtures for initialization, pagination,
headers/auth, split UTF-8 SSE, tool results, unsupported negotiation, redirect
refusal, bounded responses, errors without retries, tenant identity and native
durable approval/reentry. No external MCP service credentials are required.
