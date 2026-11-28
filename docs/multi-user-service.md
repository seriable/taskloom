# SQL identities and a project-scoped JSON service

With both `app` and `database`, the optional `app::multiuser::router` mounts
host-registered project Applications at `/tenants/T/projects/P/api/...`.
SQL stores users, project roles, token hashes, expiration, revocation and an
append-only administration audit. Every request authenticates the actual bearer
and freshly checks its SQL project membership. Request ownership fields cannot
choose an Application, state root, credential or tool grant.

This is a bounded identity/service increment. Project Applications use file
journals by default or explicitly configured [SQL persistence](sql-application.md).
The durable queue, global quotas, distributed workers and operational/load acceptance remain unfinished. P2.2
remains open. The existing single-operator `router`/CLI/UI is unchanged; selecting
it does not enable the new multi-user boundary.

## Roles and identity

| Role | Allowed operations |
| --- | --- |
| reader | Inspect shared project runs/workflows/sessions/knowledge/configuration; pure configuration and workflow previews |
| runner | Reader operations plus task/session/workflow submission, explicit resume and configured retrieval queries |
| approver | Reader operations plus exact run/session/workflow-child approval or rejection |
| operator | All preceding operations, pause/cancel/abandon, explicit uncertain-effect reconciliation, knowledge management/index builds and explicit telemetry export |

Each account has one role per tenant/project and can belong to up to 64 projects.
Roles are not a hierarchy: approvers cannot submit/resume and runners cannot
approve. Project tool/skill/credential authority remains the trusted host's
configuration. A runner can select the host's registered roles; there is no
additional per-role ACL in this increment. Exact approval remains a separate
required operation for configured effects. Approval never dispatches a tool.

Project members with read authority share its run, workflow and session history;
these are not private per-user conversations. User-private memory endpoints and
memory-aware session turns are unavailable here. The router refuses a project's
fixed single-operator `AppMemory` configuration rather than giving every member
that owner's memory. The new SQL memory persistence primitive is independent;
authenticated per-user memory API integration remains unfinished. A host must
explicitly select state intended to be shared when mounting existing journals.

`SqlIdentityStore` is a trusted administration primitive, not an unrestricted
HTTP configuration API. `put_user` requires the expected catalog revision.
`issue_token` creates a bearer with two independent random UUID-v4 values
(244 random bits), persists only SHA-256 and returns its plaintext once in a
type with neither Debug nor Serialize. Lifetimes are 1 second–366 days. The
example issues 24-hour tokens to a new private file and prints only receipt
metadata. It never overwrites an existing file or prints the bearer/database URL.
Unix files use mode 0600; Windows files inherit the destination's ACL.

`AuthenticatedActor` has no public constructor/Deserialize. `authorize` rereads
SQL even for an already-created actor; revocation, membership removal and role
changes affect the next authorization without server restart. Disabling a user
also permanently revokes its existing tokens; reenabling requires new issuance.
Expiry uses database time. Revocation does not retroactively cancel an already
authorized active operation. Cancellation remains explicit.

For approval, cancellation, abandonment and reconciliation routes, the middleware
sets `actor` from authenticated identity before the existing typed handler sees
the body. A client-supplied actor cannot impersonate an operator. The saved exact
tool input, ticket and host tool contract remain unchanged. Body size remains
bounded to 1 MiB. Future application routes inherit no permission: the service
uses an explicit method/path allowlist and refuses ambiguous encoded paths.
An inaccessible known operation returns 403, missing/expired/revoked credentials
401, and an unavailable identity database 503; unknown routes can return 404.

Project state roots must be absolute and canonically disjoint. Store-directory
aliases/junctions that escape the project root are refused at startup. These
checks assume a trusted filesystem administrator; changing symlinks behind the
running service is not a supported tenancy mechanism. The root single-operator
API, static UI and identity-administration endpoints are not mounted by this
router. It is a JSON service; the existing UI remains single-operator.

## Reproducible local example

From the repository root:

