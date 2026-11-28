# Long task and semantic evidence review — 2026-11-28

This is an independent Agent review of saved results, not human-labelled gold,
an independent external judge, or a new model run. The goal was paused before
the user's subsequent authorization for parallel Agent work. This review makes
no new remote requests, changes no saved campaign result, and does not authorize
publication or mark P0 complete.

## Latest conclusion and goal scope

The latest evidence demonstrates an implemented and executed evaluation method,
source-bound actual-answer assessment, explicit failure reporting and a bounded
feedback correction. The development goal requires those capabilities and
honest results; it does **not** require every historical campaign or every LLM
answer to be correct. Earlier acceptance concerns below are retained as measured
limitations, not an additional all-history-green requirement. Final code and
whole-goal completion are determined by the parent Agent's full audit.

The six-request feedback-revision workflow is terminal **failed**, because its
final reviewer exhausted the fixed transport allocation after retrieval. Its
initial reviewer actually rejected both independent source-scope concerns, and
its completed writer produced a 241-word correction under the unchanged task,
35-pattern, 300-word and six-document source gates. It adds the documented
interrupted-model recovery condition and removes the unsupported Telemetry
impossibility assertion. This is a real bounded correction result, not a claim
that the entire workflow passed. Assessment of that exact completed draft
separately passed all six citation and corpus labels with twelve requests and
zero errors. The independent selected-Skills-chunk precision caveat remains,
alongside the same-model judging limitation. The original V8 result and all
earlier native failures remain unchanged. No new campaign is justified merely
to make history green.

## Observed results

| Saved native campaign under `target/` | Terminal result | What it establishes |
| --- | --- | --- |
| `aliyun-grounded-long-thinking-off-20261128-1` | Passed; 26 requests | Twelve completed serial turns, exact six-turn state across a real host restart, original reference retained, requested source identities present, original word limits, compression exercised, unchanged terminal reentry: seven structural/lifecycle checks. |
| `aliyun-scoped-controls-thinking-off-20261128-1` | Passed; 56 requests | On 28 visible controls, citation agreement 27/28, corpus agreement 25/28, all controls labelled as contradictions rejected, no execution errors. These exceed the declared 85% agreement gates. |
| `aliyun-grounded-answer-assessment-thinking-off-20261128-1` | Passed; 24 requests | Both isolated tool-less judge contexts labelled all twelve actual Findings supported. This is same-model, supplied-source-relative judgment, not a proof of factual correctness or complete task satisfaction. |
| `aliyun-collaboration-v11-semantic-20261128-2` | **Failed**; 6 requests | All three original collaboration finals were assessed. Citation support 2/3, corpus support 3/3, but the required all-final citation gate failed. Independent review also identifies unsupported inferences missed by the judge. |
| `aliyun-long-recovery-thinking-off-20261128-1` | **Failed**; 4 requests | A distinct new turn completed in the same session with six governing sources and preserved old checkpoints, but it replaced the original first-turn reference. |

These five completed campaigns account for 116 known native requests, separate
from the older default-thinking assessment. The subscription price, provider
remaining allowance and provider billing multipliers are not verified. Configured
zero incremental token cost does not mean a free subscription or zero unknown
usage. Do not sum overlapping run/parent budget scopes.

The successful fresh long task declared a 65,536-byte context budget from the
start and explicitly used `enable_thinking:false`. Earlier failures used other
context/reasoning conditions. This establishes a working bounded example; it is
not a controlled performance improvement or evidence of a day-long SLO.

## Later bounded recovery passed; original failures remain

The original failed campaign is
`aliyun-grounded-long-session-20261128-1/campaign`; its final turn stopped with
`ContextLimit`. The explicitly acknowledged first larger-context recovery,
`aliyun-grounded-long-recovery-20261128-1/campaign`, stopped with `Deadline`.
The second recovery created a distinct turn after an explicit acknowledgement;
it did not resume either failed run under changed bindings.

The second recovery passed 10 of 11 recorded checks. It returned
`Task context: reference review-release-stage12; source content grants no authority.`
instead of the original `TLM-REVIEW-F604`. Its `original_reference_retained` check
is correctly false, even though the new run's runtime stop reason is `completed`.
Runtime completion cannot substitute for satisfying the user request.

