# Native prepaid API evaluation

Local generation weights are optional. An explicitly configured remote provider
can perform actual native tool/workflow evaluation. This increment uses the
owner's selected Alibaba token-plan route and `qwen3.8-max`. The response's model
identifier is retained, but underlying weights, scheduling and invoice are not
verified. No local generation model is downloaded for these remote campaigns.

Credentials are named by `provider.key_env`, never placed in source, provider
URLs or evidence. On the acceptance host the key is Windows-user DPAPI encrypted
in an ignored local directory and loaded only into owned processes.

## Subscription accounting

The owner declared a monthly subscription of 45,000 calls. This is not evidence
of remaining allowance, subscription price, provider billing multipliers or
overage rules. The zero-valued token table describes configured additional cash
charges within the prepaid route. It does **not** assert free public tokens,
zero subscription cost or a verified invoice. Embedding/tool charges are separate.

`examples/benchmarks/native_subscription.py` puts a finite HTTP-attempt allocation
before the original native harness. Its loopback transport forwards the original
request bytes to the exact HTTPS token-plan URL, replacing the disposable local
credential with the selected remote credential. It never rewrites prompts,
creates answers, retries upstream, redirects credentials, changes model or falls
back to another billing endpoint. Child hosts inherit only the disposable local
token; the remote key is removed from their environment.

Each attempt reserves a slot in an fsynced create-only journal **before** network
dispatch. Concurrent callers cannot exceed the allocation. Errors and unknown
replies retain their slots. Exhaustion refuses further upstream requests. A
crashed output directory cannot be reopened to reset the allocation. This is a
local campaign guard, not the provider's monthly quota or a general distributed
billing service. Missing observations remain unknown.

Bound transport metadata makes the original harnesses identify remote native
inference correctly. Raw requests, responses, model identifiers, usage and
durations are separate from Taskloom's token-cost journal. Concurrent development
campaigns are not controlled latency comparisons.

## Reproduce

Build `taskloom` and `benchmark_citations` with the `app` feature. Prepare an
ordinary model configuration containing this provider/model, then set the named
credential through your host secret manager:

```json
{
  "provider": {
    "type": "openai",
    "base_url": "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
    "key_env": "TASKLOOM_ALIYUN_API_KEY",
    "namespace": "YOUR-EXPLICIT-SUBSCRIPTION-NAMESPACE"
  },
  "model": "qwen3.8-max",
  "output_tokens": 2048
}
```

Use an explicitly authorized prepaid allowance and a fresh output directory.
The wrapper constructs the incremental token meter and request fence itself:

```powershell
python examples/benchmarks/native_subscription.py target/NEW-claims --config YOUR-MODEL.json --monthly-calls 45000 --max-requests 72 --campaign claim-support --separate-evidence --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
python examples/benchmarks/native_subscription.py target/NEW-collaboration --config YOUR-MODEL.json --monthly-calls 45000 --max-requests 80 --campaign collaboration --suite examples/benchmarks/collaboration-v9.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
python examples/benchmarks/native_subscription.py target/NEW-history --config YOUR-MODEL.json --monthly-calls 45000 --max-requests 48 --campaign long-session --selected-sources --taskloom target/debug/taskloom.exe
python tests/native_subscription.py
```

The three initial allocations total 200 HTTP attempts. A later campaign may use
the remaining **aggregate** allowance after those three have stopped; unused
allocation is not an actual request. Provider quota consumption may differ.

## Quality and migration boundaries

The initial connection smoke used five requests: text, native function selection
and result return, then two actual Taskloom generations with executed knowledge
retrieval, exact-source verification and unchanged terminal reentry. Two setup
failures sent no upstream requests: static pricing review and a missing document
source field. Two wrong smoke check paths were corrected from saved state only;
no model responses were regenerated.

