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
- [x] Comparable adapters/results for the applicable capabilities of DeerFlow,
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

Later 2026-11-28 collaboration increment: explicit verified Qwen3-4B weights
and V5 role/graph guidance produced 1/3 native passes, with overlapping analysts
in all three cases. The PEER case still failed analyst marker repairs; the
submitted-draft audit completed but failed review rejection/source coverage.
Separate non-blind session-assisted labels cover nine final claims: seven
supported by their attached citation, two insufficient, with corpus-relative
agreement on nine and explicit task omissions. Those supplied labels are not
native judge API or blind truth measurements. An actual agentUniverse
PeerWorkPattern/ExecutingAgentTemplate adapter completed all native roles on
the corresponding PEER task but scored 0/1 for missing reservation/source
coverage; Taskloom's corresponding PEER task also failed. Other DAGs are marked
not comparable in this PEER-only adapter. Native search uses shared Taskloom
retrieval, with wrapper/repair/retry differences, so no leadership or identical
conditioning claim is made. Retained V4 guidance errors, a first adapter HTTP
422 harness failure and the original dependency resolver conflict are recorded.
DeerFlow and broader complex/long/recovery comparisons remain open. See
`collaboration-comparison.md` and `acceptance/p0-collaboration-20261128.json`.

Later DeerFlow increment: the actual pinned DeerFlowClient ran the corresponding
natural-research task on the same Qwen3-4B model/corpus/output allowance and
zero-priced cap. The final guarded trial completed source retrieval and one
format repair but scored 0/1 for no delegated analyst/reviewer. Native prompts
and natural delegation differ from the reference DAG; other exact staged
lifecycles are explicitly not comparable. All three failed/exploratory probes,
raw requests/usage, environment-guard differences and limits are retained in
`acceptance/p0-deerflow-20261128.json`. This supplies a qualified actual result,
not general DeerFlow inability or Taskloom leadership. Complex LangGraph,
broader long/recovery/repair and quality gates remain open, so P0.1 stays open.

Later complex LangGraph increment: actual StateGraph parallel joins, dynamic
Send fan-out and conditional native review completed all three corresponding
V5 tasks, scoring 2/3. PEER failed reservation/source coverage; submitted-draft
audit observed real initial rejection and passed the declared corrected-output
gate. All three cases had overlapping analyst role lifecycles. Distribution
control hashes, dependency versions, actual native requests/usage and failed
outputs are retained. The applicable framework-comparison gate now has actual
qualified DeerFlow, agentUniverse and LangGraph results, including explicit
adapter omissions and conditioning differences. These outcomes are not an
overall ranking or independent semantic/factual-quality evidence. See
`collaboration-comparison.md` and `acceptance/p0-langgraph-complex-20261128.json`.

Later workflow crash increment: a real fsynced approved MCP append was interrupted
by killing its actual Taskloom host before the held reply. A second process
could not steal the live workflow lease; restart retained the same uncertain
call, and reentry made no new model/tool attempt. Explicit actual note-status
observation and labelled operator reconciliation allowed the native append role
and separate tool-less auditor to finish with one external effect. The final
probe passed nine controls; the first mutex/timing fault-harness failure and its
incomplete native-usage qualification remain visible. This is one active append
role at death, not simultaneous generation death or distributed-worker service
acceptance. Broader PEER lifecycle/long-task/native tool-error-repair and quality
gates stay open. See `workflow-recovery-acceptance.md` and
`acceptance/p0-workflow-crash-20261128.json`.

Later explicit tool-error repair increment: actual MCP notes_status returned one
explicit error, then a later native assistant decision requested a new granted
safe read. Two dispatches (one error, one success), actual empty-store agreement,
read-only authority and unchanged terminal reentry passed six controls. This
is not an uncertain-write retry or broad repair benchmark. The current helper
also reran the nine-control native write-crash acceptance with read-error
injection disabled; old fault probes remain retained. See
`acceptance/p0-tool-repair-20261128.json`. Long-task realism/quality, broader
PEER lifecycle and semantic/retrieval gates remain open, so the full P0 package
is still incomplete.

