# Pi feature adoption

Implementation tracker for [the Pi review](pi-reference-review-20260930.md).
Source: Pi `ee602414c703be8da722ec56de7f2399e62581ac`; starting Rook:
`f62c540`. This tracks the full requested transfer, not just the first patch.

| Capability | State | Completion evidence needed |
|---|---|---|
| Bounded live delivery and snapshot recovery | Complete | Queue/replay/input limits, WebSocket backpressure, atomic recovery, current controls, PTY reconnect/goal checks, browser form preservation and full CI passed |
| Editable steering and follow-up queue | In progress | Core/CLI/API/TUI/browser, durable IDs, goal vs ordinary-turn boundaries, revoke/accept races, restart |
| Configurable keyboard actions and prompt undo | Complete | Shared registry/config/help, bounded Unicode edit tests, remapped-key and external-editor PTY checks, full CI passed |
| Branch navigation and optional branch summary | In progress | Existing session/event IDs, bounded tree/history, explicit workspace semantics, attributable summary |
| Inline tool cards | In progress | Compact/expanded results, errors/duration/diffs, bounded loading, TUI/browser verification |
| Context provenance inspector | Pending | Request-specific sources, discovered vs loaded skills, deferred tools, CLI/API/TUI/browser |
| Local HTML export | In progress | Selected history scope, bounded streaming, escaped content, no publication, cross-frontend access |
| Opt-in phase-based model routing | Pending | Explicit policy, continuity/capability constraints, actual route/cost reporting, comparison without claiming unmeasured savings |
| Declarative extension UI | Pending | Bounded status/progress/forms/result contract, trust boundary, text fallback and frontend parity |

Regular-scrollback TUI and terminal image preview remain experiments from the
review, to assess after the main capabilities. Existing MCP/OAuth, durable goals,
skills, editor launch and storage are reused. No replacement runtime is planned.

## First integrated block

The socket delivery queue admits encoded bytes and frame slots before JSON
allocation, including the in-flight frame. A slow view backpressures only its
relay. Config validation and editor help expose both limits. Focused tests cover
escaped JSON, paced readers, disconnects and terminal delivery. Engine/registry
guards are released before async error reporting.

Named keyboard actions and bounded prompt undo/redo use one registry for config,
palette, help and dispatch. Undo retains deltas, evicts before copying text, and
supports Unicode, paste, mention completion and external-editor replacement.
Both remapped-undo and external-editor undo/redo PTY tests passed on the integrated
tree. Pending-input cancellation cleanup is also included and verified.

The first full gate failed in doctests with missing dependent crates after
parallel worktree builds shared one target directory. Its ordinary tests passed,
but that was not a green gate. The combined tree subsequently passed
`CARGO_TARGET_DIR=/tmp/rook-pi-ci-target cargo xtask ci` with exit status 0,
including doctests. The gate reported 677.6 seconds. This isolated target must
not be reused by another checkout while it is validating this tree.

## Snapshot recovery completed

The second integrated block provides:

- Current-input count/byte bounds and cleanup on answer, expiry or cancellation.
  Revision notifications clear resolved controls across windows without further
  model output; reconnect reads current requests rather than historical controls.
- Atomic replay/subscription, count/byte-bounded replay and sequence-only
  broadcast notifications. Missing events trigger replacement snapshots.
- Retained current metrics and terminal outcomes. Shortened oversized endings
  force a partial snapshot even for an already attached reader. CLI JSON exposes
  `live_view_truncated`; its retained Unicode text tail is also bounded.
- Shared transport leases held through terminal reception and TUI forwarding
  until actual processing. A separate socket reader allows cancellation and
  answers while the display queue is full; disconnect aborts that reader.
- CLI/TUI/browser handling and `live_snapshots=true` negotiation. Legacy clients
  receive ordinary events and a partial-history text marker.

Focused checks passed for queue/replay/input limits, atomic joins, lag convergence,
terminal results, config validation, legacy protocol behavior and a real WebSocket
whose consumer holds its queue full while sending Cancel. A real-daemon PTY check
closed a window on a pending approval, rejoined with a typed Unicode draft and
answered the recovered request. The existing steering PTY found an unnecessary
reattachment on every ordinary steering message; keeping the same live subscription
fixed the erased local receipt, and that scenario then passed.

A Chrome check using actual browser modules and injected protocol events passed
for transcript replacement, prompt/form preservation, duplicate input events,
sending an answer, remote resolution and terminal state. Its temporary harness
and log are `/tmp/rook-pi-browser-check/recovery.cjs` and
`/tmp/rook-pi-browser-recovery.log`. This is not a live-model browser benchmark.
The first integrated gate exposed a missing reattachment status in the goal
PTY scenario: the snapshot erased the earlier Attached notice. The TUI and
browser now restore that status, and the existing goal switch/pause/resume PTY
passed on the correction. Fresh turns also keep the current conversation and
submitted prompt; only reattachment or an actual replay gap replaces the view.
The browser scenario passed again after this change.

The final integrated `CARGO_TARGET_DIR=/tmp/rook-pi-ci-target cargo xtask ci`
passed with exit status 0, including the full TUI suite and doctests. The gate
reported 501.9 seconds; output is in `/tmp/rook-pi-snapshot-final-ci.log`.
The earlier gate finished with exit status 1 for the missing goal reattachment
notice; it is retained as failure evidence, not counted as a pass.

The preparation worktree `/tmp/rook-pi-snapshot-recovery` retains the first block
as a staged baseline, with this feature in its unstaged diff. It must not be
merged as a diff against its old HEAD. Main is now the authoritative integrated
tree; subsequent fixes belong there. The other capabilities retain their scope.

## Queue implementation

CodeGraph located `Interjections` but resolved no caller edges; directed source
reading established the two original paths. Ordinary turns used an in-memory
`Vec<String>`. Durable work had separately persisted a transcript message and its
acceptance receipt, recognizing the receipt by parsing a prefix from message text.

The first queue change makes durable acceptance one immediate store transaction:
the transcript message, goal note, current goal value and updated receipt commit
together. The loop carries receipt IDs separately from ordinary text, reloads the
current instruction under the queue mutation lock, and only injects the text
returned by successful acceptance. Paused work leaves instructions queued; stale
or concurrent acceptance cannot duplicate a message. Unaccepted corrections no
longer enter the goal through an accepted correction's goal update.

Focused managed-work and store tests passed for concurrent acceptance, rollback on
transcript failure, restart and retry without duplication, and a plain message
spelling a receipt prefix without acknowledging it. The full isolated CI passed
with exit status 0 (529.3 seconds; `/tmp/rook-pi-queue-foundation-ci.log`).
`cargo xtask compaction` also passed; 4.02 MiB on disk, 37.1x dictionary compression
and 5.8x end-to-end match the existing README/storage measurements.

The second queue block is integrated in the main checkout and passed the full
gate. Session steering and durable-work steering share receipt transitions
and a mutation lock: revision-checked edits and withdrawals, original-submission
hashes, and idempotent retries after editing or withdrawal. Accepted messages are
immutable. A queued `/goal` command now changes the goal only on acceptance.

Daemon-owned ordinary turns persist corrections in a session companion value;
queueing alone does not start a turn or create a goal. The agent consumes the
latest text at request boundaries, and the transcript and acceptance receipt
commit atomically. Session queue mutations validate the session in the same store
transaction, preventing orphaned queue values after deletion. Session deletion
also removes the queue. Existing `work.max_messages` and `max_message_bytes`
limits cover the queues, with accepted/withdrawn receipts retained for retry
recognition. Reaching the receipt cap requires a new session or a configured
higher cap; receipts are not silently discarded.

Both queues have HTTP read/submit/edit/withdraw paths, documented in
`durable-work.md`. Ordinary daemon chat submissions from all existing frontends
now use the durable queue. Promotion attaches an already constructed AgentLoop to
its goal at safe boundaries, including pause. Corrections queued before promotion
remain in the session queue; new ones use the goal queue. Goal completion checks
also consider remaining pre-promotion corrections. The obsolete daemon-owned
in-memory input field was removed; local/embedded `Interjections` still exists.

Format 3 prevents old runners interpreting withdrawn instructions as pending.
JSON defaults read older receipts without adding fields to postcard records.
Focused checks passed for queue bounds, Unicode byte limits, race winners,
revision conflicts, restart/retry, deletion cleanup, goal-command editing and
both sets of HTTP routes. The first full gate found the unused old daemon field
and stopped at Clippy; it was removed before the next gate. The subsequent full
isolated CI passed with exit status 0 in 529.8 seconds
(`/tmp/rook-pi-session-queue-final-ci.log`), including real daemon/TUI steering and
goal lifecycle checks. The combined CI/compaction process exited 0. Compaction
(`/tmp/rook-pi-session-queue-compaction.log`) retained the published measurements:
4.02 MiB on disk, 37.1x dictionary compression, and 5.8x end-to-end. No check was
waived. The failed first gate is `/tmp/rook-pi-session-queue-ci.log`.

The third queue block adds combined, bounded session/goal views and interactive
controls across CLI, TUI and browser. Opaque receipt references distinguish the
two queues and include a goal generation, preventing stale edits from affecting
a replacement goal. The generation is an additive JSON field with a legacy
default; postcard records are unchanged. Pagination keeps working when its
cursor message is accepted. Tests explicitly fill the byte limit, including
escaped Unicode text, before checking that the page stays within it.

`rook session queue`, TUI `/queue` and the browser's Message queue panel expose
full-message reads, revision-checked editing and withdrawal. Both interactive
frontends can withdraw into the existing draft without sending it. A single
reserved handoff retains a restoration across session switches, and conflicting
edits preserve the user's text. The browser prompt now accepts multiple lines:
Enter sends and Shift+Enter inserts a newline. Queue refresh is explicit.

Local and daemon PTY scenarios passed for editing, withdrawal and restoration
without starting a turn. A Chrome harness using the actual frontend modules and
controlled HTTP/protocol events passed for conflicts, accepted-message rejection,
session switches during withdrawal, snapshot recovery during editing, Unicode
limits, text-only rendering and multiline submission. Its harness is
`/tmp/rook-pi-browser-check/queue.cjs`; the final log is
`/tmp/rook-pi-queue-controls-browser-snapshot.log`. A separate check against an
actual scratch daemon passed for full multiline editing, draft preservation,
withdrawal receipts and unchanged ordinary form layout; it confirmed that no
turn started. Its harness and log are
`/tmp/rook-pi-browser-check/queue-live.cjs` and
`/tmp/rook-pi-queue-controls-browser-live.log`. These are UI checks, not live-model
benchmarks. The rendered page was also inspected.

