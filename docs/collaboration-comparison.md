# Native collaboration and qualified framework comparison

These visible local trials measure actual model and framework decisions. Failed
quality gates remain failures even when a workflow or PEER loop finishes. No
leadership, latency ranking, blind evaluation or general factual benchmark is
claimed. P0's broader evaluation and all P2 service/UI requirements remain open.

## Reproduce the larger local model trial

The existing standard model profile is unchanged. The explicit `generation-4b`
profile pins public Qwen3-4B safetensors/config/tokenizer files from the official
Qwen ModelScope repository, rejects remote Python/pickle artifacts, verifies
each size/SHA-256 and caps downloads at 10 GiB. Use a separate ignored root:

```powershell
python examples/local_models/download.py target/local-models-4b --profile generation-4b --prepare
python examples/local_models/download.py target/local-models-4b --profile generation-4b
```

The acceptance lock is `acceptance/native-models-4b-20261128.lock.json`:
8,060,908,199 bytes, including three weight shards. The existing standard profile
and this profile both validated; choosing the wrong profile refuses the lock
without rewriting it. The acceptance server used official Transformers 5.20.2,
PyTorch 2.14.1+cu126, CUDA bfloat16, reasoning off, seed 42, on the same RTX 3080 Ti
Laptop 16 GiB machine as previous trials. Start an explicitly owned loopback API:

```powershell
$env:HF_HUB_OFFLINE='1'
$env:HF_HUB_DISABLE_TELEMETRY='1'
transformers serve E:/projects/taskloom/target/local-models-4b/Qwen3-4B --host 127.0.0.1 --port 18082 --device cuda:0 --dtype bfloat16 --reasoning off --default-seed 42
```

Configure its exact model path, loopback URL, a host credential environment name
(`TASKLOOM_LOCAL_API_KEY` in acceptance), output allowance 1,024 tokens, and
matching explicit USD-zero input/output API prices. The campaign admits a
one-micro-USD cap; each durable role has a zero-fee cap. Hardware/electricity and
this chat's usage are unmetered and excluded. No paid remote generation,
embedding or external tool service is called by this collaboration suite.

```powershell
cargo build --locked --all-features --bin taskloom --example benchmark_citations
python examples/benchmarks/native_collaboration.py target/native-collaboration-v5 --suite examples/benchmarks/collaboration-v5.json --config local-4b.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
```

V4 retains an actual Qwen3-1.7B development run with three failures. Its guidance
incorrectly mentioned a `text` argument and `max_bytes` for the model tool, which
actually accepts `query` and optional `top_k`. V5 corrects this and allows the
submitted-draft reviewer to run concurrently with both analysts. It uses concise
role instructions and an explicit defective submitted draft, with actual model
review before writing. Those changes and the larger model prevent interpreting
V4→V5 as an improvement under identical conditions.

## Native results and independent source-support labels

Qwen3-4B V5 passed **1/3**:

| Case | Observed outcome | Why it counts this way |
|---|---|---|
| Parallel policy brief | Passed | Two overlapping analysts; final exact sources/quotes and requested topics; final model approval; terminal reentry unchanged. |
| PEER release check | Failed | One analyst exhausted its bounded locator-marker repairs; no final report. |
| Submitted initial-draft audit | Failed | The workflow finished, but the initial reviewer approved three contradicted seeded claims; the final omitted the required recovery source and reservation-before-dispatch explanation. |

The three cases reported 27 native requests, 32,791 input/2,786 output tokens,
USD-zero API charges and 131,233 ms of summed case time. All three had overlapping
analysts. These are observations on a small visible policy corpus, not a general
throughput or effectiveness estimate. Raw prompts, model outputs, tool results,
failures, output repairs and saved task state are retained in
`acceptance/p0-collaboration-20261128.json`.

Exact provenance and factual support are separate. A non-blind **current Codex
session-assisted** review split both existing final reports into nine atomic
claims. It labelled seven as supported by the attached citation and two as
insufficiently supported by that citation. All nine agreed with the supplied
corpus. In the defective audit result, two true recovery claims were attached to
the approval quote. The quote itself was authentic, so exact validation passed;
the source did not support those attached recovery claims. Missing reservation
timing is recorded as an omission, not silently excluded from task success.

