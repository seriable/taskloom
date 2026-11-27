# Reproducible task acceptance campaigns

This is an executable baseline, not evidence of hosted-model quality.
Actual local Qwen API trials and a LangGraph comparison are now documented in
[native evaluation](native-evaluation.md), including unsuccessful outcomes.
Broader effectiveness gates remain in the [P0–P2 roadmap](p0-p2-roadmap.md).

## Suite and CLI

`examples/benchmarks/acceptance-v1.json` contains 12 controlled tasks:

- Three cited research tasks over supplied, versioned source documents.
- Five exact arithmetic tasks, including two- and three-step tool use.
- Two explicitly approved local note writes, including Unicode text.
- Two interrupted-controller recovery tasks which must preserve one original
  local write without repeating it.

Install the development executable with `cargo install --path . --locked
--features app`. Configure the provider, endpoint/model, environment-key name
and actual prices using [the application guide](application.md). Then:

```sh
taskloom --config provider.json benchmark examples/benchmarks/acceptance-v1.json target/benchmarks/model-a-run-1 100000
```

The last positional argument is the entire campaign's model-cost ceiling in
the configured currency's millionths. `100000` means 0.1 currency units; it is
an example ceiling, not a prediction of task cost. Each run also obeys the
configuration's per-run limits. The sequential scheduler deducts committed and
uncertain charges before allocating the next case, preventing independent
per-case allowances from multiplying the requested total ceiling.

Use an **unused output directory**. Its parent directories are created as
needed, but the campaign directory itself is create-only: reusing it fails
before model/tool dispatch. This avoids mixing old model answers or effects
into a fresh evaluation. Repeat comparisons into new directories with the same
suite and configuration, explicitly choosing each new campaign's spending cap.

The command returns nonzero if any requested case failed or was not run. Failed
outcome checks still produce a report and retain trajectories. Validation errors
in suite/config or an existing output directory fail before task dispatch.
Provider credentials are loaded from the application's environment names, not
from a suite or report. Configured MCP endpoints and arbitrary application tools
are not initialized or inherited by this runner.

## Results and interpretation

Each campaign stores:

```text
suite.json                      exact manifest used
report.json                     atomic incremental/final report
cases/<case-id>/runs/*.json      complete durable run and budget journal
cases/<case-id>/knowledge/       case-local documents and chunks
cases/<case-id>/effects/         actual fixture note, when applicable
```

The report records suite/configuration fingerprints, execution kind/model,
requested/executed/passed/failed/not-run counts, the current active case,
success rate, observed usage, actual serving models from completed decisions,
model steps, tool attempts, observed effects,
wall-clock elapsed time, conservative model spending, uncertain spending,
lifecycle flags, failure reasons and trajectory locations. Final text is bounded
to 8,192 characters in the report; full text remains in the trajectory.

Success is **passed/requested**. Not-run cases remain in the denominator. A
case passes only when execution completed, all declared assertions passed,
required approval/recovery boundaries were exercised, and completed reentry
left its revision/events/effects unchanged. Research checks validate exact
source citations and document coverage; arithmetic checks require actual tool
attempts and exact JSON results; write checks require the declared arguments.

These automated checks do not score semantic entailment, expert reasoning,
writing quality or factual accuracy outside the supplied assertions. A quoted
source can still be misinterpreted. Human review or a separately calibrated
quality evaluation should accompany published research results. The report
includes its success definition rather than presenting the rate as a universal
agent quality score. Per-case latency is full wall time, including fixture
control/storage, not just provider inference latency.

Execution kinds distinguish `offline_demo`, `fixture` and `configured_provider`.
The last means a configured provider adapter was used; it does not by itself
prove a paid public endpoint or a particular backend/model. Keep provider-side
acceptance evidence when making that claim. Fixtures are useful for verifying
the evaluator and lifecycle machinery, not for comparing model intelligence.

The default application model is a local knowledge-excerpt demo. It cannot
perform the arithmetic and write tasks in this suite. Running the whole suite
with that demo will correctly report failures instead of inventing a successful
model baseline. Zero-cost offline execution can use a ceiling of 0; a configured
provider campaign with a zero ceiling is marked not-run before dispatch.

## Approval and recovery fixture boundaries

The runner exposes only bounded arithmetic, case-local knowledge lookup and
`record_note`. Note writing is non-idempotent, permissioned and requires exact
input approval. Research/arithmetic cases cannot execute the write tool.
Approval/recovery cases use a clearly named **benchmark fixture operator** to
approve this one isolated local operation. This does not implement automatic
approval of arbitrary production tools, remote MCP tools or external services.
The note path is derived from hashed run/call identities; model text never
selects a filesystem path. Only one note is allowed per case.

Recovery deliberately drops the controller future after a note has been
flushed, while its result is still uncheckpointed. A fresh durable controller
must observe `NeedsRecovery`. The harness verifies the original file's bytes,
records the observed result, then resumes and checks one write. This is a
**controller-interruption** scenario, not an OS-process crash. Existing
`agent_durable` and `workflow_durable` tests separately terminate actual child
processes after effects; do not substitute this fixture for those checks.

Campaign reports are written before dispatch and after every case. While a case
is active, the report marks `running`, `completed: false`, unknown cost and a
conservative allocation at its ceiling. If the process exits then, the report
does not pretend that the unfinished task was free or successful. Use its
trajectory/journal for investigation; the runner does not silently restart an
interrupted campaign. A newer budget journal takes precedence over an older
checkpoint when reporting observed reservations. Unsettled/dropped reservations
remain charged and uncertain. An unobserved budget stops further case admission.

Model-spend limits do not meter unrelated tool/service charges. The supplied
tools are local fixtures and do not contact paid external services. Host `Model`
implementations must honor the same reservation/usage contracts as normal agent
execution; this runner is not a sandbox for arbitrary host Rust code.

Campaign directories contain complete prompts, sources and model outputs.
Keep them private by default (the documented `target/` path is ignored). Review
and sanitize artifacts before choosing which benchmark evidence to publish.

## Verification status

Local evaluator regressions cover outcome failures, mandatory lifecycle
boundaries, exact note input, actual local writes, controller interruption,
completed reentry, a shared spending ceiling, unsettled paid attempts,
incomplete active-campaign reporting, strict manifests, output-directory reuse,
and real CLI subprocesses. Mock-model charges in these regressions are explicitly
fixtures, not provider bills. The later local-weight trials have separate
provenance; no paid hosted-model results are claimed here.

The complete offline-demo suite is also a regression: three source-excerpt
research tasks pass and nine arithmetic/write tasks fail as unsupported, with
zero model cost and no not-run cases. This is the demo's limited behavior, not
a taskloom model-quality score. It exposed a pre-existing decimal-score
round-trip issue in persisted context fingerprints; exact float parsing fixes
reloads without weakening fingerprint validation. The regression exercises the
whole CLI campaign rather than only testing the JSON parsing option in isolation.

Evaluator increment local acceptance (2026-11-27): 206 all-feature tests pass
on Windows, Rust 1.86 and independent WSL Linux, including 11 benchmark tests.
Warning-free Clippy/docs and package verification pass. A retained fresh CLI
campaign confirms the expected offline-demo 3 passed/9 failed/0 not-run result
with zero cost and no unobserved budget. This is not hosted-model acceptance.
