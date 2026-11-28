# Bounded final-output validation

An opt-in host contract prevents a malformed final answer from completing a
run. `AgentLoop::with_output_policy` accepts `FinalOutputPolicy::CitedReport`,
`FinalOutputPolicy::LocatedCitedReport` or `FinalOutputPolicy::JsonSchema`.
The typed `AgentTask` API is unchanged.
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

## Exact source locators (opt-in)

Models that cannot reliably copy long chunk hashes can use a distinct input
contract, `{"type":"located_cited_report","max_repairs":2}`. It accepts the
same retrieval-tool, minimum-source and required-document settings. The model
must return exact `LocatedReport` JSON, for example:

```json
{
  "text": "Changing the approved arguments requires a new decision [[1]].",
  "citations": [{
    "document_id": "approval",
    "corpus_revision": 1,
    "start": 0,
    "quote": "Changing the payload requires a new decision."
  }]
}
```

Set the role/system prompt to request this format as well. Selecting a policy
only configures validation; it does not rewrite the application's default
full-ID prompt or an existing retrieval tool description. The versioned skill
example supplies matching guidance explicitly.

The locator values must come from an actual successful retrieval in this run:
the result's `corpus_revision`, and a hit's `chunk.document_id` and `chunk.start`.
`start` is a UTF-8 **byte** offset. Neither revision 1 nor offset 0 is a default.
`[[1]]` refers to citation-array entry one; ordinals must be positive canonical
decimal integers, and every entry must appear in the text. Unknown fields,
stale revisions, wrong offsets, ambiguous sources, duplicate chunks, invented
quotes and full-hash markers fail validation. No source is chosen merely
because its quote happens to match. Repeated identical evidence is permitted;
different chunks at the same document/revision/start are an error.
Hosts using multiple independent corpora should namespace document IDs or select
a dedicated reader; the locator contract never guesses between colliding stores.

`SearchResult::verify_located_all` resolves only exact, unambiguous retrieved
locators. It derives a canonical `CitedReport` by replacing ordinal markers with
verified hashes, then applies the existing exact-quote/provenance checker.
`ResolvedLocatedReport` retains the original input, explicit ordinal-to-hash
mappings and verified report. Claim prose and quotes remain unchanged. This
does not evaluate entailment or truth, and an empty or irrelevant finding can
still fail a separate quality rubric despite valid provenance.

The durable child journal and `RunReport::final_text()` retain the **original**
model JSON. `WorkflowOutput::Validated` hands the parent the canonical report.
The application's report API and CLI add `rendering` with the original report,
mappings and `model_transcript_unchanged:true`; the ordinary `report`/`sources`
fields contain the verified rendering. SDK callers can explicitly obtain both
using `FinalOutputPolicy::resolve_located`. `verified_citations` and
`validated_value` provide the verified report and workflow value respectively.
They require executed evidence, but callers verifying arbitrary persisted state
must first validate its journal; the application does this automatically.

This format changes the model contract and must be selected for **new runs**.
Old `CitedReport` runs keep their strict full-ID contract and are never repaired
or migrated into locators. Resume rejects switching formats. Rust exhaustive
matches on `FinalOutputPolicy` need to handle the new variant. Tool definitions,
default prompts, default features and legacy serialized policy hashes are
unchanged. `examples/skills/research-located` is the explicitly versioned
`research-brief` **1.1.0** example; `research-brief` 1.0.0 remains intact.

The JSON variant uses `{"type":"json_schema","schema":{...},"max_repairs":1}`.
It checks exact JSON and the schema, not application semantics. Its schema is
limited to 128 KiB; external schema retrieval is unavailable in the runtime's
dependency configuration.

Citation contracts can additionally declare `answer_schema` for the original
model JSON. Both that schema and exact source/quote/marker/coverage checks must
pass before success. For example, a checklist template can require an explicit
reservation-timing section instead of relying on a model reviewer to notice an
omission:

```json
{
  "type": "cited_report",
  "max_repairs": 2,
  "required_documents": ["approval", "recovery", "budget"],
  "answer_schema": {
    "type": "object",
    "required": ["text", "citations"],
    "properties": {
      "text": {
        "type": "string",
        "allOf": [
          {"pattern": "[Aa]pproval:"},
          {"pattern": "[Rr]ecovery:"},
          {"pattern": "[Rr]eservation timing:"},
          {"pattern": "[Bb]illing:"}
        ]
      }
    }
  }
}
```

The schema must be at most 128 KiB and compile before model dispatch, using the
same offline schema validator as `JsonSchema`. This is host-selected structure,
not a model grant or entailment test: an empty/misleading section can still
match a pattern. Review its factual content and completeness separately. Extra
top-level output fields are not enabled by a permissive schema; the ordinary
strict `CitedReport`/`LocatedReport` parser still applies. Schema failure uses
the same bounded, metered, durable repair loop and narrowed read-only tools.

`located_cited_report` checks this schema on the original locator JSON before
rendering canonical chunk references. For SDK use,
`FinalOutputPolicy::cited_report(2).with_answer_schema(schema)?` validates the
configuration eagerly. API/CLI/UI configuration uses the same policy JSON and
existing validation path. The increment has SDK/API configuration regressions
and actual native workflow evidence; no new browser interaction is claimed.

## Repairs, recovery and permissions

An invalid draft emits `OutputRejected`, including policy identity, step,
attempt, detail and whether another repair was admitted. The durable runtime
saves the draft, feedback and settled budget together before another dispatch.
Every model attempt retains the ordinary durable spending reservation rules.
`OutputValidated` is required before a contracted run can finish successfully.
Exhausted validation repairs produce `ProtocolError`, without successful final
text. A budget or deadline may stop the run earlier.

Repair diagnostics identify missing ordinal markers, actual source-count and
missing required document IDs (at most six shown). Missing run-local evidence
explicitly distinguishes dependency citations from completed tool observations
and names the configured citation readers. These are instructions to repair
the answer using existing grants, never a fabricated search or a quotation/ID
rewrite. Raw model drafts stay in the journal. New prompt feedback can change
model behavior; later native campaigns are qualified separately from earlier
conditions. Resuming an existing unfinished repair may receive the newer error
explanation at its next validation; already saved feedback remains unchanged.

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

Absent/null `answer_schema` remains omitted during serialization and preserves
the prior citation-policy identity. Adding/changing a schema changes the frozen
contract; create a new run/workflow instead of resuming old work with it. Direct
Rust `CitedReport`/`LocatedCitedReport` enum literals need `answer_schema: None`
or a constructor. The separate typed `AgentTask` API is unchanged.

`cargo test --locked --all-features --test agent_output` exercises control,
budget, storage, permission, coverage, checkpoint compatibility, restart and
workflow handoff with explicitly labelled model fixtures. Native-model results
are recorded separately in `native-evaluation.md`; these fixtures are not
evidence of real model effectiveness.
`--test located_reports` additionally covers multilingual offsets, ambiguous
locators, malformed ordinals, source tampering and strict format separation.
