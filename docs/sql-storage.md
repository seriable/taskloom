# SQL runtime stores

The opt-in `database` feature supplies SQLite and PostgreSQL implementations
of `RunStore`, `WorkflowStore`, `SessionStore`, `CorpusStore` and `MemoryStore`. Default features remain
empty and the supported Rust version remains 1.86. The later
[SQL Application integration](sql-application.md) explicitly selects these stores
for the API/CLI/UI, skills and embedding state. [SQL identities](multi-user-service.md)
protect a separate project-scoped service. [Generation quotas](generation-quota.md)
now use transactional project admission. Explicit
[durable queues and workers](durable-queue.md) add fenced cross-process
execution/control, migration and bounded load evidence. P0 quality gates remain
separate from these persistence/service controls.

## Explicit setup

```toml
taskloom = { version = "0.2.0-alpha.1", features = ["database"] }
```

```rust,no_run
use taskloom::runtime::database::{DatabaseNamespace, DatabaseOptions, SqlDatabase};
# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
let db = SqlDatabase::sqlite("runtime.sqlite", DatabaseOptions::default()).await?;
db.migrate().await?; // explicit transaction; no migration during a model call
let scope = DatabaseNamespace::new("tenant-a", "project-a")?;
let runs = db.runs(scope.clone());
let workflows = db.workflows(scope.clone());
let sessions = db.sessions(scope);
// Pass these stores to the existing durable agent/workflow/session APIs.
// Stop and await active invocations, then drop their leases before closing:
db.close().await;
# Ok(()) }
```

For PostgreSQL, read a host-managed credential environment locally and call
`SqlDatabase::connect(&url, options)`. Never log the URL or put it in a model
prompt. The native example uses `TASKLOOM_DATABASE_URL`; tests use
`TASKLOOM_TEST_POSTGRES_URL`. `DatabaseNamespace` is selected by the trusted
host after authorization. A caller-provided namespace is not authentication.
The example's tenant/project arguments are trusted local operator input only.

`SqlDatabase::sqlite(path, options)` freezes the absolute path and escapes
Windows drive names, spaces and Unicode. `sqlite_url(path)` returns the same
explicit file URL for other SQL clients. Generic SQLite URLs must name a file;
memory mode and custom VFS are refused. Keep database files and WAL/SHM files
out of the source repository.

## Transactions, ownership and limits

Schema version 3 contains `taskloom_meta`, `taskloom_records`,
`taskloom_leases`, `taskloom_jobs` and `taskloom_job_bindings`. Versioned JSON TEXT envelopes bind the namespace, record
family and object ID. Checkpoints and budget generations advance monotonically;
equal revisions require identical content. Trace parent bindings are immutable.
Session updates use append-only compare-and-swap. Large integers and decimal
values retain the runtime's existing JSON semantics.

