# Source skill examples

Read `../../docs/skill-packages.md` for the format, grants, lifecycle and limits.
Archive creation is explicit and create-only:

```powershell
python examples/skills/pack.py examples/skills/evidence-base target/evidence-base.tar
python examples/skills/pack.py examples/skills/research-brief target/research-brief.tar
python examples/skills/pack.py examples/skills/connector-notes target/connector-notes.tar
```

Install in dependency order with the current catalog revision. Installation
does not activate anything. Copy the exact returned pins into trusted host
configuration and grant only the tools/permissions/credential names needed.

`connector_service.py` is separately started source code, not an install hook.
It binds loopback, requires an explicitly configured local bearer environment,
pins one GitHub repository and owns one bounded local note directory. Public
GitHub reads need no GitHub credential. Optional private access is host-configured.
The non-idempotent append example requires exact host approval; its deliberate
response-loss mode must be reconciled against actual persisted records.

The native campaign is reproducible with an explicitly zero-priced loopback
model configuration; it records failed research/citation output as failed.
These packages do not claim model effectiveness or competitor superiority.

`research-located/` contains `research-brief` **1.1.0**. Its new, opt-in
`located_cited_report` contract uses exact document/revision/byte-offset locators
and ordinal markers; the host derives canonical citations while retaining the
original model output. Version 1.0.0 in `research-brief/` keeps its strict full-ID
contract. Install/activate 1.1.0 explicitly; existing runs cannot switch formats.
See `../../docs/final-output.md` for verification and migration details.

```powershell
python examples/skills/pack.py examples/skills/research-located target/research-located.tar
python examples/benchmarks/native_skills.py target/native-skills-located --config target/native-generation-config.json --taskloom target/debug/taskloom.exe --research-package research-located
```

The runner's default remains 1.0.0. Campaigns using 1.1.0 have changed prompts
and output contracts; their outcomes are not a controlled same-conditions score
improvement over historical failures.

For explicit runtime fault acceptance, the separately started connector supports
`--hold-after-append-once-seconds 0..30` (default 0) and
`--fail-first-status-once` (default off). The former holds only the response
after a real fsynced append, allowing an owned caller process to be killed; it
cannot be combined with `--fail-after-append-once`. The latter returns one
explicit read error so a model can request a new safe read after observing it.
Both keep host grants and exact approvals authoritative. See
`../../docs/workflow-recovery-acceptance.md` for actual native results and
reproducible scripts; neither feature is an install hook or a silent retry.
