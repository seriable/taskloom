# Project generation quotas

The opt-in SQL stores enforce one cumulative monetary cap and one outstanding
attempt cap per authenticated host-selected tenant/project. Every SQL run writer
uses the same project control lock, including Application/API/CLI, workflows,
sessions, memory extraction and direct `db.runs(scope)` calls. Creating another
pool or omitting a quota from a process configuration does not bypass a stored
policy. File journals do not gain distributed quotas.

This is generation accounting. Embedding has its separate ledger; generic tool
service charges remain separately unknown. A generation quota is not a durable
task queue, a rate limit, per-user billing or load acceptance. P2 remains open.

## Configure explicitly

```rust,no_run
use taskloom::runtime::database::*;
# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
let db = SqlDatabase::sqlite("service.sqlite", DatabaseOptions::default()).await?;
db.migrate().await?;
let project = DatabaseNamespace::new("tenant-a", "project-a")?;
let quota = db.generation_quota(project);
let current = quota.snapshot().await?;
quota.configure(Some(GenerationQuota {
    currency: "USD".into(), max_cost_micros: 2_000_000,
    max_outstanding_attempts: 4,
}), current.policy_revision, "local-admin", "explicit reviewed USD generation cap").await?;
db.close().await;
# Ok(()) }
```

Only trusted administration exposes `configure`; it is not a model tool or a
public HTTP mutation. The source example supplies local administration without
model/connector credentials:

```powershell
cargo build --all-features --example multiuser_host
# Resolve TASKLOOM_DATABASE_URL privately on this host; never print its value.
target/debug/examples/multiuser_host quota-get tenant-a project-a
# policy.json: {"currency":"USD","max_cost_micros":2000000,"max_outstanding_attempts":4}
target/debug/examples/multiuser_host quota-set tenant-a project-a policy.json 0 local-admin "explicit reviewed cap"
```

Policy changes require the exact `policy_revision`, operator and a bounded audit
note. Ordinary usage changes advance `revision` without invalidating policy CAS.
The database supplies audit time. At most 1024 policy changes are retained; the
store refuses further changes rather than silently truncating audit history.

Prices in the model gateway must use the declared currency; this is an explicit
host attestation, not conversion or provider-billing verification. Currency is
immutable after initialization, including while enforcement is disabled.
Custom trusted model adapters must obey `RunBudget::requires_cost_ceiling()`,
reserve a conservative known ceiling through `reserve_before_dispatch` for
every retry/fallback and settle only with known usage. The built-in gateway does
this. **All SQL runs require known prices even without an individual run cap or
an active project quota**, closing the race with later quota configuration.
Lease/journal wrappers must forward `requires_cost_ceiling`; defaults preserve
compatibility for existing file/non-expiring implementations.

## Accounting and recovery

Admission and the run's reservation journal commit in the same transaction,
before provider dispatch. The lock order is runtime ownership, project quota,
then accounting records. Administrator imports lock the project control row
and read committed journals without taking runtime leases. PostgreSQL row locks
and SQLite's immediate writer serialize competing processes.

Held cost is `committed_micros + in_flight_micros`. Uncertain cost is already
included in committed and is not counted twice. Outstanding attempts are
`in_flight_attempts + uncertain_attempts`: zero-priced requests occupy slots.
Known settlement releases only that attempt's slot. A lost response or killed
process retains its ceiling and slot; converting in-flight into uncertain does
not free capacity. The ledger does not silently refund or erase uncertain work.

A quota `Budget` refusal means a transaction was rolled back and no dispatch
was admitted. The runtime removes only the refused reservation and records the
budget stop without inventing uncertain fees. Other storage/commit failures
remain ambiguous and retain conservative uncertainty. Reservation registration
is serialized per run; provider execution can still overlap.

Already incurred cost is saved even if it exceeds the project cap or its
reservation. Such a checkpoint sets a sticky `overrun` marker and stops fresh
admission. A trusted administrator can explicitly review/reconfigure the policy
when retained usage fits. Increasing an outstanding cap permits recovery/new
work while retaining the original unknown attempt. It cannot change a finished
run's terminal result or replay an uncertain external write.

