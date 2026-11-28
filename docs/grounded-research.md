# Evidence scope and research guidance

The application default system instruction now distinguishes source findings,
host/user task constraints, inferences and proposed investigations. The shared
source is `src/runtime/app/research-guidance.txt`. This is prompt guidance, not
a deterministic semantic verifier: the model can still ignore or misunderstand
it. Exact quotations and source hashes remain structural provenance checks.

The guidance addresses observed native failures:

- Absence of an exactly-once guarantee establishes neither at-least-once
  delivery nor permission to replay an uncertain operation.
- Absence of a fairness guarantee does not establish unfair allocation.
- A source about reservation retention does not invent an administrator refund
  or manual reconciliation API. An operator recommendation needs evidence of
  the operation and its preconditions.
- A statement about what the supplied excerpt establishes must be scoped to
  that excerpt; it does not prove a feature is absent from the whole product.
- Task identifiers, instructions and actual runtime checks come from task/host
  context. Document citations do not certify those host observations.

## Contracts and assessment

The native history example's `--grounded` mode uses this same guidance and an
additional frozen original-answer schema. Every technical assertion remains
inside `Findings: ...`. A final line can contain only the remembered original
review reference and the fixed source-authority constraint. Its literal format
is checked; arbitrary claims cannot be moved into the unassessed task-context
line. The expected reference is never inserted into a later model input.

`grounded_assertions` records the exact original character range and hash for
assessment without altering the model's transcript. This separates technical
facts from a bounded task-context line; it is not an excuse to exclude difficult
technical claims. All operational recommendations inside Findings are judged.
Length, source coverage, task completion and reference checks remain separate.

The judge's explicit v3 rubric distinguishes modal guarantees, finite-excerpt
scope and unsupported global feature/interface claims. Its 28 visible controls
retain the original 18 claims/labels/gates and add ten fixed scope/modal cases.
The actual remote model agreed on 27/28 attached-source labels, 28/28 corpus
labels and all known contradictions, with zero execution errors. It still
disagreed on one scoped-absence claim. This accepts the declared 85% calibration
gate on a visible development set; it is not blind or external truth validation.

Actual generated-answer assessment can now use `--assessment`, with **no gold
labels**. It reports the source-relative fraction labelled supported, errors,
fixed expected count and final-answer support. A missing final answer or an
insufficient final answer fails even if earlier short answers pass. Agreement
with a supplied insufficient label is never counted as good generated content.
The same configured model family and source-relative evidence remain limits.

## Retained long-task failures and recovery

The first grounded twelve-turn run completed eleven source-grounded turns, then
stopped with ContextLimit. A final six-search native tool batch is indivisible;
the actual protected instructions, question, tool schemas and complete tool
block exceeded the configured 32 KiB projection. The runtime refused before
another dispatch instead of truncating or fabricating tool results.

`native_long_session_recover.py` explicitly acknowledges that known terminal
read-only failure, preserves the old run and opens a **distinct** turn in the
same session with 64 KiB context and the same user request. It never resumes
the failed run under changed bindings or inserts a missing reference/answer.
Actual acknowledgement, previous-run preservation and distinct child identity
passed, but the recovered answer did not finish within the declared deadline.
Two upstream timeouts and later schema/length repair usage remain retained;
reported usage is not assumed to equal reserved request attempts. This is a
failed recovery-quality acceptance, not a successful long task.

The subscription transport also handles a local caller abort without converting
already observed upstream usage into a fictitious model failure. Offline
controls cover broken pipe/reset/abort responses. Earlier stack traces and
unknown upstream responses remain in their original campaign evidence.

## Migration and reproduction

Existing configuration files with an explicit `system` string keep that string.
Fresh defaults and configurations omitting `system` receive the new guidance,
which changes the frozen host/model label. Complete old suspended runs using
their original explicit configuration; do not replace their system contract.
The typed `AgentTask`, minimal default features, Rust 1.86 and tool grants are
unchanged. This adds no implicit API call, paid summary or external service.

Use the finite subscription wrapper in `native-subscription-evaluation.md` with
`--campaign long-session --selected-sources --strict-length --grounded` and
`--campaign claim-support --separate-evidence --rubric v3`. The recovery example
uses `--campaign long-recovery --recover-from PATH-TO-FAILED-CAMPAIGN`, accepts
only the known final ContextLimit with completed read observations, and changes
only the new turn's declared context budget. It is an explicit test/operator
action, never an automatic retry policy.

The whole P0 quality package remains open. Prompt guidance, semantic labels and
structural checks must not be presented as guaranteed factual accuracy.

## Explicit provider reasoning mode

The selected remote model supports an explicit `enable_thinking: false`
extension. One actual compatibility request returned HTTP200, the requested
model identifier, 39 reported completion tokens, no reasoning field and a
1,868ms observation. This single request demonstrates endpoint compatibility,
not a performance benchmark or task-quality improvement.

Application configuration can now set top-level `enable_thinking` explicitly;
SDK hosts use `OpenAiCompatible::with_enable_thinking(Some(false))`. It is
accepted only with an OpenAI-compatible generation provider. None/absence sends
no extension and preserves legacy cache/host labels; false and true each change
both provider cache identity and the durable host label. Unsupported gateways
may reject the extension. It is never silently dropped and does not change
model, tools, prices, subscription quota or embedding settings. It is not a
portable guarantee of a total output-token ceiling across all providers.

Existing thinking-default campaigns and timeouts remain unchanged. New runs
using this extension must report the changed generation conditions. Do not use
it to resume an old suspended run with a different provider contract.