V1/v2→v3 migration is explicit and requires stopping old hosts with no live leases.
The version barrier fences binaries without quota/queue admission;
dataset identity and version-1 JSON envelopes/checkpoints are preserved. See
[quota migration](generation-quota.md#schema-v2-migration). All SQL models now
require known priced ceilings even when no individual run cap is configured.

SQLite uses WAL, synchronous FULL and immediate write transactions. PostgreSQL
uses synchronous commit and five-second statement/lock timeouts. The default
pool has four connections (allowed 2–32); records are bounded to 64 MiB, and
record IDs to 256 UTF-8 bytes. Tenant/project identifiers each accept 1–64
ASCII letters, digits, dots, underscores and hyphens.

Database-clock leases renew every third of their TTL (default ten seconds,
allowed 100 ms–300 s). An expired owner cannot revive its lease. Successors
increase a fencing epoch; delayed old-owner release cannot unlock the successor.
Writes verify the live fence within their transaction before committing.
Durable model/tool and workflow dispatch also verifies ownership after saving
intent and before invoking the callback. A loss of ownership stops new dispatch.
An external service cannot be made transactional by a database fence: a crash
after invocation may leave an unknown effect. Original exact approvals,
durable pre-dispatch spending reservations and explicit reconciliation still
apply. No exactly-once external-effect guarantee is added.

Drop the invocation/lease before `close()`. The pool drains already-enqueued
conditional releases for up to five seconds; an actually killed process relies
on lease expiry. Closing a pool while still running invocations is unsupported.
`SqlRunStore::inspect_budget` reads the raw reservation journal. A later saved
checkpoint may contain a newer settled generation, so these observations must
not be summed or interpreted as independent charges.

## Migration and backup

Existing file-based users need no change. Enabling the Cargo feature does not
change their store. `migrate()` explicitly creates schema v1 in a transaction;
an unsupported version is refused. There is currently no supported bulk
file-to-SQL import and no automatic
schema downgrade. Keep existing file state until a separately verified importer
is available. Take a backup before future schema changes.

Stop writers and wait for ownership to drain before the reproducible backup
procedure. Use SQLite's online backup API rather than copying a live database
file without its WAL. For PostgreSQL use `pg_dump -Fc`, then restore into a new
database with `pg_restore --exit-on-error --single-transaction --no-owner`.
Verify cold runtime snapshots before admitting new work. Leases retained in a
restored database must expire before takeover. Database clocks and permissions
remain operational responsibilities. These procedures cover the three core
tables only; they are not backups of model weights, uploaded files, skills,
external embedding ledgers or the whole application. The initial core acceptance
inspected only run/workflow/session state. The later `--kind knowledge` backup
probe additionally verifies corpus, source session and memory tombstones.

## Reproduction and acceptance

```powershell
cargo test --locked --features database --test database sqlite
python examples/database/postgres_fixture.py --help
python examples/database/postgres_fixture.py create target/my-owned-postgres
# Load TASKLOOM_TEST_POSTGRES_URL privately from the fixture credentials file.
cargo test --locked --features database --test database postgres -- --ignored
cargo build --locked --features database,openai --example database_native
python examples/database/accept_native.py target/my-native-sql --config target/native-generation-4b-config.json --example target/debug/examples/database_native.exe --postgres-fixture target/my-owned-postgres --model-log target/my-model.log
python examples/database/backup_acceptance.py target/my-backup-check --native target/my-native-sql --postgres-fixture target/my-owned-postgres --example target/debug/examples/database_native.exe
python examples/database/postgres_fixture.py remove target/my-owned-postgres
```

The fixture helper validates ownership before removal and never prints its
password. It uses the exact cached official image digest declared in its source
and refuses an absent or mismatched image; it never pulls an image. Do not publish its private
`credentials.json`, database files or dumps. Native generation requires an
already-running explicit zero-priced loopback API, its configured key
environment and prices; the harness refuses paid endpoints. See
`native-evaluation.md` for the separately retained model manifest/startup.

`acceptance/p2-sql-stores-20261128.json` retains actual SQLite and official
PostgreSQL 16.15 tests, native two-role workflows/two-turn sessions, separate
cold readers, scope isolation, actual process death with persisted spending,
TTL takeover, unknown usage and backup/restore snapshots. Each backend passed
11 lifecycle controls. Each interrupted request retains unknown response usage;
the recovery generation deliberately truncates at 128 tokens and stops with
`model_error`. That is storage/recovery evidence, not a successful long-task
quality result. All native API prices/caps are explicitly zero; hardware,
electricity and the current chat are excluded. No embedding/tool billing or
framework ranking is inferred.

Earlier Windows URL handling and immediate-reentry shutdown failures remain
recorded. The first native campaign passed its core operations but failed
terminal reentry due to an asynchronous release/pool shutdown race. The final
campaign reran both backends after the release-draining fix. Broad concurrent
service/load acceptance, authenticated isolation and application integration
remain open in `p0-p2-roadmap.md`.

## SQL corpus and memory follow-up

`db.corpus(namespace)` and `db.memory(namespace)` use the same schema-v1
record table, adding record families without DDL changes. Corpus uses a bounded
whole-snapshot update, validated documents/chunks and optional expected-revision
CAS. This is suitable for small collections, not a database vector search engine.
The existing `ingest`, retrievers and `RagTool` accept it through `CorpusStore`.
SQL mutation locks the aggregate row transactionally, including first creation;
concurrent CAS writers cannot silently overwrite one another.

Pass the SQL memory store to `MemoryManager` with a trusted `MemoryAccess`.
Every memory scope must match the store's tenant/project namespace; the owner
remains separately keyed and envelope-bound. The manager retains its exact
source, candidate review, explicit conflict replacement, editing provenance,
recall, erasure tombstone and no-new-authority rules. SQL CAS preserves immutable
audit prefixes and monotonic forgotten-source sets. Reserve capacity for erasure
is retained within the configured record limit. Erasure removes live record
values; old backups/acceptance transcripts and physical database pages are not
cryptographic erasure targets. Do not describe this as secure deletion of all
historical copies.

```powershell
cargo test --locked --features database --test database_knowledge sqlite
# Set TASKLOOM_TEST_POSTGRES_URL privately to the owned fixture first.
cargo test --locked --features database --test database_knowledge postgres -- --ignored
cargo build --locked --features database,openai --example database_native
python examples/database/accept_knowledge.py target/my-native-knowledge --config target/native-generation-4b-config.json --example target/debug/examples/database_native.exe --postgres-fixture target/my-owned-postgres --model-log target/my-model.log
python examples/database/backup_acceptance.py target/my-knowledge-backup --kind knowledge --native target/my-native-knowledge --postgres-fixture target/my-owned-postgres --example target/debug/examples/database_native.exe
```

The fixture-model database tests cover real concurrent transactions, cold
connections, corrupt/transplanted envelopes, revision mismatches, owner/project
isolation, title recall, atomic invalid/oversized admissions, extraction spending,
review/edit/recall/erasure and unchanged session history. The separately retained
native API campaign used actual Qwen3-4B generation for a short source turn and
preference extraction, then labelled local operator review/edit/erasure. Each
database passed ten controls, plus three backup/restore controls overall. Original
model output remains distinct from the human's edited value. This visible small
example does not accept broad extraction quality, semantic retrieval or
authenticated multi-user isolation. See
`acceptance/p2-sql-knowledge-20261128.json`. At that increment all P2.2 boxes
remained open. The later `sql-application.md` accepts the bounded persistence
integration, and `generation-quota.md` accepts bounded project generation
admission. `durable-queue.md` adds independently verified queue/worker,
cross-process control, migration/backup and finite local load behavior.
