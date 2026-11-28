# Explicit source loaders and bounded reranking

The `runtime` feature provides two opt-in extension points without adding a
network service or default feature dependency. Existing text/Markdown, web/PDF,
lexical, vector and hybrid behavior is unchanged.

## CSV, JSONL and simple EML sources

`CsvLoader`, `JsonlLoader` and `EmlLoader` implement `DocumentLoader` and share
`StructuredTextLimits` (`max_bytes`, `max_records`, `max_fields`). The host
chooses the local path, stable document ID and title. Files are canonicalized,
checked as regular files and read through an8MiB maximum; inputs over a host's
smaller byte limit fail before parsing or corpus writes. Invalid UTF-8 fails.
No URL, formula, email instruction, attachment, command or external reference
is executed or fetched.

The loaders **retain the complete original UTF-8 text unchanged**. Parsed cells
or email fields are not manufactured as new quoted prose. Document metadata
records the format/profile and raw source SHA-256; CSV/JSONL additionally record
validated record/field counts. The normal `WindowChunker`, CAS corpus ingestion,
retrieval and quote validator preserve exact UTF-8 source offsets. A header may
be retrieved/quoted because it really occurs in the raw source. Loader metadata
is not implicitly searchable: only configured lexical metadata keys participate.

Supported profiles:

| Loader | Validation and scope |
| --- | --- |
| CSV | Comma-separated fields, doubled quotes, quoted multiline fields, consistent columns and LF/CRLF record endings. Quotes inside unquoted fields, trailing bytes after closing quotes, unclosed quotes, NUL or bare-CR record endings fail. Cells are never evaluated. Records include the first row; there is no inferred header schema. |
| JSONL | Each nonempty physical line must be a JSON object. Scalars, arrays, blank records and incomplete JSON fail; field and record limits apply. Nested object contents remain raw JSON, not flattened semantic facts. |
| EML | Simple unique-header, UTF-8 plaintext message with header/body separator and nonempty body. Folded headers are accepted after a valid field. Only `text/plain`, UTF-8/US-ASCII declarations and absent/7bit/8bit transfer encodings are supported. Multipart, HTML, encoded attachments, base64/quoted-printable bodies and other charsets require an explicit MIME loader and are rejected here. Headers remain raw; encoded words are not decoded. |

```rust,no_run
use taskloom::runtime::rag::{CsvLoader, DocumentLoader, FileCorpusStore, WindowChunker, ingest};
# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
let source = std::path::Path::new("reviewed-policy.csv");
let document = CsvLoader::default().load(source, "policy-table", "Reviewed policy table").await?;
let store = FileCorpusStore::new("explicit-corpus-state");
let snapshot = ingest(&store, document, &WindowChunker::default(), Some(0)).await?;
# let _ = snapshot;
# Ok(()) }
```

The host remains responsible for allowed source paths and OS permissions. A
loader failure returns before `ingest`, so an existing valid corpus is not
replaced by malformed input. Large collections, SQL/object-store access, OCR and arbitrary MIME parsing are
outside these bounded text loaders. An optional DOCX profile is described below.

## Reranking composition

`Reranker` is an explicitly injected host component. It receives the validated
snapshot, query and eligible candidate ranking, and may reorder or omit those
IDs. `RerankingRetriever::new(inner, reranker, candidates, max_candidate_bytes)`
implements the existing `Retriever` trait, so it works with normal `search` and
`RagTool` without changing citation or permission APIs.

The wrapper validates the entire snapshot first, then asks the backend for at
most20 results and validates **every** returned ID/score before selecting its
first configured candidates. Duplicate/unknown IDs, negative/nonfinite scores,
oversized backend rankings and corrupt chunks fail even when a bad item would
fall below the final top-k. Candidate count is1..20 (matching the existing query
limit) and must cover the requested top-k. Selected chunk/metadata serialization
is bounded by `max_candidate_bytes` (128 bytes–16MiB) before reranker execution.
An empty backend result makes no reranker call.

After reranking, results must be a unique subset of the actual candidate IDs,
with finite nonnegative scores and bounded count. The wrapper sorts by descending
score, breaking ties by source-bound chunk ID, then applies requested top-k.
A reranker cannot resurrect a document excluded by an upstream exact-identifier,
authority or other host filter. It cannot replace source bodies or citation
identities. Place required host filters in the backend chain, and retain the
normal `search` result byte limit.

`LexicalReranker` is a reproducible local baseline. It uses the existing
`LexicalConfig`: query aliases expand once, stop words are removed, and enabled
body/title/allowed metadata fields receive host-selected weights. Each field's
score is the fraction of distinct query terms present, times that field weight.
Zero-overlap candidates are omitted. This is lexical overlap, **not** a semantic
cross-encoder, relevance proof or improvement claim over the existing profiles.

