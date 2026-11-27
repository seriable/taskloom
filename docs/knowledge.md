# Knowledge retrieval and source citations

Requires the preview's `runtime` feature. The complete offline pipeline is:

```sh
cargo run --locked --features runtime --example agent_research
```

It imports two Markdown files, chunks and indexes them, exposes a permitted
read-only search tool to a durable agent, writes a cited report, and verifies
both exact quotations against the archived tool evidence. The example model
is deterministic and free. Replace it with the existing gateway model adapter
for provider execution; tool permissions and citation checks remain the same.

## Extension contracts

`runtime::rag` separates four host-controlled interfaces:

| Interface | Built-in implementation | Replacement contract |
|---|---|---|
| `DocumentLoader` | `TextLoader`: bounded UTF-8 text/Markdown file | Return a validated document with stable ID, source, title and extracted text; host decides external access |
| `Chunker` | `WindowChunker`: character windows with overlap | Preserve exact source text, UTF-8 byte offsets and provenance through `Chunk::from_range` |
| `CorpusStore` | `FileCorpusStore`: atomic versioned local snapshots | Consistent snapshots and atomic replacement/removal; enforce revision CAS when requested |
| `Retriever` | `LexicalRetriever`: deterministic BM25-style scores | Rank IDs in the exact supplied snapshot; obey top_k; finite nonnegative scores, no duplicate/unknown IDs |

The default lexical path requires no embeddings or external service and supports
basic Unicode words and individual CJK characters. Optional snapshot-bound vector
indexes and reciprocal-rank hybrid retrieval are described in
[research tools](research-tools.md). Embedding/index service credentials and
charges remain separate from generation budgets. Language-specific segmentation
and reranking remain extension work; no benchmark parity is claimed.

`Document` contains host-assigned `id`, `title`, display `source`, UTF-8 `text`
and bounded metadata. The plain loader accepts at most 8 MiB, rejects invalid
UTF-8, and does not execute Markdown/HTML or fetch URLs. The optional `research`
feature adds bounded HTML/web and trusted Poppler PDF extraction. Office formats
still require a host loader. Sources are data; their references are not
automatically followed. This library does not sandbox custom loaders.

`ingest` validates document and plugin chunks before changing storage. Importing
the same document ID atomically replaces its prior chunks; it does not append
stale versions. A chunk ID hashes its document ID, title, source, exact text and
byte range. Updated provenance/content produces a new identity. Remove uses the
same optional expected-revision contract; stale CAS operations fail without
mutating the existing corpus.

File storage captures an absolute root, serializes writers with an OS lock,
validates existing snapshots, and atomically replaces `corpus.json`. The default
snapshot limit is 64 MiB, 10,000 documents and 100,000 chunks. It is intended for
small local collections. Large collections should replace the store/retriever
with indexed database snapshots. Corrupt or unsupported corpus versions fail
closed. Corpus format is `CORPUS_VERSION = 1`.

## Tool integration and bounded search

```rust,no_run
use std::sync::Arc;
use taskloom::runtime::{ToolRegistry, ToolPolicy};
use taskloom::runtime::rag::*;

let corpus = Arc::new(FileCorpusStore::new("state/knowledge"));
let mut tools = ToolRegistry::new();
tools.register(RagTool::new("knowledge_search", corpus, Arc::new(LexicalRetriever)))?;
let policy = ToolPolicy::default()
    .allow("knowledge_search")
    .grant("knowledge:read");
# Ok::<(), taskloom::runtime::RuntimeError>(())
```

The tool takes `{ "query": "...", "top_k": 5 }`. Query bytes, hit count and
the complete serialized result have explicit limits. The default tool output
limit is 24 KiB. Search drops whole chunks that do not fit rather than modifying
the original text or inventing offsets. An empty hit list is valid; it is not
evidence for an answer. Every result includes corpus revision, exact chunk
text, title, source and original byte range. A denied tool is not exposed or
executed. Its read-only effect permits ordinary durable recovery; importing or
deleting documents is a separate explicit host action.

Retrieved text can contain malicious instructions. The supplied examples treat
it as untrusted evidence in the system prompt, and tool policy independently
controls capabilities. Prompt wording does not provide a security sandbox.
External retriever service charges are outside the agent's model-spend budget.

## Citation verification and its limits

Ask the model for `CitedReport { text, citations: [{ chunk_id, quote }] }`.
The text uses `[[full_chunk_id]]` references. Validate it using the actual
`SearchResult::verify` from the run's tool output, not a new search of a possibly
updated corpus. Verification rejects unknown/unretrieved sources, duplicate
citations, chunks whose text no longer matches their own ID, absent or invented
quotes, quotes below the minimum size, absent/unclosed markers and references
without corresponding citations.

A quote must be an exact substring of the cited chunk and long enough to be
meaningful: by default at least 12 non-whitespace characters, or 3 words, or
6 CJK (Han/kana/Hangul) characters, whichever is met first. Use
`SearchResult::verify_with`/`verify_all_with` and `QuotePolicy` to change
these minimums; `verify`/`verify_all`, the app and benchmarks use the default.

What this proves, precisely: each cited chunk appeared in the supplied tool
output, its ID equals the hash of its own content, and the quote occurs in it.
The chunk ID is a *self*-hash. It detects accidental corruption, not tampering:
whoever can edit archived tool output can edit a chunk and recompute its ID,
and the edited chunk then verifies. It is not compared with the original
document's hash or the current corpus, and the app accepts output of any tool
step named `knowledge_search`. Integrity therefore rests on the durable run
store. Binding evidence to a specific tool instance/corpus (for example signed
or store-anchored results) is future work.

`VerifiedReport` retains exact chunks and corpus revision, so a saved report
can still be audited after the corpus is updated or deleted. The durable run
also retains the full tool result. Store these artifacts for the required
retention period; the standalone example uses a temporary directory.

For multiple searches, use `SearchResult::verify_all` with their actual archived
outputs. Reports retain all `corpus_revisions`; `corpus_revision` is present only
when they agree. This avoids hiding updates between retrievals. The application
report endpoint gathers the run's recorded `knowledge_search` outputs this way.

Verification establishes provenance and exact quotation. It does not establish
that the source is correct, that a quotation supports a conclusion, or that all
prose is supported. Evaluate those separately, optionally with the bounded
reviewer workflow. Source/display paths may reveal local details; access
control, source redaction and private-corpus retention are host concerns.
