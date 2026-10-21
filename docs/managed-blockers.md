# Managed goals that need the user

An admitted root managed stage can call:

```json
{"name":"block_work","arguments":{"reason":"Log into the client and test the physical mouse wheel, then explicitly resume this goal."}}
```

`reason` must contain 1–2048 UTF-8 bytes of nonblank text without control
characters. It names the prerequisite and the action needed from the user.
The call saves `Blocked` and its reason, closes the pending operation and
retires the unfinished-stage marker in one transaction. It counts the bounded
stage and records its changes. It does not schedule another attempt, ask for a
final answer, run a completion classifier or certify that the goal is complete.
Remaining calls in the same model batch are skipped and retained as skipped
results for protocol-correct replay.

The existing goal controls resume or cancel the goal; a conversation goal also
uses the existing chat continuation control. Explicit resume retains the same
transcript and prior tool results. Restart alone never resumes a human blocker.
The existing CLI/API/TUI/browser work status surfaces show its reason. This tool
does not collect answers or credentials: use the existing `ask` and private
input paths when a supported interactive channel can supply them.

Only the admitted root stage may use it. Read-only policy, recipe tool scopes,
current generation/control revision and pending user guidance are checked before
commit. Running scoped children must first be collected. Unknown external
effects still require recovery review. A late model response cannot replace a
newer pause, resume or cancellation. Existing goal budgets remain in force.
The checkpoint is not final output and cannot write an output-contract artifact.

For replies that end in prose, completion classification remains a fallback.
A failed classifier retains that reply and retries only classification. After
three consecutive failures while classification remains unresolved, the run is
parked for explicit review/resume. Repeated classification failures cannot
replay the original tool work or evade the iteration counter by starting new
working turns. A recovered finish verdict still requires independent goal
verification.

The classifier output allowance is 4096 tokens, including reasoning, bounded by
remaining spend. The classifier is instructed to treat mandatory user actions
as blockers even if the same reply suggests further investigation. This prompt
is guidance; the explicit tool is the deterministic checkpoint boundary.

## Reference and verification

The design independently adapts Pi's committed task states and parked-task
recovery at pinned `482c1381a00d8d00bc81697e4a980c593db6803e`, especially
`packages/durable/src/types.ts`, `harness/scheduler.ts` and the ownership tests.
Pi's generic task states are an architectural reference, not a claim that it
implements this Rook tool. No upstream implementation is copied.

Managed-engine fixtures cover dispatch, skipped batch tails, restart, explicit
resume, loss immediately after commit, invalid arguments, retained classifier
answers and failure limits. Context/policy fixtures exercise forged calls from
ordinary, checker, child, recipe-restricted and read-only contexts. Shared
checkpoint fixtures cover changed controls and final-output contracts. These
are engine and persistence checks; they do not establish live-model quality.
