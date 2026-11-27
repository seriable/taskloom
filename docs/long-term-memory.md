# Reviewed cross-session memory

Enable `runtime` for the SDK and `app` for the local API/CLI. Memory is opt-in
and separate from conversation history, the knowledge corpus and model spending.
The minimal default feature set and typed `AgentTask` API are unchanged.

## Host configuration and access

Add this fragment to a complete application configuration:

```json
{
  "memory": {
    "scope": {"tenant":"local", "project":"assistant", "owner":"alice"},
    "actor": "local-operator",
    "curate": true,
    "recall": {"max_items":8, "max_bytes":16384, "include_preferences":true}
  }
}
```

The host chooses this scope and authenticated actor. API/model input cannot
select a tenant, project, owner, actor or curator grant. `curate` defaults to
false; readers can inspect/recall their namespace but cannot extract, review,
edit or forget. Memory is disabled when the optional property is absent.
Extraction additionally requires `limits.max_cost_micros` and explicit prices
for the configured model. No extraction happens automatically after a chat.

For the SDK, construct `MemoryAccess::reader` or `MemoryAccess::curator` only
after authenticating and authorizing the caller. The type deliberately cannot
be deserialized. `MemoryManager` takes this access object, with no second
caller-selected scope. `MemoryStore` is a trusted persistence interface;
calling its raw methods is not an authorization API. Protect its filesystem.

The current local application still has one bearer-authenticated operator.
Its memory namespace isolation does **not** provide multi-user authorization
for sessions, runs or the rest of the application. P2's authenticated service
scope and database work remain separate delivery gates.

## Extraction and review

Choose a successful, closed session turn. The host loads that session and
selects its actual User message. Assistant/tool replies, open/failed turns and
previously recalled memory are ineligible sources. For a memory-aware turn,
only its original `request` field is eligible; injected records never become
fresh user testimony.

The configured native model proposes up to sixteen `{kind,key,value,quote}`
records. `kind` is `fact` or `preference`; `key` is a bounded lowercase semantic
slot such as `language` or `project.runtime`. The host supplies source session,
turn/run/message identity and fingerprints, and checks the quote verbatim.
Model output cannot supply ownership or permission fields. The extraction
child has no tools, uses exact JSON/schema validation, reserves its model
spending durably and retains failed or uncertain attempts under normal rules.

All proposals begin as **candidates**. Only explicit host/operator review
activates them. An exact quote establishes provenance, not that the proposed
value follows from it. Review values and their sources before accepting; a
wrong statement or sensitive value can still be proposed by a model. This is
not a general secret detector or factual-truth verifier.

Slots use exact `(kind,key)` identity. A second candidate for an occupied slot
cannot become active without `replace:true`; replacement retains the previous
version as superseded. There is no automatic semantic deduplication between
`language`, `reply.language` and `preferred_language`. Inspect such proposals
and reject inappropriate aliases; do not assume model-selected keys are a
canonical ontology. Editing an active value creates a new active version
linked to the previous version and the operator's note. Its original source
quote remains provenance of the earlier proposal, not proof of the edit.

## API and CLI

All HTTP routes below are under `/api` with the existing bearer authentication.
JSON requests deny unknown identity/grant fields. Every mutation requires the
exact current `expected_revision`; conflicts require inspection, not a force
overwrite or silent model retry.

| Operation | API | CLI |
|---|---|---|
| Export records, versions, sources and audit hashes | `GET /memory` | `memory` |
| Bounded recall | `POST /memory/recall` with `{"text":"project runtime"}` | `memory-recall "project runtime"` |
| Explicit model extraction | `POST /memory/extract` | `memory-extract extract.json` |
| Accept/reject a candidate | `POST /memory/review` | `memory-review review.json` |
| Edit an active value | `POST /memory/edit` | `memory-edit edit.json` |
| Forget a slot and all its versions | `POST /memory/forget` | `memory-forget forget.json` |
| Start a turn with frozen recall | `POST /sessions/{id}/memory-turns` | SDK/API |

Example request files, used with `taskloom --config local-provider.json COMMAND`:

```json
{"expected_revision":0,"session_id":"source","turn_id":"one","request_id":"learn-source"}
```

```json
{"expected_revision":1,"id":"actual-candidate-hash","accept":true,"replace":false,"note":"checked against the user's source statement"}
```

```json
{"expected_revision":2,"id":"actual-active-hash","value":"Chinese","note":"owner clarified the preference"}
```

```json
{"expected_revision":3,"kind":"preference","key":"language"}
```

