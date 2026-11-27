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

- [ ] Workflow creation, inspection, event and control APIs.
- [ ] Session creation, turn execution, history and recovery APIs.
- [ ] UI shows role handoffs, child runs, allocations and review revisions.
- [ ] Approval/reconciliation remains bound to the correct owning role/run.
- [ ] Restart/concurrency/error coverage and browser acceptance.

## 4. Research capabilities and isolated execution

- [ ] Configured web search and bounded webpage ingestion.
- [ ] PDF text extraction with explicit source provenance and failure behavior.
- [ ] Pluggable semantic retrieval and a tested hybrid ranking path.
- [ ] Explicitly isolated code-execution interface; no unsafe host-shell fallback.
- [ ] End-to-end examples, extension docs and relevant security/recovery checks.

## External preflight

2026-11-27: no configured OpenAI/Anthropic key, target model or taskloom config
was found in the current process environment. Live model acceptance needs an
endpoint/model, a credential environment name and a spending ceiling. Never put
secret values into committed manifests or benchmark reports. The user has been
asked for nonsecret configuration while independent implementation proceeds.

Docker CLI exists; the Docker Desktop Linux engine was not running at preflight.
Actual container isolation acceptance must be distinguished from command/config
tests. Automatic command approval rejected the combined Docker-start/CI-read
command with `blocked by policy` and no more specific reason; the command did
not execute. Docker startup has not been retried through another route.

Hosted CI was inspected read-only at
<https://github.com/seriable/taskloom/actions/runs/45910274041>.
GitHub reports: "The job was not started because your account is locked due to
a billing issue." Matrix cancellation follows that account-level failure;
it is not evidence of a Rust test failure. Account billing/Actions availability
needs the repository owner's intervention. Local checks cannot make that hosted
run green. Git remote read access works; remote HEAD remains `d4b70f6`.
None of the unexecuted external checks is recorded as passing.
