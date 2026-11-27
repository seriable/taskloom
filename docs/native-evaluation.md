# Native local model and comparison evaluation

This workflow calls actual downloaded model weights through HTTP. It is separate
from the earlier session-assisted bridge and from deterministic protocol fixtures.
The retained initial results include failures; none establish competitor leadership.
Model weights, virtual environments and raw campaigns stay under ignored `target/`.
Only example source and reviewed evidence summaries belong in the open-source repo.

## Local model setup

The bounded downloader fetches public `Qwen/Qwen3-1.7B` and
`Qwen/Qwen3-Embedding-0.6B` artifacts from their ModelScope publisher repository.
It first records each file's exact revision, SHA-256 and byte count in a create-only
lock, then streams and verifies every artifact. No account, download token, remote
Python code or pickle weights are used. Existing mismatched files are retained and
rejected. These two models total about 5.29 GB; the downloader has a 7 GiB ceiling.
The publisher's model license continues to apply to separately downloaded weights.

```powershell
python examples/local_models/download.py target/local-models --prepare
# Inspect target/local-models/models.lock.json, then download those exact artifacts:
python examples/local_models/download.py target/local-models
```

Install PyTorch for your hardware and Transformers with its serving dependencies
from their official distributions in a separate environment. The initial acceptance
used Python 3.14, PyTorch 2.14.1+cu126, Transformers 5.20.2 and an NVIDIA RTX 3080 Ti
Laptop GPU with 16 GiB VRAM. This is an example environment, not a crate dependency
or a claim that all Python/library/GPU versions are compatible. Rust 1.86 and the
minimal Rust build do not need these packages.

Run generation with the **official Transformers server**, rather than a scripted
answer bridge. Use the exact absolute model folder as both the server model name
and the Taskloom configuration's `model`. With the downloaded files verified:

```powershell
$env:HF_HUB_OFFLINE='1'
$env:HF_HUB_DISABLE_TELEMETRY='1'
transformers serve E:/projects/taskloom/target/local-models/Qwen3-1.7B --host 127.0.0.1 --port 18082 --device cuda:0 --dtype bfloat16 --reasoning off --default-seed 42
```

The command is platform-specific only in its example path; replace it with your
verified folder. Remote-code trust stays off. Thinking is off and generation is
greedy in the supplied Taskloom benchmark. The official server parses native Qwen
tool calls; no human decides individual model replies. Its loopback evaluation
endpoint does not enforce API-key authentication; Taskloom still names a local
dummy key environment for its provider contract. Do not expose this server as a
public or multi-user service.

In another terminal, start the bounded embedding example:

```powershell
$env:TASKLOOM_LOCAL_API_KEY='taskloom-local-evaluation-only'
python examples/local_models/serve_embeddings.py target/local-models --max-requests 128
```

It verifies local artifacts again, binds only to `127.0.0.1:18083`, requires the
named bearer key, permits one inference at a time, bounds body/batch/token counts
and uses last-token pooling with normalized actual model hidden states. It never
silently truncates input, adds a query instruction, retries or downloads more code.
The local key shown here is a disposable example credential, not a remote account
secret. `max-requests` bounds resource use; restart explicitly for a new allocation.

The **API fee is explicitly USD 0**, with zero input/output token prices for these
local services. Hardware/electricity costs and this chat's model usage are not
metered. This is not a claim that hosted Qwen services are free. The examples reject
remote endpoints and paid price tables rather than assuming an unpriced service
costs zero. Paid evaluation requires a separate explicit price/cap admission policy.

## Generation acceptance

Create a configuration using the application defaults, then set these properties:

```json
{
  "provider": {
    "type": "openai",
    "base_url": "http://127.0.0.1:18082/v1",
    "key_env": "TASKLOOM_LOCAL_API_KEY",
    "namespace": "local-qwen3-files-YOUR_VERIFIED_MANIFEST_IDENTITY"
  },
  "model": "E:/projects/taskloom/target/local-models/Qwen3-1.7B",
  "output_tokens": 1024,
  "prices": [{
    "prefix": "E:/projects/taskloom/target/local-models/Qwen3-1.7B",
    "input_per_million": 0,
    "output_per_million": 0
  }]
}
```

Bind `namespace` to the verified model-file manifest and retain it with the campaign.
The initial experiment used the SHA-256 of the sorted per-model file manifest as
that identity. Then run into a fresh directory:

