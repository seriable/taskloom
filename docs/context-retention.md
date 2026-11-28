# Optional first-user context retention

`ContextPolicy.pin_first_user` protects the actual earliest User message from
lossy history compression. It retains the original message, role and chronological
position. It does not extract a reference, invent a summary, copy an expected
answer into a later prompt or turn historical content into a system instruction.
Current user requests and complete tool-call/result blocks remain protected.

```json
{
  "context": {
    "max_input_bytes": 65536,
    "keep_recent_blocks": 2,
    "max_summary_bytes": 4096,
    "pin_first_user": true
  }
}
```

The first message consumes the same serialized byte budget as every other
protected input. If instructions, schemas, pinned input, current question and
latest complete observation cannot fit, the runtime stops with ContextLimit
before the next model call. It never silently truncates the pinned message or
a tool batch. A model can still ignore retained context; retention is not a
guarantee of correct reasoning or reference selection.

The setting is host-controlled and optional, default false. False is omitted
from JSON and preserves old serialized configuration identities. True changes
the frozen context policy. Old runs must use their original policy; a new turn
can explicitly choose a different policy while preserving prior checkpoints.
Rust full `ContextPolicy` literals need the new field or `..Default::default()`.
The typed AgentTask API and minimal default features are unchanged.

Saved ContextRecord indices and source fingerprints reconstruct the exact
projection on recovery. Restoration rejects removal of the protected first User
message, changed source bytes, split tool blocks or over-budget input. The full
session archive remains unchanged. Selecting this option does not grant access
to another session or tenant and does not override later user corrections.

## Actual failure and recovery

The original grounded history trial stopped at its final32KiB retrieval block.
An explicit larger-context continuation failed its deadline; a later completed
answer lost the original reference. All failures remain retained. Adding actual
first-user retention recovered the reference, but that first trial omitted one
required source. Its task-level failure remains separate from runtime completion.

The next explicit turn pins the real first message and freezes all six original
document IDs in its output policy **before dispatch**. It preserves every prior
run/budget file, uses a distinct run and requires fresh executed retrieval.
The actual native recovery passed twelve lifecycle/reference/source/length
checks in two requests. Two unknown reservations from the older interrupted
generation remain in the original journals; a new successful turn does not
release or relabel them. Its source-relative model verdict is recorded separately
from deterministic retention and source identity checks, with known semantic
evaluation limits retained.

Source: `native_long_session_recover.py`; evidence:
`target/aliyun-long-recovery-bound-sources-20261128-1/campaign/report.json`.
This is explicit continuation of a read-only source task, not permission to
retry an uncertain non-idempotent write or resume a run under changed bindings.
