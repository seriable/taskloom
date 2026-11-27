# P0–P2 development and acceptance

Requested on 2026-11-27. Baseline: `fc76e7e`.
The active Codex Goal covers all seven packages below, including the P2
service capabilities. Unchecked work is unfinished; a type, mock or feature
name alone is not acceptance.

The preceding source-delivery scope has shipped. Its hosted Actions gate was
explicitly waived by the owner. Native-provider effectiveness now has limited
local measurements; broader quality and framework comparison remain P0 gates.
The retained 12/12 session-assisted
trial is a controlled integration baseline, not an independent API benchmark.

## Invariants

- Preserve the typed `AgentTask` API, minimal default features and Rust 1.86.
- Bind approvals to exact operations and frozen host contracts; reserve model
  spending durably before dispatch and retain uncertain effects without replay.
- Model output, retrieved content, memories and skill documents cannot grant
  permissions. Actor/tenant/project identity must come from authenticated host
  context rather than caller-supplied ownership fields.
- No implicit paid service, embedding rebuild, telemetry upload or new grant.
  External charges require explicit configuration, prices and an overall cap.
- Keep native API, session-assisted and fixture evidence separate. Do not infer
  semantic truth from exact quote matching or report unavailable checks as green.
- Deliver source code to GitHub `main`; hosted Actions/account billing is not
  a prerequisite. Keep public release tags and their attached assets immutable.

## P0.1 — Complex tasks and multi-agent effectiveness

- [ ] A versioned suite with realistic research, planning, role handoff,
  parallel work, bounded reviewer revisions, tool-error repair and long tasks.
- [x] Real model decisions through an explicitly configured local or remote
  native API, with prompts, outputs, tool observations, framework/model versions,
  cost/uncertainty, wall time and failure reasons retained.
- [ ] Actual DAG/PEER lifecycle acceptance, including process interruption,
  approval occupancy, cancellation and no repeated uncertain writes.
- [ ] Comparable adapters/results for the applicable capabilities of DeerFlow,
  agentUniverse and LangGraph under the same model, task and spending rules.
  Unsupported cases remain visible with reasons; manually assisted latency or
  cost must never enter native-API rankings.
- [ ] Evaluate source coverage/exact quotes separately from claim support,
  factual accuracy and research quality. Publish the method and limitations.

2026-11-27 native increment: verified public Qwen weights ran through the
official Transformers generation API and a bounded learned-embedding API.
The 12-case native Taskloom run scored 5/12. A real LangGraph StateGraph adapter
scored the same 5/12; a separate audit matched all 30 actual model-conditioning
steps (only opaque control IDs normalized) and all 31 HTTP attempts, with the
same suite, output allowance and one-micro-USD campaign cap at explicit zero
local API prices. This is a basic controlled comparison, not broader leadership
evidence. The three native collaboration cases all failed citation/review
acceptance, despite two overlapping analysts in every case and real reviewer
rejection/rewrite in both PEER cases. Tool-error repair, long-task and native
process-death coverage, DeerFlow/agentUniverse results and independent claim
support/factual scoring remain open. See `native-evaluation.md` for commands,
model/hardware provenance, failed trials and interpretation limits.

2026-11-28 increment: opt-in durable final-output contracts now reject invalid
JSON/citations before success and bound repairs within the same budget and
narrowed read-only permissions. Native collaboration v2 still failed 0/3 in
both retained campaigns; two analyst outputs passed exact provenance, including
one semantically empty placeholder. Claims are therefore still a separate gate.
An actual native-generation process kill/restart retained the crash-time
reservation and one uncertain attempt, emitted explicit model recovery and
performed no work on terminal reentry. Its second generation was truncated and
failed; no native uncertain-tool-effect or multi-agent process-death acceptance
is claimed. See `final-output.md` and `acceptance/p0-output-20261128.json`.

## P0.2 — Default research retrieval

- [x] Retrieve evidence through titles and useful metadata as well as body text,
  with bounded, deterministic ranking and meaningful quality regressions.
- [x] Controlled query rewriting with visible provenance and budget accounting
  for any model-backed step; untrusted documents cannot issue rewrite commands.
- [x] Embedding/provider/index settings and explicit build/update operations in
  CLI, API and UI; index/model/corpus binding remains strict.
- [x] The default research host can select lexical or configured hybrid search;
  operator search and model tools use the same retriever.
- [ ] A relevance-labelled retrieval set and real embedding acceptance report;
  preserve empty recalls, skipped evidence and stale-index failures.