These supplied annotations are separate from native API measurements. The
reviewer saw the task, sources and expectations; there was no blind human review
or metered native judge call. Corpus-relative agreement does not establish
independent real-world truth. The seeded initial review's false acceptance is
one observed failure, not a reviewer reliability rate.

```powershell
python examples/benchmarks/score_research_review.py target/native-collaboration-v5/report.json docs/acceptance/p0-collaboration-v5-source-review-20261128.json
```

The scorer verifies suite/output fingerprints, exact claim ranges and declared
citation identities, then aggregates **supplied labels**. It does not prove
entailment. The actual changed-output negative probe refused stale annotations.
Missing final reports stay failed and unscored for claim precision.
Ranges use Python Unicode-character offsets into final prose, independently of
the UTF-8-byte source offsets used by exact retrieval locators.

## Actual agentUniverse PEER adapter

The upstream source commit is
`f69b130bacaa784289ec8dc2a9798d122cd8fe5a` (distribution 0.0.19). Its default
dependency resolution initially failed around the grpcio 1.63.0 pin and newer
OTLP exporters. The compatible historical OTLP family in
`examples/benchmarks/agentuniverse-constraints.txt` installs without modifying
framework code or declared dependencies; `pip check` passed. Python 3.12.16 was
installed in a project-local environment, not over the user's Python.

```powershell
git clone https://github.com/agentuniverse-ai/agentUniverse.git target/competitors/agentUniverse
git -C target/competitors/agentUniverse checkout f69b130bacaa784289ec8dc2a9798d122cd8fe5a
python3.12 -m venv target/competitors/agentuniverse-venv
target/competitors/agentuniverse-venv/Scripts/python.exe -m pip install -c examples/benchmarks/agentuniverse-constraints.txt ./target/competitors/agentUniverse
target/competitors/agentuniverse-venv/Scripts/python.exe -m pip check
target/competitors/agentuniverse-venv/Scripts/python.exe examples/benchmarks/agentuniverse_compare.py target/native-collaboration-v5 target/agentuniverse-v5 --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe --source target/competitors/agentUniverse
```

The adapter invokes the real `PeerWorkPattern.invoke` threshold/retry loop and
the real `ExecutingAgentTemplate._execute_tasks` thread pool. Installed control
source hashes matched the pinned checkout. Native role subclasses adapt only
model/knowledge I/O and protocol field shapes; no scripted model answer or
replacement PEER loop is used. The model/task/role system prompts/corpus/tool
schema/output allowance, per-role step/tool bounds and zero-priced spending
rules match the reference. Native outputs determine the plan, tool calls,
answers and reviewer decisions. The boolean verdict maps to framework scores
0/100; threshold 60, `retry_count=2`, `jump_step=expressing` permit one rewrite.

The completed actual trial scored **0/1** on the shared PEER release task. Both
analysts overlapped and all five native roles completed. The reviewer approved
the draft, but it omitted reservation timing and the required recovery source.
Taskloom also failed this corresponding V5 PEER task, earlier in analyst output
validation. Different failure stages do not establish a better framework.

This is a qualified orchestration comparison. The shared read-only knowledge
service uses Taskloom retrieval, so it does not compare framework-native search.
The role adapter is not agentUniverse's default provider/ReAct/memory stack. Its
executor delays each submission by one second; absent optional task fields,
repair feedback and HTTP retry behavior also differ from Taskloom. Raw traffic
and conditions are retained; identical model conditioning, speed or leadership
are not inferred. This adapter does not implement the two arbitrary-DAG cases,
which remain visibly **not comparable**, rather than receiving zeroes or passes.
Process-death durability, approvals, uncertain effects and long tasks are not
accepted by this adapter.

