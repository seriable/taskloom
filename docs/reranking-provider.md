# Explicit HTTP cross-encoder reranking and independent spending

The optional `research` feature adds `HttpReranker`, which implements the existing
`Reranker` interface. It composes with `RerankingRetriever` after the backend's
full result validation and host filters. No HTTP reranker is selected by default,
and no model/provider, credentials, prices or fallback route are inferred.

## Supported protocol and host policy

This initial adapter supports one explicit input-token-only `/rerank` JSON
profile:

```json
{"model":"reviewed-rerank-model","query":"original question","documents":["candidate body one","candidate body two"],"top_n":2}
```

The complete body (including model, query, all actual candidate bodies and JSON
framing) is serialized and checked before admission. Source paths, remote source
URLs and candidate IDs are not sent; returned indices are mapped only to the
actual host-selected candidate IDs. Candidate texts are untrusted data, never
endpoint/model/credential or permission instructions.

A response uses:

```json
{"model":"reviewed-rerank-model","results":[{"index":1,"relevance_score":0.9},{"index":0,"relevance_score":0.1}],"usage":{"total_tokens":123}}
```

`model` is optional, but if present must exactly match the requested identity.
`usage.total_tokens` is required. Optional `input_tokens` or `prompt_tokens`
must agree with total_tokens. Output/completion tokens, other usage/billing
units and unsupported top-level billing profiles are rejected: they are **not**
priced at a fabricated zero output tariff. The allowed top-level fields are
results/usage/model/id/object/request_id. Use a separately implemented, reviewed
adapter for another service contract. This adapter does not attest the provider's
invoice or discover omitted service charges.

`HttpRerankConfig` explicitly declares endpoint, credential environment name,
nonsecret account/model namespace, model, currency, input price in micro-currency
units per million tokens, input/response byte limits, fixed per-request cost
ceiling, total cost cap, maximum HTTP-attempt allocation, timeout and absolute
local ledger directory. Prices are integer micro-units; rounding is upward.
For example,1,000,000 micro-units per million tokens is1 currency unit per
million tokens. Missing prices/limits are configuration errors, including for
local services declaring a zero tariff.

HTTPS is required except explicit loopback HTTP (`localhost` or loopback IP).
Credentials in URLs, query strings and fragments are refused. The reqwest client
has redirects and environment proxies disabled. The bearer comes only from the
named host environment value; it is sensitive/redacted in headers and never
enters the ledger, config fingerprint or error text. Debug output exposes only
model, namespace and currency. A request whose body contains the configured
credential value is refused before admission. This is not a general secret
detector: do not place other secrets in candidate source text or namespace.

## Independent durable ledger

`FileRerankLedger` is separate from generation budgets, embedding journals,
project generation quotas and generic tool-service accounting. Using one does
not silently initialize or increase any of the others. Its protected absolute
local directory contains an OS lock and atomic/fsynced `rerank-ledger.json`.
The journal stores explicit model/namespace/currency metadata and validates it
against the host config. Its configuration fingerprint binds endpoint/model/account/currency/tariff and
all limits/path. Changed configuration cannot reinterpret an existing journal;
retain old state and perform an explicit host migration.

Before HTTP dispatch, under the cross-process lock the adapter:

1. Checks complete serialized input bytes and an explicit byte-count input-token
   **estimate** against the per-request allowance.
2. Counts every prior attempt, including zero-priced and unknown attempts.
3. Checks known cost plus reserved/unknown ceilings against the total cap.
4. Persists a new ordinal, request SHA-256, input-byte count and fixed cost ceiling.

Only after the atomic write succeeds is the HTTP request sent. The ledger does
not retain source/query text or credential values; source hashes can still carry
privacy implications, so protect its files. A reserved attempt begins unsettled:
it might be active, cancelled before dispatch, or have an unknown remote result.
Reading the journal does not invent knowledge of which state happened.

Cancellation (including dropping the future), timeout, network/HTTP interruption,
malformed/missing/unsupported usage and served-model tariff mismatch keep that
attempt's ceiling and count. There is no automatic retry, redirect, model
fallback, Drop-based refund or silent journal reset. A task cancelled while its
blocking reservation is being flushed can leave an admitted slot even though no
HTTP dispatch followed; that conservative ambiguity is retained.

Valid supported usage settles the actual rounded input charge **before** score
validation. Thus duplicate/out-of-range indices, negative/nonfinite/malformed
scores, missing result arrays and even unsuccessful HTTP statuses cannot hide
already reported usage. Invalid JSON or an oversized response cannot be treated
as attested usage. Every result is checked before downstream top-k truncation;
response documents/IDs never replace host sources.

Reported charges above the reserved per-request ceiling are retained in the
journal and stop further dispatch, even if the total cap still has headroom.
A byte estimate/fixed reservation is not a provider-side token or invoice cap:
if a service exceeds its declared assumptions, the framework cannot reverse the
remote charge. Incorrect prices, omitted billing categories and unreported usage
remain explicit limits. This is accounting/admission within a selected provider
contract, not an unconditional billing guarantee.

Aggregate monetary totals use Rust u128 so multiple large known observations do
not overflow a u64 summary. JSON exports render those totals as exact decimal
**strings** (`settled_micros`, `reserved_or_unknown_micros`) to preserve precision
for JavaScript clients; individual persisted tokens/charges remain u64. There is
no implicit reconciliation/refund API for unknown attempts in this initial
ledger. Inspect and retain them; do not edit/remove them to admit more work.
State and lock paths refuse symlink/reparse components and parent traversal at
access; protect the host directory from concurrent untrusted writers because
these checks are not an OS capability filesystem. This is a local file ledger
with actual same-host cross-process evidence, not a
SQL distributed quota or multi-machine lock service.

## SDK example

```rust,no_run
# #[cfg(feature="research")]
# fn example(config: taskloom::runtime::rag::HttpRerankConfig) -> Result<(), taskloom::runtime::RuntimeError> {
use std::sync::Arc;
use taskloom::runtime::rag::{HttpReranker, LexicalRetriever, RerankingRetriever};
config.validate()?; // pure validation: no secret read, network or spending mutation
let reranker = Arc::new(HttpReranker::new(config)?); // named credential lookup only
let retrieval = RerankingRetriever::new(Arc::new(LexicalRetriever), reranker, 20, 256*1024)?;
# let _ = retrieval;
# Ok(()) }
```

Use `FileRerankLedger::new(config.ledger_dir.clone())?.snapshot(&config).await?`
for credential-free inspection. Pure configuration validation never reads a
credential or contacts a service. New HTTP executions require a configured
host-selected service and its named credential. External service access is
explicit; this example does not silently download a cross-encoder model.

## Actual verification and remaining quality scope

`cargo test --locked --features research --test agent_http_rerank` uses a real
loopback HTTP fixture. The server inspects the durable reservation **before**
replying; tests cover original candidate-index mapping, complete body limits,
source/citation preservation, valid usage retained before bad scores, failed
HTTP with usage, unsupported billing, response limits, redirect refusal,
zero-price attempt caps, observed overruns, timeout and dropped caller futures.
Two independent actual child processes share one remaining paid reservation;
only one reaches the server and consumes the fixed cap. The ignored helper test
is invoked explicitly by that parent test and is not counted as an additional
normal-suite pass.

These are actual HTTP/process/accounting controls with deterministic fixture
scores. They do **not** establish learned cross-encoder effectiveness, semantic
entailment, same-model ranking superiority, factual truth or production billing
reconciliation. A separately installed, explicitly configured real reranking
model/service needs its own frozen relevance set, quality gates, reported
usage, price and finite request allocation before claiming native model quality.