Create the destination session through `POST /sessions` as usual. Its
`memory-turns` body is the ordinary `{turn_id,prompt}` shape. Ordinary `turns`
continues to omit memory. The memory-aware route freezes query, namespace,
revision and source-linked hits in a clearly labelled **User-data** JSON
envelope with a separate original `request`. It changes neither the system
prefix nor tools/grants. Prompt-like memory content cannot grant a tool. The
envelope is visible in the archived transcript and durable input; clients can
render its `request` separately. Reentering the same turn reuses this envelope
even if memory changed. A different original request or scope is rejected.

Recall is local lexical overlap using the existing Unicode tokenizer. Explicitly
reviewed preferences can also be included without overlap; turn this off for
strict query-only recall. Candidate/rejected/superseded records are excluded.
Item/serialized-byte limits are enforced; oversized higher-ranked records are
reported and lower-ranked fitting records may still be returned. Inspect
`matched_terms`, `preference`, `omitted` and `skipped_oversize`. This is not an
embedding index or a promise of semantic recall across languages/paraphrases.

CLI export, recall, review, edit and forget operate without loading generation
or MCP credentials. CLI extraction loads only the configured generation model;
it does not contact configured MCP services or offer tools. SDK usage:

```rust,no_run
use taskloom::runtime::memory::*;
# async fn example() -> Result<(),taskloom::runtime::RuntimeError> {
let store = FileMemoryStore::new("state/memory");
let access = MemoryAccess::reader(MemoryScope {
    tenant:"local".into(), project:"assistant".into(), owner:"alice".into()
}, "authenticated-actor")?;
let manager = MemoryManager::new(&store);
let recalled = manager.recall(&access, "project runtime", &MemoryRecallPolicy::default()).await?;
// Pass a curator access object for review/edit/forget; never derive it from
// the model's answer. Extraction uses an explicitly bounded AgentLoop/RunStore.
println!("{} source-linked memories", recalled.hits.len());
# Ok(()) }
```

## Recovery, erasure and migration

Version-1 file snapshots live under `state_dir/memory/<scope-hash>.memory.json`.
Writes take per-scope OS locks, verify CAS/audit prefixes, flush a temporary file
and atomically replace the previous snapshot. Reads fail on corruption; they
do not silently reset state. Limits are 16 MiB, 4096 records, 16,384 forgotten
source bindings and 20,000 audit events. Admission also reserves 2 KiB, one
possible tombstone and one possible erasure event per retained record, so
administrative forgetting is not stranded by an admitted full snapshot.
A limit refuses the operation without
replacing the valid snapshot. Export/backup before planned migration; retain
the scope and revisions when implementing another `MemoryStore`.

Extraction request IDs bind a scope and the exact selected input to a durable
`memory-<hash>` child. Repeating a completed request with a fresh inspected
memory revision reuses its answer without another model request. If memory CAS
or capacity fails after generation, the completed extraction run stays in the
run store; inspect it and retry the **same request ID**, with the correct
revision/configuration. Do not silently mint a new paid request. Interrupting
generation preserves its spending uncertainty; normal interrupted-model
recovery can admit a further attempt within the original cap. API admission
is released even if the extraction caller drops its future.

Forgetting physically removes every candidate/current/superseded/rejected
version of that slot from the memory snapshot. It retains only audit record
hashes and source-binding tombstones, preventing an old extraction of that same
source/slot from resurrecting the value, even if a later extractor proposes
different wording. A genuinely new user statement can propose the slot again.
This is memory-store erasure, not secure disk wiping or deletion of source
sessions, completed extraction runs, already frozen turn inputs, exports or
external backups. Those histories remain independently retained. New recalls
exclude forgotten records; old frozen inputs remain reproducible.

No automatic legacy-history import or schema conversion runs on upgrade. Absent
`memory` preserves previous application behavior, session format and durable
configuration fingerprints. Direct `AppConfig` struct literals need the new
optional field or `..Default::default()`.

## Reproducible acceptance

```powershell
cargo test --locked --all-features --test agent_memory --test app_memory
python examples/benchmarks/native_memory.py target/native-memory-new --config local-provider.json --taskloom target/debug/taskloom.exe
```

The tests explicitly label model fixtures and cover CAS/isolation, source
checks, bounded Unicode/negative recall, permission injection, CRUD/restart,
frozen inputs, no resurrection, model-spend uncertainty and credential-free
CLI management. The native example calls only an explicitly zero-priced
loopback API. It retains model proposals unchanged, curates only prelabelled
source-supported values and tests actual new-session use, conflicts, CLI edits,
host restarts and erasure. `acceptance/p1-memory-20261128.json` records results
and limitations; this visible small set is not a broad extraction benchmark.