The first adapter probe had a harness-only missing `max_bytes` parameter on the
operator search API (which differs from the model tool). It failed with HTTP
422 and is excluded as comparison evidence; the corrected probe includes the
same 24 KiB result bound as `RagTool`. Both probes and the original dependency
resolver failure remain in the acceptance record. The Rust verifier's added
`{policy,run}` input reuses SDK final-output validation from observed dispatch
events; its original `{report,evidence}` citation input remains supported. It
does not attest arbitrary imported journals or supply missing tool observations.
The completed native run, removal-of-dispatch-events, original canonical-report
input and mixed-input refusal probes passed. Thirty citation/output regression
tests, the Rust 1.86 example check, strict example Clippy, Python compilation and
source-only package listing also passed. No runtime API, default feature or
production permission/budget contract changed in this evaluation increment.

## Actual DeerFlow natural-research adapter

DeerFlow's pinned source `acafc36dfe654af9f10c35923a51f05efa64cd7f`
(2.3.0-dev) was installed in an isolated Python 3.12 environment; `pip check`
passed. The adapter executes its real `DeerFlowClient.stream` and native
lead/subagent machinery, checking the installed client hash against the
checkout. Actual upstream RBAC offers only read-only `knowledge_search` and
the framework's `task` delegation tool; skills, memory, title, summarization,
task continuity, unrelated tools and external tracing are disabled. An owned
worker runs in a fresh cwd/home/extensions/skills environment. The final probe
also disables dotenv discovery before importing the framework. Earlier two
exploratory probes did not set that guard and remain separately qualified.

