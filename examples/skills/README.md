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
