# Deferred managed work

A root managed stage can call:

```json
{"seconds":60,"reason":"Wait for the external build before checking it again"}
```

using `defer_work`. It saves the current checkpoint and ends the physical turn.
The daemon resumes the same session and transcript when the deadline is due;
earlier tools are not replayed. Once ordinary turn closing and hooks finish, no model or worker slot is held
for the requested delay; no shell sleep is created. Existing single-writer workspace ownership remains in force:
released capacity can admit unrelated work in another project.

`seconds` must be an integer from 1 to 86,400. The optional reason admits at most
512 UTF-8 bytes without control characters. The tool is offered only to admitted
root managed stages, with existing policy/approval and recipe restrictions.
It is unavailable to ordinary turns, checkers and delegated children. Running
scoped children must be collected first. `tool_status` explains an unavailable
tool without probing or changing equipment.

The wait is a stage boundary, not a final answer or a provider failure. It does
not request goal verification, validate/repair a final-answer schema or write a
requested final-answer artifact. Its completed physical request is charged once,
and the boundary consumes one iteration. A wait cannot consume the last available
iteration while promising another one, or exceed the goal's remaining lifetime.
No token/time budget is reset. Provider retry counters retain their own meaning.

## Inspect and control

```sh
rook task show ID
rook task pause ID
rook task resume ID
rook task cancel ID
```

CLI inspection labels the saved wait and its Unix deadline separately from a
provider retry. HTTP `GET /api/work/ID` exposes optional `waiting` metadata and
`next_attempt_at`. Terminal and browser chat show a saved checkpoint and stop
being busy while a conversation goal waits. Saved state is not evidence of a
currently running executor.

Pause prevents automatic continuation, including after the original deadline.
Explicit resume clears the wait and permits early continuation within the
existing budgets. Cancel clears it and ends the goal. Identified control retries
keep their existing once-per-ID behavior: an old resume cannot erase a later
wait. A stage admitted before newer user controls cannot recreate an old wait.
Queued guidance remains durable and is read before the next model request;
declined or unanswered human input never grants permission.

The daemon must be running for a due continuation to execute. If it is stopped
at the deadline, startup and subsequent scheduling passes recover due work.
Other active owners still occupy their configured capacity; becoming due does
not authorize an extra executor or turn a capacity queue into a task failure.

## Persistence and recovery

One durable transaction records the tool result, closes its pending execution
operation and saves the generation-bound wait. Its source identity includes the
execution and call, so an exact retry cannot move the original deadline. The
same transaction retires the unfinished-stage marker: a committed intentional
yield does not count as another interrupted stage for the restart guard.

Optional JSON state preserves reads of earlier runs; stored postcard layouts and
status enum representations are unchanged. Due admission retains the active
session rather than creating another iteration transcript. A later closing
failure cannot replace an already committed wait with provider backoff.

External commands/hooks/jobs retain their own recovery rules. A turn-end hook
interrupted after the checkpoint can leave an unknown external effect, requiring
inspection before mutation resumes. A wait never certifies or retries that
effect. It also does not create independent durable child-agent owners or a
cross-session mailbox.

## Current verification

Managed-work integration checks cover real engine dispatch, same-session reopen,
no repeated file write, skipped batch tails, strict argument/budget admission,
pause/cancel/identified resume, late responses after controls, loss immediately
after the transaction, unknown hook effects and final-output contracts.
Workspace/all-target clippy, frontend builds and browser module checks pass.

The owned native probe is reproducible with built binaries:

```sh
python3 xtask/probes/deferred-work/run.py --bin-dir target/debug
```

It exercises the actual HTTP model adapter and daemon with a single worker slot,
unrelated work in a second project, due capacity admission, physical process
loss/restart, unchanged session/file effects, answered tool-call history, pause,
cancel, CLI inspection, conversation-goal owner release/wake, automatic chat
observer reconnection and early chat resume queued behind another owner. It uses its own
store/config/model server and tears down its processes before temporary state.
The existing CLI/daemon/Chrome/PTY regression probe also passes.

Concurrency/policy audit checks pass, including exact retry identity, changed
controls, pending hook effects, context/recipe restrictions and older JSON state.
The complete local `cargo xtask ci` passes. Required platform CI is checked on
the exact source revision before tagging; public packages and host binaries are
verified separately before delivery is reported. This is engine/transport evidence,
not a model-quality improvement claim.