Later 2026-11-28 contract increment: citation contracts can carry a host-selected
JSON Schema for the original answer. The schema and exact source/quote/coverage
checks must pass together; repairs retain only the original read-only retrieval
grants, and changed schemas refuse old-run resume. SDK/API planning tests and a
real Qwen single-source checklist smoke cover this bounded structural gate.
The broader v9 three-case native campaign still failed 0/3 on malformed/missing
fields or provenance. The smoke passed one case with six zero-priced requests;
it is not complex-task quality. Native claim-support controls scored only
72.2%/33.3% in the first rubric and 61.1%/61.1% after clearer instructions, with
2/7 known contradictions identified in both. Neither judge passed its gates.
The v7 full-topic audit found only 1/3 complete even though the original
mechanical counter reported 2/3: the submitted-draft output omitted the
explicit reservation-timing requirement. Exact matches do not establish truth
or complete task coverage. Failures remain retained in the new acceptance
receipt; see `final-output.md` and
`acceptance/p0-output-contract-20261128.json`. P0 remains open.

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

Later 2026-11-28 relevance increment: explicit host stop words and exact long
mixed letter/digit identifier constraints are available through the default
research host and SDK, without changing old empty/disabled configuration
bindings. An actual raw/instruct probe did not solve negative discrimination;
the retained first 70-query profile campaign failed one nonexistent-hash query.
Adding the generic exact-identifier constraint, with the same dataset, labels,
cutoff and gates, passed the six fixed evaluation checks. Configured hybrid
recall@5/MRR@5 across all queries was 46/48 and 0.9184, with 22/22 negatives
empty; the 36-query evaluation group was 24/24 and 1.0, with 12/12 negatives
empty. It is now an observed development regression, not fresh held-out/blind
evidence. There were 67 actual local embedding requests at explicit USD 0 API
prices/cap, no generation calls, and two retained calibration misses.
This accepts that bounded configured profile; the general default/realistic
corpus quality gate above stays open. See `retrieval-relevance.md` and
`acceptance/p0-retrieval-relevance-20261128.json` for failures and limitations.

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

- [x] Correlated run/workflow/session/model/tool/retrieval spans and metrics.
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

At that initial increment the complete unified-trace box stayed unchecked:
inspecting a child run alone created a standalone trace identity. The subsequent
host-binding increment below closes that specific gap for new managed runs.
Old journals retain missing wall clocks; sessions have no terminal close event,
and their open root spans are not exported as completed. These are explicit
snapshot projections, not an implicit live subscriber or W3C header injector.
See `observability.md`, `native-evaluation.md` and
`acceptance/p1-observability-20261128.json` for commands, migration, failed
prerequisite/harness probes, raw evidence and limitations. P0/P2 remain open.

Later 2026-11-28 host-binding increment: new workflow/session children now persist
an immutable parent reference before the first checkpoint. Standalone CLI/API/UI
inspection retains the parent's trace ID and exact child span observations;
unknown parent context references have no invented outcome/time and are omitted
separately from actual open spans. Snapshot gauges include selected scope, so
overlapping parent/child views must not be summed. Legacy unbound runs retain
their hashes and resume without backfill; only their parent view joins them.

Real local Qwen native workflow approval/restart/response-lost reconciliation and
two-turn session restart campaigns passed 28 and 27 diagnostic checks respectively.
All twelve explicit exports were accepted by the actual official Collector,
with 38 workflow and 18 session span observations (repeated snapshots are not
unique operations). Native template execution separately passed four templates
and thirteen controls. Initial probes exposed a real 64-character diagnostic-ID
limit; the fixed route now admits generated child IDs, with CLI/API regression
coverage. Browser checks passed linked child inspection/export and a repaired
390-pixel heading layout. See `acceptance/p1-trace-binding-20261128.json` for
raw reports, failed probes, version/digest evidence and check limits. This accepts
the bounded P1 observability package; P0 quality/comparisons and P2 remain open.

## P2.1 — Product experience

2026-11-28 foundation: pure configuration review and actual workflow compilation
preview now exist through SDK/CLI/API, with exact-plan launch binding and
additive routes. All nine short native Qwen3-4B integration controls passed;
the all-feature suite has 330 passing tests and six explicit ignored checks.
This does not provide a visual editor/wizard or multi-user/database service.
At that foundation, every P2 box remained unchecked. See `workflow-design.md` and
`acceptance/p2-planning-20261128.json` for migration and deferred checks.

- [x] Model/tool/retrieval configuration wizard with validation and safe secret
  handling; template selection and explicit capability grants.
- [x] Visual workflow editing, validated role/dependency/condition bindings and
  a code/API representation that round-trips without changing semantics.