An absent/unconfigured scope reports `usage: null`, meaning historical spending
is uninitialized. First configuration imports the newest checkpoint/reservation
generation for **every** existing run, including orphan reservation journals,
under the same project lock. Equal generations require identical budgets;
committed/uncertain amounts cannot decrease. The bounded import streams up to
100000 run/journal rows and 64MiB and refuses oversized/corrupt histories; it
does not use the UI's truncated listing or reset history. Each run gets a small
accounting record. Unconfigured SDK users do not trigger a historical scan.

Disabling enforcement requires an explicit `configure(None, ...)` (CLI JSON
`null`). Accounting continues so reenabling cannot reset spending. There is no
automatic calendar reset, historical currency conversion, pruning/refund API or
renewable rate window in this increment.

## Schema-v2 migration

`SqlDatabase::migrate` / `taskloom database-migrate` explicitly create schema v2
or upgrade v1 in one transaction. Stop all old hosts and workers first. Upgrade
refuses while any database-clock lease is live; PostgreSQL also locks the lease
table during the version barrier. Dataset UUID and existing version-1 JSON
envelopes/runtime checkpoints are preserved. Unknown versions and downgrades are
refused. Back up the complete database before upgrading and restore the complete
backup with its matching binary if rollback is required.

Old v1 binaries do not participate in quota enforcement. Their schema/ownership
checks fail after the barrier and must not be used with v2. New SQL acquisition,
renewal, ownership and commit checks require `DATABASE_SCHEMA_VERSION` (2).
Opening an Application does not run migrations. Re-run the explicit migration
on the intended database; do not create an empty replacement and call it migrated.

The schema changes neither `AgentTask`, minimal default features, Rust 1.86 nor
the file journal format. The added default trait methods preserve source
compatibility for existing stores; wrappers around SQL must forward them.

## Reproduce controls

```powershell
cargo test --all-features --test database_quota
# Owned local PostgreSQL only; resolve its private URL into the named env.
cargo test --all-features --test database_quota postgres_project_quota_atomic_admission_and_accounting -- --ignored --exact
cargo build --all-features --bin taskloom --example multiuser_host
python examples/database/accept_quota.py target/NEW-quota-native --config target/native-generation-4b-config.json --taskloom target/debug/taskloom.exe --admin target/debug/examples/multiuser_host.exe --postgres-fixture target/OWNED-postgres --model-log target/OWNED-generation.log
python examples/database/backup_acceptance.py target/NEW-quota-backup --native target/NEW-quota-native --postgres-fixture target/OWNED-postgres --example target/debug/examples/multiuser_host.exe --taskloom target/debug/taskloom.exe --kind quota
# Upgrade copies of retained v1 native data, never the original artifacts:
python examples/database/accept_quota_migration.py target/NEW-quota-migration --sqlite-v1 target/V1-native/application.sqlite --postgres-v1-dump target/V1-backup/application.dump --postgres-fixture target/OWNED-postgres --native-campaign target/V1-native --admin target/debug/examples/multiuser_host.exe
```

The native harness requires explicitly zero-priced loopback inference, uses
separate CLI/admin processes, observes a real native request before killing its
own child, waits the production ten-second lease, retains uncertainty and tests
explicit quota review plus cold terminal reentry. Failed probes and actual
acceptance results are recorded separately; fixture tests are not model quality,
native cost/latency rankings or service-worker/load evidence.

2026-11-28 acceptance: each real database passed ten native/separate-process
controls, observing two real generation requests (one interrupted with unknown
response usage). Both retained the original uncertain slot after known new-run
settlement. Quiescent SQLite online backup and PostgreSQL transactional restore
passed three additional controls. Copies of the actual preceding schema-v1
native databases passed four migration/import controls: all 31 SQLite and 146
PostgreSQL records remained byte-identical across the version barrier, and the
explicit native scopes imported their latest seven/eight run budgets. The
PostgreSQL dump also contains separately labelled fixture scopes; those are not
native API costs. See `acceptance/p2-generation-quota-20261128.json`.

Retained harness faults include missing required RunLimits fields before any
native dispatch, a Windows default-text-codec failure reading Unicode SQL data,
and an intermediate migration probe that automatically selected a fixture scope.
The final migration probe selects the recorded native scope explicitly. These
do not become successful native trials or evidence of old-binary dispatch/rolling
upgrade safety; operators must quiesce old hosts before migration.