2026-11-27 increment: field retrieval, deterministic host aliases, configured
hybrid loading, explicit full index build/update and an independent durable
embedding spending ledger are implemented. CLI/API integration includes two
separate CLI processes contending for the same remaining cap. Configuration
validation/export, index status/build and operator search controls exist in the
UI; browser checks covered connection, actual document upload, title-only
recall, visible expansion, invalid hybrid settings and validated configuration
export, not a real embedding build. The 12-document/18-query synthetic
regression corpus improved body-only recall@5/MRR@5 from 0.3125/0.3125 to
1.0/1.0; this visible maintainer-authored set is not a held-out quality benchmark.
At that initial increment, native embedding relevance, full browser index
acceptance and model/competitor effectiveness were unchecked. See the later
native increment below and `research-tools.md` for migration,
commands, billing limitations and frozen-index restart behavior.

The later native increment completed actual embedding index build/update through
CLI/API/UI and a browser hybrid query after host restart. The UI ledger advanced
19→20 for a build and 20→21 for a query; an independent CLI build also passed.
Actual Qwen embedding trials retain an important quality failure: default hybrid
returned unrelated evidence on both negative queries; an exploratory cosine
cutoff/weight configuration improved this to 1/2, not full acceptance. The
configured cutoff is available and identity-bound, but this small visible set
does not establish general semantic relevance or answer abstention.

## P1.1 — Cross-session memory

- [x] Versioned, source-linked fact/preference records with scoped ownership.
- [x] Extraction, bounded retrieval and explicit injection into new sessions.
- [x] Conflict/update policy, user edit/delete/export and deterministic recovery.
- [x] Tenant/project isolation, prompt-injection tests and evidence explaining
  each retained or superseded fact; ordinary history is preserved separately.

2026-11-28 memory increment: an opt-in local memory store, trusted host access
objects, explicit tool-less metered extraction, pending candidates, operator
review, exact-slot conflict replacement, source-linked versioned edits and
erasure/tombstones are integrated through SDK/API/CLI. Explicit memory-aware
session turns freeze bounded recalled User data; they never grant tools or
turn recalled records into fresh source testimony. Runtime/HTTP/CLI tests and
actual local Qwen campaigns cover the declared lifecycle. The final native
campaign checked another owner while the first owner's records were still
live. This proves memory-namespace behavior in the single-operator host, not
multi-user authorization of all runtime state. Recall remains lexical plus
reviewed preferences, source extraction is from closed User turns, semantic
slot aliases require operator review, and broad extraction/factual-quality
benchmarks remain open. P2's service identity/database gates remain unchecked.
See `long-term-memory.md` and `acceptance/p1-memory-20261128.json`.

## P1.2 — Skills, templates and connectors

- [x] Versioned installable packages declaring tools, dependencies and required
  credential names; reject archive traversal, symlinks and oversized contents.
- [x] Host validation and grants before activation. Skill content cannot execute
  host commands, reveal secrets or enlarge permissions on its own.
- [x] Frozen package identity in durable runs; explicit upgrade/removal behavior.
- [x] Useful task templates and working connector examples with reproducible
  installation, execution and failure/recovery acceptance.

2026-11-28 skill increment: inert USTAR imports, immutable version/digest objects,
exact dependency/resource resolution, host tool/credential-name grants, mounted
workflow templates and recovery-only retired pins are integrated through SDK,
CLI and API. Tests cover malformed archives, authority/effect mismatches, exact
approvals, upgrades and recovery admission. Actual Qwen/MCP runs matched live
GitHub repository/license data and retained a response-lost filesystem append
as uncertain until explicit observation/reconciliation, with one write total.
The connector templates have native execution/recovery evidence, but the
source-summary template still failed native exact-citation acceptance. Complete
template acceptance and the corresponding P0 research-quality gate therefore
remain open; the failure is not relabelled as a passing installation result.
Retirement affects new activation; loaded processes keep frozen snapshots and
recovery still requires original host contracts. These are data packages and
single-operator examples, not a package registry trust service or P2 multi-user
authorization. See `skill-packages.md` and `acceptance/p1-skills-20261128.json`.

