# Explicit source selection and host-rendered quotations

`selected_sources` is an opt-in output contract for tasks where the model should
select evidence instead of reproducing its literal quotation. It is distinct
from `cited_report` and `located_cited_report`. Those contracts still reject an
incorrect model-authored quote; no whitespace normalization or fuzzy repair is
introduced.

The model returns exactly:

```json
{
  "text": "A claim [[EXACT_RETRIEVED_CHUNK_ID]].",
  "sources": ["EXACT_RETRIEVED_CHUNK_ID"]
}
```

Replace the placeholder with the actual complete lowercase SHA-256 chunk ID
from the role's executed retrieval. For each selected source, the host generates
a canonical `CitedReport` citation containing that chunk's **full unchanged body**
as its quote. The text and source choice remain the model's; those quotations
are explicitly host-authored. A summary sentence must not be presented as a
model-authored quotation by changing its spelling until it happens to match.

The contract checks exact executed evidence, chunk hashes, body integrity,
duplicate/unknown IDs, every text marker, minimum quote size, source count and
required document coverage. It refuses malformed IDs instead of resolving a
short or approximate identifier. Sources cannot come from model text or an
earlier dependency's citations alone. Conflicting archived chunks are errors.
At most 20 selected sources and 1 MiB of total selected bodies are admitted;
ordinary workflow/output limits may be smaller. A whole chunk is a broad source
reference, not a claim-specific entailment or relevance guarantee.

`answer_schema` can additionally constrain the original `text`/`sources` JSON.
The selected contract uses the same durable, metered, bounded repair loop and
intersection of originally granted read-only citation readers. It never adds
tools, credentials, approvals or grants. Finished reentry does no work and a
changed contract refuses resume. Source text remains untrusted User/Tool data.

The default style requires inline markers. An explicit host policy field
`"bibliography": true` instead appends a labelled answer-level source list to
the canonical display. The original model `text` stays in `rendering.original`;
the API additionally marks `source_list_generated_by_host: true`. Inline source
markers are then optional, but any markers the model supplies must still refer
to selected sources. Unknown, ordinal or unclosed markers remain errors, and
empty original answers are refused. This style is a bibliography, not a
per-claim inline-citation or entailment guarantee. It changes the frozen policy
identity and is never enabled to silently repair an old inline-contract run.

## Configure and inspect

```json
{
  "type": "selected_sources",
  "max_repairs": 2,
  "min_sources": 2,
  "required_documents": ["approval", "recovery"],
  "answer_schema": {
    "type": "object",
    "required": ["text", "sources"]
  }
}
```

Use this policy as `final_output` in an explicitly configured Application, or
inside a registered workflow role's `{"type":"validated","policy":...}`
output. Set matching system instructions telling the model to return `text`
and `sources`, select actual retrieved IDs and include the markers. It should
not produce `quote` or `citations` fields. Configuration validation is shared by
SDK, CLI, API and the existing UI JSON editor. It does not implicitly apply the
policy, grant knowledge access or contact a model. No new browser interaction
is claimed for this increment.

SDK hosts can use `FinalOutputPolicy::selected_sources(2)` and
`.with_answer_schema(schema)?`. `SourceSelectionReport` and
`SearchResult::verify_selection_all` expose the data-only selection contract.
For explicit bibliography style use `.with_host_bibliography()?` or
`SearchResult::render_selection_bibliography_all`. The additional schema checks
the original model JSON before display, so host-added labels cannot make a
missing answer heading satisfy a schema.
The ordinary `CitedReport`/`LocatedReport` types and their checks are unchanged.

The API report includes a `rendering` record:

```json
{
  "type": "host_selected_source_bodies",
  "original": {"text": "...", "sources": ["..."]},
  "model_transcript_unchanged": true,
  "quotations_generated_by_host": true
}
```

Workflow handoffs use verified canonical `text`/`citations` so consumers retain
the existing report format. The durable child transcript still contains the
original model `text`/`sources`. A writer must use its own executed evidence
under its output policy; inheriting a previous role's canonical rendering does
not invent a completed tool call.

## Migration and evidence boundaries

Existing policy JSON and identities remain unchanged. Select this new policy
for new runs or explicitly versioned templates; do not change an old suspended
run's contract. Rust exhaustive matches on `FinalOutputPolicy` need to handle
the new variant. The typed `AgentTask` API, minimal default features and Rust 1.86
baseline are retained.

This contract was prompted by an actual native repository-review failure: the
model joined Markdown line breaks inside a claimed quotation. The old strict
model-quote run stays failed. Source selection avoids asking the model to
reproduce that body, while explicitly declaring that the host generated the
quote. A separate native run is required; a new type or fixture alone is not
real-model acceptance.

Unit/runtime tests cover wrapped multilingual bodies, unknown/duplicate/corrupt
sources, undeclared markers, output bounds, actual run-local tool observations,
repair permission narrowing, old-contract refusal and explicit API rendering.
The tests deliberately demonstrate that **false prose can select real sources**.
Semantic support and factual accuracy must still be evaluated separately; neither
host-rendered quotes nor a model review is an automatic truth certificate.