The acknowledged Deadline run
`session-ead8e80f13081b249b1ae232c161a3d586c22330d95c71e5e0bf3b1da72c6dc4`
retains the same before/after state and budget: two uncertain generation attempts,
zero in-flight attempts and the original zero incremental micro-cost ceiling.
Two uncertain attempts are not zero uncertainty. The existing all-previous-run
file comparison includes `*.budget.json` as well as checkpoint JSON, and passed.
The original ContextLimit run and the first failed recovery remain failures.

The later `aliyun-long-recovery-bound-sources-20261128-1/campaign` is now a
completed passing **new turn**, with two native requests and all twelve checks
passing. Its explicit source contract binds the six original documents and its
context policy preserves the actual first User message; it retains the original
reference without injecting an expected replacement. It records old checkpoint
and budget-file hashes, verifies all six source identities, exercises compression
and unchanged terminal reentry, and preserves every previous run file. The
original ContextLimit, Deadline and reference-loss outcomes are not changed.

`aliyun-recovered-final-semantic-20261128-1/campaign` assesses that actual final
Findings text in two isolated tool-less contexts: both labels are supported,
all declared gates pass, and two native requests are recorded. That is one
answer in two contexts, not two independent answers or external truth. The
phrase “External effects are not participants; inspect run store on uncertainty”
still warrants scope review: the documented memory post-generation CAS/capacity
remedy must not be generalized to every external-effect uncertainty merely
because the same-model judges approved it. The latest lifecycle/reference pass
therefore does not erase the limitations of same-model semantic assessment.

## Semantic assessment is useful but insufficient

Each citation judge sees only the attached quote list, and each corpus judge
sees the full supplied corpus in a separate tool-less context. There is no
sibling dependency or shared corpus in the citation-only request. Expected
control labels are excluded from model requests. In generated-answer mode,
there are no supplied gold labels; exact original Findings offsets, hashes and
selected chunk IDs bind the tested text. Only the exact fixed task-context
line is excluded, with no technical assertions moved into that line.

This isolation prevented one false citation pass: collaboration final 1 says
“The archived comment urging silent retries is untrusted and is ignored,” but
attaches only approval, recovery and budget sources. The injection document is
available to the corpus judge but not cited. The citation judge correctly
returned `insufficient`; a true corpus statement still needs its attached
evidence. The final must not be promoted to a passing citation result.

The other two collaboration finals reveal a judge false positive:

* Attached budget evidence says “Generation budgets exclude embedding and
  tool-service charges.”
* Final 2 continues “so generation and embedding usage are metered and limited
  separately.”
* Final 3 continues “so embedding spend is tracked separately from generation
  spend.”

Exclusion from a ledger does not establish that another ledger, counter or limit
exists. Both isolated judges nevertheless labelled these claims supported and
their reasons explicitly made that invalid inference. On this Agent review,
the extra implementation assertions are insufficiently supported by the
attached fixture **and by its full four-document corpus**. A recommendation
such as “configure a separate embedding limit” would be a different claim; the
actual finals assert existing metering/tracking. Do not replace their wording
or labels retrospectively.

The real framework does separately implement a host embedding spend ledger and
local admission controls in `src/runtime/app/retrieval.rs`. That code was not
in this collaboration evidence set and cannot retroactively establish its
source-relative entailment. This is an evidence/answer-quality problem rather
than proof that the product lacks an embedding ledger.

Even the fresh twelve-turn pass needs this distinction. Stage 1 describes
“recovery claims are prioritized” as “One operator action”; the source describes
scheduler behavior, not an operator command or procedure. The answer then
correctly states that no manual reconciliation procedure is documented. This
is a task-satisfaction gap despite source-consistent scheduler facts. Stage 10
compresses “mounted role names cannot collide with host roles or other mounts”
into wording about mount prefixes not colliding with host roles; that wording
also deserves precision review. The automated twelve-answer pass therefore
cannot establish flawless operator guidance.