```powershell
cargo build --locked --features app,database --example multiuser_host
$env:TASKLOOM_DATABASE_URL='sqlite://target/service-identities.sqlite?mode=rwc'
target/debug/examples/multiuser_host.exe migrate
target/debug/examples/multiuser_host.exe put-user examples/service/alice.json 0 bootstrap
target/debug/examples/multiuser_host.exe issue alice target/alice.private-token bootstrap
target/debug/examples/multiuser_host.exe serve examples/service/host.json
```

Use `catalog` only as the trusted local administrator to inspect the current
revision before later `put-user` calls. The example JSON selects an explicitly
offline model and no tools, with an explicit zero model cap. It is not native
model evidence. A separately reviewed project config can select a native model,
its credential environment, known prices and spending limit using the ordinary
Application configuration; the identity layer does not grant a paid endpoint.

In another terminal, read the issued token locally into an Authorization header
and call `GET http://127.0.0.1:18083/tenants/team-a/projects/main/api/identity`,
then `POST .../api/runs` with `{"id":"example","prompt":"Acknowledge this task."}`.
Alice has no team-b membership. Do not put bearer values in shell history,
source files, URLs or model prompts. Revoke by the printed nonsecret token ID:

```powershell
target/debug/examples/multiuser_host.exe revoke TOKEN_ID bootstrap
```

The tested transport is loopback HTTP. Exposing bearer authentication to remote
clients requires a separately configured TLS endpoint/reverse proxy. No hosted
identity provider, browser sign-in, secret delivery channel or TLS certificate
provisioning is claimed. Database credentials remain host-managed; an SDK/database
administrator can change grants and must be trusted.

## Migration, bounds and recovery

Run `migrate` explicitly before admitting requests. Identity records add the
`identity` family in the existing schema-v1 record table, with a reserved host
control namespace and envelope/revision validation. No existing file state or
single-operator bearer is silently imported; create accounts and explicit
memberships first. No automatic schema downgrade or bulk legacy migration exists.

The transactional catalog is bounded to 1,024 users, 4,096 issued token records
and 20,000 administration events, plus the database record byte limit. Admission
fails atomically when full. Automatic token/audit compaction is not implemented;
do not treat this bounded catalog as an unbounded identity provider. Failed CAS
updates leave the original catalog unchanged. A database outage fails closed.
Keep SQL credentials, the database and issued bearer files outside source.

The SQL Application backup example in `sql-application.md` verifies core state,
skills, indexes/ledger and identity metadata for SQL projects. File-backed
projects still need a coordinated backup of their file trees. Full deployment
and credential-rotation acceptance remains a P2 gate. Restoring
older identity data can also restore older revocation state; admission must
remain stopped until operators review/rotate credentials. Historical audit has
no bearer plaintext. No whole-application backup claim is made here.

## Acceptance

```powershell
cargo test --locked --features app,database --test database_identity --test app_multiuser sqlite
# Privately set TASKLOOM_TEST_POSTGRES_URL to the owned PostgreSQL fixture.
cargo test --locked --features app,database --test database_identity --test app_multiuser postgres -- --ignored
python examples/service/accept.py target/my-service-check --example target/debug/examples/multiuser_host.exe --postgres-fixture target/my-owned-postgres
```

Both real databases passed role/expiry/revocation/CAS and actual loopback HTTP
checks. Fixture-model requests produced actual fsynced file effects only after
exact approval; spoofed decision actors were replaced, foreign tenants refused,
and a panic after a real append retained an in-flight operation. Explicit reentry
marked uncertainty without another model/tool call; only an operator could
reconcile the observed result. Session and workflow-child approval/control paths
are verified separately with no extra external write.

A separate actual service executable/admin-process campaign passed ten controls
per database: scopes/roles, completed file journals, live downgrade/revocation,
no plaintext in SQL and cold restart with unchanged state. Its model is explicitly
offline. These are real storage/transport/authority checks, not native generation
quality, load/throughput, overall P2 completion or framework leadership evidence.
Initial test expectation mistakes and an invalid partial-limits example startup
are retained in `acceptance/p2-multiuser-20261128.json` with the repaired reruns.
