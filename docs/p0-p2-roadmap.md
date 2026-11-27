# P0–P2 development and acceptance

Requested on 2026-11-27. Baseline: `fc76e7e`.
The active Codex Goal covers all seven packages below, including the P2
service capabilities. Unchecked work is unfinished; a type, mock or feature
name alone is not acceptance.

The preceding source-delivery scope has shipped. Its hosted Actions gate was
explicitly waived by the owner. Native-provider effectiveness is still
unverified and is a required P0 gate here. The retained 12/12 session-assisted
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
- [ ] Real model decisions through an explicitly configured local or remote
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

## P0.2 — Default research retrieval

- [ ] Retrieve evidence through titles and useful metadata as well as body text,
  with bounded, deterministic ranking and meaningful quality regressions.
- [ ] Controlled query rewriting with visible provenance and budget accounting
  for any model-backed step; untrusted documents cannot issue rewrite commands.
- [ ] Embedding/provider/index settings and explicit build/update operations in
  CLI, API and UI; index/model/corpus binding remains strict.
- [ ] The default research host can select lexical or configured hybrid search;
  operator search and model tools use the same retriever.
- [ ] A relevance-labelled retrieval set and real embedding acceptance report;
  preserve empty recalls, skipped evidence and stale-index failures.

## P1.1 — Cross-session memory

- [ ] Versioned, source-linked fact/preference records with scoped ownership.
- [ ] Extraction, bounded retrieval and explicit injection into new sessions.
- [ ] Conflict/update policy, user edit/delete/export and deterministic recovery.
- [ ] Tenant/project isolation, prompt-injection tests and evidence explaining
  each retained or superseded fact; ordinary history is preserved separately.

## P1.2 — Skills, templates and connectors

- [ ] Versioned installable packages declaring tools, dependencies and required
  credential names; reject archive traversal, symlinks and oversized contents.
- [ ] Host validation and grants before activation. Skill content cannot execute
  host commands, reveal secrets or enlarge permissions on its own.
- [ ] Frozen package identity in durable runs; explicit upgrade/removal behavior.
- [ ] Useful task templates and working connector examples with reproducible
  installation, execution and failure/recovery acceptance.

## P1.3 — Observability and diagnosis

- [ ] Correlated run/workflow/session/model/tool/retrieval spans and metrics.
- [ ] Configured OpenTelemetry export, default-off external transmission,
  bounded buffers, teardown and failure handling; no prompt/secret export by
  default. Verify against a real local receiver.
- [ ] Error attribution, retrieval inspection and evaluation comparison views.
- [ ] Distinguish generation, embedding and tool-service charges; missing
  prices/usage remain unknown rather than silently becoming zero.

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
