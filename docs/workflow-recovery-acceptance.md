# Native workflow process-death acceptance

`examples/benchmarks/native_workflow_crash.py` starts an owned Taskloom host,
official zero-priced local native model configuration and a real loopback MCP
note service. The native append role requests an exact write. The harness
operator checks and approves its actual name/arguments; the model cannot grant
itself approval. The service fsyncs one real note, writes a fault marker and
holds only the caller's reply. While that callback is still in flight, a real
second host fails to steal the live workflow lease and the harness kills the
original Taskloom OS process.

After restart, explicit parent resume retains the same approved call as
uncertain. A second resume neither repeats the append nor advances its native
model. The harness then reads actual MCP `notes_status`, confirms one exact note
and records a clearly labelled operator-observed result through the parent
reconciliation route. Native generation completes the append role and a separate
tool-less auditor consumes its output. Terminal reentry keeps the saved parent,
children and one external effect unchanged.

The final actual Qwen3-4B trial passed nine controls, using three native requests,
1,233 input / 64 output tokens at explicitly declared USD 0 API fees and cap 0.
Credential names are `TASKLOOM_LOCAL_API_KEY` for generation and
`TASKLOOM_EXAMPLE_CONNECTOR_TOKEN` for the owned local helper. Hardware,
electricity and chat usage are unmetered. No embedding, web, public GitHub or
paid tool service is called. The runtime's generic tool-service billing boundary
remains separate; the example's configured local service price is explicitly 0.

This is a two-role workflow with one active append role at the kill point; the
auditor follows after reconciliation. It is not two simultaneously killed native
generations, long-task quality, distributed queue/database ownership, competitor
recovery or a general exactly-once guarantee. The operator observation is not
relabelled as an original server response or model-generated authorization.
File-store exclusion and durable recovery remain the mechanisms under test.

The raw crash-time model budget file has reservation generation 1, older than
the completed model checkpoint. It is retained for inspection; it does not prove
an unknown active generation. This trial's uncertainty is the lost external tool
result. The earlier separate native-generation kill trial still documents model
reservation recovery and its truncated-output failure. Both evidence types
remain distinct.

The first fault probe failed its timing condition because its service held the
shared session mutex while delaying the reply. The second host's initialization
consumed the hold window. The harness refused to call that an in-flight process
kill; the failed partial transcript and incomplete usage qualification remain in
`acceptance/p0-workflow-crash-20261128.json`. The helper now delays the caller
outside the session/notes lock, and the actual kill/restart/reconcile trial was
rerun. This change affects only an explicitly enabled example fault control.

## Reproduce and migrate

No runtime API, default feature, checkpoint version or storage migration changes
in this increment. Existing example invocations keep response holding disabled.
`connector_service.py --hold-after-append-once-seconds N` accepts 0..30 seconds,
default 0, with a persisted marker under its explicit owned note directory.
Choose a fresh directory per probe. It cannot be combined with the existing
`--fail-after-append-once` control. Both are test controls, not implicit retries.
The operator-status helper's optional timeout preserves its previous 10-second
default; the crash harness explicitly allows 30 seconds for observation.

With the actual pinned local model running and zero prices/cap configured:

```powershell
cargo build --locked --features app --bin taskloom
$env:TASKLOOM_LOCAL_API_KEY='taskloom-local-evaluation-only'
python examples/benchmarks/native_workflow_crash.py target/native-write-crash --config target/native-generation-4b-config.json --taskloom target/debug/taskloom.exe
```

The directory must be new. Only the created helper/Taskloom subprocesses are
terminated. The caller's model service is left running; stop it explicitly when
finished. Raw before-approval, live second-process conflict, in-flight kill,
restart, uncertainty, actual observation, reentry and reconciled snapshots are
saved. The source-only distribution includes these scripts and evidence, not
model weights, installed dependencies, note-store state or local executables.
