# Durable SQL queues and workers

The `app,database` features provide explicit SQLite/PostgreSQL project queues
and independent worker processes. Submission persists immutable task input and
host contracts before execution. Workers reuse the existing durable run,
workflow, PEER and session controllers; a second job log never decides that an
unknown external write is safe to repeat.

Default features remain empty. File-based/local execution and `AgentTask` remain
compatible, with Rust 1.86. Queues are explicitly configured by a trusted local
administrator. The multi-user JSON service uses the same SQL project and live
role/token checks. The existing browser UI is the local operator UI; queue
submission/control currently uses the queue API or CLI. Legacy `job.active`
fields describe process-local workers. **Use queue job state and its runtime
bindings for distributed activity**, including a worker running on another host.

## Setup and migration

1. Stop old API hosts and workers. Back up the complete database and separately
   protect host configs, credentials, model weights and external tool data.
2. Build/install with `app,database` and run `taskloom database-migrate` against
   the intended database. This explicitly upgrades schema v1/v2 to **v3** in
   one transaction. No live database-clock lease may remain. PostgreSQL locks
   the lease table during the barrier. Unknown versions/downgrades are refused.
3. Configure model/prices, tools, role grants and SQL tenant/project normally.
   Initialize a reviewed generation quota separately; queue capacity does not
   replace the generation spending ledger.
4. Configure queue capacity, then start the project API and separate workers
   with the same frozen host contracts and SQL dataset/project identity.

Schema v3 adds `taskloom_jobs` and `taskloom_job_bindings` with claim/resource
indexes. Existing dataset UUID, runtime envelopes, budgets, quotes, approvals
and exact host bindings are preserved. V1/v2 binaries fail schema/ownership
checks after migration; do not mix them into a running v3 deployment. There is
no automatic file-to-SQL import, partial journal import or schema downgrade.
Restore a complete backup with its matching binary when a rollback is required.

The retained v2 migration probe rebuilds the actual pre-queue SDK at `8f681b0`
in an ignored, separate source directory. Copy
`examples/database/legacy_owner.rs` to that archive's
`examples/old_queue_owner.rs` and build it plus `database_native` with
`--all-features --target-dir target/archived-v2-build`. Always use a different
Cargo target directory from the current checkout: equal package names/versions
can otherwise collide in cached artifacts. The lease-only fixture refuses any SDK schema other than v2.
Pass those binaries as `--old-owner` and `--old-probe` to
`accept_quota_migration.py --source-version 2`, using copies of the retained
v2 native SQLite file/dump and matching `--native-campaign`. The probe verifies
a real old process blocks upgrade while live, kills only that owned process,
waits its original lease, then verifies record/quota preservation and old-binary
schema rejection. It makes no model/tool call and is not a rolling-upgrade or
old-binary native-dispatch benchmark. Original databases stay untouched.

```powershell
cargo build --all-features --bin taskloom --example multiuser_host
taskloom --config project.json database-migrate
# limits.json: {"max_open_jobs":24,"max_running":4}
taskloom --config project.json queue-configure limits.json 0 local-admin "reviewed project capacity"
# task.json: {"type":"run","id":"hello","prompt":"Summarize the supplied task."}
taskloom --config project.json queue-submit task.json local-operator
taskloom --config project.json worker worker-a
# Another independent process, same host contract and namespace:
taskloom --config project.json worker worker-b
```

`queue-configure` needs expected **policy** revision, operator and note. A JSON
`null` disables new queue admission/claiming. It never erases jobs, refunds
generation uncertainty or reopens the multi-user service's direct execution
routes. Already executing owners can finish. Explicit control is required to
stop them. Host-contract changes require all open jobs to be resolved first;
use the original config/grants for recovery rather than rewriting input.

`max_open_jobs` (1–100000) includes pending, running, waiting, paused and failed
jobs. `max_running` (1–256, within open capacity) bounds live queue/runtime
owners across processes. Reducing it constrains recovery as well as new work;
unexpired original owners are not retroactively killed. Expired jobs without
live runtime owners can be recovered without consuming stale capacity forever.

Workers are trusted host programs with their configured model/tool credentials;
ordinary service users do not receive these credentials. Custom model adapters
and SDK workers must use the guarded SQL stores and priced generation
reservations. The built-in Application worker does so. CLI queue inspection,
resume requests and control requests do not initialize model/MCP clients.
Submission/configuration/worker boot currently validates the configured host
model and MCP bindings and therefore needs their named credentials, even though
enqueue/configuration performs no model generation or tool invocation.

The source service example listens on loopback. Use a host-configured TLS reverse
proxy when serving remote clients; keep database/model/tool credentials in the
backend environment. Supervise the API and worker processes independently,
monitor Failed/Waiting jobs and unresolved effects, and drain admission/workers
before backup or host-contract changes. SQLite is one durable database file/WAL
on a suitable local filesystem; PostgreSQL supports separate process hosts using
the same project database. The supplied acceptance uses one local host and a
real loopback PostgreSQL container, not a multi-machine failover or production
network partition campaign.

