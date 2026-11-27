# v0.2.0-alpha.1 candidate notes

Status: local release candidate preparation. This file does not establish that
a new public tag/release, hosted CI run or crates.io package exists. The
[delivery checklist](preview-delivery-roadmap.md) records actual publication.

## Changes

Taskloom extends typed LLM tasks with optional durable tool agents and a local
single-operator application. Existing `AgentTask` consumers keep the minimal
default feature set and compatible task API.

- Native bounded tool loops, persisted runs, exact-input approvals, uncertain
  effect reconciliation, durable budget reservations and archived trajectories.
- Persistent conversations, bounded context projections, parallel/conditional
  role DAGs and PEER planning/execution/review with bounded draft revisions.
- Authenticated CLI/API/UI for runs, workflows, sessions, role handoffs, child
  runs, budgets, approvals, recovery and cited source inspection.
- MCP HTTP tools, local corpus/citations, granted web search/fetch, bounded
  Poppler PDF import, snapshot-bound vector indexes and hybrid retrieval.
- Explicit audited Linux-container Python execution with a separate root
  supervisor, unprivileged code child, bounded resources and no host fallback.
- Versioned 12-case task campaigns with measured success/cost/time/failures,
  shared generation-spend caps and explicit fixture/provider provenance.

## Validation and limits

Local Windows, independent Linux and actual Rust 1.86 regression lanes, warning-
free Rust checks and real CLI/API/browser scenarios are tracked separately in
the delivery checklist. The `0.2.0-alpha.1` source package compiles with all
features and contains the UI/scripts/supervisor/suite assets. A fresh release
installation reports the correct version, imports two documents, verifies their
quotes and does not replay completed work. A real Rust documentation page
was imported and three archived source quotes verified using the offline excerpt
model. The real Linux Poppler parser extracted a two-page PDF and enforced limits.
Five separately invoked Docker Desktop Linux-engine cases verified isolation,
numeric cgroup/file limits, time/output bounds and cancellation cleanup; the
[image provenance](acceptance/sandbox-20261127.json) is retained.

These checks do not establish model answer quality or semantic recall. Paid
model/search/embedding effectiveness requires explicit targets, credentials and
spending limits. No paid model request has been run during this acceptance.
The 12-case offline demo campaign produced three passes and nine expected
capability failures; this is demonstration behavior, not framework/model quality.

The host is a local single-operator preview, with file-backed stores and polling.
It does not provide multitenant access control, distributed scheduling or a
large-scale production control plane. Linux containers share a kernel; use an
appropriate dedicated worker/VM for adversarial workloads. PDF extraction runs
a trusted host parser and is separate from code isolation. Service/infrastructure
charges are separate from generation budgets.

GitHub currently reports an account billing lock preventing hosted jobs from
starting. Git pushes from this environment have also hit connection failures.
Neither configured CI nor local checks establish a successful hosted run. The
published v0.1.0 tag predates all new framework capabilities; no crates.io upload
is part of this preview delivery.