The corresponding V5 parallel-policy-brief task uses the same Qwen3-4B native
model, task goal, corpus, 1024-token output allowance and explicit zero API
prices/one-micro-USD cap. A bounded loopback recorder permits at most 64 native
requests (the reference's four roles times sixteen), two concurrent subagents,
three delegations, sixteen turns per subagent and a 300-second campaign window.
Reservations are recorded before dispatch; unknown responses remain visible.
The local dummy key is named `TASKLOOM_LOCAL_API_KEY`. Hardware, electricity
and this chat are not metered. The adapter never calls paid remote providers.

The final guarded probe scored **0/1** on the declared collaboration gate. It
completed one native knowledge call and three native model requests, including
one bounded format repair. Final SDK exact-locator/citation/source-coverage
validation passed, but no delegated analyst or independent reviewer was
observed. The model chose direct research despite `task` being advertised and
the host requesting delegation. This is a failure of this trial, not proof
that DeerFlow cannot delegate. Actual usage was 14,872 input / 412 output
tokens, 33,844 ms campaign wall time, at declared zero API fee. The first probe
failed ordinal citation format; the second passed format repair but also lacked
delegation. All failures and raw native traffic are retained in
`acceptance/p0-deerflow-20261128.json`.

This adapter uses DeerFlow's native system prompts, tools, natural delegation
and provider wrappers; it is not identical conditioning or a speed/leadership
ranking. Taskloom's corresponding fixed DAG passed; this alone establishes no
overall framework advantage. Shared knowledge uses Taskloom lexical retrieval,
so framework-native retrieval quality is not compared. Two bounded final-format
repairs disable further delegation. Verification uses actual recorded native
tool calls through an SDK-validator bridge, not an invented Taskloom run or
billing ledger; zero placeholder bridge usage is excluded from cost evidence.
Exact quote matching is separate from claim support/factual truth. No blind
factual or semantic-quality verdict is asserted. Exact staged PEER and submitted
draft-review lifecycles remain visibly **not comparable** in this adapter.

```powershell
git clone https://github.com/bytedance/deer-flow.git target/competitors/deer-flow
git -C target/competitors/deer-flow checkout acafc36dfe654af9f10c35923a51f05efa64cd7f
python3.12 -m venv target/competitors/deerflow-venv
target/competitors/deerflow-venv/Scripts/python.exe -m pip install ./target/competitors/deer-flow/backend/packages/extension-api ./target/competitors/deer-flow/backend/packages/harness
target/competitors/deerflow-venv/Scripts/python.exe -m pip check
target/competitors/deerflow-venv/Scripts/python.exe examples/benchmarks/deerflow_compare.py target/native-collaboration-v5 target/deerflow-v5 --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe --source target/competitors/deer-flow
```

The reference campaign must be a completed V5 native campaign with its explicit
loopback generation configuration; start that exact native model first. Worker
events, responses, knowledge, prompts, control-source hashes, installed versions
and validation attempts are saved below the fresh output directory. Vendored
frameworks, dependencies, weights and binaries stay under ignored `target/`.

## Actual LangGraph complex-agent comparison

The earlier basic LangGraph comparison remains 5/12 against Taskloom 5/12 with
Qwen3-1.7B. The later V5 complex comparison uses actual LangGraph **1.2.24**,
Python 3.14.6, the same Qwen3-4B model, all three research goals, corpus, role
prompts, 1024-token output allowance and explicit zero API prices/one-micro-USD
campaign cap. Installed StateGraph and Pregel source hashes matched their
distribution RECORD. The isolated dependency check passed; JSON-schema 4.26.0
and the retained environment constraints are explicit.

`langgraph_collaboration.py` uses real StateGraph parallel branches/join edges,
dynamic `Send` analyst fan-out, and conditional review/rewrite routing. Shared
NativeRoles supplies actual model/knowledge I/O, bounded output repairs and the
same SDK verifier. A native plan determines PEER tasks; no fixture replaces role
answers or reviewer decisions. Three separately labelled scheduler-only fixture
controls check two parallel callbacks, join completeness, submitted-review
handoff and a bounded rewrite before native execution; they are not model scores.

The actual native trial scored **2/3**. The policy brief passed. PEER completed
its roles but failed reservation-topic and required-source coverage. The
submitted-draft audit observed actual initial reviewer rejection and produced a
corrected writer output that passed the declared topic/provenance/review gate.
Both analyst role lifecycles overlapped in every case; this does not measure GPU
inference parallelism. Thirty received native requests reported 38,978 input /
3,108 output tokens at declared USD 0 API fee, with summed case wall time
130,444 ms. Exact quotes/topic coverage are not independent entailment/factual
scores. Full native messages, requests, outputs, usage and failures are retained
in `acceptance/p0-langgraph-complex-20261128.json`.

| Corresponding V5 task | Taskloom | LangGraph adapter | agentUniverse adapter | DeerFlow adapter |
| --- | --- | --- | --- | --- |
| Parallel policy brief | Pass | Pass | Not comparable: PEER-only adapter | Fail: no delegated analysts/reviewer |
| PEER release checklist | Fail | Fail | Fail | Not comparable: natural-research adapter |
| Submitted-draft audit | Fail | Pass | Not comparable: PEER-only adapter | Not comparable: natural-research adapter |

These are descriptive outcomes under qualified adapters, not an overall ranking.
Different opaque run IDs, wrappers, repair feedback, HTTP retry behavior and
planning/control policies can change model output. The LangGraph adapter uses
explicit model/tool I/O rather than a prebuilt agent; shared retrieval does not
compare framework-native search. No checkpoint saver, approvals or external
effects are configured in this comparison. Adapter omissions are **not** claims
that the upstream frameworks cannot support those tasks. Native process death
and long tasks need separate acceptance. The SDK validation bridge consumes
actual observed tool dispatches rather than imported dependency citations.

```powershell
python -m venv target/competitors/langgraph-venv
target/competitors/langgraph-venv/Scripts/python.exe -m pip install -c examples/benchmarks/langgraph-collaboration-constraints.txt langgraph==1.2.24 jsonschema==4.26.0
target/competitors/langgraph-venv/Scripts/python.exe -m pip check
target/competitors/langgraph-venv/Scripts/python.exe tests/langgraph_collaboration_controls.py
target/competitors/langgraph-venv/Scripts/python.exe examples/benchmarks/langgraph_collaboration.py target/native-collaboration-v5 target/langgraph-v5 --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
```

Start the exact explicitly configured local model first. The reference is a
completed V5 native campaign, not this chat's responses. Prices are declared
zero for this local API only; hardware, electricity and chat usage are excluded.
Broader realistic/long tasks, native failure repair, independent semantic/source
support and retrieval relevance remain open. Actual two-role Taskloom process
death during an approved external write is documented separately in
`workflow-recovery-acceptance.md`; it is not a competitor recovery comparison.