- [x] Inspect, approve, pause, cancel and resume actual runs from the UI,
  including child handoffs, reviewer revisions and actionable failures.
- [x] Browser acceptance of complete user flows and narrow-screen behavior.

Later 2026-11-28 UI increment accepts the bounded single-operator product
package: a configuration wizard statically reviews generation/tool/retrieval
declarations and exports explicitly, without applying grants or reading secret
values. Node cards, directed connections, role/dependency/condition/select
bindings and a lossless JSON code/file representation use actual compiler
preview and exact launch. Template preview validates active pinned input schemas.
Unknown roles/advanced conditions and large integer/decimal values are preserved;
invalid visible edits prevent launch and any edit invalidates prior review.

Actual browser operations completed native Qwen3-4B DAG and installed-template
runs, then paused a separate composite-condition DAG, restarted the host and
resumed the second node with its first child retained at one generation/dispatch.
A separately labelled offline fixture performed exact approval, one real local
note write, reviewer rejection/rewrite and cancellation; its model decisions are
not native quality evidence. Final 390px wizard/page checks measured client/scroll
width 375/375. Early input-commit, provider-option and long budget-text overflow
bugs are recorded with repairs and affected-flow reruns. All-feature tests remain
330 passed/six ignored; Rust 1.86, strict Clippy, rustdoc and codec controls pass.
See `workflow-design.md` and `acceptance/p2-design-ui-20261128.json` for raw
journals, fee/provenance qualifications and reproducible steps. This accepts
P2.1's local product flows, not P2.2 services, broad task quality or the entire
goal; P0 and every P2.2 box below remain open.

## P2.2 — Service capabilities

2026-11-28 SQL SDK foundation: opt-in SQLite/PostgreSQL run, workflow, session
and budget journals have transactional schema v1, namespace-bound envelopes,
CAS and database-clock renewable/fenced ownership. Actual SQLite and official
PostgreSQL 16.15 each passed eleven native lifecycle controls, including killed
generation recovery and unchanged terminal reentry; quiescent backup/restore
passed three controls. The interrupted response usage remains unknown and
the recovery output truncates to model_error, so this is not long-task quality
acceptance. File stores still back the local Application/CLI/UI, and SQL corpus,
memory, skills, embedding ledger, authentication, quotas and queue/worker
integration are unfinished. Earlier native shutdown failures remain retained.
All P2.2 boxes stay unchecked. See `sql-storage.md` and
`acceptance/p2-sql-stores-20261128.json` for migration and reproducible evidence.

Later SQL knowledge increment: namespace-bound CorpusStore and MemoryStore now
use transactional aggregate-row locking/CAS in schema v1. Both real databases
passed concurrent-write, cold-read, invalid/corrupt-record, scope and erasure
controls. Actual native Qwen source/extraction plus explicitly labelled operator
review/edit/erasure passed ten controls per backend; separate backup/restore
verified corpus, source sessions and tombstones in three controls. Short visible
preference examples do not imply broad semantic quality. SQL skills/embedding
ledgers, Application/CLI/UI backend selection, multi-user identity/RBAC, quotas
and queue/worker/load integration remain unfinished; no P2.2 box is checked.
See `acceptance/p2-sql-knowledge-20261128.json`.

Later identity/service increment: SQL host-managed users, project roles,
high-entropy hashed API tokens, database-clock expiry, live revocation and CAS
administration now protect an explicitly scoped JSON service router. Each
request checks current membership and binds decision actors from authenticated
context. Real SQLite/PostgreSQL HTTP checks cover approvals, actual fsynced
effects, lost-result recovery, isolation and parent-specific control routes.
Separate service/admin processes passed ten authority/restart controls per
backend with an explicitly offline model. Project Applications still use file
journals, private-memory service endpoints are unavailable, and SQL Application
selection, global quotas, queue/workers and operational/load acceptance remain
unfinished. All P2.2 boxes remain open. See `multi-user-service.md` and
`acceptance/p2-multiuser-20261128.json`.