The full isolated `cargo xtask ci` passed with exit status 0 in 535.1 seconds
(`/tmp/rook-pi-queue-controls-ci.log`). The combined CI/compaction process exited
0; compaction retained 4.02 MiB on disk, 37.1x dictionary compression and 5.8x
end-to-end (`/tmp/rook-pi-queue-controls-compaction.log`). A CSS selector correction
during the test phase was additionally exercised by the real-daemon browser
check; Rust sources were unchanged during the gate.

The fourth block carries receipt identity, revision and state through live
notifications. Acceptance returns both text and metadata from the same committed
mutation, including the goal generation; it never rereads a newer goal to label
an older acknowledgement. Optional metadata on the existing `agent` and
`interjected` events keeps the wire event kinds readable by older clients.
CLI output reports identity; TUI and browser update the matching receipt row.
Queued events cannot overwrite a terminal status at the same revision, older
revisions cannot overwrite newer ones, and events from another session are
ignored. Metadata is evicted with bounded scrollback rather than accumulated in
an independent receipt history. Local unidentified input now matches the actual
submitted text, rather than ticking the oldest provisional marker.

The combined queue mutation endpoint publishes edits/withdrawals to attached
running views. Local TUI and browser controls also apply their mutation responses
directly, including when no turn runs. The legacy scope-specific HTTP mutation
endpoints still require refreshing the queue; live prompt IDs are still assigned
by the daemon. This block does not claim idempotent live submission or complete
local/embedded parity.

Focused core and TUI identity/bound tests passed. Chrome scenarios passed for
identity, revision ordering, terminal-state precedence, session isolation and
actual scrollback eviction, as well as the previous queue controls. The real
scratch daemon/browser check passed including the withdrawn receipt mark.
Logs: `/tmp/rook-pi-receipt-core.log`, `/tmp/rook-pi-receipt-tui.log`,
`/tmp/rook-pi-receipt-browser-identities.log` and
`/tmp/rook-pi-receipt-browser-live.log`. The real PTY scenario edits and withdraws
messages while a tool waits, observes HTTP changes in the TUI, then checks the
next actual request and accepted mark; it passed locally and through the daemon
(`/tmp/rook-pi-receipt-pty-held.log`). Its first failures identified a missing TLS
initialization in the new test client, and a mock repeatedly returning tools so
fast that the acknowledgement scrolled away before inspection. Initializing the
client and holding the second tool at an explicit release boundary fixed the
test setup. No production timeout was raised.

The fourth block's full isolated CI passed in 543.2 seconds, including the full
TUI suite, goal lifecycle and doctests (`/tmp/rook-pi-receipt-ci.log`). The combined
CI/compaction process exited 0. Compaction retained the published measurements:
4.02 MiB on disk, 37.1x dictionary compression and 5.8x end-to-end
(`/tmp/rook-pi-receipt-compaction.log`). CodeGraph's fresh structural query stayed
CPU-active without returning a slice for over seven minutes; that diagnostic
process was stopped, and directed source reads supplied the call-path evidence.

The fifth block adds scoped, retryable correction submission. The combined page
advertises an admission target; CLI, TUI and browser save that target with a
caller-generated ID and the original text before their first write. Core checks
the goal generation under the mutation lock. An identical retry returns the
existing receipt after editing, withdrawal or acceptance; replacing the goal
rejects its old target. Submission alone neither starts a turn nor resumes a
paused goal. Concurrent retries and retries at a full receipt cap use one
receipt. Ordinary receipts remain retryable after promotion to a goal.

The CLI adds `session queue ... submit` and prints its ID and target before the
write. Both explicit retry flags are required together. TUI corrections use the
same durable path for local and daemon turns once the session ID is known. One
pending send is retained in that TUI process; `/queue` offers retry and forgetting.
The browser retains one bounded pending send in `sessionStorage`, including
across tab reloads. Finite queue requests have a 30-second deadline. A malformed
acknowledgement retains the request; storage failure before saving its target
prevents the first write. Neither client silently falls back to a fresh prompt.
Forgetting only discards the local retry and does not withdraw a server receipt.
Refreshing pending-send controls preserves a separate unsaved queue edit.

Focused core and TUI checks passed for restart, replacement, concurrent retries,
receipt caps, retained identity and UTF-8 byte limits. Local and daemon PTY checks
passed for live acceptance and explicit CLI retry after withdrawal. Logs:
`/tmp/rook-pi-scoped-core.log`, `/tmp/rook-pi-scoped-tui.log`,
`/tmp/rook-pi-scoped-pty.log` and `/tmp/rook-pi-scoped-controls-pty.log`.
Chrome checks using actual frontend modules passed for lost replies, reloads,
edited accepted receipts, stale targets, invalid acknowledgements, storage
failure, timeout/retry, and pending controls preserving an unsaved edit. Logs:
`/tmp/rook-pi-scoped-browser-deadline.log` and
`/tmp/rook-pi-scoped-browser-preserve-edit.log`. An actual scratch daemon plus
Chrome and a scripted provider confirmed one correction in the next model
request and the matching accepted receipt
(`/tmp/rook-pi-scoped-browser-live-final.log`). This checks queue delivery, not model
quality or completion checking; the scripted provider does not implement the
completion-check protocol.

The fifth block's full isolated `cargo xtask ci` passed with exit status 0 in
531.9 seconds (`/tmp/rook-pi-scoped-ci.log`). The combined CI/compaction process
also exited 0; storage stayed at 4.02 MiB, 37.1x dictionary compression and 5.8x
end-to-end (`/tmp/rook-pi-scoped-compaction.log`). Rust sources were unchanged
during the gate. The browser-only pending-controls correction during its test
phase passed the final Chrome regressions and a repeated real-daemon check;
JavaScript syntax and the rendered page were checked too.

The queue capability remains in progress. Still required: explicit follow-up
boundaries after a complete ordinary turn or whole goal, idempotent admission
for initial prompts, `/goal` controls, plain REPL and early TUI input before a
session ID, local embedded input parity, and restart/compaction/delegation/
acceptance tests for the full lifecycle. The TUI's read-only message detail
still needs scrolling; its editor already has a cursor viewport. These scoped
correction retries do not establish every execution lifecycle boundary.

### Next queue admission boundary

Directed reads identified the existing pieces to reuse for follow-ups:
`execution::Journal::start` reserves one execution per session and persists its
turn ID; `Rook::completed_turn` verifies that the saved outcome belongs to that
turn. The sixth block makes `AgentLoop::begin_turn` commit the prompt and its
execution admission together after prompt hooks permit it. A store transaction
encodes the receipt using the actual event sequence, rather than a sequence
observed before a concurrent writer. Encoding failure rolls back the event,
receipt and session counters. The optional JSON marker contains the prompt's
sequence, object ID and label; existing postcard records do not change. Admission
of an identical prompt into the same execution returns that sequence, while a
different prompt or stale execution owner is rejected. Attachment prompts use
the same path with their existing encoded payload and label. Older execution
JSON without the marker remains readable; absence is not treated as proof that
an old prompt was never admitted.

Focused store and core checks passed for encoding failure, concurrent appends,
concurrent identical admission, interrupted/reopened journals, stale ownership
and old JSON. The full agent-loop integration suite passed, including added
assertions for rejecting prompt hooks, notes before the prompt and attachments.
The existing killed-process recovery scenario now verifies that the original
prompt and its matching admission marker survive together, before resumption
checks unknown operations. Logs: `/tmp/rook-pi-admission-store.log`,
`/tmp/rook-pi-admission-core.log`, `/tmp/rook-pi-admission-loop.log` and
`/tmp/rook-pi-admission-recovery.log`.

The sixth block's full isolated `cargo xtask ci` passed with exit status 0 in
549.1 seconds, including the full TUI suite and doctests
(`/tmp/rook-pi-admission-ci.log`). The combined CI/compaction process exited 0.
Compaction retained 4.02 MiB on disk, 37.1x dictionary compression and 5.8x
end-to-end (`/tmp/rook-pi-admission-compaction.log`). Rust sources remained
unchanged during this gate. The earlier targeted integration compile failure
was a missing qualified `EventKind` in a new test; it was fixed before the
passing targeted run and full gate.

The seventh block pins a managed consumer to `RunIdentity` (run ID and
generation), including a legacy generation derived from the existing creation
time. `AgentLoop`, observed durable inputs, queue acceptance and safe-boundary
checks all carry that identity. Missing or replaced runs stop their old
consumer; old receipt IDs cannot consume an identically named receipt from a
new goal. Acceptance checks the generation under the same mutation lock as
replacement and also rejects a different conversation. Pre-promotion ordinary
messages wait until the loop joins the current runnable goal; a stale goal
consumer cannot accept them. The loop checks again after consuming a batch and
immediately before a work-model request, since context preparation and callbacks
can outlive the goal. Queue references share `Run::identity()` for the legacy
fallback. No persisted fields or transport event shapes change. Embedded Rust
callers now pass `run.identity()` to the consumer APIs rather than only its ID.

Focused tests passed for replacing a promoted goal between observed messages,
replacing it after context preparation but before the model request, identical
receipt IDs across generations, reopen, forgotten goals, conversation isolation,
legacy generation references, and retained ordinary messages across promotion,
pause and replacement. The replacement's fresh loop accepts its own message;
the stale loop issues no further work-model request. Logs:
`/tmp/rook-pi-generation-managed-final.log` and
`/tmp/rook-pi-generation-queue-final.log`.

The seventh block's full isolated `cargo xtask ci` passed with exit status 0 in
551.3 seconds, including the complete TUI suite and doctests
(`/tmp/rook-pi-generation-ci.log`). Rust sources were unchanged during the gate.
The combined CI/compaction process exited 0; storage remained at 4.02 MiB on
disk, 37.1x dictionary compression and 5.8x end-to-end
(`/tmp/rook-pi-generation-compaction.log`).

Follow-up admission still needs a durable queue-to-execution reservation that
names a complete ordinary turn or whole goal, and recovery distinguishing
reserved, admitted and completed work. The new transaction is a prerequisite,
not automatic follow-up execution or frontend submission deduplication. Simply
accepting a steering receipt and calling `run_with` still logs it twice; the
future follow-up path must bind its reservation to this admission instead.

