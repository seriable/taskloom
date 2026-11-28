# SQL-backed Application, API, CLI and UI

Enable `app,database` to select SQLite or PostgreSQL for the application's
persistent runtime state. Selection is explicit in the host configuration;
the default remains file storage, with empty crate default features and Rust
1.86 support. SQL mode does not fall back to old file journals on missing
schema, credentials, records, indexes or corrupt data.

```powershell
cargo install --path . --locked --features app,database
```

For example, save this host config (or add `database` to an existing config):

```json
{
  "state_dir": "local-host",
  "provider": { "type": "offline" },
  "database": {
    "url_env": "TASKLOOM_DATABASE_URL",
    "tenant": "team-a",
    "project": "main"
  }
}
```

From the repository/work directory, explicitly initialize a SQLite database:

```powershell
$env:TASKLOOM_DATABASE_URL='sqlite://target/taskloom.sqlite?mode=rwc'
taskloom --config sql-host.json database-migrate
taskloom --config sql-host.json run example "Acknowledge this task."
taskloom --config sql-host.json inspect example
```

Use an absolute escaped SQLite file URL for a deployment whose working directory
can change; `SqlDatabase::sqlite_url(path)` constructs one, including Windows
drives/spaces/Unicode. A relative URL in a host environment resolves against the
process working directory, independently of the configuration file's `state_dir`.
For PostgreSQL, set that same named environment to the operator's database URL.
Values are resolved locally and never serialized in configuration, model prompts,
preview, diagnostics or error details. Database access still needs its credential
for cold reads; model/MCP/embedding credentials are unnecessary for inspection.

`serve` uses the existing local operator UI/API and its `TASKLOOM_API_TOKEN`.
Explicit native model settings still require known generation prices and a
spending cap. The example above is the labelled offline demonstration, not a
native model benchmark. See `multi-user-service.md` for the separate
authenticated project router: a registered SQL application's tenant/project
must exactly match the route's host registration. Project Applications can use
either configured SQL or the original file storage.

## Persistent state and boundaries

| Record family | Stored state |
| --- | --- |
| metadata | Stable random dataset identity established by explicit migration |
| run / budget | Checkpoints, exact approval/recovery input, immutable parent binding and pre-dispatch spending journals |
| workflow / session | Definitions, child bindings, handoffs and append-only history/CAS |
| corpus / memory | Validated source/chunk snapshots and per-owner reviewed records, audits and erasure tombstones |
| skill_catalog / skill_object | Transactional install/retirement catalog and immutable content-addressed inert packages |
| app_object | Explicitly built embedding index and independent embedding spending ledger |
| identity | Host-managed users, project memberships, token hashes and revocation/admin audit |

Runs, workflows, sessions, corpus, memory, skills and indexes use the configured
tenant/project namespace. Identity administration uses a reserved host control
namespace. Host configuration and environment/secret delivery remain outside
the database; external tool effects and model weights are not runtime records.
`state_dir` can still locate explicit host-managed resources, but no file
runtime/corpus/memory/skill/index/spending fallback is created in SQL mode.

