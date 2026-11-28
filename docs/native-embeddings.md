# Native Gemini and Vertex embedding adapters

The `openai` feature exposes the shared `NativeProvider` HTTP adapters as well
as OpenAI-compatible transport. Native embedding support now implements Gemini
`batchEmbedContents` and Vertex publisher-model `predict`. These use their native
request/response schemas; they are not text-generation prompts or synthetic
vectors. Existing generation/tool-call functions remain separate.

| Protocol | Explicit base endpoint | Embedding request |
| --- | --- | --- |
| Gemini | `https://generativelanguage.googleapis.com/v1beta` | `POST BASE/models/MODEL:batchEmbedContents`, `x-goog-api-key` header, one `requests` entry with exact text per input |
| Vertex | `https://REGION-aiplatform.googleapis.com/v1/projects/PROJECT/locations/REGION/publishers/google` | `POST BASE/models/MODEL:predict`, explicit preissued Bearer, `instances[].content`, `parameters.autoTruncate=false` |

The host supplies protocol, complete versioned base, credential and a bare model
identifier. A model resource path containing `/` is refused rather than appended
as another URL path. No ADC lookup, metadata-server call, OAuth refresh, SigV4
signing, model discovery, fallback endpoint or implicit service is used. HTTPS
(or explicit loopback HTTP for local services/tests), redirect refusal, disabled
environment proxies and bounded request/response bodies apply. Credentials go
only in sensitive headers, never URLs, serialized model text or error details.
An obvious credential value appearing in the serialized input body refuses
before dispatch; this is not a general secret detector.

## Batches, vector identity and usage

Each adapter call sends **one** HTTP request for1..32 nonempty input texts. The
complete serialized body is at most2MiB and the complete response at most4MiB.
The actual model/service may impose a smaller batch/text/token allowance; those
are upstream errors, never an instruction to split/retry a request invisibly.
Vertex's false autoTruncate request is explicit. A returned `truncated:true` or
malformed truncation flag fails vector validation instead of silently accepting
an embedding of shortened input.

Both APIs use documented positional array order. If a response additionally
supplies an `index`, it must match that exact position; duplicate/out-of-order
indices are refused, not sorted into a guessed new identity. Without an index,
server adherence to its positional contract is assumed; the client cannot prove
that a server did not permute otherwise anonymous vectors. Array cardinality,
numeric values, f32 finiteness, nonempty vectors and common dimensions must all
pass before Gateway/Agent success or cache writes. There is no vector-space
fallback to a different model; existing `GatewayTransport` embedding calls keep
`allow_fallback=false` even with partial per-input cache hits.

Gemini usage comes only from explicit `usageMetadata.promptTokenCount` and
`totalTokenCount`. Missing counters remain absent. Inconsistent counters,
nonnumeric token counts or nonzero generation/thinking-output charges do not
become synthetic zero input usage. Many actual Gemini embedding responses omit
usage entirely; these are unknown billing observations, not free service.

For Vertex, when no top-level token metadata exists, all returned predictions
must supply integer `embeddings.statistics.token_count` before they are summed
as reported input tokens. Partial statistics, missing counters and sum overflow
remain unknown. Character-count metadata is not converted into guessed tokens.
An explicit mismatched served-model identity rejects vectors and leaves tariff
usage unknown rather than pricing another model using the requested tariff.

`EmbeddingResponse` has no error-with-usage field. For a parsed malformed vector
or unsuccessful HTTP envelope with valid supported usage, the native adapter
returns an invalid vector envelope **with that usage retained**. The strengthened
Gateway embedding validator records reported usage first, then rejects the whole
array's cardinality/empty/nonfinite/mixed-dimension failure. The Agent runner's
existing final validation remains a second check. Direct low-level `Provider`
callers must validate vector envelopes; use Gateway/Agent or the index builder
for validated results. Unsupported/invalid JSON or an oversized incomplete body
cannot be treated as attested usage. Response bodies/remote error text are not
inserted into the error string.

The adapter itself performs no retry. Gateway's separately configured transient
attempt policy remains explicit; use a one-attempt transport or the metered
Application index boundary when that is required. Structural vector failures
are schema failures after accounting, not transient fallback triggers.

## Application index configuration

`AppEmbedding.provider` accepts the existing OpenAI-compatible adapter or explicit
`AppProvider::Native` Gemini/Vertex. Native generation and embedding configurations
are independent. Keep the selected model, provider namespace, credential
**environment name**, input-only tariff, currency, request ceiling, total cap,
input-byte limit and timeout explicit. For example, the embedding portion of a
complete host configuration may select:

```json
{
  "provider":{
    "type":"native",
    "protocol":"gemini",
    "base_url":"https://generativelanguage.googleapis.com/v1beta",
    "key_env":"HOST_GEMINI_EMBEDDING_KEY",
    "namespace":"reviewed-account-embedding-v1"
  },
  "model":"HOST_SELECTED_EMBEDDING_MODEL"
}
```

This is a fragment, not a complete model/budget configuration or a promise that
that example model name exists. Application embedding reservation still persists
**before** provider dispatch and remains separate from generation and HTTP
reranking ledgers. Known input token usage settles its actual declared charge;
missing/partial/invalid usage, cancellation and lost responses keep the held
ceiling. Wrong vector shape still records supported usage before index rejection.
Index creation/update is explicit and binds exact model/corpus/provider identity;
no automatic paid rebuild is added. The SDK's bare `NativeProvider` is a trusted
transport primitive, not a standalone durable spending cap.

Generic Bedrock, Responses and Azure generation adapters deliberately do not
claim native embeddings here. In particular, a model-specific Titan request
cannot safely be simulated by blindly issuing multiple single-text calls for one
batch or copying a vector across inputs. Choose a dedicated reviewed embedding
adapter/endpoint instead. Anthropic embedding service is also not inferred.

## Verification and scope

`cargo test --locked --features openai --test native_embeddings` sends actual
loopback HTTP requests for both native protocols. Tests check exact versioned
paths, sensitive header kind, complete input order, one-request batches, disabled
truncation, complete/unknown usage, explicit wrong indices, nonnumeric/overflowed
vectors, mixed dimensions, output bounds, unsupported protocols, malformed input,
redirect/no replay and error-body privacy. Gateway statistics retain reported
input/cost and a schema-failure outcome for malformed nonempty vector arrays;
these are deterministic protocol fixtures, not learned embedding quality or
cloud credentials/permissions acceptance. Application native index/ledger tests
are reported separately. No cloud model or paid service was called by these
controls; target model compatibility, relevance and actual tariffs need their
own explicit finite acceptance campaign.
