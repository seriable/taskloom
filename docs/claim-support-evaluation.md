# Native claim support controls and retained collaboration failures

Exact source quotations, requested-topic coverage, attached-citation entailment
and corpus-relative factual agreement are separate measurements. A valid source
hash and matching quote do not prove the neighboring assertion. A model reviewer
can approve a contradiction or omit part of the task. This increment measures
that failure explicitly instead of promoting reviewer approval to truth.

## Fixed controls

`examples/benchmarks/claim-support-controls-v1.json` contains 18 visible
maintainer-authored cases over the same three approval/recovery/budget source
documents used in the earlier native collaboration campaign. Labels are:

| Measurement | Evidence used |
| --- | --- |
| Exact provenance | Actual operator-retrieved chunk identities/body quotes and SDK verification |
| `cited_support` | Only the attached citation quotes |
| `corpus_factuality` | Union of all three supplied corpus source bodies |

For both semantic fields, `supported` means every material assertion follows;
`contradicted` means a material assertion directly conflicts; `insufficient`
means missing/unestablished evidence. Corpus factuality is relative to those
sources, not objective outside-world verification. A supported corpus statement
with no attached quotation has insufficient cited support. A conjunction with
one contradictory part is contradicted. A passage about recovery cannot prove
that recovery completes within two seconds.

Expected labels, case identities and gates never enter native judge prompts.
They remain visible in the published development source, so this is neither
blind nor externally held out. All attached quotes are operator-prepared actual
local knowledge results and checked using `benchmark_citations`; they are not
fabricated model tool calls. The tool-less native judge has no retrieval or
effect permission and sees claim, attached quotes and full corpus as untrusted
User data. No provider result is substituted with a fixture.

The predeclared agreement gates are 85% for each field, all known contradictions
identified and zero execution errors. Errors/missing responses stay in the
denominator. Offline scoring tests verify that error records cannot pass and
that citation and corpus verdicts are counted separately. `rescored.json`
recomputes the same retained native outputs and their confusion matrices without
new model calls; original reports remain unchanged.

## Actual native results

Both campaigns used pinned Qwen3-4B weights, the official Transformers 5.20.2
server, PyTorch 2.14.1+cu126, the same local RTX 3080 Ti Laptop 16 GiB GPU,
reasoning off, seed 42, a 1024-token output ceiling, no tools and explicit local
USD 0 generation prices/caps. `TASKLOOM_LOCAL_API_KEY` names the disposable
loopback provider credential. Hardware/electricity/current chat are unmetered.

| Native rubric | Attached support agreement | Corpus agreement | Known contradictions identified | Result |
| --- | --- | --- | --- | --- |
| v1 | 13/18 = 72.2% | 6/18 = 33.3% | 2/7 | Failed |
| v2 | 11/18 = 61.1% | 11/18 = 61.1% | 2/7 | Failed |

Each campaign made 18 actual calls, all with reported usage. V1 recorded 8,191
input / 2,082 output tokens; v2 recorded its own full usage in the acceptance
receipt. V2 clarifies that unrelated corpus documents do not cancel support and
adds unrelated switch/car examples. The control labels, source facts and gates
stay unchanged. This is prompt development on the observed set, not a same-
conditioning comparison or evidence that the model is a reliable truth judge.
Reasons sometimes directly contradicted their returned labels. Both failures
remain acceptance failures; neither is used to certify generated research.

Reproduce with the verified local server from `native-evaluation.md`:

```powershell
$env:TASKLOOM_LOCAL_API_KEY='taskloom-owned-local-evaluation'
python examples/benchmarks/native_claim_support.py target/NEW-claims --config target/native-generation-4b-config.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe --rubric v1
python tests/native_claim_support.py
```

Use a fresh output directory per campaign. Compile `taskloom` and
`benchmark_citations` with `--features app` first, and declare zero-priced local
model settings explicitly. The helper refuses remote/paid configurations; it
does not assume an unpriced service is free. There is no implicit hosted judge.

## Collaboration evidence and full-task audit

The later trials retain their actual failures and changed conditions:

| Trial | Observed outcome | Qualification |
| --- | --- | --- |
| V5, reasoning on / 4096 tokens | 0/3 | 25 received requests, 21 usages and four uncertain attempts; schema/provenance failure and 300-second deadline. Initial review did identify the defective draft. |
| V6, located contracts / 2048 tokens | 0/3 | Explicit writer required-document contracts; wrong quotes/locator identities remained invalid. |
| V7, canonical contracts / 2048 tokens | Mechanical 2/3; full original-topic audit 1/3 | Same format across model/dependency reports; PEER and submitted-draft outputs omitted reservation timing. |
| V9, cited answer schemas | 0/3 | Missing fields and quote/provenance failures; all requests/outputs retained. |
| Repository documentation audit | 0/1 | Six planned analysts over actual source excerpts; first pair failed, minimum completed-role/request thresholds not met. |
| Single-source cited-section smoke | 1/1 | Six native requests; exact source, answer schema and final review passed. This is a narrow integration acceptance. |

V7's original report still says 2/3. A later audit of every original goal found
the submitted-draft answer also omitted the explicitly requested reservation
timing. `full-topic-audit.json` records that weaker old mechanical gate; V8
source adds the missing criterion without changing the goal or old result.
V9 adds host-selected structural section requirements using
`FinalOutputPolicy::answer_schema`. A heading is still not an entailment test.
The unchanged old V7 writer transcripts all fail the new schema during a separate
offline verifier recheck; old journals and model answers are never rewritten.

No current trial supersedes the existing qualified V5 DeerFlow/agentUniverse/
LangGraph results as a same-conditions ranking. Role prompts, output allowance,
schemas, wrappers and reasoning modes differ. Broader complex/PEER lifecycle,
realistic long/multi-session quality, independent semantic/factual assessment
and general retrieval quality remain open P0 requirements.

The repository audit suites pin full UTF-8/LF source hashes and exact contiguous
character ranges before any host/model dispatch. Missing or changed sources
are refused. The corpus contains only selected visible documentation excerpts,
not the complete application or external reality. It is an actual repository
task with a bounded failed result, not successful long-task acceptance.

See `acceptance/p0-output-contract-20261128.json` for raw results/usage, model
configuration, errors, frozen sources and final source/check identities. Model
weights, executables, private raw states and credentials stay under ignored
`target/`; only source and reviewed receipts are published.

## Instruct model and isolated remote controls

The separately verified Qwen3-4B-Instruct-2507 profile was tested without treating
its changed weights as an identical-conditions improvement. Joint judgments
agreed on 11/18 attached labels and 15/18 corpus labels, with 4/7 contradictions
and one execution/JSON error. Two genuinely isolated tool-less role contexts
improved agreement to 15/18 attached and 16/18 corpus, with 5/7 contradictions
and zero errors, but still failed the predeclared gates. The attached judge
receives only attached quotes; the corpus judge independently receives the
corpus. Opaque workflow IDs, no sibling dependencies and no shared parent
evidence prevent full-corpus leakage into the citation context.

Using the owner's subsequent actual remote `qwen3.8-max` API, the same isolated
eighteen controls agreed on 18/18 attached labels, 18/18 corpus labels and 7/7
contradictions, with zero execution errors. Thirty-six native requests reported
17,760 input and 5,304 output tokens. This passes the declared control gate,
while the visible source labels, same configured model, provider-controlled
weights and limited corpus remain qualifications. It does not validate arbitrary
claims generated by that model or turn its reviews into independent truth.

The prepaid route's request-count allocation, invoice uncertainty, separate
token meter, raw observations and reproduction are documented in
`native-subscription-evaluation.md`. Current remote results do not update the
older local-model competitor comparisons into a matched ranking. Broader
generated-answer/task and retrieval-quality gates remain open.

The later actual history-answer assessment is harder than the eighteen short
controls. All twelve generated technical answer bodies and one separate
contradiction control were judged in isolated contexts. Citation-label agreement
with the explicitly non-blind session-assisted references was 10/13 (76.9%);
corpus-label agreement was 9/13 (69.2%). The contradiction was identified and
there were no execution errors, but both 85% agreement gates failed. No reference
label was changed to match the native result. The actual answer subset received
five `supported` / seven `insufficient` citation judgments and six / six corpus
judgments; these are model judgments, not externally established truth rates.

The final answer's at-least-once/manual-reconciliation recommendations were not
established by its sources. Other disagreements concern source scope, absence
claims and host-authority assertions. They need review rather than automatic
promotion of either this session's references or the native judge to a truth
oracle. Thus the strict long session passes structural checks while its complete
semantic-quality acceptance remains open. The assessment made 26 requests with
39,688 input / 38,903 reported output tokens. Provider completion usage includes
reasoning and can exceed the client's nominal `max_tokens`; no total reasoning
ceiling, public token tariff or extra cash cost is inferred from that parameter.