```powershell
$env:TASKLOOM_LOCAL_API_KEY='taskloom-local-evaluation-only'
taskloom --config local-provider.json benchmark examples/benchmarks/acceptance-v1.json target/native-basic-1 1
```

`1` is the total campaign admission cap in micro-USD. The documented zero-price
provider reserves/settles zero API fees. A cap of zero intentionally blocks a
configured-provider campaign in this runner; that behavior is not evidence that
local inference costs a positive API fee. The command exits nonzero on failed
tasks while retaining every case, budget journal and transcript. Do not remove
failed cases from the success-rate denominator or strip Markdown/repair model
JSON after generation to make the outcome checks pass.

The initial actual Qwen3-1.7B run on this environment scored **5/12**, not 12/12.
It performed 30 model decisions and 21 tool attempts in about 53 seconds, with
known zero external API fees. Errors included missing exact JSON keys/markers,
incorrect multi-step arithmetic, an attempted ungranted tool and an upstream
server-parser failure on malformed generated tool JSON. The framework denied
the ungranted tool. This is a small-model integration
measurement, not a general framework quality score or a commercial-model result.
Its duration includes the local inference/server warm state and framework work.

## Learned embedding relevance

```powershell
python examples/benchmarks/native_retrieval.py target/native-retrieval-1 --taskloom target/debug/taskloom.exe --model-root target/local-models
```

The script imports the same 12-document/18-query labelled corpus through the real
authenticated application API, builds a real index, restarts the host in hybrid
mode and retains every search result, error, index identity and embedding ledger.
Default lexical and hybrid modes share the same corpus. The embedding service's
model ID and file identity are recorded. This small visible synthetic dataset is
not a held-out semantic benchmark and does not judge generated claims.

Initial learned-embedding results exposed a real limitation: positive cosine
similarity returned unrelated evidence for both negative queries. Hybrid recall@5
was 1.0, MRR@5 0.96875 and negative-query accuracy 0/2. The positive-only vector
policy remains the compatible default; it is not a calibrated relevance rule.

The host can now configure `retrieval.semantic_min_score` in 0..=1. It filters the
semantic candidates before RRF fusion and is frozen in retrieval identity. It
does not remove lexical candidates, assert claim support, or change permissions.
No index rebuild is needed when changing just this threshold; restart the host
to adopt the changed tool contract. The lower-level equivalent is
`VectorRetriever::with_min_score`.

An exploratory calibration over four positive paraphrases and four unrelated
questions found a gap between the largest unrelated similarity (~0.381) and
the lowest labelled relevant similarity (~0.536). A cutoff of 0.46 and lexical
weight 2 restored MRR on the original small set, but **still returned irrelevant
semantic evidence for the technical nonsense query `unsearchablehash`**. The
negative result is retained: 1/2 negative queries correct, not full acceptance.
This cutoff is an example calibrated to one model/corpus, not a universal default.
Further out-of-domain calibration and answer abstention remain open work.

```powershell
python examples/benchmarks/native_retrieval.py target/native-retrieval-2 --taskloom target/debug/taskloom.exe --model-root target/local-models --semantic-min-score 0.46 --lexical-weight 2
```

## Native multi-agent cases

`collaboration-v1.json` adds a parallel analyst DAG, a planner/executor/writer/
reviewer PEER checklist, and a deliberately incorrect initial-draft audit that
requires reviewer rejection and a rewrite. Roles make actual native model calls
under separate durable child IDs. Source content cannot add tool grants. Parent
events measure overlap only for analyst roles, excluding pure validators.
The runner imports only the generation provider settings; production MCP,
research, sandbox and embedding capabilities from that file are explicitly
withheld. Only case-local knowledge search is offered to these roles.

```powershell
cargo build --locked --features runtime --example benchmark_citations
python examples/benchmarks/native_collaboration.py target/native-collaboration-1 --config local-provider.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
```

The shared Rust verifier applies Taskloom's existing exact quote/chunk/marker
validation to actually observed child retrieval outputs. It does not attest to
arbitrary caller-provided evidence or prove a claim true. A model reviewer's
approval is retained separately and cannot override a failed automatic citation
check. Native planner/role/schema failures and unreached rework remain failures.
Failed workflows do not silently rerun finished children to improve their score.

These cases do not yet cover native tool-error repair, long session tasks or
OS-process crash recovery. The basic suite's deliberate controller interruption
must not be relabelled as process death. The source manifest lists uncovered cases.

## LangGraph comparison adapter