Eligibility must use `agent::finished` for an ordinary completed turn, and the
whole managed run's `Status::Completed` for `/goal`; stage endings, limits,
pause/cancellation and provider retries are not completion boundaries. Live
observers currently treat `Done` as terminal, and replay stops accepting events
after it, so a follow-up chain also needs an explicit continuation boundary that
preserves the session's observer and controls. Stable caller IDs must name both
the submission and its intended goal generation; a retry after replacement must
never silently retarget a new goal. All these changes still require CLI/API/TUI/
browser parity and restart/race tests before the queue capability is complete.

The next integration must carry a reserved execution identity from the queue
into `Journal::start` and this prompt admission, rather than generating another
turn on retry. It also needs to exclude a fresh manual turn while that
reservation owns the session, preserve unresolved operation recovery, and reuse
a recorded outcome after a crash between outcome recording and publication.
The daemon's `work::supervise` currently restarts runnable managed goals only;
ordinary follow-up recovery needs its own durable eligibility scan using the
existing live-session registry. Frontends must keep the observer and input
channels across the continuation and publish terminal `Done` only when the
chain ends. Creating an `AgentLoop` per follow-up is necessary: its secrets,
deadline, token allowance, failed claims and delegation counter are turn-local,
whereas policy, approval/input channels and MCP/LSP/jobs equipment are shared.
The seventh block establishes generation-aware message consumption; the future
follow-up reservation and supervisor handoff still need their own generation
checks and restart tests. Legacy in-memory input remains outside this durable
identity contract. Do not infer completed follow-up lifecycle guarantees from
these consumer checks.

`/tmp/rook-pi-message-queue` retains its older staged baseline and preparation
patch. Main is now authoritative; do not reapply that worktree or merge its diff
against the old HEAD. Main additionally includes promotion/pause handling and
removal of the obsolete daemon field, which are not in that preparation tree.

## Live follow-up integration (eighth block)

Follow-ups now have an explicit completion target and captured goal identity in
JSON queue receipts. They are excluded from steering. The engine reserves the
queue item and execution together, then admits its goal note, prompt, acceptance
and exact prompt sequence in one transaction after hooks allow it. Admission
rechecks goal identity to reject a replacement between reservation and admission.
Format 4 prevents an older runner from consuming these records as steering.

The live root loop and completed-goal worker drain eligible follow-ups using fresh
AgentLoops with shared policy, approvals, MCP/LSP and jobs. Pauses, limits and
failed work do not release them. The typed `follow_up` event retains the observer
and distinguishes consecutive answers. CLI `--follow-up`, TUI/REPL `/followup`,
and the browser button use the common queue; retry identity includes mode and
completion target. Browser reload retains this identity in the existing outbox.

This is live execution integration, not completed queue recovery. Outstanding:
ordinary idle-session supervision with persisted effective workspace/model/effort/stance;
applying frontend model/effort changes at each new follow-up boundary;
resuming a reserved interrupted execution without duplicate prompt/hooks or
unknown-operation replay; explicit continuation lineage for interrupted ordinary
predecessors; worker handoff when a follow-up promotes itself to a new goal;
per-turn outcome history and aggregate reporting (the current final summary is
only the last turn). Queue detail scrolling and legacy admission identity gaps
remain as previously listed. New goals must not inherit stale follow-ups.
Pending messages survive reopen; a stopped unaccepted reservation can be withdrawn
and resubmitted. No autonomous restart guarantee is claimed by this block.

Focused checks cover turn separation, latest edit/withdrawal, identical retries,
step-limit exclusion, whole-goal completion, replacement during reservation,
exclusive execution ownership, durable interrupted reservation, atomic admission,
shared caps and rollback of multi-event admission. Local and daemon PTY checks
also submit `/followup` during a held tool and prove the next request excludes it.
The browser outbox test covers reload/lost replies with the exact mode, target,
ID and text. A real rookd and Chrome with a scripted HTTP provider completed two
separate turns, one accepted follow-up and an identical HTTP retry. The first
browser fixture answered the non-streaming completion classifier with SSE; the
corrected fixture serves JSON and proves actual completion rather than bypassing
that classifier. Logs: `/tmp/rook-pi-followup-browser-outbox.log`,
`/tmp/rook-pi-followup-browser-live4.log`, `/tmp/rook-pi-followup-pty.log`.

The initial full gate found a test mutex held over an await and a channel error
variant grown past Clippy's size threshold; both were fixed. The final isolated
`cargo xtask ci` exited 0 in 543.1 seconds, including the complete PTY suite,
atomic rollback test and doctests (`/tmp/rook-pi-followup-final-ci.log`). Rust
sources were unchanged during that gate. `cargo xtask compaction` also exited 0:
4.02 MiB on disk, 37.1x dictionary compression and 5.8x end-to-end, unchanged
(`/tmp/rook-pi-followup-compaction.log`). The queue capability remains in progress
until the recovery and continuation work listed above is verified.

## Follow-up recovery (ninth block)

The daemon now saves a bounded JSON driver snapshot for workspace, selected model,
effort and stance. Idle supervision reuses the live-session registry and shares
its admission lock with manual prompts, cancellation and goal workers. A bounded
key-only store scan visits `work.followup_scan_sessions` IDs per tick (default 128,
validated 1..4096), with an exclusive cursor that survives deletions and wraps at
the end. Full session-family recovery checks run only for eligible queue entries.
Concurrent starts respect `work.max_parallel_runs`. A frontend settings change is
saved immediately and model/effort are resolved again at each follow-up boundary.
Restart applies current configured rules; transient tool grants are not retained.
Tool-initiated stance changes are persisted at boundaries, not atomically with the
tool. A local-only session without a saved daemon profile is not auto-started.

Lost-owner reservations can reattach to the same execution ID. Prompt admission
also commits session and prompt hook context in its JSON companion, so recovery
restores those inputs without rerunning hooks or appending another UserMessage.
Setup operation completion follows prompt admission, closing the gap where hook
effects could previously look safe to repeat. Recorded outcomes are reused. An
unknown operation still blocks execution until explicit recovery acknowledgement;
old admissions without context and pre-admission completed setup effects require
inspection. Hook context is bounded to 1 MiB before copying it into the admission.
The new JSON fields are optional; existing postcard records and format 4 remain
unchanged. Prompt hook context is now included in the volatile model context,
which the killed-daemon scenario found was previously only written as a Note.
Recovery obtains a fresh turn allowance rather than retaining the lost deadline.

Cancellation persists a paused driver before stopping its worker. An explicit
prompt resumes it; a setting change does not. Recovery failures pause instead of
retrying every tick. Queue pages expose paused/recovery status in CLI, TUI and
browser within the page byte allowance. Error diagnostics and saved settings are
bounded before encoding. A REPL follow-up now opens the explicitly selected
workspace when it loads its queue.

The actual daemon scenario kills a reserved follow-up during a model request,
changes configuration defaults, and restarts it. It verifies the same execution,
prompt and acceptance identity, restored readonly/model/effort settings, hook
context in the next request, no repeated hook or prompt, ordered continuation,
idle submission and cancellation surviving another restart. Its targeted pass is
`/tmp/rook-pi-followup-kill-test4.log`. Focused checks also cover retained pause,
escaped settings limits, exclusive bounded scanning after cursor deletion,
oversized hook context, recorded outcomes and unknown-operation acknowledgement:
`/tmp/rook-pi-followup-driver-tests.log`, `/tmp/rook-pi-followup-scan-tests.log` and
`/tmp/rook-pi-followup-recovery-unit-final.log`.

Chrome using the actual browser modules passed lost replies, reload/retry,
retained submission identity and the visible paused recovery status
(`/tmp/rook-pi-followup-recovery-browser.log`); JavaScript syntax also passed.
The first full isolated gate exited 0 in 589.3 seconds. Final review then removed
a session-hook context clone before size admission. The full gate was repeated
on that final source and exited 0 in 560.2 seconds, including the daemon kill
scenario, complete PTY suite and doctests
(`/tmp/rook-pi-followup-recovery-final-ci.log`). Rust sources stayed unchanged
during each gate. `cargo xtask compaction` exited 0: 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end, unchanged
(`/tmp/rook-pi-followup-recovery-compaction.log`).

The queue capability remains in progress. Interrupted ordinary predecessors still
need explicit continuation lineage; replacement goals must never inherit stale
messages. Goal-ending follow-ups that promote to another goal still need worker
handoff. Durable per-turn outcome history and aggregate reports, read-only TUI
detail scrolling, legacy caller admission identity and old API mutation notices
remain outstanding. The full Pi adoption scope remains the table above.

## Ordinary continuation and goal handoff (tenth block)

Explicit `/continue` keeps an ordinary task's completion boundary across fresh
executions and process restarts. The execution JSON stores one optional root ID;
it does not accumulate ancestors or change postcard records. New prompt and
execution receipts remain independent. Follow-ups submitted during a continuation
use the same root, and only successful completion releases them. Unrelated
prompts, recipe-expanded prompts, checker executions and fresh follow-up
reservations do not inherit an ordinary boundary.

The goal worker now keeps the same observer when a completed goal's follow-up is
promoted to a replacement goal. Existing promotion steering reaches the running
turn at a safe boundary, then the worker advances the new goal. The first managed
stage loads that goal's persisted options rather than the old connection's
options. Pending follow-ups addressed to the old generation remain unaccepted.

Focused checks passed for repeated step limits, queueing during continuation,
reopen with and without explicit continuation, legacy JSON defaults, and separate
prompt receipts. An actual daemon scenario kills the unfinished ordinary
predecessor, verifies it does not restart autonomously, then uses `/continue` and
observes ordered follow-ups with their original receipt identities. Logs:
`/tmp/rook-lineage-unit.log`, `/tmp/rook-lineage-loop.log`, and
`/tmp/rook-lineage-daemon.log`.

The actual goal-handoff scenario observes both independent completion checks,
promotes a held follow-up, checks the new attachment in the first managed stage,
and receives the final result on the original socket. A second queued old-goal
message remains unreserved and unaccepted. Its passing log is
`/tmp/rook-handoff-test2.log`. The first fixture incorrectly expected promotion to
skip steering of the current turn; it was corrected to exercise the existing
promotion boundary before asserting the next managed stage's options. That first
failure is retained in `/tmp/rook-handoff-test.log`.

The final isolated `cargo xtask ci` exited 0 in 601.9 seconds, including all
daemon scenarios, the complete PTY suite and doctests
(`/tmp/rook-lineage-final-ci.log`). Rust sources stayed unchanged during the
gate. `cargo xtask compaction` also exited 0, retaining 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end (`/tmp/rook-lineage-compaction.log`).

