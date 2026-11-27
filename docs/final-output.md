# Bounded final-output validation

An opt-in host contract prevents a malformed final answer from completing a
run. `AgentLoop::with_output_policy` accepts `FinalOutputPolicy::CitedReport`
or `FinalOutputPolicy::JsonSchema`. The typed `AgentTask` API is unchanged.
The default remains no final-output contract.

For top-level application runs and session turns, add this to the host config:

```json
{
  "final_output": {
    "type": "cited_report",
    "retrieval_tools": ["knowledge_search"],
    "min_sources": 1,
    "required_documents": [],
    "max_repairs": 2
  }
}
```

This is a config fragment, not a complete provider configuration. Set
`limits.max_cost_micros` explicitly when `max_repairs` is positive, configure
provider prices, and grant the named read-only retrieval tools. Zero repairs
validates once and stops on rejection without an extra model request. At most
three repairs are accepted. Step, tool, deadline, context and storage limits
continue to apply; a repair admission does not guarantee a later dispatch.

Workflow roles choose their own contract. Their `output` field can be:

```json
{
  "type": "validated",
  "policy": {
    "type": "cited_report",
    "min_sources": 2,
    "max_repairs": 2
  }
}
```

`min_sources` counts distinct cited chunks, not independent publishers. Use
`required_documents` when coverage must include particular document IDs.
See `examples/benchmarks/collaboration-v2.json` for complete role declarations.
The application does not implicitly impose a top-level contract on roles.

## What gets checked

Citation output must be exact JSON in the existing `CitedReport` shape. The
validator checks actual retrieved chunk identity, body quotations, citation
markers and configured coverage. Eligible evidence must have successful tool
execution events and a successful result in this run. Older session history and
another role's findings are context; the child must retrieve its own evidence
to satisfy this contract. This conservative rule avoids trusting caller-supplied
historical tool messages as newly executed retrieval.

The validator never strips fences, repairs an ID, replaces a quote or rewrites
a generated claim. Feedback includes bounded error details and up to six actual
retrieved chunk IDs. All other returned evidence remains in the transcript.
Exact provenance **does not establish claim support, factual accuracy or overall
research quality**. A model reviewer cannot override the host citation contract.

The JSON variant uses `{"type":"json_schema","schema":{...},"max_repairs":1}`.
It checks exact JSON and the schema, not application semantics. Its schema is
limited to 128 KiB; external schema retrieval is unavailable in the runtime's
dependency configuration.

## Repairs, recovery and permissions

An invalid draft emits `OutputRejected`, including policy identity, step,
attempt, detail and whether another repair was admitted. The durable runtime
saves the draft, feedback and settled budget together before another dispatch.
Every model attempt retains the ordinary durable spending reservation rules.
`OutputValidated` is required before a contracted run can finish successfully.
Exhausted validation repairs produce `ProtocolError`, without successful final
text. A budget or deadline may stop the run earlier.

After the first rejection, citation repair can use only the intersection of
the original allowed tools and the contract's permitted read-only retrieval
tools. JSON repairs have no tools. A request for any other tool is denied
before dispatch. Effects performed earlier in the run are never repeated by
the repair controller. Prompt contents cannot enlarge this policy. Embedding
and paid tool-service spending still have their separate host-owned boundaries;
the generation ceiling is not an all-service billing ceiling.

A crash during generation follows the existing interrupted-model path: emit
`RecoveredModel`, keep outstanding spending reservations and admit a new model
attempt only within the remaining limits. This is not exactly-once generation.
Uncertain tool effects still require explicit reconciliation. Paused repairs resume
with the narrowed tool/context binding; finished reentry does no more work.
Cancellation and approval rejection remain available if a configured reader is
subsequently removed.

## Migration and acceptance

Old serialized `RunConfig` values omit `output_policy`; absence deserializes to
`None` and serializes identically, preserving previous config fingerprints.
Add or change a contract only for new runs. Strict resume rejects a changed
policy, including a changed repair count, schema or coverage requirement.
Existing `WorkflowOutput::Text` and `WorkflowOutput::Json` semantics remain;
`Validated` returns a typed JSON object after durable child validation, rather
than a JSON string. Rust exhaustive matches on `WorkflowOutput` and
`RunEventKind` need to handle the added variants. Direct struct literals for
`RunConfig`/`AppConfig` must supply the new optional field or use an applicable
default update. No feature is added to the
minimal default build, and the MSRV remains Rust 1.86.

`cargo test --locked --all-features --test agent_output` exercises control,
budget, storage, permission, coverage, checkpoint compatibility, restart and
workflow handoff with explicitly labelled model fixtures. Native-model results
are recorded separately in `native-evaluation.md`; these fixtures are not
evidence of real model effectiveness.
