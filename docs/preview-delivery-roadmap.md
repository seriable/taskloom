# Preview delivery and real-task acceptance

The first three development phases have local acceptance at `082eb80`.
This follow-up tracks product delivery and task effectiveness separately.
Unchecked items are unfinished. Offline fixtures do not establish model quality.

## 1. Reproducible task evaluation

- [x] Versioned suite covering cited research, tool use, approval and recovery.
- [x] Executable assertions, retained trajectories and per-case failure reasons.
- [x] Aggregate success, cost/uncertainty, model/tool calls and elapsed time.
- [x] Explicit offline/provider provenance and a shared campaign spending ceiling.
- [x] CLI invocation and fresh-directory reproduction documentation.
- [ ] Run against explicitly configured real models; publish actual results.

## 2. Open-source delivery

- [ ] Inspect hosted CI and resolve actionable failures.
- [ ] New-user clean-install verification of the release candidate.
- [ ] Updated public source, preview release/tag and installable distribution.
- [ ] Published instructions distinguish the old tag from new capabilities.

## 3. Multi-agent operator experience

- [x] Workflow creation, inspection, event and control APIs.
- [x] Session creation, turn execution, history and recovery APIs.
- [x] UI shows role handoffs, child runs, allocations and review revisions.
- [x] Approval/reconciliation remains bound to the correct owning role/run.
- [x] Restart/concurrency/error coverage and local fixture browser acceptance.

## 4. Research capabilities and isolated execution

- [x] Configured web search and bounded webpage ingestion (local HTTP acceptance).
- [x] PDF text extraction with source provenance and real Linux Poppler acceptance.
- [x] Pluggable semantic retrieval and a tested hybrid ranking path (fixture embeddings).
- [x] Explicit isolated code-execution interface and audited-image supervisor;
      command/approval/fail-closed checks pass, with no host-shell fallback.
- [x] Real-container isolation suite: five cases passed on Docker Desktop Linux.
- [x] End-to-end examples, extension docs and relevant security/recovery checks.

## External preflight

2026-11-27: no configured OpenAI/Anthropic key, target model or taskloom config
was found in the current process environment. Live model acceptance needs an
endpoint/model, a credential environment name and a spending ceiling. Never put
secret values into committed manifests or benchmark reports. The user has been
asked for nonsecret configuration while independent implementation proceeds.

Docker Desktop was initially stopped. Automatic approval rejected the combined
Docker-start/CI-read command with `blocked by policy`; it did not execute and
was not retried through another route. The owner subsequently started Docker
manually. The audited supervisor image was built from a cached pinned Python
base; all five separately invoked real-engine cases then passed. See the
[recorded acceptance](acceptance/sandbox-20261127.json), distinct from command/
config tests and hosted CI. Docker Hub download itself was unavailable.

Hosted CI was inspected read-only at
<https://github.com/seriable/taskloom/actions/runs/45910274041>.
GitHub reports: "The job was not started because your account is locked due to
a billing issue." Matrix cancellation follows that account-level failure;
it is not evidence of a Rust test failure. Account billing/Actions availability
needs the repository owner's intervention. Local checks cannot make that hosted
run green. Git remote read access works; remote HEAD remains `d4b70f6`.
None of the unexecuted external checks is recorded as passing.