The queue remains in progress. Durable per-turn outcome history and aggregate
reports, read-only TUI detail scrolling, legacy caller admission identity, old API
mutation notices and remaining cross-lifecycle/frontend parity checks are still
outstanding. Ordinary-to-goal promotion does not migrate pending follow-ups into
a different goal scope. The full adoption scope remains the table above.

## Recorded turn history and totals (eleventh block)

Each saved outcome now appends an immutable summary/result pair and updates a
constant-size ledger in the same transaction as the latest recovery outcome and
follow-up readiness. The execution ID prevents duplicate accounting on retry.
The existing full-outcome JSON limit remains 8 MiB; summaries have bounded reply
previews, and result bodies are readable through existing byte-pageable history.
Both notes stay outside model context. Postcard schemas and store format 4 are
unchanged; session deletion and retention include the ledger and event objects.

`rook session turns`, TUI/REPL `/turns`, the history viewer's `t` action, the HTTP
turn-results route and the browser history panel expose the same recorded
outcomes and cumulative counters. Pagination bounds decoded records and encoded
page bytes, scans only `transcript.search_events` metadata records, and returns
an advancing cursor even through pages without outcomes. TUI reads use the
existing bounded history worker and discard responses from a previous session.
The browser preserves totals while opening full results and wraps long bodies.

Accounting is explicitly scoped to saved outcomes in the selected session.
The report identifies its first covered prompt, excludes inherited branch
outcomes from new-execution totals, and distinguishes completed turns from limits.
Token counts are provider reports, cached input is not added twice, and elapsed
time is a sum of execution wall-clock spans. Pre-upgrade history, attempts without
a saved outcome, and usage lost before a process resumed are not backfilled.
Full accounting of interrupted attempts and live-chain totals remains outstanding;
the final live Done event still describes the final turn.

Focused tests passed for restart, idempotent recording, independent fork totals,
session deletion, byte/entry/scan bounds, cursor progress, escaped Unicode and
rollback before committing an oversized result (`/tmp/rook-turn-results-tests.log`).
The agent-loop suite initially found two assertions that expected the old event
shape; both now check the additional summary/result pair explicitly. The loop's
continuation scenario also checks all saved outcomes and separate completion counts.
That initial failure is `/tmp/rook-turn-results-loop.log`.

The actual killed-daemon continuation scenario checks result identity and matches
HTTP output to CLI reads both with and without the daemon
(`/tmp/rook-turn-results-daemon.log`). Local and daemon PTY scenarios passed for
paging, full-result inspection, direct `/turns`, history switching and preserved
drafts (`/tmp/rook-turn-results-pty2.log`). The first PTY fixture sent Escape and
the next text without allowing terminal escape disambiguation; separating those
actions fixed the fixture (`/tmp/rook-turn-results-pty.log`). Chrome with an actual
daemon and scripted provider passed totals, paging, full results, viewport fit
and draft preservation (`/tmp/rook-turn-results-browser2.log`); the rendered page
was inspected. Its harness is `/tmp/rook-pi-browser-check/turn-results-live.cjs`.

The full Pi scope remains unchanged. The queue still needs the remaining admission
identity/parity work, read-only queue detail scrolling, old API mutation notices,
and cross-lifecycle coverage; the other pending capabilities remain in the table.

The final isolated `cargo xtask ci` exited 0 in 635.4 seconds, including the full
PTY suite, daemon recovery/result parity scenario, agent-loop tests and doctests
(`/tmp/rook-turn-results-final-ci.log`). Rust sources stayed unchanged during
the gate. `cargo xtask compaction` also exited 0, retaining 4.02 MiB on disk,
37.1x dictionary compression and 5.8x end-to-end
(`/tmp/rook-turn-results-compaction.log`). JavaScript syntax and the corrected
real-daemon browser scenario passed before the gate.

## Validation environment

macOS showed long cold-start pauses before test code executed: a process sample
contained only `_dyld_start`. A list-only launch of a copied test binary took
about 25 seconds; its repeat took about 0.01 seconds. This did not establish the
OS cause. A parallel list-only probe also encountered long startup waits and was
stopped; only those diagnostic processes were stopped. No check was waived.
Use isolated build targets for simultaneously active Rook worktrees to prevent
artifact interference. The external shared target is also used by another project;
do not clean it or stop that project's builds.

## Conversation tree navigation

The first branch block adds lazy navigation over existing session parent links.
`rook session tree ID` and `GET /api/sessions/ID/tree` expose the same bounded
ancestor/selected/direct-child page. The TUI has `/tree`, history `v` and
sessions `b`; the browser has branch panels in Chat and Sessions. Explore, read
history and continue are distinct actions. A plain REPL prints the tree page.
See [conversation branches](../conversation-branches.md) for keys and limits.

The tree preserves existing IDs and record formats. Count, encoded-byte, scan
and ancestry limits apply to each page. Sparse child scans return a cursor even
when no direct child is encountered; deleting the cursor session does not invalidate
its position. Missing parents, truncated metadata and earlier ancestors are
explicit. Cycles encountered in the loaded ancestry are rejected. Delegated tasks are labelled separately.
Ordinary forks now record their exclusive boundary, fixed before copying so
a concurrently growing parent cannot change the intended cutoff. Existing
unmarked forks remain unknown rather than inferring a boundary from a child's
current length. Browsing and switching do not restore workspace files.

Continuing a TUI branch preserves the text draft and uses the selected node's
ID directly. Browser switching detaches the old observer and attaches the new
one, ignoring queued messages and close notifications from the old socket.
A daemon-owned turn in the departed branch keeps running.

Core checks cover count/byte/depth limits, escaped metadata, sparse scans,
deleted cursors, absent/cyclic parents, fork boundaries and workspace retention.
The real CLI produces matching local/daemon pages. A PTY scenario exercises
paging, ancestors, sibling history and explicit continuation in both local and
daemon modes, retaining the draft and leaving stored event counts/files intact.
The Chrome scenario uses an actual scratch daemon and scripted provider; it
checks Chat/Sessions panels, pagination, history, continuation, mobile wrapping
and leaving a live turn without cancelling it or receiving its late response
in the selected branch. A separate browser protocol scenario checks recovery
forms and stale socket messages. Logs are `/tmp/rook-branches-core3.log`,
`/tmp/rook-branches-cli.log`, `/tmp/rook-branches-pty.log`,
`/tmp/rook-branches-browser3.log` and `/tmp/rook-branches-recovery2.log`.

The branch capability remains **in progress**: optional attributable summaries,
branching/editing from a selected historical message (including attachment
semantics), session naming and event bookmarks still need implementation and
verification. Other pending capabilities in the table remain in scope.

The final `CARGO_TARGET_DIR=/tmp/rook-pi-ci-target cargo xtask ci` and
`cargo xtask compaction` completed with exit status 0. CI reported 597.9 seconds;
logs are `/tmp/rook-branches-final-ci2.log` and
`/tmp/rook-branches-compaction.log`. The previous gate was deliberately stopped
during review to fix the fork cutoff race, and is not counted as a pass.

## Editing historical messages into branches

The next branch block exposes `rook session branch ID EVENT`, the matching HTTP
POST, TUI history **Shift+B**, and browser history actions in Chat and Sessions.
A user event is excluded from the copied prefix and returned as a complete
editable draft; other events are included and start with an empty editor.
Selection creates the branch without calling the model. Existing drafts and
attachments are protected, and neither the source conversation nor workspace
files are changed. The operation is not idempotent; uncertain responses require
inspecting the tree before retrying.

Attachment records retain optional JSON metadata for the admitted prompt and
original attachment names/text. Image payloads occur once and are referenced by
index. Replay still reads the original Message shape, including in older readers;
postcard records are unchanged. Legacy records return complete prepared text
with an explicit notice and retained images. Recipe-expanded text is preserved,
not the recipe invocation or output settings. Oversized/invalid drafts fail
before creating a branch: `branches.edit_bytes` bounds text, and encoded records
and responses have an additional 16 MiB ceiling.

The browser displays historical attachment names and keeps them across tab
changes. Sending consumes them once. JSON frame admission counts UTF-8 and
escaping before serialization; refusal keeps both text and attachments.
`node --test web/tests/json-size.mjs` checks exact encoded boundaries, Unicode,
escaped oversized input and deep nesting. It passed; log:
`/tmp/rook-event-branch-json-size.log`.

Core checks cover exact event boundaries, complete Unicode drafts, legacy/new
attachment records, rejected oversized and inconsistent metadata, and actual
edited model requests. CLI checks exercise local and daemon paths. PTY checks
exercise draft refusal, editor loading, Enter submission and image retention in
both modes. Chrome with an actual scratch daemon and scripted provider checks
both history panels, tab changes, duplicate context prevention, assistant-event
boundaries and oversized submission refusal without losing attachments.
The rendered draft was inspected. The harness and final browser log are
`/tmp/rook-pi-browser-check/event-branch-live.cjs` and
`/tmp/rook-event-branch-browser3.log`.

The first gate was deliberately stopped to remove serialization before the
browser's size check. The next full gate exited 1: the existing follow-up-to-goal
scenario did not observe a tool-role message in its second checker request.
Its original assertion did not print that request. The assertion now prints it
without weakening the condition. The complete CLI suite and ten subsequent
isolated executions passed; independent API probes with and without hooks also
passed. The cause of the original failure remains unresolved and belongs in the
remaining queue lifecycle investigation. Evidence: `/tmp/rook-event-branch-ci2.log`,
`/tmp/rook-event-branch-cli-diagnostic.log`, `/tmp/rook-promotion-repeat-*.log`,
`/tmp/rook-promotion-probe.log` and `/tmp/rook-promotion-probe-hooks.log`.

The final isolated `cargo xtask ci` and `cargo xtask compaction` process exited 0.
CI reported 621.3 seconds (`/tmp/rook-event-branch-ci3.log`). Sources stayed fixed
during that gate. Compaction retained 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end (`/tmp/rook-event-branch-compaction.log`).

The branch capability remains in progress: names, bookmarks, bounded-memory
forking and optional attributable summaries follow in the next block. Other
pending capabilities retain their full scope.

## Branch names, event bookmarks and long TUI input