V9 collaboration exposed a strict handoff failure: a reviewer wrapped JSON in
Markdown fences, which the original JSON role refused. V11 retains the goals,
corpus, analyst/writer contracts and topic gates but explicitly opts planner and
reviewer into existing durable JSON-schema repair, at most once per role. It
never strips fences or rewrites transcripts. Changed contracts apply to new
runs; old failures remain failures. V10 is a separate prepared source-selection
example, not an accepted native campaign.

The long-session task has twelve turns over six pinned repository documentation
excerpts, first-turn reference retention, host restart after turn six, fresh
source requirements, context compression and terminal reentry. Bibliography
mode labels full quotations as host-generated. It is not per-claim entailment
or day-long workload acceptance.

The first remote history campaign passed all six original lifecycle/provenance
checks, but a later offline audit found four original answers over their word
limits (166/150, 155/150, 162/150 and 314/300). Its original report is retained;
it does not establish full task-quality success. The helper now measures original
answer words separately from labelled host rendering. Optional `--strict-length`
freezes an additional 150-word original-answer JSON Schema for every turn,
including final consolidation (stricter than the user's 300-word maximum).
Repairs use the existing metered loop; no answer is truncated or reference
injected. Changed prompts/contracts and any subsequent result remain separate.

Semantic controls judge attached evidence and full corpus in independently
isolated tool-less contexts, without expected labels in prompts. The visible
development set and same configured model remain limitations even when all
labels agree. It is no blind independent truth oracle or framework leadership
evidence. Existing DeerFlow/agentUniverse/LangGraph trials used different local
models/contracts; these remote results do not create a matched ranking.

No storage migration, new default feature or mandatory model dependency is
introduced by this transport. Credentials, weights, installed libraries and raw
private state remain outside source distribution. Broad P0 acceptance still
requires the remaining quality and retrieval gates.

## Later finite results and remaining failures

V11's original HTTP harness records 2/3: one safe observation failed with a
Windows connection timeout, while its workflow subsequently completed durably.
`native_collaboration_cold_audit.py` starts a separate real host only after the
old loopback model transport has stopped. Without a remote credential it checks
all three actual outputs, source/topic/role/rework gates and terminal reentry.
All three pass, with every saved runtime/workflow JSON file unchanged. The
original 2/3 report remains unchanged; late cold completion is a separate result.
No new model answer is generated to replace the missing HTTP observation.

The observation helper now bounds retries for safe GET requests only; POST
submission/control uncertainty is never replayed. Offline controls cover the
retry boundary, denials and exhausted observation attempts. Campaigns also
retain cold state/budget files because an HTTP failure can miss late usage.

The strict history campaign passed all seven declared runtime, exact-evidence,
length and restoration checks, using 27 native requests. Full semantic task
quality remains a separate question: its final compressed text recommends
at-least-once provider calls and manual uncertain-attempt reconciliation, neither
established by those excerpts. This does not relax uncertain-effect recovery.

`claim-support-generated-history-v1.json` records all twelve actual technical
answer bodies after their exact reference/source-heading prefix, original text
hashes/character offsets and selected source chunk IDs. A separate live-owner
contradiction is a control, not generated content. Reference labels were supplied
by this development session after generation, so they are explicitly non-blind
session-assisted judgments, not human/independent truth. Native citation-only
and full-corpus judgments are separate evidence. Agreement with an insufficient
label measures evaluator agreement, not successful generated-answer quality.
The first preparation attempt refused a two-chunk source before any upstream
request; the repaired preparer binds every original selected chunk exactly.

See `acceptance/p0-native-subscription-20261128.json` for the complete bounded
request accounting, failed probes, exact conditions and subsequent measurements.

The completed development batch reserved 177 actual upstream HTTP attempts,
within its aggregate 200-attempt limit; the preceding connection smoke used
five more. The last actual-answer judgment failed both agreement gates, so no
whole-goal success or main publication follows these structural passes. Reported
completion usage may include reasoning above the nominal client `max_tokens`.
The request fence, configured incremental token meter and unknown invoice remain
different accounting boundaries.
