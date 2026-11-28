# Authenticated private-memory service (bounded CRUD)

This document records the bounded CRUD increment. The later
[private-session controller](private-sessions.md) adds separately enabled,
owner-isolated conversation, generation and extraction with shared project
quota accounting. The generation restrictions below apply to the CRUD router
and shared session routes; they do not describe that later private controller.

The SQL identity/project JSON router can explicitly expose per-user memory
export, recall, review, edit and forgetting. The existing `multiuser::router`
keeps private-memory routes disabled. A trusted host opts in separately:

```rust,ignore
use taskloom::runtime::app::multiuser::{
    router_with_private_memory, PrivateMemoryPolicy,
};

let service = router_with_private_memory(
    identities,
    project_applications,
    PrivateMemoryPolicy { curate: true, ..Default::default() },
)?;
```

This policy is Rust host configuration. It is not request JSON and does not
reuse the fixed single-operator `AppMemory` scope. Fixed `AppMemory` registration
remains rejected in the multi-user router.

## Ownership and permissions

Every request authenticates its bearer and rechecks current project membership
in SQL. Its memory scope is the registered tenant/project and authenticated user
ID; its audit actor is the same authenticated user. A request cannot select
another tenant, owner, actor, grant or curator flag. Even a project Operator
can manage only their own namespace through these routes. Identity catalog
administration remains a separate trusted SDK operation.

All project members with Read permission can export/recall their own records.
Review/edit/forget additionally require current Runner or Operator authority
and host `curate: true`. With `curate: false`, the opt-in API is read-only.
Token expiry, disabled users, token revocation and role changes take effect on
the next request. Revocation does not roll back an operation already admitted.

The router rejects memory query parameters rather than interpreting identity
selectors. Mutation/recall bodies deny unknown fields; `owner`, `scope`, `actor`,
`tenant`, `project`, `curate` and `grant` are never accepted. Memory responses
and API errors set `Cache-Control: no-store`. The trusted filesystem/database
administrator still controls raw stores; this API is not encryption at rest.

## Routes

Prefix: `/tenants/T/projects/P/api`.

| Method and path | Body | Permission |
| --- | --- | --- |
| `GET /memory` | none | Read own snapshot |
| `POST /memory/recall` | `{text}` | Read own bounded recall |
| `POST /memory/review` | `{expected_revision,id,accept,replace?,note}` | Curate own candidate |
| `POST /memory/edit` | `{expected_revision,id,value,note}` | Curate own active record |
| `POST /memory/forget` | `{expected_revision,kind,key}` | Curate own slot and versions |

The host chooses `PrivateMemoryPolicy.recall` bounds. Source-linked candidates,
exact-slot conflict review, revision CAS, immutable audit prefixes, erasure
headroom and anti-resurrection bindings use the existing `MemoryManager` and
`MemoryStore` contracts. A record ID from another owner is not found in the
caller's scope. Client retries must inspect the latest revision; no force
overwrite or model call is performed by these CRUD routes.

## CRUD boundary: private generation uses a separate controller

`POST /memory/extract` and shared `/sessions/ID/memory-turns` remain unavailable.
Project sessions and durable runs are currently shared by project readers.
Using those sessions as private extraction sources, or writing recalled private
records into those runs, would leak owner-specific data. The router refuses
these operations before generation or source access rather than pretending
ordinary project history is private.

At the time of this CRUD increment, a complete private conversation/extraction API needed owner-isolated
session/run storage, privacy-preserving diagnostics/queue/report views and
durable generation reservations against the original project quota. Merely
prefixing an ID and hiding its direct route is insufficient: list, report,
trace and workflow projections can also expose stored messages. A separate
SQL namespace without explicit shared project-quota accounting would silently
change the budget boundary. These capabilities are not accepted by this CRUD
increment. See the later [private-session increment](private-sessions.md) for
its implementation, validation and remaining limits.

Existing candidates may be seeded/migrated by a trusted host SDK that has
independently established source ownership. The HTTP service never accepts a
caller-authored transcript or arbitrary candidate as an authenticated source.

## Local validation

`tests/app_private_memory.rs` uses real loopback HTTP and actual SQL stores.
SQLite covers owner/tenant separation, a project Operator's inability to target
another user's record, rejected identity fields, revision conflicts, live role
downgrade/revocation, review/edit/forget, and no model requests during HTTP CRUD.
Source seeding is explicitly a trusted SDK fixture; its source turn/model reply
are fixtures, not model-quality or private extraction acceptance.

The PostgreSQL counterpart is explicitly ignored unless an owned
`TASKLOOM_TEST_POSTGRES_URL` is supplied. An ignored external check is not a
passing PostgreSQL result. Existing default-router authorization tests keep
asserting memory routes are unavailable when the host does not opt in.

```powershell
cargo test --locked --all-features --test app_private_memory --test app_multiuser
```