Branches now have editable UTF-8 names in the CLI, TUI and browser. Event
bookmarks are ordered, bounded per session, atomically updated under concurrent
windows, and visible even when the event was later pruned. A fork inherits only
marks whose source events it actually copied. The store fork path streams the
source range from a read snapshot into the child rather than retaining every
event record in a temporary vector. The index uses a bounded JSON companion;
postcard records and the store format are unchanged.

The TUI's palette, memory, checkpoint-name and history inputs now grow for long
lines and show counts of hidden rows after reaching their viewport cap. The
chat composer also reports hidden rows. Oversize history-field pastes report a
limit instead of disappearing without feedback. A real PTY test covers a long
bracketed paste in the composer, a long branch title and an over-limit label.

Core tests cover reopen, fork boundaries, deletion, concurrent marks and both
bookmark limits. The CLI test exercises local and daemon paths; the PTY test
does the same for branch editing. An actual daemon and Chrome scenario covers
browser rename, mark, jump and remove, followed by historical message branching
and attachment retention. These focused tests passed. The final
`cargo xtask ci` exited 0 in 639.2 seconds; its 58 PTY scenarios all passed.
`cargo xtask compaction` also exited 0 and measured 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end. Evidence:
`/tmp/rook-branches-names-final-ci.log`,
`/tmp/rook-branches-names-final-compaction.log`, and
`/tmp/rook-branches-names-browser.log`. Optional attributable branch summaries
remain in progress; the rest of the Pi scope remains open.

## Reviewed branch-summary transfer (next branch block)

A user can explicitly carry a reviewed, 16 KiB UTF-8 summary from one session
to another in the same workspace. The target records a `branch-summary` Note
with the source session and its last saved event; replay wraps the text as
source data and says that historical file and test observations require
verification in the current workspace. History displays the attribution rather
than the raw record. No postcard field or store format changed. Oversized text
is refused before storage encoding, and replay/history check the record size
before reading it. The CLI command and HTTP route share core validation. TUI
and REPL accept `/summary TARGET_SESSION text`; the browser offers an optional
review editor on each other branch before continuing there.

This block does not generate the draft summary from abandoned events. That
remains the branch capability's open item, along with final cross-frontend
verification. Queue work and all Pending rows above remain in scope.

The core replay/boundary test and CLI integration test passed. The latter wrote
one summary locally and another through an actual daemon, then read attributed
target history. `node --check web/dist/branches.js` passed. On this Windows
runner the PTY target contains zero runnable tests, so a live TUI interaction
was not established here. The first full gate found a new Clippy warning in an
existing store test. The second full gate found an unused public helper after
the context refactor and a Unix-only command in an existing agent-loop test;
both failed checks were fixed and passed individually. The final `cargo xtask ci`
exited 0 (`ci: ok`, 335.5 seconds). `cargo xtask compaction` exited 0 and kept
the published 4.02 MiB, 37.1x dictionary and 5.8x end-to-end measurements.

## Read-only queue detail scrolling

The TUI queue panel now scrolls the full read-only message with PageUp/PageDown
or the mouse wheel. The offset resets when a different receipt or page is
selected; the edit field keeps its own cursor and scroll behavior. The rendered
row count and a sparse byte index are cached by viewport width, so a long
message is not measured anew or copied into a display string on every frame.
The same panel works with local and daemon queue reads; no
queue protocol or stored record changed. A TestBackend check reached the last
line, scrolled back with the wheel, reached a row beyond the widget's 65,535-row
scroll limit, and verified that the receipt text stayed unchanged. The Windows
PTY target is disabled, so live TUI key delivery in both
modes remains to be checked on a Unix runner.

Queue admission identity for legacy callers, live notices from old mutation
routes and remaining lifecycle/frontend parity cases are still open. The
broader Pi adoption scope remains the table above.

Both focused queue-render tests exited 0. The final `cargo xtask ci` exited 0
(`ci: ok`, 388.0 seconds), including the queue unit tests. No store format or
retention code changed, so the preceding block's compaction measurement remains
the latest storage check.

## Legacy queue mutation notices

The older session and managed-work HTTP instruction routes now publish receipt
updates to attached live views for submission, edit and withdrawal. They keep
their original JSON response shapes. Each notice carries the text, revision and
reference returned by the same committed mutation. Goal references capture the
run generation while its update lock is held, so a replacement goal cannot be
used to label an older mutation. Failed revisions publish nothing. The combined
queue route shares the same live publication helper. Receipt-only embedded Rust
functions remain available for compatibility.

Focused core checks passed for ordinary and goal notice identity, and the HTTP
route test passed with a running observer in both scopes, checking successful
and rejected mutations. Queue admission identity for legacy callers and broader
lifecycle/frontend parity remain outstanding, as do all Pending rows above.

The full `cargo xtask ci` exited 0 (`ci: ok`, 394.8 seconds). A final
`cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. No postcard schema or store format changed.

## Socket correction receipt identity

The chat socket's `prompt` frame now accepts an optional caller-owned `id`.
For a correction while an ordinary turn or a conversation goal is running,
the daemon uses this ID in the existing durable steering receipt. Repeating
the same ID and original text returns the same receipt, including after an
edit or acceptance; conflicting text is rejected. Older frames without `id`
still get a generated ID. The daemon validates a supplied ID's 64-byte bound
and alphabet before copying it. CLI and TUI socket senders generate IDs for
their frames; the browser already sends its in-flight corrections through the
scoped HTTP queue.

A real-daemon socket test passed for duplicate and conflicting corrections in
both ordinary and goal scopes, and for a legacy frame without an ID. The
socket ID alone does not pin a goal generation, and the CLI/TUI do not retain
that ID for manual retry after an uncertain disconnect. Use the scoped queue
with its saved target for that case. Initial prompt, `/goal` admission and
first-session creation still lack caller-owned retry identity. Queue remains
In progress; branch draft generation and all Pending rows remain open.

The focused daemon test exited 0. The full `cargo xtask ci` exited 0
(`ci: ok`, 412.1 seconds). No storage code or format changed, so the previous
block's compaction check remains the latest one.

## Reviewable branch-summary evidence draft

The summary editor can now load bounded recorded excerpts from the departing
branch. Core scans at most 128 recent event records, selects at most 12 text
messages, and reads at most 768 body bytes per excerpt; attachment envelopes
and tool payloads are excluded. The response identifies source session, last
event and truncation/coverage, and its text remains within the existing 16 KiB
summary limit. This is an evidence draft for a person to rewrite, not a model
conclusion. CLI `session summary-draft SOURCE TARGET`, REPL/TUI
`/summary-draft TARGET`, HTTP `GET .../summary-draft?source=SOURCE`, and the
browser's summary editor expose the same core result. The browser sends the
loaded source boundary when saving; the daemon rejects a stale draft if new
source events arrived. Existing manual summary calls and stored format remain
compatible.

The focused core boundary/size test and local plus real-daemon CLI test passed;
`node --check web/dist/branches.js` exited 0. Model-generated synthesis and
live browser/TUI interaction checks remain open, so the branch row stays In
progress. Queue first-prompt admission and all Pending rows also remain open.
The full `cargo xtask ci` exited 0 (`ci: ok`, 438.7 seconds). The required
`cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. No postcard field or store format changed.

## Scoped socket correction retries

The socket `prompt` frame now also accepts optional `target` alongside its
caller ID. When present, the daemon submits through the same combined queue
admission as HTTP, using the saved `submission_target`; a stale goal generation
or ordinary-session target is rejected before creating a receipt. Old frames
without either field retain their behavior. The target is length/alphabet
checked before copying. CLI/TUI senders keep using the compatible unscoped
path, since they do not persist a target across reconnect; browser corrections
already use scoped HTTP. A real-daemon socket test now covers scoped ordinary
and goal duplicate requests, a rejected stale goal target and a legacy frame.

Initial prompt and goal-control admission identities, CLI/TUI retry retention,
and remaining queue lifecycle/frontend parity remain open. The queue row stays
In progress; branch semantic synthesis and all Pending rows remain open.
The focused real-daemon socket test exited 0. The first full CI exited 1:
one existing model-catalog fixture failed under the parallel CLI suite, while
the other 69 CLI scenarios passed. Its test server previously assumed a whole
HTTP request line arrived in one read and allowed only short timeouts. The
fixture now reads the bounded line and gives the unrelated catalog operation
more time under load; the isolated test exited 0. The second full
`cargo xtask ci` exited 0 (`ci: ok`, 389.3 seconds). `cargo xtask compaction`
exited 0 with 4.02 MiB on disk, 37.1x dictionary compression and 5.8x
end-to-end. Stored receipt and session formats remain unchanged.

## Pinned next-message preview in the TUI

During a running turn, the first pending message now stays in a short panel
above the prompt while model output grows or the conversation is scrolled back.
The TUI obtains its ordering and count from the existing bounded combined queue
page, off the terminal thread, and retains only 160 characters of the first
message. It refreshes after receipt changes and every two seconds to catch
another client. An unconfirmed local submission appears immediately; before the
first session ID is assigned, the in-memory interjection has a bounded preview.
Opening `/queue` or submitting during a background read is deferred until that
read finishes, preserving the draft and the panel action. Background refreshes
pause while the queue panel is open so editing keys are not delayed. Small
terminals keep the composer and approval controls ahead of the optional preview.

Focused tests cover ordering, session isolation, stale snapshot rejection,
opening the queue during refresh, and submitting during refresh. The first
`cargo xtask ci` exited 1 because the `no_panics` gate rejected a new bare
`unreachable!`; the preview now handles that variant without a panic. The
targeted `no_panics` and TUI queue suites each exited 0, and the final full
`cargo xtask ci` exited 0 (`ci: ok`, 384.3 seconds). The Windows PTY target
contains zero runnable tests, so a live TUI screen check remains for Unix.
This is a display change using
the existing local and daemon queue-page paths; no stored or wire format changed.
The queue admission and lifecycle gaps above, branch synthesis, and all Pending
rows remain open.

## Model-assisted branch summary draft

An explicit request can now ask the configured model to condense the existing
bounded source excerpts. The model receives at most the 128-event/12-excerpt
evidence draft as untrusted historical data, with no tools. The response is
limited while streaming, before a second copy is retained, and the returned
draft names the source session and event boundary with a current-workspace
verification warning. Generation never writes the target branch. A person
reviews and edits the text, then saves it through the existing attributed
`branch-summary` event. Pinned source boundaries reject a source that changed
between draft and save. Stored records and old API payloads remain compatible.