The source adapter uses actual `StateGraph`, native checkpointing and `interrupt`/
`Command` control. It sends canonical OpenAI HTTP requests to the same local model
with the frozen Taskloom campaign's system/user prompts, tool definitions, output
allowance and resource ceilings. It copies each case's corpus and uses the same
Rust retrieval and citation-verification engines, rather than precomputed answers.
Local arithmetic/note execution and fixture approvals/reconciliation are disclosed
harness code. LangSmith/legacy tracing are explicitly disabled before imports.

Install the optional comparison packages in a separate environment (the initial
environment used LangGraph 1.2.24; retain `pip freeze` and the installer report):

```powershell
python -m venv target/competitors/langgraph-venv
target/competitors/langgraph-venv/Scripts/python.exe -m pip install langgraph==1.2.24
target/competitors/langgraph-venv/Scripts/python.exe examples/benchmarks/langgraph_compare.py target/native-basic-1 target/langgraph-basic-1 --config local-provider.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
```

The adapter records raw requests/responses, tool observations, actual effects,
recovery and completed-reentry checks, versions, failures and local API fee boundaries.
Native in-memory checkpoints cover controller interruption here; OS-process recovery
is **not evaluated** by this configuration. Tool IPC overhead, server warmup and
execution order also preclude attributing latency differences to the frameworks.
Do not compare a LangGraph application configured differently to this one as though
the entire library had the same behavior. This is not a LangSmith hosted evaluation.

The retained audited local comparison scored **5/12 for both frameworks**. The
first two adapter trials exposed retry/history-serialization differences and are
retained as preliminary records. The final adapter matches Taskloom's two transient
generation attempts and compact argument serialization. `compare_parity.py` audits
the actual saved native contexts against the comparison's raw HTTP request bodies:
all 30 conditioning steps and all 31 HTTP attempts matched in the final trial,
normalizing only opaque tool-call control IDs. Twenty-nine responses had known
usage; the last task's two failed server-parser attempts retained unknown usage.
Both providers are explicitly zero-fee local services, not an unpriced remote API.
An offline strict JSON/quote recheck retained the same pass/fail outcomes.

```powershell
python examples/benchmarks/compare_parity.py target/native-basic-1 target/langgraph-basic-1 target/parity.json
```

These facts show parity on the declared basic task conditions, not native
multi-agent leadership, a general performance ranking or factual correctness.
The three actual native collaboration tasks observed two overlapping analysts in
each case and real reviewer-driven rewriting in both PEER cases, but **0/3 passed**
their final citation/review requirements. A model reviewer incorrectly approved
missing/invalid citation markers/IDs. The first collaboration harness read PEER's
final field incorrectly; the corrected fresh campaign and original failed record
are retained separately. The citation failure is not hidden by that harness repair.

DeerFlow and agentUniverse adapters/results, cross-framework complex-agent cases,
independent factual/entailment scoring and the remaining lifecycle scenarios are
still required by the active P0–P2 Goal. Neither this document nor a failed local
campaign closes those gates.

## Host output validation and real process interruption (2026-11-28)

`collaboration-v2.json` opts analysts/writers into the host's final-output
contract, with two bounded repairs and only permitted knowledge reads during
repair. Writers must retrieve their own evidence and cite two chunks. This
changes both role contracts and the writer prompt; do not treat v1/v2 as a
same-conditions model-score comparison. See `final-output.md` for configuration,
permissions, budget and migration details.

```powershell
python examples/benchmarks/native_collaboration.py target/native-collaboration-v2 --suite examples/benchmarks/collaboration-v2.json --config local-provider.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
```

Both retained native v2 campaigns scored **0/3**. The second, final campaign
used bounded diagnostics showing actual retrieved IDs. Each observed two
concurrent analysts; 24 total model steps, six tool attempts, 13 rejected
outputs, ten admitted repairs and two validated analyst outputs were recorded.
Incorrect IDs and extra trailing answer content exhausted the repair limit;
the failed analysts prevented writer/reviewer execution. No native v2
reviewer-driven rewrite was reached. Output repairs do not make an unreliable
model reliable by definition, and no pass is fabricated by replacing IDs.

A separate session-assisted, visible-output audit of the two validated analyst
outputs illustrates the distinction: one accurately repeated the recovery
policy, while the other returned the placeholder prose `concise finding with
[[...]] markers` with a valid budget quotation. The latter passes exact
provenance but supplies no substantive finding. This is disclosed current-chat
inspection, not an independent or blind factual score, a new native request,
or competitor evidence. Broader claim-support acceptance remains open.

