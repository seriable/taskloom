# Calibrated retrieval relevance

Retrieval scores need an explicit host policy. A positive cosine is not a
relevance judgment, and matching a common word or an identifier's surrounding
sentence does not establish that the requested record exists.

The default research host supports two optional controls in addition to the
existing semantic cutoff and field ranking:

```json
{
  "retrieval": {
    "lexical": { "stop_words": ["a", "is", "the", "what"] },
    "semantic_min_score": 0.6,
    "exact_identifiers": true
  }
}
```

This is a fragment, not a complete hybrid/provider configuration. The 0.6 cutoff
belongs to the measured Qwen profile below; it is not a universal recommendation.
Mode, model, endpoint, named credential and separate embedding prices/cap must
still be configured explicitly. Generation limits do not pay for embeddings.

## Controls and limits

`lexical.stop_words` contains at most 256 lowercase normalized tokens, each at
most 64 bytes. It filters both query tokens and indexed fields when scoring.
The document and its citable body remain unchanged. Multi-token phrases and
non-normalized entries are rejected. Languages/domain vocabularies require
host review; filtering a meaningful token can reduce recall. Query aliases are
still expanded once by host policy before token filtering. Source text never
changes this policy.

`exact_identifiers` requires every query token that contains both ASCII letters
and digits and is at least eight bytes long to occur as an exact case-folded
token in each returned candidate. Token punctuation separates components. Pure
words, pure numbers and short names such as `Qwen3` are not constrained. Matching
uses chunk body, title and the lexical metadata allowlist. Document IDs, source
paths and excluded internal metadata never become implicit searchable evidence.
This intentionally narrow rule does not recognize every ticket/product-ID format.

The constraint wraps lexical or hybrid rankings and applies to both lanes. A
provably absent identifier returns empty before an embedding call. Otherwise,
the inner retriever considers at most 20 candidates before filtering to the
requested count; a relevant candidate outside that window can be missed. All
identifiers must occur within the same chunk/title/allowed metadata context.
Queries asking to compare separate records may therefore need separate searches.
An unidentifiable custom backend stays unidentifiable; the wrapper cannot
invent its model/configuration binding. Invalid returned IDs, duplicates,
negative/nonfinite scores and backend errors remain errors.

Neither control proves that a passage answers a question or supports a claim.
The exact quote/locator check and semantic claim-support assessment remain
separate. A topical passage can still be insufficient evidence for an answer.

## Configuration and migration

Empty stop words and `exact_identifiers: false` are omitted during serialization.
Old configuration fingerprints and default lexical behavior are preserved.
Opting into either policy changes the retriever/tool and host configuration
binding. Finish old runs with their original host configuration or explicitly
retire their scheduling; do not resume them under a changed contract. The
embedding model/index identity is unchanged, so these scoring/filter settings
do not require re-embedding existing documents. A changed corpus/model still
invalidates its index before query dispatch.

Configure these properties in the ordinary host JSON, CLI/API configuration
validation, or the UI retrieval JSON editor. Restart to apply exported settings.
`POST /api/retrieval/explain` exposes `lexical_terms` and
`required_identifiers` without contacting a provider; the original and expanded
queries remain visible. Custom injected retrievers retain their own behavior,
as the explanation endpoint states. Existing UI controls need no new grant.
The new fields have API/CLI tests; no new browser interaction is claimed here.

SDK hosts can use `LexicalConfig::stop_words`, `LexicalConfig::query_terms` and
`IdentifierConstrainedRetriever::new(inner, metadata_keys)`. `AgentTask`, minimal
default features and the Rust 1.86 baseline remain supported.
Downstream full Rust struct literals for `LexicalConfig`/`AppRetrieval` need the
new fields or `..Default::default()`; old JSON files need no edit.

## Actual local embedding acceptance

The retained 2026-11-27 raw/0.46-cutoff campaigns returned irrelevant evidence
on 2/2 and 1/2 original negative queries respectively. Those failures remain
published in the earlier acceptance records. An additional actual raw versus
model-recommended query-instruction probe did not resolve discrimination, so no
query prefix is silently introduced in this change.

`retrieval-quality-v2.json` keeps the original twelve-document synthetic corpus
and adds two explicit query groups: 34 observed calibration queries (24 positive,
10 negative) and 36 initially fixed evaluation queries (24 positive, 12 negative).
The corpus and labels are maintainer-authored and visible. The fixed evaluation
group is a development split, not an externally held-out or blind benchmark.
Calibration's maximum raw negative cosine was about 0.574; the explicit profile
rounded its cutoff to 0.60 and retained lexical recall with weight 2 and ordinary
English function-word filtering.

The first application campaign with that profile (`v1`) passed positive recall
and MRR but incorrectly retrieved generic recovery guidance for a nonexistent
operation hash: 11/12 evaluation negatives empty. It failed the frozen quality
gate. `v2` adds only the generic exact-identifier constraint; corpus, queries,
labels, cutoff, stop words and numeric gates are unchanged. The now-observed
evaluation group is a regression on v2, not a fresh held-out result.

| Actual Qwen profile v2 | Positive recall@5 | MRR@5 | Empty negative queries |
| --- | --- | --- | --- |
| Lexical, all 70 queries | 44/48 = 0.9167 | 0.8819 | 22/22 |
| Hybrid, all 70 queries | 46/48 = 0.9583 | 0.9184 | 22/22 |
| Hybrid, 36-query evaluation group | 24/24 = 1.0 | 1.0 | 12/12 |

The two remaining hybrid calibration misses are retained in the report, not
removed from the denominator. The original two negative queries are now empty
with this configured profile. Six frozen evaluation checks passed. Index build
plus queries made 67 actual embedding requests, all with reported usage and
explicit USD 0 API prices/cap. Four absent identifier queries avoided dispatch;
there were no generation calls. Hardware/electricity/chat costs are excluded.
The owned local service's 143 total requests comprise five exploratory requests,
71 requests in failed v1 and 67 in v2. This is not inference throughput evidence.

Reproduce using the verified local model setup in `native-evaluation.md`:

```powershell
$env:TASKLOOM_LOCAL_API_KEY='taskloom-owned-local-evaluation'
python examples/local_models/serve_embeddings.py target/local-models --max-requests 512
# In another terminal, with the same named local credential:
python examples/benchmarks/native_retrieval.py target/NEW-retrieval --taskloom target/debug/taskloom.exe --model-root target/local-models --dataset tests/data/retrieval-quality-v2.json --profile examples/benchmarks/retrieval-profile-qwen-v2.json --protocol examples/benchmarks/retrieval-relevance-protocol-v2.json
```

Build the binary with `--features app` first. Protocol files bind exact dataset
and profile SHA-256 before dispatch and retain failing checks with exit 1.
They allow only LF/CRLF line-ending equivalence for cross-platform checkouts;
labels, JSON whitespace and all other source text must still match exactly.
The example forbids remote endpoints, profile-supplied credentials/services or
permission changes and uses separately metered embedding operations.

The scoped configured-profile regression is accepted. Legacy uncalibrated
hybrid mode is still positive-only, and this one small corpus does not validate
general default settings across models, realistic external corpora, semantic
claim support or factual correctness. Broader P0 quality work remains open.
See `acceptance/p0-retrieval-relevance-20261128.json` for raw results, frozen
protocols, failures, source hashes and final checks.
