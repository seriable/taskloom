# Research sources and hybrid retrieval

`research` adds bounded webpage/search clients and an explicitly configured
Poppler PDF loader. `app` includes this feature but grants no web or PDF access
by default. Vector/hybrid retrieval is available with `runtime` alone.
These additions do not change the existing `AgentTask` API.

## Web sources and search

Merge the relevant section into the application's configuration:

```json
{
  "research": {
    "web": {
      "allowed_origins": ["https://www.rust-lang.org", "https://rust-lang.org"],
      "allow_private": false,
      "max_body_bytes": 2097152,
      "max_text_bytes": 131072,
      "max_redirects": 2,
      "timeout_ms": 15000
    },
    "search": {
      "endpoint": "https://api.search.brave.com/res/v1/web/search",
      "key_env": "BRAVE_SEARCH_API_KEY",
      "namespace": "my-search-tenant-v1",
      "access": {
        "allowed_origins": ["https://api.search.brave.com"],
        "allow_private": false,
        "max_body_bytes": 262144,
        "max_text_bytes": 131072,
        "max_redirects": 0,
        "timeout_ms": 15000
      }
    }
  }
}
```

Omit `search` when no search account/key is available. Key values are read from
the named environment variable; never commit them. Search supports the Brave
Web Search JSON contract (`web.results`), not an arbitrary search response
format. It returns titles, URLs and snippets as unverified leads. Returned URLs
never expand the web fetch allowlist. Search-service charges are separate from
the generation `RunBudget`; configure service quotas and inspect its billing.

```sh
taskloom --config research.json web-search "Rust async cancellation"
taskloom --config research.json import-web rust-learn "Rust learning" https://www.rust-lang.org/learn
taskloom --config research.json search "Rust"
```

`WebClient` grants exact HTTP(S) origins, including ports. Wildcards, embedded
credentials and fragments are rejected. Every redirect must remain within
granted origins; HTTPS downgrade is denied. DNS results are checked against
private/reserved address ranges and pinned per connection to resist rebinding.
System proxies, cookies and automatic redirects are disabled. Private network
access requires the separate explicit `allow_private: true` opt-in; use it
only for deliberately configured internal sources or local fixtures. Search
credentials are never forwarded through redirects.

Responses have total deadlines, hop limits and streamed byte limits, even
without Content-Length. Only UTF-8 HTML/plain text/Markdown is accepted.
HTML extraction excludes scripts, styles, templates, SVGs and frames; it never
executes JavaScript or fetches subresources. `timeout_ms` covers the whole
fetch including extraction. At most four HTML workers run concurrently.
The HTML5 tokenizer handles quoted attributes, raw text and foreign content;
a guarded tree builder checks the deadline per token and bounds its live
open-element/formatting references to 512. Input is fed in bounded UTF-8 chunks
with deadline checks, so tokenization also stops without requiring a closing
tag. An expired request cannot leave an unrestricted full-document parse
running. Text extraction then traverses the tree once, skipping excluded
subtrees. Script-rendered pages, login flows,
OCR, bot challenges and other encodings are unsupported and require a suitable
host-controlled loader. Sources remain untrusted data.

Imported documents retain requested/final URL, response-body SHA-256, extraction
version and network configuration identity. Chunk offsets refer to extracted
UTF-8 text. Search snippets alone do not establish source support. Import the
original page into the corpus, retrieve its chunks and verify exact quotations.

## PDF import

Install Poppler from a trusted OS/package source and configure its explicit
executable path; no binary is downloaded or guessed automatically:

```json
{
  "research": {
    "pdf": {
      "executable": "/usr/bin/pdftotext",
      "extractor_id": "poppler-pdftotext-your-installed-version",
      "max_input_bytes": 8388608,
      "max_text_bytes": 8388608,
      "max_pages": 500,
      "timeout_ms": 15000
    }
  }
}
```

On Windows select the installed `pdftotext.exe`; JSON backslashes must be
escaped. Relative executable paths resolve against the config file directory.
Keep `extractor_id` aligned with the installed/pinned binary version. The loader
runs this trusted parser as the host user, without a shell. This parser is not
a hostile-code sandbox; use an isolated host/parser service for adversarial PDFs.

```sh
taskloom --config research.json import-pdf report "Research report" ./report.pdf
```

The loader takes a bounded private copy, validates the PDF header and records
its original SHA-256. Extraction enforces input/output/page/time limits and kills
the child on failure. Empty/scanned, encrypted or malformed PDFs fail explicitly;
there is no automatic OCR or network fallback. Form-feed characters delimit
pages. Citations refer to extracted UTF-8 byte ranges, not offsets in the binary
PDF, and do not imply visual-layout or OCR accuracy.

## Application API and UI