Later SQL Application increment: explicit backend selection now covers runtime
and budget journals, workflows/sessions, corpus/memory, skill catalog/content,
embedding index/spend and host identities. Explicit CLI migration establishes a
stable dataset identity without rewriting legacy SDK journals; file-to-SQL bulk
import remains unsupported. Both databases passed 15 short native API lifecycle
controls, 11 separate-process SQL service authority/restart controls, real
learned-index/ledger integration and quiescent backup/restore comparisons. The
backup verifier excludes only newly generated diagnostic capture time and retains
raw observations. Earlier harness, shutdown-reentry and fixture readiness faults
remain visible. Actual SQL embedding relevance still failed both negative queries
per backend, so P0.2 stays open. Browser SQL run/template/trace controls also passed.
The bounded persistence gate below is accepted; global generation quotas,
queue/workers, cross-process service control and load/deployment gates remain
unfinished. See `sql-application.md` and
`acceptance/p2-sql-application-20261128.json`.

Later generation-quota increment: project-wide cumulative cost and outstanding
attempt caps now share the reservation/checkpoint SQL transaction across every
run writer. Unknown zero-priced attempts retain their slots after process death;
known atomic refusal incurs no invented uncertainty. Explicit CAS administration
imports prior journals, audits cap changes and keeps accounting while disabled.
All SQL gateway attempts require known prices even without private run caps.
Quiescent schema-v2 migration fences old v1 binaries and preserves existing
dataset identity/checkpoints. Both databases passed ten actual native API /
separate-process admission/recovery controls; backup/restore passed three and
retained actual-v1 migration/import passed four controls. Earlier configuration,
Unicode-codec and automatic fixture-scope probe faults remain recorded. These
accept bounded generation quotas, not durable queue/workers, cross-process
service controls, load or P0 effectiveness. P2.2's broader boxes remain open.
See `generation-quota.md` and
`acceptance/p2-generation-quota-20261128.json`.

- [x] SQLite and PostgreSQL transactional backends for required persistent state
  with versioning, migration, CAS, budget journals and ownership semantics.
- [x] Authenticated users/roles, tenant/project isolation and scoped quotas.
- [x] Durable task queue, cross-process workers, lease renewal/fencing and
  recovery after worker death. Lost ownership stops new dispatch; external
  effects do not acquire an invented exactly-once guarantee.
- [x] Concurrent-worker, stale-owner, isolation, approval, cancellation,
  uncertain-effect and restart tests, including real PostgreSQL acceptance.
- [x] A reproducible bounded-load report, operational backup/restore and
  deployment/migration documentation.

Later 2026-11-28 durable-queue increment: schema v3 adds immutable project task
inputs, transactional admission/claims, renewable cross-process leases and
queue-bound runtime fencing. Both databases passed seventeen actual native
Qwen3-4B lifecycle controls: eight API requests per backend, seven reported
usages and one unknown cancelled response, exact approval, actual process
death during an fsynced MCP append, no replay and labelled observed operator
reconciliation. Separate workers also completed two session turns and
overlapped role lifecycles. This does not establish simultaneous GPU inference,
long-task quality or production throughput.

A separate four-worker/four-producer finite offline load passed fourteen
controls per backend, completing 48 healthy jobs and one explicitly resumed
post-service-restart job. One additional job was never dispatched and its
scheduling was explicitly abandoned without model/MCP credentials. Persisted
event intervals respected four running and 24 open jobs; polling peaks are
reported separately. All 49 fixture generation attempts settled, with no
unknown usage or external API charges. These are fixture service measurements,
not native model latency or an unlimited scalability claim.

Cold SQLite/PostgreSQL queue backup/restore passed three controls, retaining
runtime associations, all seven native runs and the unknown cancelled attempt.
Actual retained v1 migration passed four controls; retained v2 migration passed
ten, including real old-binary schema rejection and an actual old-process lease
blocking upgrade until expiry. This is a quiescent upgrade, not rolling-upgrade
or old-binary native dispatch acceptance. The later cold abandonment and
inspection/error-handling additions have SQL/fixture/CLI evidence separately
from the earlier native campaign. Failed MCP teardown, contended short-TTL and
archived-build-cache probes remain visible. See `durable-queue.md` and
`acceptance/p2-durable-queue-20261128.json` for final checks and limitations.
These close the documented bounded P2 service scope; P0 complex/long/PEER,
semantic/factual and retrieval-quality requirements still keep the Goal open.

## Delivery gates and order

Start with retrieval recall and runnable complex-agent evaluation, then memory,
skills and observability; finish the UI and service backends using those same
host contracts. Each coherent increment needs meaningful checks and its own
evidence before the corresponding boxes are checked. Preserve current changes
and commit history. Complete the Goal only after all seven packages and final
source-publication verification are accepted; missing external prerequisites
stay recorded while independent implementation continues.