Later 2026-11-28 locator increment: an explicit research-brief 1.1.0 source
version uses exact retrieved document/revision/byte-offset locators and ordinal
markers, with host-derived canonical handoff and original model JSON retained.
The first native probe still failed; the second, with clarified role guidance,
passed all four declared templates and thirteen control checks. This accepts
the bounded template/connector examples, not general research quality. Old
1.0.0 failures and the first 1.1.0 probe remain visible; changed prompts/format
are not a same-conditions comparison. See `final-output.md` and
`acceptance/p0-locators-20261128.json`. P0 collaboration/research-quality and all
P1.3/P2 gates remain subject to their own acceptance.

The accompanying native collaboration v3 campaign still scored 0/3. Five
analysis outputs validated and two analysts overlapped in each case, but missing
writer-owned retrieval and invalid model JSON prevented completion; required
reviewer rework was not reached. This is retained failure evidence, not complex
collaboration acceptance. Role prompts/format changed from v2. See
`native-evaluation.md` for token/cost/time accounting and source-support limits.

## P1.3 — Observability and diagnosis

- [ ] Correlated run/workflow/session/model/tool/retrieval spans and metrics.
- [x] Configured OpenTelemetry export, default-off external transmission,
  bounded buffers, teardown and failure handling; no prompt/secret export by
  default. Verify against a real local receiver.
- [x] Error attribution, retrieval inspection and evaluation comparison views.
- [x] Distinguish generation, embedding and tool-service charges; missing
  prices/usage remain unknown rather than silently becoming zero.

2026-11-28 observability increment: validated journal projections now link
workflow/node/run and session/turn/run records, with model/tool/retrieval,
approval-wait, reconciliation and output-validation spans and snapshot metrics.
Explicit bounded OTLP/HTTP JSON export sends no prompt, quote, tool arguments,
actor or credential by default. A real pinned official Windows Collector
accepted the final native probe's 24 spans and 54 gauge observations; the four
native templates passed and cross-restart parent/span identity was retained
within the selected workflow trace. Mock HTTP tests separately cover partial
acceptance, redirects, timeout, backpressure, payload limits and shutdown.

CLI/API inspection needs no model/connector credential; export binds both saved
revisions and destination settings. Browser acceptance covers native trace
reading/export, saved retrieval details, a 390-pixel layout, condition-aware
local comparison and unsupported-schema rejection with stale results cleared.
Generation metrics are saved-checkpoint observations; live reservation journals
may be newer. Embedding has an independent host ledger and no per-run allocation
here; generic tool-service billing is unknown, not zero. Fixture/chat ledgers
and wall time do not become real API billing or inference benchmarks.

The complete unified-trace box stays unchecked: inspecting a child run alone
currently creates a standalone trace identity. A durable host-owned binding
across parent and standalone entry points remains to be implemented and tested.
Old journals retain missing wall clocks; sessions have no terminal close event,
and their open root spans are not exported as completed. These are explicit
snapshot projections, not an implicit live subscriber or W3C header injector.
See `observability.md`, `native-evaluation.md` and
`acceptance/p1-observability-20261128.json` for commands, migration, failed
prerequisite/harness probes, raw evidence and limitations. P0/P2 remain open.

## P2.1 — Product experience

- [ ] Model/tool/retrieval configuration wizard with validation and safe secret
  handling; template selection and explicit capability grants.
- [ ] Visual workflow editing, validated role/dependency/condition bindings and
  a code/API representation that round-trips without changing semantics.
- [ ] Inspect, approve, pause, cancel and resume actual runs from the UI,
  including child handoffs, reviewer revisions and actionable failures.
- [ ] Browser acceptance of complete user flows and narrow-screen behavior.

## P2.2 — Service capabilities

- [ ] SQLite and PostgreSQL transactional backends for required persistent state
  with versioning, migration, CAS, budget journals and ownership semantics.
- [ ] Authenticated users/roles, tenant/project isolation and scoped quotas.
- [ ] Durable task queue, cross-process workers, lease renewal/fencing and
  recovery after worker death. Lost ownership stops new dispatch; external
  effects do not acquire an invented exactly-once guarantee.
- [ ] Concurrent-worker, stale-owner, isolation, approval, cancellation,
  uncertain-effect and restart tests, including real PostgreSQL acceptance.
- [ ] A reproducible bounded-load report, operational backup/restore and
  deployment/migration documentation.

## Delivery gates and order

Start with retrieval recall and runnable complex-agent evaluation, then memory,
skills and observability; finish the UI and service backends using those same
host contracts. Each coherent increment needs meaningful checks and its own
evidence before the corresponding boxes are checked. Preserve current changes
and commit history. Complete the Goal only after all seven packages and final
source-publication verification are accepted; missing external prerequisites
stay recorded while independent implementation continues.