## Task input and service roles

The API accepts only the following task forms; input cannot install tools,
select another database or grant itself permissions:

```json
{"type":"run","id":"answer","prompt":"A bounded user request"}
```

```json
{"type":"session_turn","id":"conversation","turn":"first","prompt":"A bounded request"}
```

Create the conversation explicitly first. Different open jobs cannot occupy the
same conversation, including approval/recovery waits. A completed turn permits
the next queued turn; failed terminal turns still require the existing explicit
abandon operation. Historical child runs retain their original queue association.

For a workflow, call `/workflow-design/preview`, review its registered roles,
limits and exact input, then submit:

```json
{"type":"workflow","launch":{"request":{"id":"flow","input":{},"definition":{"nodes":[]},"peer":null,"limits":{}},"preview_key":"the actual reviewed preview_key"}}
```

This schematic example omits a useful graph and complete limits; use the real
validated request returned by your preview. DAG and PEER tasks use the same role
compiler and frozen task definitions as local execution. The worker rejects a
changed preview/host configuration before dispatch. Reusing a job identity with
different exact input or resource is refused. Repeating the same submission
returns its original receipt and submitter, including after completion.

The single-operator routes below are under `/api`. In the authenticated service
they are under `/tenants/T/projects/P/api`, with per-request role checks:

| Route | Permission and meaning |
| --- | --- |
| `GET /queue` | Reader: policy and this host's binding |
| `GET /queue/jobs` | Reader: bounded job listing (1000 records/64MiB) |
| `GET /queue/jobs/ID` | Reader: job plus current runtime/child associations |
| `POST /queue/jobs` | Runner: `{task, actor}`; service replaces actor with authenticated user |
| `POST /queue/jobs/ID/resume` | Runner: exact `expected_revision`, actor and note |
| `POST /queue/jobs/ID/control` | Operator: exact revision, `action: pause|cancel|abandon`, actor and note |

Approvers retain their existing exact-ticket approval/rejection routes, and
operators retain explicit child reconciliation and failed-turn abandonment.
Neither operation dispatches work. An explicit queue resume is required after
approval/reconciliation. Actor spoofing is rejected by replacing body identity
at the authenticated boundary. Role changes, expiry and revocation are checked
on every request. Revocation does not undo an already accepted task: explicitly
cancel it when required.

Once the project has a queue policy, the multi-user service refuses its direct
run/workflow/session dispatch/resume/pause/cancel routes with 409. Users must use
queue admission/control, including while queue admission is disabled. This
prevents bypassing queue capacity through the old endpoints. Trusted local SDK
and single-operator administration remain separate host authority. A previously
queue-bound runtime cannot dispatch without its current queue capability even
through a plain `db.runs(scope)` handle.

## Ownership, interruption and recovery

Claiming a job, increasing its attempt count and marking it Running is one SQL
transaction. Database time determines renewable lease validity. A candidate is
eligible only when both its queue owner and every associated runtime owner have
expired. A live owner is never stolen. Recovery claims are prioritized over new
pending jobs; healthy workers then take jobs in creation/ID order. This is not a
guarantee of fair CPU allocation among worker processes.

Run/workflow/session stores bind to the exact queue capability. Runtime writes
and pre-dispatch checks verify both fences, with another check before commit
after potentially expensive decoding/accounting. Generation reservations still
commit before each provider attempt. Removing a capability cannot authorize
dispatch through a plain SQL handle. Lease loss or an unverified monitor stops
new work; the former owner cannot overwrite the successor's checkpoint or budget.
External services are not transactional participants, so no exactly-once external
effect guarantee is added.

| Job state | Next action |
| --- | --- |
| Pending | Eligible for one fenced worker |
| Running | Current owner executes; only expired abandoned owners recover automatically |
| Waiting | Approval, reconciliation or session abandonment required; explicit resume afterward |
| Paused | Explicit queue resume |
| Failed | Inspect saved error/runtime intent; explicit resume using the same immutable input |
| Completed / Stopped | Terminal; idle workers never reclaim it |
| Abandoned | Operator permanently stopped scheduling; original runtime outcome remains unchanged |

`abandon` is an explicit scheduling retirement for an unavailable old host
contract or malformed host-only input. It requires the exact open job revision
and no live queue/runtime owner; an expired Running journal is eligible. The
cold CLI needs only SQL access. It records actor/note and releases queue capacity
without modifying runtime journals, session occupancy, spending or uncertain
effects. It does not claim that an external call failed or cancel a request
already executing at an external service. Abandoned scheduling cannot resume.
Old conversations can remain occupied until their existing controller receives
the required observed reconciliation/abandonment; use a new conversation for
independent work rather than rewriting history. After all open scheduling is
resolved or explicitly abandoned, a reviewed host-binding change is possible
while original unknown spending remains held.

