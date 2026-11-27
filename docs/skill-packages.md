# Data-only skills, templates and connectors

The `runtime` SDK and optional `app` host support installable, content-pinned
skill packages. They contain UTF-8 guidance, typed role/output contracts,
workflow templates and connector configuration examples. Installation stores
inert JSON objects: it does not execute scripts, register a tool, start a
service, read a declared credential or grant a permission.

## Package format and authoring

Examples live in `examples/skills/`:

| Package | Purpose |
|---|---|
| `evidence-base` | Reusable evidence and permission guidance dependency |
| `research-brief` | Local corpus research role and cited source-summary template |
| `connector-notes` | Repository metadata, note-status and approved note-append templates |

Each directory has a root `skill.json`. Format 1 declares name, exact version,
description, SHA-256 for every asset, exact dependency `{name,version,digest}`
pins, tool effects/permissions, required credential **names**, roles, templates
and optional connector-example metadata. Names use lowercase ASCII letters,
digits and hyphens; versions are exact canonical `major.minor.patch` integer
triples. Ranges, prereleases, network URLs, install hooks and implicit dependency
downloads are unavailable in this format.

Asset paths are portable relative paths. Roles select an instruction asset,
optional assets from explicitly pinned direct dependencies, declared tool names
and a `WorkflowOutput` contract. Template DAGs/PEER specs refer to local roles;
the host mounts them under its own prefix. Template input is schema-validated
before workflow persistence or model dispatch. Content includes do not expand
directives found inside text. Connector JSON is documentation, never applied
automatically to the host configuration.

Create deterministic plain POSIX USTAR archives from trusted authoring sources:

```powershell
python examples/skills/pack.py examples/skills/evidence-base target/evidence-base.tar
python examples/skills/pack.py examples/skills/research-brief target/research-brief.tar
python examples/skills/pack.py examples/skills/connector-notes target/connector-notes.tar
```

The helper creates a new archive only and checks declared asset hashes. Preserve
LF content; the repository attributes pin these source files to LF on Windows.
After an intentional asset edit, update its manifest hash and publish a new
version. Updating a dependency requires its new exact pin. The dependency pin in
the shipped research example matches the shipped evidence-base package.

The Rust reader independently enforces an 8 MiB transport limit, at most 128
assets/256 entries, 256 KiB per entry, 4 MiB total content and a 128 KiB manifest.
It accepts uncompressed USTAR regular files/directories and rejects links,
devices, GNU/PAX extensions, gzip/zip, traversal, absolute/drive/stream paths,
reserved Windows names, case collisions, malformed checksums, nonzero padding,
duplicate entries and ambiguous trailing bytes. Assets become in-memory strings
and a single digest-addressed JSON object; no archive path is extracted to disk.
Combined role guidance is capped at 128 KiB. Dependency resolution is bounded
to 32 packages/16 MiB, with a depth guard.

The package digest is the format-1 normalized manifest/assets identity, not the
tar byte hash or a publisher signature. Formatting/default serialization is
normalized; all declared content and dependencies are bound. Review provenance
and contents before granting a package access. Hashes establish identity and
integrity, not publisher trust or factual quality.

## Install, inspect and retire

```powershell
taskloom --config host.json skills
taskloom --config host.json skill-install target/evidence-base.tar 0
taskloom --config host.json skill-install target/research-brief.tar 1
taskloom --config host.json skill-install target/connector-notes.tar 2
```

The final argument is the exact current catalog revision. Install dependencies
first. The output includes the exact `package` pin and `activated:false`.
These commands do not initialize model/MCP services or load their credentials.
Write the bare `{name,version,digest}` object to `pin.json` to inspect or retire:

```powershell
taskloom --config host.json skill-inspect pin.json
taskloom --config host.json skill-retire pin.json 3
```

Catalog/object writes use a catalog OS lock, CAS, flushed atomic replacement and
immutable content objects under `state_dir/skills`. A same-version/different-
content install is refused, even after retirement. Identical reinstalls are
idempotent after inspecting the current revision. An interrupted object write
cannot create an activated package; retry the same archive and current catalog
revision. Corrupt catalogs/objects fail rather than silently resetting. Limits
are 256 known versions and 20,000 audit events, with retirement-event headroom.
Keep this directory trusted and back up catalog plus objects together.

Retirement removes the package from new activation/selection. Installed
dependents must be retired first; there is no implicit cascade. Objects remain
for audit and explicit recovery pins. An already loaded application keeps its
frozen snapshot; stop/reconfigure/restart it to change admissions. Retirement
is not a running-process kill or deletion of old run history.

## Host activation and durable identity

Host configuration opts in separately, using the verified install pin:

```json
{
  "skills": [{
    "package": {"name":"research-brief", "version":"1.0.0", "digest":"COPY_THE_EXACT_INSTALLED_DIGEST"},
    "mount": "research",
    "mode": "active",
    "grant": {"tools":["knowledge_search"], "permissions":["knowledge:read"], "credentials":[]}
  }]
}
```