All routes require the existing bearer token:

| Method/path | Behavior |
|---|---|
| `GET /api/research/capabilities` | Enabled source types, granted fetch origins and retrieval binding; no keys |
| `POST /api/research/search` | `{query,count}` with count 1–10; unverified search leads |
| `POST /api/knowledge/web` | `{id,title,url,expected_revision}`; fetch/extract then atomic corpus import |
| `POST /api/knowledge/pdf` | `{id,title,filename,data_base64,expected_revision}`; upload bytes only, never a host file path |

The console supports PDF upload, web search and explicit webpage import. The
existing 1 MiB HTTP body and 750 KB UI upload limits apply; larger PDFs use the
CLI. Models receive `web_search` and `web_fetch` only when configured. Roles must
also explicitly allow those names. Both tools are read-only network capabilities;
they do not silently modify the corpus or grant further access. Tool definitions
freeze nonsecret endpoint/tenant/network policy identities for durable reentry.

## Vector indexes and hybrid search

`EmbeddingModel` is the host-owned embedding interface. `ProviderEmbeddingModel`
adapts an existing `Provider` (including OpenAI-compatible embeddings), with an
explicit model, tenant namespace, input-byte ceiling and per-call deadline.
It does not retry or change models. The configured identity is not proof of the
actual model served by a proxy. Provider responses must remain in input order.
Embedding-service usage/charges are separate from the generation budget and
are not included in task benchmark model-cost totals.

`EmbeddingIndex::build` explicitly embeds the supplied corpus in batches of at
most 32. It stores model identity and a full corpus fingerprint, with 1–4096
dimensions, at most 10,000 chunks and 4 million total vector values. Indexes are
serializable; hosts should persist them atomically and load with a size limit.
No automatic paid rebuild occurs. Replaced/deleted documents, metadata changes
or a different corpus revision require an explicit rebuild. Invalid dimensions,
zero/nonfinite vectors, changed identities and stale snapshots fail before a
query service call.

```rust,no_run
use std::sync::Arc;
use taskloom::runtime::rag::*;
# async fn example(snapshot: CorpusSnapshot, model: Arc<dyn EmbeddingModel>) -> Result<(), taskloom::runtime::RuntimeError> {
let index = EmbeddingIndex::build(&snapshot, model.as_ref()).await?;
let vector = VectorRetriever::new(index, model, &snapshot)?;
let hybrid = HybridRetriever::new(Arc::new(LexicalRetriever), Arc::new(vector));
let results = hybrid.retrieve(&snapshot, &RetrievalQuery::new("research question")).await?;
# let _ = results;
# Ok(()) }
```

`VectorRetriever` uses positive cosine similarity. `HybridRetriever` combines
lexical and semantic rankings using weighted reciprocal rank fusion, with up
to 20 candidates per backend. Scores from different services are not directly
compared. Unknown/duplicate IDs and invalid scores fail before fusion; ties are
deterministic. A zero backend weight avoids its service call. A failed semantic
backend surfaces an error; it never silently substitutes lexical retrieval.

Use `Application::with_retriever` to share a host-built hybrid retriever between
the knowledge tool and operator search API. The CLI's default local search and
`Application::open` remain lexical; a configured hybrid host must explicitly
build/load its index. Built-in vector/fusion identities are frozen into RAG tool
definitions. Custom backends should implement `Retriever::identity` and version
their own index/model/tenant contracts. Corpus updates while a hybrid host runs
fail closed until it is reopened with the matching rebuilt index.

## Validation provenance

Local HTTP fixtures verify search→fetch→ingest→retrieve→quote verification,
credential isolation, redirects, private-address rejection, size/deadline/status
failures, API import and shared retrieval. Vocabulary embeddings are explicit
test fixtures: they verify vector/fusion behavior, not semantic recall quality.

On 2026-11-27 a real CLI fetch of `https://www.rust-lang.org/learn` followed the
permitted redirect to `https://rust-lang.org/learn/` and imported one document
as three chunks. An offline excerpt run completed and verified three archived
source quotations at zero generation cost. The browser separately imported the
same page and verified the saved report with no console errors. This proves
public-web ingestion and quote provenance on that source, not live-LLM research
quality, visual rendering fidelity or arbitrary-site compatibility.

On 2026-11-27 the real `/usr/bin/pdftotext` parser in Linux extracted a two-page
generated PDF and passed page/output-limit checks. Reproduce this separate
environment-dependent acceptance with:

```sh
TASKLOOM_TEST_PDFTOTEXT=/usr/bin/pdftotext cargo test --locked --features research --test agent_pdf -- --ignored
```

The default suite reports this test as ignored if not explicitly invoked.
Hosted search, real embedding recall and provider-generated research reports
require configured services and are not established by these fixtures.