`native_process_recovery.py` adds an actual OS-process kill, using an owned
Taskloom child and an exclusively observed official Transformers server log.
Run it separately from other native traffic:

```powershell
python examples/benchmarks/native_process_recovery.py target/native-process-recovery --config local-provider.json --taskloom target/debug/taskloom.exe --model-log target/local-generation.log
```

The retained run observed server receipt and a durable `ModelInFlight` cursor,
killed the caller process, archived its crash-time reservation journal and
started a new host on the same state. The journal retained one in-flight
attempt; restart recovery emitted `RecoveredModel` and preserved one uncertain
attempt while admitting a new request under the original explicit zero-price,
zero-micro-USD cap. Generation can repeat after process death; this is not
exactly-once generation. Two native requests were observed in total. The second
used 58 input/1024 output tokens and ended in `ModelError` because generation
was truncated. The first request's usage remains unknown. Terminal reentry
changed neither state nor request count. The control checks passed in 61,775 ms;
task/answer quality did not pass. No tools, approvals, uncertain effects or
multi-agent process death were exercised by this isolated check.

The acceptance manifest `acceptance/p0-output-20261128.json` preserves both
failed campaigns, the process-control result, artifact/source digests and check
boundaries. All local API prices were explicitly zero, with
`TASKLOOM_LOCAL_API_KEY` as the dummy credential environment; hardware,
electricity and chat usage remain outside the fee record. P0-P2 is unfinished.

## Native reviewed-memory lifecycle (2026-11-28)

The memory campaign uses the same verified local Qwen generation model. Native
output proposed an English preference and Rust 1.86 project fact, then a French
language update with exact User-source quotations. The harness operator accepted
only declared reference values; the model did not activate its own memories.
An actual new-session answer used the recalled runtime. Conflict/replacement,
restart, CLI edit, erasure, old-extraction replay and retained source history
passed the final campaign. Another owner was checked while Alice's records
were still live. All twelve declared lifecycle checks passed in 9,182 ms.

The first preliminary campaign checked the other owner after erasure, which
was weak isolation evidence; it is retained and superseded by the second and
final third campaigns' live-record checks. The final binary also reserves
administrative erasure capacity. All proposals, controller actions and native
follow-up outputs remain separate in the artifacts. This is a visible synthetic
integration case, not broad semantic-memory quality or multi-user service
authorization. For example, the French proposal retained the language but did
not separately retain the source's `concise` style preference. No full-preference
coverage is inferred. See `long-term-memory.md` and
`acceptance/p1-memory-20261128.json` for commands, transcripts and limits.

## Installed skill/template and MCP connector campaign (2026-11-28)

The final source-package campaign installed evidence-base, research-brief and
connector-notes as inert archives with generation/connector credentials removed
from the installation subprocesses. Host activation separately pinned their
content/dependencies and granted tools/credential names. The source example MCP
service then read live public GitHub metadata for `seriable/taskloom` and served
a bounded owned note store. Model decisions came from the actual local Qwen API.

Three connector templates completed: repository summary matched the observed
repository name and `Apache-2.0` license, note status read the actual empty
store, and note append completed after explicit observed-outcome reconciliation.
The research template failed exact citation identity after two bounded repairs.
Thus the **complete campaign remains failed, with 3/4 template outcomes**;
complete template acceptance and P0 research quality remain open.

The append case proposed the exact requested payload and waited for approval
with zero writes. The host stopped, retired the original package and restarted
with the original exact pin in recovery-only mode. New use was denied. After
explicit approval, the service persisted one note and deliberately lost its
response; the child retained an uncertain effect. An extra resume sent no new
write/model request. The harness operator read the actual external record and
recorded an explicitly labelled observed result, then the native role produced
`{"saved":true}`. Completed reentry remained unchanged with one note total.
This demonstrates controlled recovery with operator actions, not autonomous
reconciliation or exactly-once external effects.

The final campaign took 19,744 ms, with ten native model requests, 6,823 input /
450 output tokens and four model-requested tool attempts. It made one public
GitHub metadata request; separate operator status reads are not model tool
attempts. Generation/public API prices were explicitly zero, with a zero-micro-
USD generation cap and twenty-request GitHub process cap. Local hosting/electricity
and chat usage remain unmetered. There is no competitor/latency ranking here.
Twelve lifecycle/control checks passed, but the retained citation failure is
not relabelled as success. See `skill-packages.md` and
`acceptance/p1-skills-20261128.json` for source/artifact pins and transcripts.