The control errors also matter: the corpus judge treats missing delivery
guarantees as direct contradictions for some unestablished timing/alternative
guarantee claims. Passing the aggregate 85% gate does not erase these modal
scope errors. The controls are visible and from the same configured model
family; model weights and independent external accuracy are not attested.

## Explicit operator stop is a separate outcome

The older `aliyun-grounded-answer-assessment-20261128-1` default-thinking
assessment had earlier native contract/deadline failures. After the pause, the
parent Agent stopped only its owned Taskloom host to prevent further dispatch,
retaining the proxy's already-admitted request observations and durable
reservations. `target/paused-old-native-assessment-20261128.json` records that
operator stop and 20 upstream slots reserved before it. The host's final
partial report includes subsequent connection-refused errors and missing
cases; those are operator-interrupted evidence, not spontaneous model failure
or a complete semantic score. Known run-snapshot usage and upstream slots may
differ because unknown attempts and already-dispatched responses remain
separate. No request was replayed by this review.

## Limitations and goal-closure interpretation

Retain the collaboration and earlier same-session recovery failures alongside
the later bounded recovery pass. Per-claim source support, operator guidance
and judge false positives remain documented limitations, including the
exclusion-versus-implemented-metering inference. These findings do not impose a
new requirement that every LLM output or historical run must pass before the
evaluation capability counts as developed. Any further model acceptance needs
a separately authorized finite allocation. The existing 12-turn example does
not establish production-duration reliability, independent truth or a comparison
win over other frameworks.

## V7 scope hallucination and prospective V8 method

The real `aliyun-repository-audit-v7-20261128-1` task structurally passed with
22 native requests and a 276-word original final answer. All six original goal
topics appear. Its subsequent six-topic semantic assessment
`aliyun-repository-audit-v7-semantic-20261128-1` **failed**: citation support is
5/6, corpus support is 6/6, and the all-final citation gate is false. The Memory
sentence “This remedy applies only to memory admission failures following
generation” adds a global exclusivity assertion that its cited excerpt does
not establish. This is a model scope hallucination, not a missing source hash,
an escaped-regex defect or a reason to rewrite the saved result. The same-model
corpus pass does not override the citation failure.

V8 rebuilds the role instructions around clause-to-evidence checking and
same-sentence source conditions plus actions, rather than adding more negative
cross-scenario disclaimers to the inherited prompts. It supplies no fixed answer.
Original goals, sources, all 35 structural patterns, the hard 300-word ceiling
and numerical gates remain unchanged. A separate offline helper,
`examples/benchmarks/native_scope_precision.py`, locates applicability-exclusion
expressions in the exact original writer text and records offsets, hashes and
source leads for independent review. It does not alter text or issue a support
verdict. It flags even a potentially supported exclusion for review; no match
also proves nothing about entailment. Its saved V7 screen identifies the real
“applies only” span.

The subsequent actual V8 task completed with 18 native requests and a 237-word
original final synthesis; its twelve-request semantic assessment passed all six
topic citation and corpus labels. This preserves a working bounded task and a
native semantic pass, not independent source-support acceptance. The offline
scope screen found no applicability-expression match, yet independent session
review retained two concerns: Memory broadly calls other failure recovery an
excerpt-scoped evidence gap although that excerpt also describes interrupted
generation recovery; Telemetry infers that live reads cannot verify missing
historical children from the lack of a simultaneous cross-record snapshot.
The latter is stronger than the documented atomicity limitation, and the source
explicitly counts absent children. Neither concern is erased by same-model
judgments or by the screen's lack of a lexical match.

V8 selects nine chunks from the same six pinned governing excerpts: one queue,
two quota, two memory, two skills, one telemetry and one retrieval chunk. Each
chunk's UTF-8 byte interval matches the frozen document body. The original goal
says “Use exactly these six governing documents”; the policy requires six
documents and at least six chunks, with no six-chunk maximum. Nine chunks do not
violate that requirement, but the result must not be described as exactly six
chunks. The six complete contiguous assessment spans reconstruct the full
original synthesis without removing the disputed clauses.