```rust,no_run
use std::sync::Arc;
use taskloom::runtime::rag::{LexicalRetriever, LexicalReranker, RerankingRetriever};
# fn example() -> Result<(), taskloom::runtime::RuntimeError> {
let retrieval = RerankingRetriever::new(
    Arc::new(LexicalRetriever), Arc::new(LexicalReranker::default()), 20, 256*1024,
)?;
# let _ = retrieval;
# Ok(()) }
```

Backend identity, reranker identity and both limits bind the composed identity.
Custom implementations that do not supply a stable nonsecret identity propagate
`None`; the wrapper does not invent model/index/account identity. Hosts must
bind custom service/model versions, tenant/account and policies explicitly.

No HTTP cross-encoder client, credential lookup or paid request is implicit.
The separate opt-in [HTTP reranking adapter](reranking-provider.md) provides its
own durable local spending ledger and exact input-only service profile; it does
not borrow generation/embedding budgets. A custom reranker is trusted host code
with the same responsibilities: durably reserve separately priced spending
**before** dispatch, retain unknown outcomes, bound requests/deadlines and obey
declared endpoint/model/authority policy. The candidate limit describes this
wrapper's inputs; it is not a sandbox for arbitrary custom host code receiving
the full trusted snapshot.

## Verification

`cargo test --locked --features runtime --test agent_loaders --test agent_rerank`
executes actual file-load→corpus-ingest→search→exact-citation flows, including
multibyte source offsets; malformed formats/bounds refusing corpus replacement;
reordering a weaker backend result; all-result corruption validation; reranker
candidate injection; upstream identifier exclusion; query alias/stop-word/field
configuration and composed identity changes. These tests establish behavior and
source integrity, not cross-domain retrieval-quality superiority.


## Optional DOCX WordprocessingML extraction

Enable the separate `office` feature for `DocxLoader` and `DocxLimits`. It uses
pinned ZIP/XML libraries in process; it never starts Microsoft Office, a shell,
a macro engine or a network fetcher. Default/minimal library users gain no Office
dependencies. This package supports **DOCX main-body text only**. It does not
claim XLSX/PPTX, OCR, arbitrary MIME, tracked-change fidelity, page layout,
headers/footers/footnotes, image interpretation or full Office document parity.

The host selects an explicit local file. `DocxLimits` bounds compressed source
bytes (default8MiB), entry count(default256), per-entry uncompressed bytes
(default8MiB), total declared/actual uncompressed bytes(default32MiB), and XML
depth(default128). Every entry is read to EOF through a per-entry bound so CRC
errors in an unused part also fail. Stored/Deflate are the supported compression
methods. Unsafe/absolute/traversing/backslash/drive paths, duplicate case-folded
names, symlinks/devices, encrypted entries, known VBA macro, ActiveX and embedded
OLE parts are refused. The main content type must be the non-macro DOCX type.
ZIP parts are never extracted as filesystem files or executed.

All XML/relationship parts are validated as UTF-8 with matching bounded nesting.
DTD/entity declarations, unknown entities, malformed XML, excessive attributes
or element names and unsupported encodings fail. Built-in XML escapes and
numeric character references decode into the extracted snapshot. External
relationship targets, processing instructions and text URLs remain data: the
loader never follows them. The initial WordprocessingML profile uses the
standard `w:` prefix and its exact transitional namespace; strict/alternate-prefix
variants are explicitly unsupported. Namespace rebinding inside main text fails.

`w:t` text is extracted from the main body. Paragraphs/breaks add newlines, tabs
and table-cell boundaries add tabs, and table rows add newlines. XML tags,
styles and instruction-text fields are not quoted as visible document prose.
This is a deterministic extraction policy, not a faithful rendering of pages or
a semantic truth check.

The resulting `Document.text` is an **extracted UTF-8 snapshot**. Its metadata
records archive SHA-256, extracted-text SHA-256, format/policy and configured
limits. `Document.source` includes an explicit extraction-version/archive/text
fingerprint fragment. Changing non-text archive content changes source/chunk
identity even if the extracted paragraph string is unchanged. Ordinary chunk
byte offsets and exact citations refer to the extracted text snapshot, never
to ZIP bytes or the original XML markup. Reproduce and inspect that snapshot
when auditing a citation; do not claim an extracted quote is a byte range in
the compressed Office file.

```rust,no_run
# #[cfg(feature="office")]
# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
use taskloom::runtime::rag::{DocxLoader, DocumentLoader};
let document = DocxLoader::default()
    .load(std::path::Path::new("reviewed-policy.docx"), "word-policy", "Word policy")
    .await?;
# let _ = document;
# Ok(()) }
```

`cargo test --locked --features office --test agent_docx` exercises actual ZIP
fixtures: Unicode, entities, paragraphs, tables, exact extracted citations,
changed archive provenance, path/symlink/macros, XML DTD/unknown entities/depth,
unused-part corruption, decompression/count/CRC limits and unchanged corpus
after rejected loading. These are real parser/file/corpus controls, not an Office
application interoperability certification. Source paths and file permissions
remain host policy. Parsing runs in the existing blocking worker and is bounded
by input/structure sizes; it is not an adversarial process-isolation boundary.