CLI `session summary-draft SOURCE TARGET --suggest`, local and daemon REPL/TUI
`/summary-suggest TARGET`, HTTP `POST .../summary-suggest`, and the browser's
**Generate suggested summary** button share the core behavior. CLI
`session summary ... --source-through EVENT` and interactive `/summary-at`
allow an explicit reviewed boundary; browser, TUI and daemon REPL drafts pin it
automatically. The TUI runs model generation off its drawing thread. The CLI
argument parser now runs on a sized stack because the additional option
exceeded the Windows main thread's reserve during Clap parsing.

The focused core test passed for attribution, byte bound and no target write.
The real local plus daemon CLI suggestion test passed with a scripted provider,
including no automatic save. Live browser and TUI interaction checks are still
needed before the branch row is Complete. Pi also offers its branch summary
during navigation; Rook still requires an explicit command/button. Queue gaps
and all Pending rows remain in scope.

The first full `cargo xtask ci` exited 1 on Clippy's `needless_question_mark`
in the new CLI parser wrapper. The corrected tree passed a full gate with exit
0 (`ci: ok`, 389.8 seconds). After TUI and daemon REPL excerpt drafts were made
to pin their source boundary too, the final full `cargo xtask ci` exited 0
(`ci: ok`, 454.0 seconds). `node --check web/dist/branches.js` exited 0.
This block changes neither store formats nor storage algorithms, so no new
compaction measurement was required. The Windows PTY target contains zero
runnable tests; live TUI interaction remains unverified here.

## Branch summary scoped to the shared prefix

The evidence draft now traverses bounded session ancestry, locates the nearest
common ancestor, and takes the earliest exclusive fork boundary on both paths.
Only source events after that shared prefix are eligible for the existing
128-event/12-excerpt scan. A source with no text after a known boundary yields
an error instead of presenting copied history as a branch delta. Unrelated
complete lineages allow the whole source. Old forks with missing boundaries
remain usable, but both the draft and browser notice explicitly say that the
divergence is unknown and excerpts may include shared history. Cyclic or
overlong ancestry is rejected. The scope fields are additive JSON fields with
defaults for older daemon replies; saved `branch-summary` events and store
formats are unchanged.

The core tests cover sibling and nested forks, source/target ancestor direction,
no unique text, legacy boundary marks, and older JSON. The CLI integration test
checks that local and real-daemon drafts exclude the shared event and agree on
`source_from`. The browser script passes `node --check`. The full
`cargo xtask ci` exited 0 (`ci: ok`, 423.8 seconds). No storage implementation
or format changed, so compaction was not rerun. The optional offer during navigation and live
TUI/browser checks are still required for the branch row. The queue gaps and
all Pending rows remain open.

## Browser offer at branch navigation

The browser's **Continue in chat** action now offers an optional summary when
the chosen branch differs from the open one. A person can open the existing
review editor, continue without a summary, or cancel. The offer itself makes no
model request and writes no history; the editor still saves only after explicit
review. Continuing the same branch remains direct. The selection keeps the
current unsent chat draft under the existing browser behavior. A focused DOM
interaction test covers the review and skip paths without a summary write.
TUI/REPL navigation offers and live browser interaction are still outstanding;
the branch row remains In progress, as do queue gaps and all Pending rows.

The first full `cargo xtask ci` exited 1: an unrelated ACP tool-call test used
a fixed four-second drain and observed only `usage_update` under the parallel
Windows test load. The same test passed in isolation. It now waits for the
actual `tool_call` or final response, with a 30-second deadline, and passed the
targeted rerun. The corrected tree passed the full `cargo xtask ci` with exit
0 (`ci: ok`, 331.2 seconds). `node --test web/tests/branch-offer.mjs
web/tests/json-size.mjs` passed all six tests, and `node --check
web/dist/branches.js` exited 0. Store formats and algorithms did not change,
so compaction was not rerun for this block.

## TUI branch navigation offer

The TUI tree now holds the current chat session as the departing source while
exploring other nodes. Pressing `c` on another branch offers bounded recorded
excerpts (`d`), a model suggestion (`s`), explicit continuation without a
summary (another `c`), or cancellation (Esc). A selected draft is returned to
chat for review and still requires `/summary TARGET TEXT` before switching.
The offer and skip path write no history. The existing local and daemon summary
draft routes serve the same worker command, with source attribution and the
shared-prefix bound from the preceding block. Unit tests verify offer, cancel,
skip, and that both draft choices target the open source session. Live TUI
interaction and a REPL navigation offer remain to verify; the branch row is
still In progress. Queue gaps and all Pending rows remain open.

The two focused TUI offer tests exited 0. The first full `cargo xtask ci`
exited 1 in the CLI daemon suite: two existing killed-process follow-up
scenarios timed out waiting for their scripted model under unrestricted
parallel tests. Both passed separately. A full rerun with
`RUST_TEST_THREADS=4` still exited 1 on one of them. The same complete gate
with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 728.1 seconds), including all
72 CLI integration tests and both follow-up scenarios. This records the
parallel-run instability rather than counting the failed gates as passes.
The block changes no store format or algorithm, so compaction was not rerun.

## REPL branch navigation offer

The local and daemon REPL now offer a reviewed summary when `/session ID`
selects another conversation. The first command prints the departing source,
target and review commands without switching or writing history. Existing
`/summary-draft`, `/summary-suggest` and `/summary` paths supply the draft and
explicit save. Repeating the same `/session ID` continues after review or skips
the summary. A different target replaces the pending offer; an unrelated
command, prompt or Ctrl-C clears it. The daemon REPL can now resolve and switch
sessions through its routed source, attaching its socket to the selected one.
The integration test checks reviewed transfer and no-write skip in both local
and real-daemon modes. Live browser/TUI interaction checks and the queue gaps
still remain, along with every Pending row in the table. No store format or
algorithm changed. The focused real-process test exited 0. The full
`cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 667.5 seconds),
including all 73 CLI integration tests. The single-thread setting addresses
the previously observed parallel daemon-test timeouts. No compaction rerun was
needed for this block.

## Queue preview at the bottom of the TUI

The existing bounded next-message preview now occupies the bottom of the chat,
directly above the status line. The draft composer and any approval controls
sit above it. A wrapping draft or continuing model output therefore cannot
move the preview away from the lower edge. The focused layout test checks both
draft heights and an approval panel; it exited 0. The queue source, admission
rules and storage format did not change. Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 679.8 seconds); the Windows PTY
target still contains zero runnable tests. No compaction rerun was needed.
The initial-prompt and goal-control
retry identities, retry retention, cross-lifecycle coverage, branch synthesis
and Pending rows remain open.

## Caller identity for an ordinary socket prompt

The socket's existing optional `id` now covers an ordinary prompt that starts
a turn, including the first prompt with no session ID. A bounded 75-byte claim
stores the session, payload fingerprint, admission state and execution turn.
Creating the first session and its claim is atomic; the execution journal binds
the claim to a turn before hooks or model work; accepting the UserMessage and
marking the claim admitted is one transaction. A repeat of the same ID, text
and options joins that live turn or gets an explicit `already_admitted` terminal
acknowledgement. The acknowledgement does not invent a reply or claim the old
turn's counters. A turn interrupted before prompt admission requires recovery
inspection before a new request, because its hooks may already have run. A
changed payload is rejected. Session deletion removes its
claims. The browser now supplies a caller ID for a new prompt; CLI and TUI
already supplied one. Named sessions admit at most `work.max_messages` prompt
claims; retries of saved IDs still work at that limit. Older frames without an
ID still work.

Focused core tests cover first-session atomicity, payload conflicts, admitted
events and deletion, plus isolation between named sessions. A real-daemon socket
test covers retries while running and after completion for both a new and an
existing session, conflict rejection, one model request per prompt and no
correction queue entry. The first run after the journal binding change timed
out waiting for the scripted provider under a daemon rebuild; a subsequent
run exited 0, as did the core tests. The first full `cargo xtask ci` exited 1 at the clippy gate:
the new prompt admission and daemon turn-start functions each exceeded the
seven-argument lint. The claim is now owned by the execution journal and the
turn-start variant. The final full CI with `RUST_TEST_THREADS=1` exited 0
(`ci: ok`, 634.4 seconds), including all 74 CLI integration tests. Required
`cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. No postcard schema or store format version
changed.

This does not yet make `/goal` creation and controls idempotent. CLI/TUI and
browser still need retained IDs and an explicit retry action after uncertain
delivery; correction IDs also need lifecycle coverage across restart. Queue
remains In progress, as do branch live checks and all Pending rows.

## Caller identity for socket goal creation

The optional socket `id` now also guards `/goal` creation, for both a new
conversation and a named session. A bounded claim uses the existing 75-byte
record and stores the managed run generation as its admission owner. The goal
note, current goal value, managed run, run index and admitted claim commit in
one store transaction. A retry of the same text and options joins its live
generation or receives `already_admitted` without fabricating an old result;
it cannot create a second generation. An ID reused with changed text or
options is rejected. A pending claim faced with a different active goal is
held for inspection. Old frames without an ID still use the existing path.

Focused core tests verify atomic admission, generation identity and the goal
event. A real-daemon WebSocket test covers live retry, conflicting payload,
retry after cancellation, unchanged generation and the existing goal notes.
The core and daemon tests exited 0. This block does not cover caller-owned
control IDs, explicit CLI/TUI/browser retry after uncertain delivery, or
correction receipts across restart. Queue remains In progress; branch live
checks and all Pending capabilities remain open.

