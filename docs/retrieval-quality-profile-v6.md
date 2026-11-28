# Explicit technical-document retrieval profile

The new opt-in profile combines the already explicit cosine cutoff0.43 and
semantic-admission fusion with a technical-document query instruction and
`semantic_min_lexical_terms: 2`. The vector lane requires at least two distinct
query terms in each candidate's enabled body/title/allowed metadata fields,
before vector top-k truncation. Repeated words do not count twice. The same
host lexical stop words and one-step aliases determine terms. Document IDs,
paths and excluded metadata are not implicit search evidence.

The complete index is validated before this filter. A corrupt/stale/wrong-model
index remains an error even if the filter would return empty. If every candidate
is provably inadmissible, the query makes no embedding call. Otherwise normal
embedding reservation, model identity and usage checks apply. The semantic
gate then prevents lexical fusion from resurrecting rejected candidates.

Use `examples/benchmarks/retrieval-profile-qwen-v6-lexical-support.json` as an
explicit **profile fragment**, not a complete provider configuration. SDK hosts
can use `VectorRetriever::with_lexical_support(config, minimum_terms)`. The
minimum is bounded0..256; zero disables the extra constraint and is omitted
from Application JSON. Enabled lexical policy/thresholds affect the retriever
identity, so old suspended runs cannot change them. Tools, credentials, model
selection, generation prices and source bodies are unchanged.

Existing JSON remains compatible. Rust code constructing complete
`AppRetrieval` or `EmbeddingIndex` struct literals must supply the new fields;
use defaults/builders where available. `EmbeddingIndex::build` retains the
body-only format; opt into `build_with_document_context` explicitly.

## Actual bounded acceptance

All results use twelve complete public operational documents,139 chunks, actual
Qwen3-Embedding-0.6B weights and the unchanged numeric gates: recall@5≥0.90,
MRR@5≥0.85, every negative empty and no errors. No failed label or query was
removed. The old raw, union, over-strict gate and title-context failures remain
recorded separately in earlier reports.

| Group | Primary-document recall@5 | MRR@5 | Empty negatives | Errors |
| --- | --- | --- | --- | --- |
| Observed calibration | 36/36 | 0.90972 | 12/12 | 0 |
| Observed fixed-evaluation regression | 23/24 | 0.93750 | 12/12 | 0 |
| Newly frozen prospective group | 12/12 | 0.90278 | 6/6 | 0 |

The remaining regression miss asks whether a viewer can approve or change a
quota; the result returns generation-quota but misses the labelled primary
multi-user-service document. It stays in the denominator. The prospective
group's12 positive/6 ambiguous-domain negative queries, corpus, labels and
protocol were frozen59 seconds before its first host started. Source/execution
copy/report/protocol dataset hashes all match. These are visible development
labels on the same corpus, not externally blind relevance judgments.

The owned local service advanced90→185 requests for this agent's diagnostic
and acceptance work: six diagnostic,67 observed-campaign and22 prospective
requests. Local API prices/caps were USD0; hardware/electricity are unmetered.
This measures retrieval, not generated-claim entailment or external facts.

## Reproduce and limits

Start the verified local embedding example from `native-evaluation.md`, then:

```powershell
python examples/benchmarks/native_retrieval.py target/NEW-observed --taskloom target/debug/taskloom.exe --model-root target/local-models --dataset tests/data/retrieval-repository-v2.json --profile examples/benchmarks/retrieval-profile-qwen-v6-lexical-support.json --protocol examples/benchmarks/retrieval-quality-v6-observed-protocol.json
python examples/benchmarks/native_retrieval.py target/NEW-prospective --taskloom target/debug/taskloom.exe --model-root target/local-models --dataset examples/benchmarks/retrieval-quality-prospective-v1.json --profile examples/benchmarks/retrieval-profile-qwen-v6-lexical-support.json --protocol examples/benchmarks/retrieval-quality-v6-prospective-protocol.json
```

Use fresh output folders and a configured local credential environment. This
profile uses body-only document vectors. An independently optional
`embedding_document_context: true` builds index version2 with visible titles
included in exact metered embedding input; it leaves citation bodies unchanged
and requires explicit rebuild. Old version1 body-only indexes remain supported.
Changing the context flag makes the old index invalid before query dispatch;
there is no silent reinterpretation or automatic paid rebuild. Its retained
profile trial did not pass transfer gates and is not the accepted v6 profile.

The multi-term rule can reject short, paraphrased or cross-language questions.
Defaults stay minimal and uncalibrated until a host selects its own model/profile.
The accepted finite English technical-corpus results do not establish a universal
cutoff, all-domain accuracy or superiority over another framework. Native report
source/claim judgments and model task quality are separate measurements.