There is no automatic retry of Failed/Waiting/Paused jobs. A worker killed during
a non-idempotent tool leaves the original approved call uncertain. A replacement
classifies the saved intent and waits for observed operator reconciliation; it
does not repeat the write. Lost/cancelled model responses retain conservative
generation reservations, even at zero declared API prices.

Control responses acknowledge **durable request delivery**, not instantaneous
completion. Workers observe control every 100ms when SQL is responsive; active
callbacks use the runtime's cooperative cancellation and unknown-effect rules.
A cancellation request can race a just-completed operation; inspect the actual
worker/runtime result. Pause stops at a durable boundary and can wait for an
active callback. Worker Ctrl+C requests pause; process death relies on lease
expiry. There is currently no bounded forced-shutdown CLI deadline: an active
callback may run to its configured timeout. A supervisor that hard-stops a worker
must preserve its journals and allow lease expiry before recovery.

Queue histories are bounded to 2048 events/1024 claims per job. New work is
refused before these limits exhaust outcome-recording capacity; oversized or
corrupt histories are never truncated or silently reset. Host-only malformed task
payloads become Failed without model dispatch. Generic SDK jobs outside the
Application task format require an explicit host/migration review, not edits to
their immutable payload. Current runtime association inspection is bounded to
8192 entries per job. Larger retained datasets require explicit future archival
or migration rather than deleting spending/uncertainty records.

## Reproduce and operate

```powershell
taskloom --config project.json queue
taskloom --config project.json queue-inspect run-hello
taskloom --config project.json queue-control run-hello 2 pause local-operator "review execution"
taskloom --config project.json queue-resume run-hello 3 local-operator "reviewed continuation"
taskloom --config project.json worker-once diagnostic-worker
cargo test --all-features --test database_queue --test app_queue
# Set the owned test PostgreSQL environment privately:
cargo test --all-features --test database_queue --test app_queue postgres -- --ignored
```

`worker-once` returns a job outcome or `null` when nothing can be claimed. Process
success means the outcome was acknowledged; inspect `state`, including Failed
and Stopped, rather than treating exit 0 as model success. Long-running `worker`
continues with other eligible jobs. Status, errors, audit actors, immutable input,
claim count and runtime associations are inspectable without model credentials.
New immutable inputs need new job/run identities; changing host contracts while
open jobs exist requires resolving them using their original contracts first.

For backup, stop API admission and workers, then verify no database-clock lease
is live. Back up all SQL tables/indexes together: runtime/budget, quota, queue
policy/jobs and runtime associations are one dataset. SQLite online backup and
PostgreSQL `pg_dump`/transactional `pg_restore` examples verify cold inspection
without model or connector credentials. Preserve the dataset UUID. Do not restore
partial journals or treat a backup as authority to repeat external effects.
Restore host secrets/configs/tool data separately, review credential revocations
and unresolved effects, and only then enable admission/workers.

```powershell
python examples/database/accept_queue.py target/NEW-native-queue --config target/native-generation-4b-config.json --taskloom target/debug/taskloom.exe --admin target/debug/examples/multiuser_host.exe --postgres-fixture target/OWNED-postgres --model-log target/OWNED-generation.log
python examples/database/queue_load.py target/NEW-load --taskloom target/debug/taskloom.exe --admin target/debug/examples/multiuser_host.exe --postgres-fixture target/OWNED-postgres
python examples/database/backup_acceptance.py target/NEW-queue-backup --kind queue --native target/NEW-native-queue --postgres-fixture target/OWNED-postgres --example target/debug/examples/multiuser_host.exe --taskloom target/debug/taskloom.exe
```

The bounded load uses four actual worker processes and the explicitly offline
fixture, 48 healthy jobs plus a paused/restarted control job per backend. It also
retires one never-executed job with unavailable model/MCP credentials to verify
the cold metadata path. It is
service/storage evidence, not native inference throughput. HTTP polling can miss
short active intervals; the report separately reconstructs running/open peaks
from persisted enqueue/claim/finish event clocks and retains all four actual
worker labels. Negative or inconsistent clock intervals are reported as failures.
Finite local results do not establish a production SLO or unlimited scalability.

The actual Qwen3-4B campaign separately observes eight native requests per backend
(seven reported usages, one unknown cancelled response), two overlapping worker
lifecycles, exact approval and actual process death during an fsynced MCP append.
Operator observation/reconciliation is labelled. Successful short role outputs
and lifecycle controls do not close the broader P0 research/semantic/long-task
quality gates. The retained first probe found unsupported MCP DELETE teardown;
the example connector now authenticates session deletion correctly. A contended
300ms heartbeat probe expired safely; the ordinary smoke uses one second and
native/process/load acceptance uses the ten-second production lease. No short-TTL
load guarantee is claimed. See `acceptance/p2-durable-queue-20261128.json` for raw
reports, failed probes, final source hashes and qualification of later diagnostics.