The first full CI run exited 1 at a Clippy nested-condition warning. After
that syntax fix, full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0
(`ci: ok`, 659.4 seconds), including all 75 CLI integration tests. Required
`cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. No stored record size or wire schema changed.

## Browser retry of an uncertain socket prompt

The browser now retains one exact prompt frame, including caller ID, original
session target, text and options, and exposes Retry saved prompt and Discard
saved prompt beside the composer. A retry resends that frame. The bounded
outbox attempts tab session-storage persistence across reloads; if the browser
rejects a large frame for quota, the loaded tab still retains it. A new prompt
waits until the saved candidate is resolved. A start and completion seen on the
same connection clear a known prompt; an ordinary `Done` after reconnect does
not prove which caller ID it acknowledged, so the candidate remains until
explicit discard or duplicate-admission acknowledgement. This can leave a
retry button visible after successful work; repeating that ID is safe and
produces the recorded-admission response. Queue submissions keep their separate scoped
outbox.

The focused Node tests cover exact-frame restoration, retry identity, known
completion, explicit discard, new-prompt admission and storage-quota fallback.
Browser syntax and the embedded-module
test are checked. No wire or store format changed. CLI/TUI manual retry,
goal-control identity and correction lifecycle tests still remain. The queue
row stays In progress; branch live checks and Pending rows remain open.

All nine targeted Node tests, JavaScript syntax, and the `rookd` embedded-module
test exited 0. Full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0
(`ci: ok`, 602.6 seconds). This block changed only browser assets and docs,
so it did not rerun compaction. A live manual Chrome interaction check remains
open; the embedded-module check confirms that the daemon serves the new file.

## Correction receipt across a daemon restart

A real-process test now holds a conversation goal during a model request,
submits a caller-owned correction, kills the daemon, reads that receipt through
the local `rook session queue` path, and starts a new daemon. Retrying the exact
socket frame returns the original reference, ID and submission timestamp. The
managed run keeps its generation, the combined queue contains one correction,
and reusing that ID with different text is rejected. The focused test exited
0 after correcting its initial CLI command path (`session queue`, not a root
`queue` command). This establishes retry identity across one interrupted goal
run and both store access paths. It does not yet cover acceptance after the
restart, goal replacement, CLI/TUI prompt retry, or caller-owned goal controls.
Queue remains In progress; all Pending rows and branch live checks remain open.

The full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
687.2 seconds), including all 76 CLI integration tests and doctests. No
production storage code changed, so compaction was not rerun for this block.

## TUI retry of an uncertain socket prompt

The TUI now retains one exact daemon prompt frame while its process remains
open. It measures the JSON frame before copying the text and options into the
outbox, with the existing 16 MiB socket limit. `/retry` reuses the original
session target, ID, text and options, reconnecting if the old daemon channel
closed; `/discard` explicitly frees the candidate. A new prompt is held until
the candidate is resolved. The TUI clears it after observing its own start and
completion or an `already_admitted` acknowledgement. Connection and server
failures leave it available. The retry commands appear in TUI completion,
palette and help. The retained frame is in process memory, so a closed TUI
cannot restore it; the plain REPL still needs a retry action.

Focused tests cover exact retry identity, uncertain and known endings, payload
conflicts, size enforcement before cloning, and TUI command discovery. They
exited 0, as did targeted Clippy. A live TUI interaction check remains open;
the real-daemon protocol admission is covered by the earlier WebSocket tests.
Queue remains In progress, alongside caller-owned goal controls and remaining
lifecycle coverage. Pending capability rows and branch live checks are open.

The first full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 1 because
`killed_followups_resume_once_with_saved_settings_and_cancelled_ones_stay_stopped`
timed out waiting for its mock model. That unrelated integration test passed
alone on retry (exit 0). The second full CI exited 0 (`ci: ok`, 667.9 seconds),
including all 76 CLI integration tests and doctests. No storage code changed,
so compaction was not rerun. The existing bottom pinned queue preview and its
draft growth and approval layout test still pass; a live Windows TUI interaction
check remains open.

## Plain REPL retry of an uncertain daemon prompt

The daemon-backed REPL now saves one bounded prompt frame before sending it.
`/retry` resends the same session target, caller ID, text and turn options;
`/discard` explicitly frees it. A new prompt and changes to the session or
connection settings wait until the saved frame is resolved. The retry opens a
fresh socket, resolves the daemon's current address after a restart, and
reapplies the small setting snapshot from the original request. Terminal
failures print their reason and retain the prompt; an observed start and
completion or `already_admitted` clears it. The frame lives only as long as
the REPL process. Local mode reports that no daemon prompt is saved.

The existing frame identity tests cover exact bytes, bounded copying and
uncertain completion. New real-daemon REPL tests cover failure, blocked new
input, explicit retry/discard, retry after a daemon restart on a new port, and
reuse of a newly created session after its first prompt fails.
This changes no wire or store format. Queue remains In progress: caller-owned
goal controls, further lifecycle coverage and live frontend checks remain.
Branch live checks and all Pending rows remain open.

The first full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 before the
new-session failure case was added. The next attempt exited 1 at a Clippy
`collapsible_if` warning; after that fix, one existing follow-up recovery
integration test timed out in the full suite and the gate exited 1. That test
passed alone (exit 0, 33.9 seconds). The final full CI exited 0 (`ci: ok`,
742.7 seconds), including all 79 CLI integration tests and doctests. No
storage code changed, so compaction was not rerun. A closed REPL still cannot
restore its in-memory outbox; live interactive terminal checks remain open.

## Retryable managed goal controls

`POST /api/work/{id}/control` now also accepts a caller-owned control ID,
goal generation and action. The action and bounded receipt commit in the same
managed-work record. An identical retry returns the current run without
reapplying an earlier pause, resume or cancel; a conflicting action or stale
goal generation is rejected. The old bare-action request and bare-run response
remain supported, and old records deserialize with an empty receipt list.
The CLI prints the ID and generation before sending, then accepts both as
retry arguments. A pre-generation stored run falls back to the legacy action
route. Receipts are capped at 1,024 per run, with duplicate lookup preceding
the cap check. The core tests cover reopen, replacement, terminal retry and
the cap; a real-daemon CLI test covers restart, conflict, unchanged legacy
HTTP shape and a retry after a later resume.

Queue remains In progress: live chat/browser goal controls still use their
existing protocol, and more queue lifecycle and frontend checks remain.
Branch live checks and all five Pending capability rows remain open. The TUI
already pins the next queued message directly above the bottom status line
while a turn streams; the composer and approvals appear above that preview.

Focused core and real-daemon CLI tests exited 0. The full `cargo xtask ci`
with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 741.0 seconds), including all
80 CLI integration tests, the TUI bottom-preview check, Clippy and doctests.
The required `cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end. The saved managed-work JSON has
one optional bounded receipt list; postcard and existing HTTP forms did not
change.

## Socket continuation control identity

A chat socket `/continue` prompt already carries a caller-owned ID in current
TUI, REPL and browser clients. When it resumes a paused managed goal, the
daemon now uses that ID and the observed goal generation with the identified
control receipt. A delayed retry after a later pause finds the original resume
receipt and leaves the newer pause intact. Prompts without IDs and older goals
without a generation keep the legacy resume path. This changes no socket
frame or stored record format beyond the optional receipt list added above.
The real-daemon test starts a conversation goal, resumes it by socket, pauses
again, restarts the daemon, and retries the exact prompt. The second resume is
rejected as already applied while the goal remains paused; an ID-less legacy
prompt still resumes. Socket cancel/pause commands do not yet carry caller IDs,
and payload identity for a retry of an already resumed prompt still needs
broader lifecycle coverage. Queue stays In progress; branch live checks and
all Pending rows remain.

The focused real-daemon scenario exited 0 with both identified and legacy
continuation frames. The first full `cargo xtask ci` exited 1: the existing
`continuing_a_killed_predecessor_releases_its_followups_only_after_completion`
test exceeded its 90-second mock-model wait while the other 80 CLI tests
passed. That test passed alone on retry (exit 0, 60.0 seconds). The next full
CI exited 0 (`ci: ok`, 703.8 seconds), including all 81 CLI integration tests
and doctests. This block changed no storage code, so compaction was not rerun.

## Socket Stop scoped to the observed goal

The chat socket accepts an additive `stop` frame with a caller ID and optional
managed-goal generation. The daemon announces the current nonterminal goal
generation on start, attach and goal handoff; live replay retains the latest
identity even when older output is evicted. TUI Ctrl-C and browser Stop send
this frame. Repeated presses during one observed turn reuse the same ID.
For a goal, Stop uses the existing atomic identified Pause control and rejects
a missing or stale generation. A duplicate after a later resume reports that
the Stop was already applied and leaves the resumed goal alone. An ordinary
turn accepts an identified Stop without a generation. The legacy `cancel`
frame remains accepted for older clients. Rejected Stop controls emit a
nonterminal `error` event, so the UI continues watching the live turn.
When a goal completes and ordinary follow-ups take over, a `goal` event clears
the observed generation. No stored format changed.

Focused daemon replay and real-daemon socket tests cover eviction, pause,
resume, exact retry and rejection of missing or stale generations. JavaScript
syntax validation also passed. The focused replay and final real-daemon tests
each exited 0. The full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0
(`ci: ok`, 708.2 seconds), including all 82 CLI integration tests, Clippy and
doctests. No storage implementation or stored format changed, so compaction
was not rerun for this block.

Queue remains In progress. A socket Stop ID is retained only inside the live
TUI/browser process; manual retry after losing that process still needs a
visible receipt. Ordinary-turn Stop still lacks a turn identity, so a delayed
frame can target a later ordinary turn in the same session. Live TUI/browser
interaction checks and broader goal handoff/restart coverage remain open.
Branch live checks and all five Pending capability rows remain open.

## Stop acknowledgement and retained browser retry

The socket now emits `stop_applied` with the caller ID, observed goal
generation and duplicate flag after a managed Pause receipt commits. It also
acknowledges an identified ordinary-turn Stop after its recovery pause is
saved. TUI Ctrl-C prints its ID and exact CLI retry command before sending;
an uncertain disconnect retains the goal Stop ID for a same-generation retry
after reattach. A matching acknowledgement clears it. The browser writes one
small goal Stop to per-tab storage before sending, restores it after a reload,
and offers explicit Retry/Discard controls. Retry requires the same session and
saved goal generation, then uses the identified HTTP control even when the
socket is gone; a mismatched acknowledgement cannot settle it.
Browser Stop waits for a `goal` event before deciding whether it is stopping a
managed goal or an ordinary turn. Ordinary-turn Stop is deliberately not saved
for reload retry until a durable turn identity exists. Neither path changes a
stored record or the legacy `cancel` frame.

The browser tests cover exact reload retry, HTTP control payload, stale-goal
rejection, storage refusal, bounded invalid saved data and acknowledgement
matching. The real-daemon test checks first and duplicate goal receipts,
malformed Stop IDs and an ordinary-turn receipt. The focused real-daemon test
and four browser tests exited 0; JavaScript syntax and Rust formatting checks
also exited 0. The full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0
(`ci: ok`, 670.6 seconds), including all 82 CLI integration tests, Clippy and
doctests. No storage implementation or stored format changed, so compaction
was not rerun.

Queue remains In progress. Ordinary-turn Stop identity, live browser/TUI
interaction checks and a restart check for retained Stop controls remain open.
The branch and all five Pending capability rows remain open.

## Ordinary socket Stop scoped to the active turn

