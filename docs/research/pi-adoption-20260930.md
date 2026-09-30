# Pi feature adoption

Implementation tracker for [the Pi review](pi-reference-review-20260930.md).
Source: Pi `ee602414c703be8da722ec56de7f2399e62581ac`; starting Rook:
`f62c540`. This tracks the full requested transfer, not just the first patch.

| Capability | State | Completion evidence needed |
|---|---|---|
| Bounded live delivery and snapshot recovery | Complete | Queue/replay/input limits, WebSocket backpressure, atomic recovery, current controls, PTY reconnect/goal checks, browser form preservation and full CI passed |
| Editable steering and follow-up queue | In progress | Core/CLI/API/TUI/browser, durable IDs, goal vs ordinary-turn boundaries, revoke/accept races, restart |
| Configurable keyboard actions and prompt undo | Complete | Shared registry/config/help, bounded Unicode edit tests, remapped-key and external-editor PTY checks, full CI passed |
| Branch navigation and optional branch summary | Pending | Existing session/event IDs, bounded tree/history, explicit workspace semantics, attributable summary |
| Inline tool cards | Pending | Compact/expanded results, errors/duration/diffs, bounded loading, TUI/browser verification |
| Context provenance inspector | Pending | Request-specific sources, discovered vs loaded skills, deferred tools, CLI/API/TUI/browser |
| Local HTML export | Pending | Selected history scope, bounded streaming, escaped content, no publication, cross-frontend access |
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

## Validation environment

macOS showed long cold-start pauses before test code executed: a process sample
contained only `_dyld_start`. A list-only launch of a copied test binary took
about 25 seconds; its repeat took about 0.01 seconds. This did not establish the
OS cause. A parallel list-only probe also encountered long startup waits and was
stopped; only those diagnostic processes were stopped. No check was waived.
Use isolated build targets for simultaneously active Rook worktrees to prevent
artifact interference. The external shared target is also used by another project;
do not clean it or stop that project's builds.
