# Bounded agent runtime

Enable `runtime` to add autonomous tool calls above taskloom's typed task
executor. `AgentTask`, `PromptTask`, their recordings and their transport API
are unchanged. The runtime uses native tool messages, not JSON action prompts.

```text
cargo run --locked --features runtime --example agent_loop
cargo run --locked --features runtime,openai --example agent_openai
```

The first example is deterministic, offline, and makes three model decisions
with two correlated tool invocations. The second requires `OPENAI_API_KEY`
and `TASKLOOM_MODEL`, optionally `OPENAI_BASE_URL`; it can incur provider costs.
Choose a model and endpoint that support Chat Completions tool calls.

## Contracts

- `Message` represents system/user/assistant/tool history. `AssistantTurn`
  contains text and/or `ToolCall`s; `ToolResult` refers to the exact call id.
  Tool output stays in the tool role. Initial history must have complete,
  unique correlations. New call ids cannot reuse earlier ids in the run.
- `ToolRegistry` freezes definitions at registration and compiles JSON Schema
  validators. Schemas must have an object root; external reference fetching
  is disabled. It rejects duplicate/invalid names and invalid schemas.
- `ToolPolicy` denies everything by default. Allow each tool by name and
  grant all of its declared permission scopes. Unavailable tools are hidden
  from the model, and hallucinated/unauthorized calls are rejected at dispatch.
- Every batch is validated for available tools, permissions and arguments
  before the first tool executes. Tools run sequentially and are never
  automatically retried. Successful output has a byte limit; errors are
  bounded too. Read-only tool failures become error results for the model.
- A failed/timed-out tool with external effects stops the run, because the
  external operation may have succeeded. `ToolEffect` describes implementer
  guarantees; it does not enforce idempotency or create a sandbox.
- `ToolContext` supplies run/call ids for downstream deduplication. Tools
  execute in process with the application's privileges. Implement scope checks
  inside each tool (e.g. allowed filesystem root or allowed remote hosts).
  Work must be asynchronous; blocking code and synchronous event callbacks
  cannot be preempted by Tokio deadlines. Dropping a future cannot undo a write.

## Loop and observability

Construct `AgentLoop` with a `Model`, registry and model id. Configure policy
and `RunLimits`, then call `run(run_id, messages)`. The loop returns a
serializable `RunReport` even on failure, deadline, cancellation or a limit.
Only `Completed` exposes `final_text()`; callers must check `stop_reason`.

Step and tool-call limits are checked before dispatch. A step is a model
decision; gateway transient retries/fallbacks are attempts inside that step.
The run deadline covers the whole loop, and a separate timeout limits each
tool. Explicit or ambient `CancelToken`s interrupt model/tool futures.

`RunEvent`s have ordered sequence numbers, relative times and run-local
correlations. A synchronous sink can forward them to an application channel;
the report also retains events and the conversation. Gateway attempt spans,
usage, retries, concurrency admission and circuit breakers remain active.
Reported usage is retained when a reply is refused or has malformed tool
arguments. Reports contain user/tool content: store them with appropriate
application-level access controls, rather than logging them indiscriminately.

Native OpenAI-compatible and Anthropic adapters currently use non-streaming
tool turns. Each rejects truncated, refused, empty, unsupported or internally
inconsistent turns. Existing single-task text streaming is unchanged.

## Run-wide model spending

`max_cost_micros` is in millionths of the gateway price table's currency unit.
With a limit, every attempted model (including retries/fallbacks) must have a
finite nonnegative price. Before dispatch the gateway reserves the cost of
the **full configured model context as input plus the requested output**,
rounded up. This intentionally favors a conservative bound over utilization;
small jobs can require a larger reservation than their eventual actual cost.

Complete reported usage settles the reservation at the configured price.
Unknown/partial usage, transport failure or cancellation conservatively charges
the ceiling. `BudgetSnapshot` distinguishes committed, in-flight and uncertain
costs. No further attempt is sent if its reservation will not fit. Usage and
reservations differ: the ledger records only provider-reported tokens/costs,
whereas budget commitments include uncertain attempts.

These limits depend on correct model profiles, prices and provider billing
behavior. They do not cap external tool charges or guarantee a provider's
invoice. An implementation of the public `Model` trait must honor the same
reservation contract for all of its paid attempts. Use the async
`reserve_before_dispatch` method for persistent runs. The built-in
`GatewayTransport` implementation handles that contract automatically.

## Current development boundary

`DurableAgent` now provides versioned checkpoints, exact-input approval,
graceful pause/resume and explicit reconciliation of uncertain effects. See
[the recovery guide](durable-runs.md) and the offline `agent_resume` example.
An ordinary `AgentLoop::run` report alone remains insufficient for crash
recovery; use the persistent controller for that contract. Single-task answer
recording/replay does not record native agent turns or tool side effects.

Sessions/context compression, workflow orchestration, RAG and application
server/UI remain under development in [the three-phase roadmap](agent-roadmap.md).