Each top-level ordinary execution now announces its existing durable turn ID
over the chat socket. Rejoin replay retains only the latest bounded ID even
when transcript chunks are evicted. TUI and browser Stop send the observed ID;
after a queued follow-up starts, they wait for its new ID. The daemon checks
the ID against both the persisted running execution and this process's active
guard under the same writer lock that reserves follow-ups. It then saves the
follow-up pause and aborts the matching live turn before releasing the lock.
A delayed Stop for an earlier ordinary turn receives a nonterminal error. The
optional wire field preserves old socket clients and changes no stored format.

The browser test covers waiting for first and successor turn identities. The
real-daemon scenario starts two ordinary turns in one session, rejects a Stop
from the first during the second, then acknowledges and cancels the second
using its own ID. Replay eviction has a focused test. Ordinary Stop receipt
survival across daemon restart and exact retry remains open; this block adds
identity safety but no durable ordinary Stop acknowledgement record. Live
browser/TUI interaction checks, remaining queue lifecycle checks, branch
semantic synthesis and all five Pending capability rows remain in scope.

Focused browser tests (2), replay test, and both real-daemon Stop scenarios
exited 0. JavaScript syntax and Rust formatting checks passed. Required
`cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 744.6 seconds), including all 83 CLI
integration tests and doctests. The Windows PTY target had zero runnable tests;
interactive TUI behavior remains unverified on this runner.

## Durable ordinary Stop receipt and retry

An ordinary Stop now stores the 64 newest caller ID/turn pairs in the existing
execution receipt, carrying them into later turns. An exact retained repeat returns
`stop_applied` with `already_applied=true` without stopping a successor; reusing
a retained ID for a different turn is rejected. The daemon commits the receipt and
the frontend follow-up pause in one store transaction while both writers are
held, then aborts the live task. A failed commit leaves no acknowledgement.
The optional `session` field lets a fresh socket retry against an idle session
after a daemon restart; older socket frames still work. Previous execution
JSON decodes with an empty receipt list, and no version or postcard schema
changed. Loading the existing execution value now enforces its 8 MiB encoded
limit before copying it from the store. Eviction keeps long-lived sessions
stoppable; a retry older than the retained
window is rejected as stale once its original turn has ended.

The browser saves one ordinary Stop with session, turn and caller ID before
sending, restores it after reload, and offers the existing Retry/Discard
controls. Ordinary retry uses the socket and waits for its acknowledgement;
goal retry keeps its identified HTTP control route. The TUI includes session
and turn IDs but still only retains an ordinary Stop attempt while its live
process remains open. Live interactive browser/TUI checks and broader queue
lifecycle coverage remain, as do branch synthesis and all five Pending rows.

The first extended real-daemon run exited 1 while waiting for a mock model
request before the Stop; that request is unrelated to the receipt boundary.
The scenario now stops after the announced execution turn and passed on retry,
including two turns, conflicting IDs, wrong-session refusal and exact retries
through a new daemon. Focused core persistence and 64-entry rotation tests
passed. Six browser tests and JavaScript syntax checks passed. Required
`cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 743.2 seconds), including all 83 CLI
integration tests, Clippy and doctests. The Windows PTY target still had zero
runnable tests.

## Bounded local HTML review export

`rook session export-html ID --from N --through M --output new.html` writes a
standalone conversation-history review file. It selects an inclusive event
range against the saved history end observed when export starts. The writer
reads existing bounded history and body pages through `Source`, so the same
command works with a local store or a running daemon. It writes to a temporary
file beside the destination, then persists without replacing an existing file.
At most 512 events and 8192 displayed body bytes per event are admitted; a
larger selection fails without leaving a destination. Long bodies carry an
explicit shortened notice and their event number for further inspection.
Every event label, summary and body is HTML-escaped, while tool calls and
results have native expandable details. The file names its source session,
range and snapshot boundary and makes no claim about current workspace files
or current test results. Nothing is published or uploaded.

The CLI path is implemented for local and daemon modes. Browser download and
an in-chat TUI/REPL entry point remain, so the table row is In progress.
Other queue gaps, branch live checks and the remaining Pending rows stay open.
The focused local/daemon integration test exited 0, including an over-limit
selection that leaves no destination. Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 682.6 seconds), including all 84 CLI
integration tests, Clippy and doctests. This block changed no storage code,
so compaction was not rerun.

## Browser HTML history download

The browser history panel now offers first/last event inputs and a Download
selected HTML action. Blank bounds select the events on the displayed page.
The browser reads the same saved history and body pages as CLI daemon mode,
pins the initial history end, and escapes labels, summaries and body text.
Tool content is expandable. It stops after 512 events, 8192 body bytes per
event or 16 MiB of generated HTML, before creating a download. The file
identifies the saved session and source range and explicitly declines to
verify current workspace files or test results. It is a local browser
download; no daemon file, upload or publication is involved.

Three Node checks exited 0: scoped/escaped/shortened content, an over-limit
refusal, and an explicit history-panel click yielding the selected filename.
JavaScript syntax checks exited 0. A TUI/REPL in-chat entry point and live
browser interaction remain, so Local HTML export stays In progress.
The first full `cargo xtask ci` exited 1. The browser asset test incorrectly
treated the export module's literal `<title>` inside an HTML template as the
SPA fallback; it now checks that the module is in the asset set and served as
JavaScript, and its targeted rerun exited 0. An existing first-goal socket
retry test intermittently counted a third `goal` note (expected two); it passed
alone and in the repeat full suite. This queue lifecycle race remains to
investigate. The repeat full CI with `RUST_TEST_THREADS=1` exited 0
(`ci: ok`, 679.8 seconds), including all 84 CLI integration tests, daemon
asset checks, Clippy and doctests. No storage implementation changed, so
compaction was not rerun.

## Chat access to local HTML export

`/export-html [FROM..THROUGH] NEW_FILE` is now in shared REPL/TUI help and
completion. The path may contain spaces. Local REPL uses its already open
Rook instance; daemon REPL reads through `Source` in a blocking worker; TUI
uses its existing history reader thread and reports success or failure in
chat. All three call the same bounded writer as `rook session export-html`,
so they retain the 512-event and 8192-byte preview limits, escaping, source
disclaimer and no-overwrite behavior. The ordinary CLI export still works
with and without a daemon. Browser download has its own 16 MiB output cap.

The extended CLI integration scenario passed for both slash REPL modes,
including paths with spaces and byte-for-byte equality with CLI output.
Parser and TUI command-discovery checks passed. A live interactive TUI check
is unavailable in this Windows PTY test target, and live browser interaction
is still open, so the table row remains In progress.
Full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
654.9 seconds), including all 84 CLI integration tests, the 108 CLI unit
tests, Clippy and doctests. No stored format or storage implementation changed,
so compaction was not rerun.

## Correction acceptance after daemon restart

The existing real-daemon goal-correction retry scenario now verifies the next
boundary, not only persistence of the queued receipt: after restart the saved
correction occurs exactly once in the resumed model request, and its receipt
has an acceptance timestamp. Retrying the caller ID then returns the same
receipt and the queue still contains one matching entry. The focused scenario
exited 0 twice, including the once-only assertion. Five isolated repeats of
the separate first-goal retry scenario also exited 0; its earlier extra-goal
note under the full suite was not reproduced, so that race remains open.
Full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
651.2 seconds), including all 84 CLI integration tests, Clippy and doctests.
This block changed only the recovery assertion and documentation, so
compaction was not rerun.

## Browser history tool cards

Saved tool calls and tool results in the browser history now appear as native
compact `details` cards. Their title uses the recorded event number, kind,
tool name or recorded `doing`, and raw body byte count. Opening a card fetches
one bounded part through the existing history-entry API; next/previous buttons
fetch further parts only on request. Bodies are inserted as DOM text so stored
markup cannot become executable HTML. The card names saved-history scope and
does not imply that historical file or test claims hold in the current tree.

The browser DOM test confirms that tool body content is absent before opening,
that the first page and next page each require their own request, and that
saved markup stays text. Seven focused Node tests across history export,
branches and cards exited 0, as did JavaScript syntax validation. Live chat
cards, explicit failure/duration/diff metadata, TUI parity and actual browser
interaction remain. Inline tool cards are now In progress.

The first full `cargo xtask ci` on this block exited 1 after the existing
`first_socket_goal_retry_keeps_one_generation_and_no_extra_goal_event` test
observed a third `goal` note; all other targets continued. An unchanged-tree
repeat with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 677.6 seconds; 84/84 CLI
integration tests). The intermittent goal-note assertion remains a queue
lifecycle gap to resolve. This block did not change storage formats, so
compaction was not rerun.

## Conversation goal note lifecycle

The first-goal retry integration scenario exposed an unnecessary repeated
`goal` note. A conversation goal already saves its generation, initial goal
value and note atomically at admission. Accepted corrections likewise save
their new effective goal and receipt in one transaction. Managed stage startup
now reads those records without writing an extra goal note; standalone work
still records its effective goal in each new iteration session. This also
avoids a stage overwriting a correction accepted after it read its run.

The real-daemon retry scenario now requires one initial `goal` note while
checking one generation and an `already_admitted` response. The restart
correction scenario continues to check exact-once delivery. Focused and full
verification results follow below. The retry scenario passed once and then
three more isolated runs; the daemon-restart correction scenario also passed.
All five focused invocations exited 0.
Full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
727.9 seconds), including 84/84 CLI integration tests. `cargo xtask compaction`
also exited 0; the existing benchmark remained 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end. The intermittent extra-goal-note
assertion is addressed by the stage-write removal; other queue lifecycle and
frontend gaps in the table remain open.

## Live browser tool cards

The live browser chat now renders tool starts as compact expandable cards.
Completions update the earliest pending card with the same tool name, even if
other status messages followed the start. Each card shows the streamed failure
flag and elapsed time observed by this tab, explicitly distinct from a saved
tool duration or a fresh check of current files. The saved result is reached
through history. Pending card metadata is bounded at 128; a turn ending without
a completion marks remaining cards as unobserved. Four focused Node tests for
live cards, saved cards and socket Stop exited 0; JavaScript syntax validation
and the mandatory full CI result follow below.

Inline tool cards remain In progress: persisted failure/duration/diff metadata,
TUI parity and actual browser interaction are still open. The context
provenance inspector remains Pending because current context usage reports
logged-event totals rather than a request-specific manifest.
Full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
742.1 seconds), including 84/84 CLI integration tests. This browser-only
block changed no storage behavior, so compaction was not rerun.
