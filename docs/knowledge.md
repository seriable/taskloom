# Knowledge retrieval and source citations

Requires the unreleased `runtime` feature. The complete offline pipeline is:

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

The existing embedding gateway can be used by a custom vector retriever;
embedding/index services, their credentials and charges belong to that
implementation. The built-in retrieval path requires no embeddings or external
service. The lexical tokenizer supports basic Unicode words and individual
CJK characters; language-specific segmentation, reranking and semantic/hybrid
retrieval are extension work. It is a baseline, not a claim of benchmark parity
with a dedicated vector/search system.

`Document` contains host-assigned `id`, `title`, display `source`, UTF-8 `text`
and bounded metadata. The plain loader accepts at most 8 MiB, rejects invalid
UTF-8, and does not execute Markdown/HTML or fetch URLs. Supply format-specific
loaders for PDFs, Office documents and HTML. Sources are data; the library does
not follow their references. This library does not sandbox custom loaders.

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
citations, changed source hashes, absent or invented quotes, absent/unclosed
markers and references without corresponding citations.

`VerifiedReport` retains exact chunks and corpus revision, so a saved report
can still be audited after the corpus is updated or deleted. The durable run
also retains the full tool result. Store these artifacts for the required
retention period; the standalone example uses a temporary directory.

Verification establishes provenance and exact quotation. It does not establish
that the source is correct, that a quotation supports a conclusion, or that all
prose is supported. Evaluate those separately, optionally with the bounded
reviewer workflow. Source/display paths may reveal local details; access
control, source redaction and private-corpus retention are host concerns.