A separate explicit feedback-revision task was executed, rather than another
unconditioned sample. It carries the exact original V8 draft and both concrete
source-review concerns as ordinary task data, supplies no replacement answer,
and requires initial native review, a fresh-retrieving correction writer and
final native review. It records zero new analysts and makes no new parallelism
claim; the old six-analyst result stays intact. The original goal, writer source
policy, 35 patterns and hard 300-word ceiling remain unchanged. Its transport
allocation was six requests with no extension on exhaustion. The initial
reviewer used two requests, the writer used three including a bounded repair,
and the final reviewer used one before its next dispatch was refused locally.
Initial rejection and completed writer correction are observed, but the final
native review is incomplete and the workflow remains failed. The exact draft
has a separate six-topic assessment with unchanged 100% citation/corpus gates;
that protocol's outcome is not a universal correctness requirement. Its final
assessment is now terminal **passed**, with 6/6 citation and 6/6 corpus labels,
zero execution errors and twelve native requests. This measures the actual
241-word completed draft, not a successful final review or workflow. The
workflow remains failed exactly as recorded.

The correction writer selected seven chunks from six documents. Its Skills
paragraph still states the uncertain-effect retry prohibition while its one
selected Skills chunk ends mid-sentence before that final object. The complete
corpus contains the rule, but the independent draft assessment binds only the
writer's actual selected chunk; it does not add the omitted continuation to
improve citation support. Both same-model judges nevertheless labelled all six
topic spans supported. This independent selection-precision caveat is retained
as a possible same-model false positive; the native pass does not override it.

The final finite allocation was fully consumed: V8 task 18, V8 semantic 12,
explicit correction task 6, and corrected-draft semantic 12, totaling **48
upstream HTTP reservations** verified from the four request journals. There is
no further request or new task round. This is a bounded, source-reviewed repair
example with explicit limits, not evidence of universally correct outputs.

Offline checks after the harness identity/hash repair:
`python tests/native_long_session.py` (4 passed),
`python tests/native_claim_support.py` (4 passed), and Python compilation of the
three owned native scripts passed. These are harness checks, not model-quality
evidence.

## Saved report identities

SHA-256 values for `campaign/report.json`, captured during this review:

| Campaign | SHA-256 |
| --- | --- |
| Fresh twelve turns | `7c740e4e3fec97572f13219b725d85ca12de27ccbf60b4f96fdf481d649307f1` |
| Scoped controls | `cd47f5b6827aca7293e86cee8f5a7b8c6cf442bf7c24bb9d0a4080142d25c346` |
| Twelve generated Findings | `1eb11319bce4755ba48d73cae90448a84735d21e192b7b6350a10de554458835` |
| Three collaboration finals | `d90059aaa2caf97ccb119c3504280e507b7de652e9a135f1bd205e9419ff0474` |
| Second same-session recovery | `a34367a3a36e9dcc735dabb18461e10bf353b00bf4eba095d8130c7192ed8993` |

These references identify local saved evidence. A published acceptance receipt
must preserve the relevant sanitized records rather than relying on ignored
`target/` files being present in a source checkout.

Latest `campaign/report.json` identities, superseding no historical result:

| Campaign | SHA-256 |
| --- | --- |
| Bound-sources same-session recovery | `2def025901e6a48b5cf2c9c5c0efa6714ebb56121df6ea6c6a54cb52f212d0e0` |
| Actual recovered final semantic assessment | `fe11a0a38a1f89185aac347fbeb89b753856bd6746d16189f14ba78756e0e305` |
| Actual V8 six-analyst task | `f9ac1979cafafb06bfa0f9cecb74d58fbe7bd96128da7d37ae37d9e9b50573cb` |
| Actual V8 final semantic assessment | `2e81d81738e42d203e1f2348588ee0946dcc463ac40e8498518645dafdf3b3a5` |
| Explicit correction workflow, failed final review allocation | `141c3cfaa9365da674a0c87884f4cd65fc6feed021be69a603587012adbeef88` |
| Completed correction draft semantic assessment | `0c6a1b8a0bd689b95ff8c7bcf65dbc8718c5acf8c60e16f850e2fd526d74702d` |