This is a fragment, not a complete provider configuration. The research host
must enable knowledge access and set an explicit generation ceiling for output
repairs. Mount prefixes are 1–14 lowercase ASCII letters/digits/hyphens;
mounted role names cannot collide with host roles or other mounts.

Every dependency's tool requirements must match the host's actual registered
tool effects and complete permission sets. Grants must be subsets of the
original host policy. Required credential names need both explicit package
grants and a host-configured provider/service name; package text never receives
the environment values. Packages cannot downgrade a write to a read, define a
service endpoint for the host, enlarge a tool policy, waive exact approval,
remove spending ceilings or silently retry an uncertain effect. Tool-service
and embedding charges retain their separate host boundaries.

New durable skill children freeze root package, transitive pins, local role and
grant identity in `RunConfig.skill_binding`, alongside the existing exact tool
definitions, limits and approval policy. Workflow task contracts also freeze
guidance and output schema. Changing package/version/content/dependencies or
grants cannot approve or resume an old operation. Keep its original pin/config.
The original contracts remain necessary for approval and recovery; removing a
tool or changing its definition does not invent permission to replay it.

For an explicitly retired original package, `mode:"recovery_only"` allows a
host to restore its exact contracts. The app hides its templates and refuses
new workflows using those roles, while allowing old workflows to resume.
Use a separate recovery configuration rather than silently substituting the new
version. The SDK exposes mode on `ActivatedSkill`; SDK hosts must enforce
start/resume admission when using its low-level `configure_role` builder.
Host authentication/authorization remains the caller's responsibility.

`GET /api/templates` lists active templates; `GET /api/roles` includes compiled
role contracts. `POST /api/templates/{mount}/{template}/runs` accepts only
`{id,input,limits}` and creates an ordinary persisted workflow. It cannot accept
a caller-provided grant, role implementation or package. Inspect, approve,
pause/resume and reconcile it through the existing workflow/node routes.

## Working MCP connector source

`connector_service.py` is an explicitly started loopback-only MCP HTTP service:

```powershell
python examples/skills/connector_service.py --notes target/example-notes --repository seriable/taskloom
```

Configure `TASKLOOM_EXAMPLE_CONNECTOR_TOKEN` in the process environment first
(at least sixteen characters; no need to send its value to chat). The connector
example JSON shows the explicit host endpoint, namespace, token environment
and three bindings. Grant that credential name when activating connector-notes.
No service is started by installing it. A tool-calling model is needed to run
its templates; the offline demonstration is not a substitute for native evidence.

The repository identity is a server startup argument, not model input. Metadata
uses one bounded HTTPS request to public `api.github.com`, no environment proxy,
no redirects, no automatic retries and a twenty-request process cap. Private
access requires a separately supplied `--github-token-env`; ambient credentials
are never discovered. Public REST reads have an explicit zero API price in the
example; rate/response/failure observations are retained separately from model
charges. The service is a bounded example, not a production GitHub integration.

Notes use an owned directory, bounded exact text and a flushed append. The tool
is explicitly **non-idempotent**; JSON-RPC IDs do not deduplicate writes. Optional
`--fail-after-append-once` persists a note and then loses the response to exercise
reconciliation. Inspect `notes_status` and the external record before an operator
records the observed result. Never turn response loss into an automatic append
retry. The native harness performs these operator actions explicitly and labels
them separately from model generation. Repository/store identities participate
in tool descriptions, so changing them changes the frozen tool contract.

## Migration and acceptance

No automatic package import or activation occurs on upgrade. Empty `skills`
and absent `skill_binding` serialize without new fields, preserving previous
configuration fingerprints. Direct `AppConfig`/`RunConfig` literals need the new
field or an applicable default update. The typed task API, default features,
dependency set and Rust 1.86 requirement are unchanged. Packages are guidance
data; they do not install Rust/Python dependencies or sandbox an ordinary host
tool. Future package format changes must preserve format-1 digest semantics or
use a new format rather than silently rewriting old identities.

```powershell
cargo test --locked --all-features --test agent_skills --test app_skills
python examples/benchmarks/native_skills.py target/native-skills-new --config local-provider.json --taskloom target/debug/taskloom.exe
```

The automatic tests use explicitly labelled model fixtures for control,
permission, archive and approval/restart behavior. The native campaign actually
installs the example archives without model credentials, starts the MCP service,
calls the configured zero-priced local model, reads live GitHub data, appends to
the filesystem and reconciles a real lost response across a host restart and
package retirement. The cited research template's model failure is retained;
neither package installation nor passing connector cases establishes general
research quality. See `acceptance/p1-skills-20261128.json` for transcripts,
failures, prices, checks and source/artifact identities. P0-P2 is unfinished.