The database guide describes transaction, record and lease bounds. Listings
return at most 1,000 validated SQL records, ordered by ID, with a 64 MiB aggregate
read limit. The default record limit is 64 MiB; embedding indexes share that SQL
limit (the file backend's index limit is 128 MiB). Oversize admission fails and
already-incurred embedding charges remain recorded. Configurable SQL options are
`max_connections` (2–32, default 4), `max_record_bytes` (1 KiB–64 MiB, default
64 MiB), and `lease_ms` (100–300,000, default 10,000).

Each invocation uses renewable database-clock ownership and fencing. Already-
dropped owners' queued releases on the same pool are drained before reacquisition,
avoiding a spurious immediate-resume conflict. This does not steal a live owner
or retry a model/tool operation. A killed process still relies on TTL; expired
owners cannot revive, save or dispatch new work. A real non-idempotent effect
whose reply was lost remains in flight/uncertain until explicit observation and
reconciliation. Fences cannot make an external effect transactional.

Stop owned invocations before `AppStores::close()`. `Application::shutdown`
signals cancellation, awaits its grace, then closes SQL only if its jobs stopped;
an expired grace with active SQL jobs returns a conflict. CLI task completion
also drains normal release before exit, including immediate terminal reentry.
Host/process death and external effects still have their documented uncertainty.
The early `memory-extract` CLI path drains on both a completed extraction and
an extraction error, and `serve` propagates an unsuccessful worker shutdown.
Choose a TTL above expected scheduler/database pauses. A heavily delayed 600ms
test owner correctly expired during one full-suite probe; normal Application
fixtures now use the production ten-second default. Explicit short-TTL expiry,
renewal, fencing and killed-process probes remain separate. No short-TTL service
liveness or load guarantee is inferred from these controls.

Embedding reservation/settlement is a separate SQL transaction before/after its
provider attempt. It is not charged to generation `RunBudget`. Separate pools
share the same remaining embedding cap; missing usage retains the reservation,
and a reported overrun persists a block on further dispatch. SQL index builds
hold a renewable fence checked after reservation and before each provider batch.
Lost ownership prevents another batch/save without refunding unknown charges.
Indexes retain strict corpus/model/configuration binding; stale indexes fail
before query dispatch and require an explicit rebuild/restart.

Skills preserve file-store semantics: exact dependencies, bounded closure,
immutable versions, explicit retirement/recovery mode and host grants. Package
text never grants access, and database credentials are not automatically added
to a skill's allowed credential names. Existing loaded hosts retain their frozen
activation snapshots. Retirement affects new activation; recovery-only pins
cannot launch new templates. SQL install/catalog mutation is one transaction,
so refused dependencies/CAS do not leave orphan package objects.

## Migration and SDK changes

`database-migrate` explicitly creates/validates schema v3 and its persistent
dataset UUID. Schema-v1/v2 databases must stop all old hosts and have no live
leases before explicit upgrade. Existing dataset UUIDs and journals are not
rewritten; old SDK-only databases without an identity gain one. The version
barrier also fences binaries without [project quota](generation-quota.md)
or [queue ownership](durable-queue.md). All SQL model adapters require known prices and conservative ceilings,
including runs without a private cost cap. Unknown
schema versions or invalid existing identity records are refused. Merely opening
an Application never applies DDL or generates a missing identity.

There is no automatic file-to-SQL import or schema downgrade. Keep existing file
data until a separately verified importer is available. Selecting SQL begins in
the selected database namespace; absence of old file runs is visible rather than
silently mixing stores. Preserve the original dataset identity in migrations and
backups. SQL application labels and default diagnostic identity bind it plus
tenant/project; changing URL/password or moving a configuration file does not
change a correctly restored dataset's identity. Explicit diagnostic namespaces
also remain bound to SQL project identity, so equal operator labels cannot merge
two projects' traces. File trace identity/labels remain compatible.

`AgentTask` is unchanged. The alpha Application's public store fields now use
backend enums implementing their existing store traits. Code that assumed a
`FileRunStore` should call `app.runs.list().await` and use `file_root()` only when
it returns `Some(path)`. `list_persisted(&FileRunStore)` remains available for
explicit file callers. `AppStores::open` supports cold reads without constructing
a model or MCP client, and the UI/API use the actual opened stores rather than
re-resolving database environment values on each diagnostic/index request.

## Backup and restore acceptance

Stop admission and workers first. The reproducible `examples/database/backup_app.py`
uses SQLite's online-backup API and owned PostgreSQL `pg_dump -Fc` followed by
transactional `pg_restore` into a new database. It compares cold run, memory,
knowledge, skill bytes/catalog, learned index/ledger, identity revocation and
workflow/session diagnostic snapshots without model/embedding credentials.
Only `captured_unix_ms`, generated afresh by each diagnostic observation, is
excluded; raw snapshots and all persisted event clocks/IDs/revisions remain.

```powershell
python examples/database/backup_app.py target/my-sql-restore --native target/my-native-app --taskloom target/debug/taskloom.exe --admin target/debug/examples/multiuser_host.exe --postgres-fixture target/my-owned-postgres --embedding-sqlite target/my-sqlite-retrieval/taskloom.json --embedding-postgres target/my-postgres-retrieval/taskloom.json
```

The input native campaign and embedding namespaces must actually exist in the
backed-up databases. The helper refuses live leases and never dispatches restored
tasks. Keep backups, database credentials and bearer files private. Back up host
configuration/secret delivery and external resources separately. Restoring old
revocation state requires operator review/credential rotation before admission;
restoring approval records is not authority to replay external side effects.
Deployments retaining file-backed projects still need a coordinated backup of
those file trees. No production orchestration/load claim is made here.

## Evidence and remaining service work

After starting the explicit local native API described in `native-evaluation.md`
and an owned PostgreSQL fixture, reproduce the generation/storage campaign:

```powershell
cargo build --locked --features app,database --bin taskloom --example multiuser_host
python examples/database/accept_app.py target/my-native-app --config target/native-generation-4b-config.json --taskloom target/debug/taskloom.exe --admin target/debug/examples/multiuser_host.exe --postgres-fixture target/my-owned-postgres --model-log target/my-generation.log
```

For learned embedding, start `examples/local_models/serve_embeddings.py` with
verified artifacts and explicit local fee/cap settings, migrate the selected
database, then run `examples/benchmarks/native_retrieval.py` with `--database`
pointing to the `AppDatabase` JSON above. Its URL environment must select that
database; the JSON contains names/scope rather than credential values. Both
helpers create fresh output directories and retain failures without fallback.

`acceptance/p2-sql-application-20261128.json` retains actual SQLite/PostgreSQL
tests, failed probes, native API/configuration/usage, browser observations,
learned-embedding rankings and backup snapshots. Actual native Qwen3-4B workflow,
session, memory extraction/recall, template, cold CLI and restart campaigns passed
15 controls per backend. A separate actual-process project-auth campaign with
SQL Application state passed 11 controls per backend using its explicitly offline
model. Browser checks displayed SQL workflows/templates, completed a native run
and rendered the linked trace with no horizontal overflow at 1031px.
A later actual-native CLI probe additionally passed five controls per backend:
source/extraction completion, immediate cold reentry with a ten-second TTL,
unchanged candidate state and no extra provider call. It used two generations
per backend and separately identifies the final teardown binary.

Actual Qwen embedding SQL campaigns made 19 requests per backend. This proves
index/ledger integration, not acceptable relevance: both default hybrid trials
returned evidence on both negative queries (0/2 correct abstentions), despite
recall@5=1.0 on this visible small set. The quality failure stays in P0.2.
Native API prices/caps are explicitly USD 0; hardware/electricity/chat usage is
excluded, and generation/embedding evidence remains separate from HTTP fixtures.

This accepts SQL application persistence within its stated bounded scope. It
does not finish P2. The later [generation quota](generation-quota.md) increment
adds transactionally shared spending/outstanding caps. The subsequent
[queue/worker increment](durable-queue.md) adds explicit distributed activity,
cross-process control/recovery and bounded service load/migration/backup evidence.
The multi-user router still shares project histories and disables
private memory endpoints; it does not become a private conversation service.
Current `max_active_runs` is local process capacity, not a global service quota.
