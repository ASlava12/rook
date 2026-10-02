# Pi feature adoption

Implementation tracker for [the Pi review](pi-reference-review-20260930.md).
Source: Pi `ee602414c703be8da722ec56de7f2399e62581ac`; starting Rook:
`f62c540`. This tracks the full requested transfer, not just the first patch.

| Capability | State | Completion evidence needed |
|---|---|---|
| Bounded live delivery and snapshot recovery | Complete | Queue/replay/input limits, WebSocket backpressure, atomic recovery, current controls, PTY reconnect/goal checks, browser form preservation and full CI passed |
| Editable steering and follow-up queue | In progress | Core/CLI/API/TUI/browser, durable IDs, goal vs ordinary-turn boundaries, revoke/accept races, restart |
| Configurable keyboard actions and prompt undo | Complete | Shared registry/config/help, bounded Unicode edit tests, remapped-key and external-editor PTY checks, full CI passed |
| Branch navigation and optional branch summary | Complete | Existing session/event IDs, bounded tree/history, explicit workspace semantics, reviewed attributable summaries, local busy refusal, daemon live switches with retained prompts/attachments and full CI passed |
| Inline tool cards | Complete | Compact/live/saved results, attributed errors/duration/diffs and command/search/MCP facts, explicit bounded browser pixels, terminal text fallback, local/daemon and full CI passed |
| Context provenance inspector | Complete | Request-specific sources and loaded skills, deferred MCP tools in CLI/API/TUI/browser; bounded notes, old-note compatibility, local/daemon and full CI passed |
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

## Recent TUI tool-call details

The TUI's Tool calls pane now reads the newest 2,000 saved events instead of
starting at event zero. It obtains the session end from a one-entry bounded
history page; when attached to an older daemon without that route, it uses
the existing session summary's `next_seq`. The pane labels older history as
outside its window and reports read failures instead of claiming there were
no calls. Arguments and results remain capped at 8,000 displayed bytes per
event. A local-store regression test seeds a session past 2,000 events and
checks that the latest failed `ask` call and its result appear. Against the
currently running older daemon, the fallback routes found all six `ask` calls
in session `01M3T336VB22NEP5AY77T5NYB4`; the previous front-end range
found only the first two. This repairs a TUI inspection gap, while the five
invalid `ask` attempts themselves came from the agent's tool arguments.

Inline tool cards remain In progress: persisted failure/duration/diff
metadata, live TUI expansion and actual browser interaction are still open.
The queue, branch live checks and Pending rows remain open. The focused test
exited 0, as did targeted CLI Clippy. Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 780.8 seconds), including 109 CLI
unit tests and 84 real CLI integration tests. No storage implementation or
format changed, so compaction was not rerun.

## Recorded tool catalog for the last request attempt

Before each model request, Rook records a bounded service note with the
configured provider ID, native versus prompt-encoded delivery, stub versus
full schema mode, context estimate, and the tools advertised for that attempt.
The note contains at most 32 tool names, each capped at 64 bytes, plus an
omitted count; the serialized note is capped at 16 KiB. It stores no schema
body or prompt text. Schema token counts are estimates from serialized bytes,
not a provider bill. The provider ID is the configured route and does not
claim which physical endpoint a failover provider used. The note is excluded
from model replay and changes no postcard schema or existing record layout.

`rook session context ID` and its JSON/API equivalent expose the last
recorded request catalog with its event number. The entry is labelled an
attempt because a provider can reject the request after Rook records it.
Older sessions simply have no catalog. A focused agent-loop test compares the
saved names with a scripted provider request and checks that prompt text is
absent. A focused CLI test compares direct store and real-daemon JSON and text
output. This begins the inspector. Individual instruction origins,
discovered versus loaded skills, deferred tools, and a dedicated TUI/browser
view remain in scope. Full CI and compaction results follow below.

The first full `cargo xtask ci` exited 1 because three existing agent-loop
tests asserted exact journal positions and did not yet include the new service
note. Their expectations now check the note and retain the previous event
ordering checks. The targeted `agent_loop` suite then passed 233/233. The
corrected full gate with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 756.5
seconds), including 85 CLI integration tests, 233 agent-loop tests, Clippy
and doctests. `cargo xtask compaction` exited 0 with 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end. That benchmark fixture does not
contain the new service note, so its figures do not measure the note's space
cost. The new note uses the existing event type; stored record formats are
unchanged.

## Request-prefix source provenance

The request-attempt note now also records the sources assembled for the
request prefix. Project instructions retain their canonical file origin,
estimated tokens and whether the read was complete. Environment, prompted
tool schemas and session hook context are listed when included. Skills have
separate discovered, applicable and advertised counts. Each advertised skill
entry says whether Rook sent only a lazy catalog card or inlined the body;
the former is not reported as a loaded skill. The record is made while the
request is built, so changing or deleting a file later cannot rewrite what
the inspector says was selected for that attempt.

The source list keeps at most 32 entries; names and origins are clipped before
copying, and the combined service note still has the 16 KiB ceiling. Old
tool-catalog notes deserialize with an empty source manifest. The service
note is skipped during model replay without reading its body. CLI text and
JSON use the same saved note locally and through the daemon. On-demand
`load_skill` results, volatile per-step additions, deferred tool state, and
dedicated TUI/browser views remain open for the context provenance row.

Rechecking session `01M3T336VB22NEP5AY77T5NYB4` against the live daemon
found one valid `ask` at event 1641, which timed out after 1800 seconds with
no chosen answer. Events 2404, 2408, 2412 and 2416 were malformed `ask`
attempts for a later, similar decision and failed argument validation before
any question reached the TUI. The earlier malformed call was event 1637.
The TUI's previous 2,000-event tool pane limit hid the later attempts, which
explains a misleading inspection result. Current TUI question delivery uses
the pending-input map on reconnect and clears a resolved question through
`Inputs`; the stored events do not show repeated valid question delivery.
This was model-side repetition with a TUI inspection gap, not evidence that
the question panel sent the same request repeatedly.

Both focused agent-loop tests passed: the saved lazy request names the exact
project instruction origin and greeting catalog card, and eager mode records
the greeting body as inline. The first focused CLI run exited 1 because its
new fixture tried to reopen the Windows store while the daemon held its lock;
moving that fixture write before daemon startup made the rerun pass. The
rerun checks legacy notes, new notes, local text/JSON and the real daemon.
`cargo xtask compaction` exited 0 (4.02 MiB on disk, 37.1x dictionary and
5.8x end-to-end compression); its fixture does not include these notes. Full
`cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 918.1 seconds),
including 85 CLI integration tests and 234 agent-loop tests. The existing
store event layout and wire routes remain unchanged.

## Recovering from malformed `ask` calls

The late `ask` attempts in `01M3T336VB22NEP5AY77T5NYB4` included two
common shapes: bare question strings and choice objects with `{id, text}`.
Rook now gives a bounded concrete example when `questions` has the wrong
shape and explains that choices belong in the question object's `choices`
field. It accepts choice objects with visible `text` as a compatibility
input, ignoring their IDs because the answer protocol already echoes the
selected text. This would have allowed event 2408's choice shape to reach a
person or time out once, without the subsequent format retries. It does not
assert that the model would have made the right decision after that timeout.

Question text is capped at 4096 bytes and choice text at 512 bytes. Rook
inspects at most four questions and four choices each before copying text
into the pending request; malformed errors do not echo unbounded input. The
wire protocol and saved session format are unchanged. Focused `rook-tools`
tests pass 18/18, including the observed shapes and size limits. A focused
`rookd` test also passes, showing a `{id, text}` option delivered as selectable
text to a daemon window and removed from replay after answer. The first test
invocation used an exact name without its module path and selected 0 tests;
the corrected invocation ran and passed one test. Full CI result follows.

Full `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
701.4 seconds), including 85 CLI integration tests, 78 daemon tests and the
18 `ask` tests. This block changed no storage behavior or format, so the
compaction benchmark was not rerun.

## Live request sources beyond the prefix

The request-attempt manifest now includes sources added beside the latest
prompt: the runtime date, recalled memory, a saved plan or its missing-plan
reminder, the initial workspace sketch, prompt hook context, recovery block
and selected output schema. These entries are recorded when Rook constructs
that request; the inspector does not reread memory, files or settings later.

Successful `load_skill` calls add a `loaded` source only after the body is
available to the model. Rebuilding from session history discovers retained
`SkillLoaded` events, including recipe skills, and omits ones that were
summarised away by compaction. `loaded_skill_events` is an event count, not a
count of distinct skill names; repeated loads remain distinguishable. A
catalog card is still only a card. Old request notes default the new counter
to zero. The existing 32-entry source list and 16 KiB combined note ceiling
still apply, so omitted source rows are counted separately.

The focused tests compare the first request before a skill load with the
next request, then check a later replay and a compaction that removes the
individual skill event. They also check volatile sources against a scripted
provider request, and the saved plan on a later turn. The CLI fixture checks
old and new note formats locally and through the real daemon. Targeted and
full verification results follow below.

The three focused agent-loop invocations passed: on-demand load and later
replay/compaction, first-turn volatile sources, and the persisted plan on a
later turn. The focused CLI integration check passed with one test, comparing
the saved manifest locally and through a real daemon; older notes without
the counter remain readable. The post-fix skill-load test passed again.
`cargo xtask compaction` exited 0 (4.02 MiB on disk, 37.1x dictionary and
5.8x end-to-end compression). Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 775.8 seconds), including 85 CLI
integration tests and 235 agent-loop tests. This extends the existing service
note without changing the store event layout or wire routes. Deferred tool
state and dedicated TUI/browser context views remain open; the inspector row
stays In progress.

## Deferred MCP tools in the request inspector

The request-attempt note now records the MCP catalog installed for that turn:
discovered tools, tools advertised directly to the model, and tools available
through `mcp_tools` and `mcp_call`. It copies at most 16 deferred names and
counts the remainder. The summary is assembled from the same stable catalog
that chooses direct advertisements, after duplicate names and schema budgets
are resolved. It does not copy remote descriptions or schemas. The separate
`agent.lazy_tools` setting abbreviates ordinary advertised schemas; it does
not make them deferred. The count describes the catalog at request setup,
not the later reachability of a server.

The existing request note keeps its 16 KiB ceiling, trimming deferred names
if needed, and old notes default the new field to an empty summary. The
storage event layout and daemon API route stay unchanged. Focused tests
passed for a real MCP catalog with byte and count overflow, for a scripted
model request whose only MCP tool is deferred, and for local and daemon CLI
reports including an old note. `cargo xtask compaction` exited 0 (4.02 MiB
on disk, 37.1x dictionary and 5.8x end-to-end compression). Full
`cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 747.6
seconds), including 85 CLI integration tests. Dedicated TUI/browser context
views remain open, so this capability stays In progress.

## TUI context provenance pane

The palette's `context` pane and `/context [window-tokens]` now read the
current session through the existing `Source::context_usage` path, whether
the store is local or held by `rookd`. The pane shows the live estimate and
per-kind costs first, then labels the last request attempt by its event number
and renders its bounded offered tools, deferred MCP names and included source
manifest. It explicitly warns that recorded origins and tools may differ from
the current workspace or server. `r` refreshes the saved view; j/k and page
keys scroll it. An empty session is explained without opening a store.

The focused TUI test passed with a real local store note. It checks the
deferred MCP and loaded-skill rows, then appends a legacy note without these
fields and refreshes the pane to confirm that the old attempt replaces the
new one. The CLI and daemon integration test from the preceding block already
checks the same `Source::context_usage` route and serialized note on both
storage paths. No storage format or wire route changed in this block. Full
`cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 651.7
seconds), including 110 TUI/CLI unit tests and 85 CLI integration tests.
Browser context UI is still pending.

## Browser context provenance view

The browser now has a Context tab, also reachable from the selected session's
controls. It reads `/api/sessions/{id}/context` with that session's workspace,
shows the live estimate separately from the saved last request attempt, and
renders offered tools, deferred MCP tools and source origins as DOM text.
Session changes and refreshes discard responses from earlier reads. The list
shows at most 100 sessions, and the browser caps displayed tool, source and
deferred-name rows even though the recorded note is already bounded.

The focused browser suite passed 22/22 tests, including source text that
resembles HTML, old notes without provenance fields, workspace routing,
refresh and session switching. A focused `rookd` API test passed with a real
saved note and confirmed that the response contains recorded names but no
prompt body; the embedded-module route test passed too. JavaScript syntax
checks for the three touched modules exited 0. Full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 836.9 seconds), including 85 CLI
integration tests and the new daemon API test. This block changes neither
the store format nor the context API. With the CLI, API, TUI and browser
views verified, the inspector row is Complete. Queue, branch, tool-card and
HTML-export gaps and both Pending capabilities remain in scope.

## TUI slash commands during a running turn

The TUI previously executed a slash command while a turn was busy and then
submitted the same literal text to the correction queue. A request to inspect
`/context`, `/jobs` or `/diff` therefore became the next user message as
well. Slash commands now finish in the interface without reserving a queue
receipt. Ordinary text still enters the queue; `/followup` and `/goal` keep
their explicit paths. Commands that would move or rewind a running turn are
still rejected. A local TUI test executes read-only and rejected commands
under a busy turn, then verifies that a real correction can be submitted.
The dispatch branch is shared by local and daemon windows; live daemon TUI
interaction remains part of the queue row's pending coverage. This block
changes no stored or wire format. The focused TUI tests passed. The first
`cargo xtask ci` exited 1 after an existing daemon-restart integration test
timed out waiting for goal resume; the other 84 CLI integration tests passed.
That test passed alone on retry (exit 0), and the second full `cargo xtask ci`
exited 0 (`ci: ok`, 725.7 seconds), including all 85 CLI integration tests
and doctests. Queue remains In progress.

## Retry an uncertain ordinary Stop in an open TUI

A daemon socket failure used to end the TUI turn and erase an ordinary Stop's
caller ID. The open TUI now retains its bounded session/turn/ID attempt, shows
`/retry-stop` and `/discard-stop`, and resends the exact scoped Stop frame on
retry. A daemon acknowledgement clears it, while a different turn or session
cannot inherit it. A local TUI test simulates the failed socket, exact retry,
reattachment to the same turn, acknowledgement and successor turn. The daemon
protocol's restart and duplicate-Stop scenarios were already covered by CLI
integration tests. Persistence after the TUI process closes and live daemon
TUI interaction remain open; Queue stays In progress. No stored or wire format
changes in this block.

The two focused TUI tests and full `cargo xtask ci` exited 0 (`ci: ok`,
713.5 seconds), including 112 CLI/TUI unit tests, all 85 CLI integration
tests and doctests. Windows ran no PTY tests. No store format or storage
implementation changed, so compaction was not rerun.

## Ordinary Stop retry across TUI window restart

The TUI now writes an ordinary Stop's workspace, session, turn and caller ID
to a private sidecar before sending it. The file is capped at 64 KiB and 64
sessions, read under its byte limit, validated before identities are copied,
and atomically replaced under a cross-process lock. Reopening the same session
restores the uncertain attempt for explicit `/retry-stop` or `/discard-stop`;
an acknowledgement or successor turn clears only the matching saved ID.
The footer keeps the retry hint visible when a daemon snapshot replaces the
chat log, which would otherwise erase the one-time recovery notice.
The daemon's existing durable receipt still decides whether a retry was
already applied. Concurrent windows can each retry their own in-memory ID;
the sidecar retains the latest pending ID per workspace and session. No
existing store value or wire frame changed.

Focused tests cover two TUI instances, exact frame retry and cleanup, the
footer after a snapshot,
workspace isolation, an overwritten ID, invalid IDs and oversized files.
Live daemon TUI interaction and broader queue lifecycle checks remain open.
The focused journal tests and two-window TUI test exited 0. Required
`cargo xtask compaction` exited 0: 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end. An initial full CI before the footer hint
exited 0, but the footer changed afterwards, so it was not the final gate.
The first full CI on the final tree exited 1 when an existing daemon follow-up
recovery test timed out after 90 seconds; the other 84 CLI integration tests
passed. That test passed alone on retry (exit 0). The final full
`cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 758.9 seconds),
including 116 CLI/TUI unit tests, all 85 CLI integration tests and doctests.
Windows ran no PTY tests. Queue remains In progress.

## Saved tool dispatch measurements in cards (2026-10-01)

Saved tool-result cards now use the existing v1 timing notes for a measured
failure/completion and dispatch duration, with the timing event number as
source. The shared bounded page, body-part and transcript readers expose an
optional field; TUI calls/history, CLI history, browser cards and both HTML
export paths consume it. Missing fields in older daemon responses remain
compatible. No event schema, stored timing payload or format version changes.

Lookup examines at most sixteen following records and checks each timing
object's raw size against 1 KiB before loading it. It requires the exact
result sequence and tool-dispatch phase. Malformed, oversized, cancelled,
unmatched and out-of-window notes leave status/duration unknown; answer text
does not prove success. Duration includes approval waits and hooks. Copied
branch history cannot read timing notes excluded by its fork boundary, and
the UI states that saved completion does not verify current files or tests.

Focused coverage checks real measured execution, restart/fork preservation,
bounded lookup, old JSON, TUI same-name result pairing, browser expansion and
local/daemon history plus HTML export parity. A real Edge headless page against
a separate scratch daemon with synthetic history verified compact/expanded
cards by mouse click, bounded body parts and Next part, text escaping, saved
measurement after reload and measured HTML export. The scratch processes were
stopped. An overlapping CLI test's automatic daemon build exited 1 because
Windows refused to replace that running scratch executable; rerunning it
after cleanup exited 0.

The core transcript/diagnostics tests, TUI same-name pairing and TestBackend
rendering test, local/daemon CLI integration test, JavaScript syntax checks,
four browser module tests and the real Edge probe exited 0. Required
`cargo xtask compaction` exited 0: 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end; the storage claims did not move.
Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 838.3
seconds), including 117 CLI/TUI unit tests, all 86 CLI integration tests,
core suites and doctests. Windows ran no PTY tests.

Inline tool cards remains In progress: diff metadata, live TUI expansion and
live TUI interaction checks remain open. Queue lifecycle/frontend checks,
branch live checks, HTML download interaction and both Pending capabilities
(phase routing and declarative extension UI) also remain in scope. The next
implementation block should advance the remaining card behavior or a Pending
capability; this block is not completion of the overall transfer.

## Saved file-change previews in cards (2026-10-01)

`write_file` and `edit_file` now produce saved, tool-reported diff previews
from the text the call observed and wrote. Core validates borrowed metadata
before copying it, redacts it through the turn's Vault, and stores a plain
`rook:tool-changes:v1` note in the same atomic batch as the result. Image
companions remain adjacent to their result. No stored struct or format
version changes. The optional JSON `change_note` remains compatible with old
daemon responses and points only into this session's own history.

Bounds: three preview files, 512-byte labels, 8 KiB per diff, 64 KiB per
comparison input and a 32 KiB committed note. Formatting keeps a bounded head
and tail, evicting before copying new chunks; diff calculation uses a 200 ms budget with
approximation. Approval/model-result hunks now use this generator as well,
instead of allocating the full unified diff before shortening it. Optional
`write_file` baseline reads have a capability-relative 64 KiB byte limit;
large/unreadable text and editor-owned buffers are explicitly unavailable.
Omitted files and limited previews are stated. Missing preview data does not
make a successful write fail or invent file contents.

Browser cards expand the saved change note on demand, highlight patch lines
as DOM text and read more through bounded history parts. TUI calls/history
open the source with `c`; CLI history/entry and HTML export show its event
number. Selected HTML ranges do not import an excluded change note. Saved
previews do not verify current files/tests and do not enter model replay or
compaction. Core checks actual writes, later workspace changes, forks, Vault
redaction, invalid metadata and image association. File tests reach both
input/output caps and the file-count cap. Local/daemon CLI, TUI rendering/key
and browser module checks passed. An initial redaction test omitted resolving
its Vault secret; the corrected setup and repeated core test exited 0.

A real Edge headless page on a new scratch daemon verified mouse expansion
of the nested diff, bounded first/next parts, inert script-looking text,
added/removed styles, source after reload and HTML scope/escaping (exit 0).
Scratch processes and temporary source were removed. Compaction exited 0.
The first full CI exited 1 (844.6 seconds) on the edit-preview contract:
head-only truncation had replaced the existing head/tail elision. The generator
now streams into bounded head/tail storage and reports the omitted bytes,
preserving both removed and added text without building the full diff first.
Both edit/file suites and the split-Unicode streamed-buffer check passed on
that correction (exit 0). Final `cargo xtask ci` with `RUST_TEST_THREADS=1`
exited 0 (`ci: ok`, 811.4 seconds), including 117 CLI/TUI unit tests, all 87
CLI integration tests, the existing head/tail edit test, core suites and
doctests. Windows ran no PTY tests. Final-tree `cargo xtask compaction` also
exited 0: 4.02 MiB on disk, 37.1x dictionary compression and 5.8x end-to-end
for its measurement fixture. The format and published fixture claims did not
change; new previews do add bounded companion data to real sessions.
Inline cards remain In progress: live browser result loading, live TUI card
expansion and interaction checks remain; Pending phase routing
and declarative extension UI, queue lifecycle checks, branch live checks and
HTML download interaction remain in the original scope.

## Live browser result cards (2026-10-02)

Tool completion now includes an optional exact same-session `result_seq`.
Core publishes it after consuming the result's receipt/image binding and
attempting to persist dispatch timing. The daemon forwards this small reference;
old JSON without it remains readable. No event struct, stored object or storage
version changed, and result bodies are not added to live delivery frames.

Live browser cards replace their running notice with the shared saved-event
reader after completion. Opening during execution stays open. The reader keeps
the original session/event source, reads one bounded body part per expansion
or paging action, and exposes saved measurement and optional diff companions.
Each card allows one outstanding read and ignores responses after removal.
Resumed chat uses the same compact cards. The observed elapsed time in the tab
remains labelled separately from saved dispatch duration; historical results
and previews never verify current files or tests. Existing pending-call and
scrollback caps still apply. Old completion frames retain their history notice.

Local core tests read both successful and failed saved results inside the
completion callback and verify timing attribution there. A daemon integration
reads the actual write result, measurement and change-note before the next
model reply is released. Its initial fixture withheld the extra autonomous
checker request and timed out after the intended result assertions; using an
ordinary assist turn with an explicit file allow rule corrected the fixture,
and the repeated test exited 0. Wire compatibility, browser module tests and
syntax checks passed. An initial module test also caught a null child in the
new optional-button rendering; omitted controls now leave no DOM text.

A fresh scratch daemon and real Edge headless mouse interactions verified
actual write/read/error tool calls, no eager history reads, compact expansion,
bounded first/next result and diff parts, saved duration/failure attribution
and resumed-chat cards (exit 0). Scratch daemon/model/browser processes were
stopped before the full gate. Final `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 744.9 seconds), including the new
daemon integration, local core callback checks, compatibility checks and
doctests. Windows did not run the Unix PTY suite. Storage code and persisted
formats were not changed in this block.

Inline cards remain In progress: live TUI expansion/interaction still needs
work. The original queue lifecycle, branch live checks, HTML download interaction,
phase routing and declarative extension UI scope remains open.
The next card block should retain `result_seq` through the local TUI
`TurnEvent::ToolDone` and daemon event handler, associate it with the correct
bounded chat row, and open the existing history reader from that row. Verify
both local and daemon interaction; rendering alone does not close that gap.

## Live TUI result navigation (2026-10-02)

Local `TurnEvent::ToolDone` and daemon completion handling now retain the
exact `result_seq`. The compact call row gains a saved-result event number.
Metadata includes the original session and follows the existing byte-bounded
scrollback; it cannot outlive the row. Prefix eviction shifts links and
selection once per batch rather than rebuilding them for each dropped row.
Replacement snapshots/new sessions clear them. Recalled history also exposes
saved results; missing historical measurements never invent success or duration.

Named actions `prompt.tool_previous` (F5), `prompt.tool_next` (F6) and
`prompt.tool_result` (F7) select/reveal a row and open its existing bounded
history reader. F7 defaults to the latest saved result. The selected row is
highlighted, and the chat border shows configured shortcuts. The common
palette/help/config registry exposes the actions. Navigation and result/diff
parts leave the draft intact and do not pause the turn. Old completion frames
without a result reference retain their plain notice and remain available
through history.

Tests cover identical descriptions, missing starts, old frames, original
session attribution, replacement snapshot cleanup, recovered old results,
source/selection eviction after exceeding the scrollback byte cap, local and
daemon event handlers, actual asynchronous history reading, rendering a
selected row above later output, draft preservation and a remapped result key.
The targeted TUI suite and final focused tests passed. The initial render
assertion used lower-case `f7`; active function-key hints use upper-case `F7`,
and the corrected assertion passed.

Real Windows PTY interaction with fresh stores and a bounded scripted model
verified both `tui --alone` and daemon TUI: F5/F6/F7 source selection, saved
success/failure and timing, result and diff parts at offsets 0/4096, `c` change
source, scrolling the diff and retaining a busy draft. The local model reached
its first-response deadline while the viewer was open; saved navigation and
the draft survived that terminal event too. The daemon fixture streamed small
chunks during review and retained the draft after its final output. Both TUI
processes exited 0. Scratch daemon/model processes were stopped before CI.
Stored structs, formats and storage code are unchanged. Final
`cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 748.7 seconds),
including the new TUI lifecycle/navigation checks, existing queue/recovery
and branch scenarios, core suites and doctests. Windows ran no Unix PTY
tests; the Windows interactions described above used real PTY processes.

Inline cards remain In progress until the remaining tool-type/presentation
requirements are audited against the review (including command exit status,
search match count and MCP attribution). The queue lifecycle, branch live
checks, HTML download interaction, phase routing, declarative extension UI and
later terminal experiments remain in the original scope. Live TUI expansion
is now implemented and exercised; it is no longer the next missing card block.

## Structured tool card facts (2026-10-02)

The remaining type-specific audit found that command/search/MCP structured
metadata reached hooks but was lost from saved history. New display-only
`rook:tool-details:v1` notes retain bounded command exit/timeout/background
state, search matching-line/scanned-file counts and partial-scan status, and
configured MCP server/remote-tool identity with typed content counts and
validated image descriptors. The companion is atomic with its result, preceding
the existing image note if present; existing diff/image adjacency and stored
postcard formats are preserved. These notes never add model replay messages.

Names and image descriptors are admitted before copying; JSON encoding stops
at 4 KiB. The reader checks raw size before decoding and inspects at most two
preceding records in the same session. It matches the exact tool label and
stored result-body hash (bound after redaction/hooks and the image caption), rejects
malformed/contradictory data and cannot borrow facts from earlier results.
Image descriptors require an adjacent image companion. Forks/reopen preserve
copied facts; old results/responses remain readable without guessing from text.
The fork-cutoff regression also checks an orphaned copied companion followed
by a different result of the same tool; its body hash prevents false attribution.
MCP identity comes from the registered route, including `mcp_call` overflow,
and cannot be supplied by a server's `meta.server`. Known secrets are redacted.

Command completion, process exit, timeout and an ongoing background job are
different states. Job results now expose an actual available exit code as
structured metadata. Search counts are matching lines, not regex occurrences;
shortening the displayed hits alone does not mark a partial scan. The audit
also found silently skipped oversized text lines/unreadable paths, which now
mark an incomplete scan in metadata and result text.

CLI history/entry, TUI Calls/history/recalled result rows, browser cards and
both HTML exporters show the saved facts and companion source. Live links still
use exact bounded result references; expanding a result reads the saved facts
without putting bodies/images into live transport frames. Typed MCP image
descriptors preserve the distinction from textual captions; browser pixel
presentation remains open, with terminal image preview still a later experiment.

Targeted structured-note bounds/identity/reopen/fork/image pairing, real local
command/search dispatch, search scan limits and oversized lines, direct/deferred
MCP image persistence, TUI pairing/rendering and browser module/export checks
passed (exit 0). Local CLI and daemon history/entry/escaped HTML checks passed,
as did real-daemon command/search completion before a withheld next model reply.
The initial redaction test kept a secret without resolving it; resolving the
fixture value exercises the vault's handed-out redaction contract and passed.

A fresh scratch daemon and real Edge headless mouse interaction exercised
actual command/search/MCP calls, typed text/resource/unsupported/image facts,
no eager history reads, bounded result parts at 0/4096, inert script-looking
text, resumed chat and browser HTML facts/escaping (exit 0). The first helper
launch supplied malformed PowerShell argument quoting and the daemon rejected
its workspace before opening the store; the corrected helper used a fresh
process. Scratch model/daemon/browser processes were stopped before CI; the
installed user daemon was left running.

The first full CI exited 1 (25.5 seconds) on two explicit-auto-deref Clippy
findings in the new CLI fixture. Those expressions now use automatic coercion.
The repeated gate exited 0 (693.4 seconds) on the intermediate tree before
the final result-body binding change; it is not the final-tree gate. Final
binding/fork-cutoff, expanded-redaction byte bounds and direct/deferred MCP
checks passed (exit 0), as did final local/daemon detail checks. A newly built
daemon and fresh Edge/model/store repeated the full browser scenario after
binding and identity-bound changes (exit 0); those scratch processes were stopped.
Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
683.5 seconds), including the binding/bounds regressions, local/daemon detail
checks, existing queue/branch/recovery scenarios, core suites and doctests.
Windows ran no Unix PTY tests; this block's new TUI presentation was exercised
by the actual reader/pairing/rendering test, and the preceding block retains
the real Windows local/daemon F5/F6/F7 interaction evidence.
Final `cargo xtask compaction` exited 0: 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end for its measurement fixture. Published fixture
claims and storage formats remain unchanged; real tool calls now add bounded
display companions. No scratch processes remain running.

Inline cards remain
In progress pending the final special-tool/result presentation audit (including
successful `load_skill` navigation and MCP pixel presentation). Original queue
lifecycle, branch live checks, HTML download interaction, phase routing,
declarative extension UI and terminal experiment scope remains open.
The next card block should link existing non-ToolResult outcomes (especially
SkillLoaded/Error for `load_skill`) without duplicating model replay, and add
explicit bounded browser retrieval of retained pixels with historical source
attribution. Keep encoded image data out of live delivery and default history
pages; terminal pixel rendering remains a later experiment.

## Successful skill result navigation (2026-10-02)

The special-result audit found that `Journal::complete` already retains an
ordinary `ToolResult` for built-ins whose own final event is `SkillLoaded`,
`Error` or `Note`. Successful `load_skill` alone bypassed the live result
lookup to preserve its scoped source envelope for the model. Completion now
looks up the existing exact-label/body-hash result independently of that
envelope decision. Its live reference and saved dispatch measurement use the
existing journal result; no extra result/skill event is appended. The loaded
instructions and source-manifest handling remain unchanged. Stored structs,
writers, format versions and old completion-frame compatibility are unchanged.

Focused local tests passed (exit 0) for successful loading, repeated identical
loading, failure, distinct result references, timing readable inside the live
callback, preservation of the original skill envelope and one tool answer per
call in subsequent replay. A real daemon integration passed (exit 0), holding
the next model reply while reading the exact success/failure results and their
saved measurements through HTTP. Its subprocesses use the existing daemon
serialization guard.

A fresh scratch daemon and real Edge mouse interaction passed (exit 0):
identical loads have separate cards, results load only on expansion, each
request addresses the original session/event, success/failure and timing are
saved, script-looking skill text stays inert, and resumed chat retains the
cards. The next model reply was withheld during expansion; its eventual request
retained the original scoped skill envelopes. Real Windows PTY checks exercised
local `tui --alone` and daemon TUI F5/F6/F7 result navigation, saved success and
failure measurements, and draft preservation. Both TUI processes exited 0.
Browser module tests passed (exit 0). All owned scratch processes were stopped
and their absence verified before the full gate.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
661.6 seconds), including both new regression tests, existing queue/branch/
recovery scenarios, core and provider suites, and doctests. Its log is
`target/pi-skill-result-links-ci.log`. Windows ran no Unix PTY tests; the
local/daemon interactions above used real Windows PTY processes. No storage
writer was changed.

Inline cards remain In progress for explicit bounded browser retrieval and
presentation of retained MCP pixels, with original session/result attribution.
Inspect image-companion identity as well as adjacency, including a fork ending
at an image note before a different result is appended. Keep encoded images out
of live delivery and default history pages. Terminal image rendering remains a
later experiment. Original queue lifecycle, branch live checks, HTML download
interaction, phase routing, declarative extension UI and terminal experiments
remain open; successful `load_skill` navigation is no longer a missing block.

## Explicit retained image presentation (2026-10-02)

Saved results now expose an optional JSON-only `image_note` reference. A shared
reader binds the adjacent existing image object to the hash in the result's
caption, reading at most 128 tail bytes. Model replay and the explicit image
reader use that same check: an orphaned companion at a fork cutoff no longer
attaches to a different result. No stored struct, object encoding, companion
format or writer was changed. Old transcript JSON defaults the new field to None.

`GET /api/sessions/{id}/history/{seq}/images/{index}` returns one saved image,
the companion source and its validated index/count. Raw companion size is checked
before loading; a borrowing JSON visitor admits no more than four records and
checks each encoded size/MIME before copying. Existing 2 MiB raster and 4096
pixel dimension limits remain. HTTP admits two decoding workers before spawning;
owned permits stay with blocking work after a client disconnects. Extra requests
receive 429, missing pictures 404, and indices outside 0–3 receive 400. Default
history and live delivery contain no encoded pixels.

The shared browser card adds explicit show/hide and multi-image paging. Incoming
JSON is bounded while streaming and before chunk copies, including a Content-Length
precheck. At most one picture remains across the page; switching cancels the old
request, closing releases pixels, and detached cards ignore late data. A caption
pins the original session/result/image source. CLI `session image` exports checked
raster bytes into a new file, reports that source and refuses replacement. TUI
Calls/history show a source number and an export command pinned to the historical
session/result. Terminal pixel rendering remains a separate experiment.

Focused core tests passed (exit 0) for indexed retrieval, complete forks/reopen,
orphaned-companion rejection, old JSON, source metadata without pixel data,
oversized records/images and excess image count; fixtures exceed each asserted
bound. Existing direct/deferred MCP, batch/replay and compaction image scenarios
passed. Browser module tests passed for explicit retrieval, paging/source identity,
one retained picture, close/disconnect, stream/header bounds and cancellation.
The first selector assumed image 1 after paging to image 2; the fixture now selects
the current show button and passes. Local/routed CLI export and actual HTTP checks
passed, including identical PNG bytes, no overwrite and no file on a missing index.
The first isolated CLI run omitted TLS initialization for its raw HTTP client;
initializing it fixed the fixture and the repeated run exited 0. TUI pairing/render
checks passed for the image-source text fallback. A real-router test filled both
worker slots, abandoned one HTTP future while blocked, observed immediate 429
and verified slots return only after the workers finish (exit 0).

A fresh scratch daemon and real Edge mouse interaction passed (exit 0): actual
MCP output, no eager pixels, source-pinned explicit retrieval before a withheld
next model reply, a decoded 1×1 PNG, hide/reopen after chat recovery, inert tool
text and no encoded pixels in HTML export. Owned scratch processes were stopped
and their absence verified. The worker-admission correction was subsequently
verified through the actual router test above.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`,
714.9 seconds), including the new image bounds/source/fork checks, CLI local/
daemon export, TUI source rendering, router admission/disconnect regression,
existing queue/branch/recovery and MCP replay checks, and doctests. Its log is
`target/pi-retained-pixels-ci.log`. Windows ran no Unix PTY tests; earlier card
blocks retain real Windows local/daemon navigation evidence, and this block's
new terminal fallback was checked through the actual pairing/renderer.
`cargo xtask compaction` exited 0 (`target/pi-retained-pixels-compaction.log`):
4.02 MiB on disk, 37.1x dictionary compression and 5.8x end-to-end for its fixture.
Published storage measurements remain unchanged.

The main inline-card requirements in review section 5 are now Complete:
compact/expanded live and saved results, attributable failure/dispatch duration,
saved file diffs, command exit/background state, search matching-line/partial-scan
facts, typed configured MCP identity/content and explicit retained pixels, bounded
loading and terminal text fallback. The preceding blocks retain local/daemon TUI
navigation and browser lifecycle evidence, and the final gate verifies the
integrated tree. Terminal image rendering remains a separate experiment rather
than a missing main-card requirement.

Original queue lifecycle, branch live checks (including the remaining navigation
summary-draft work documented in conversation-branches), HTML download interaction,
phase routing, declarative extension UI and terminal experiments remain open.
Next return to the queue lifecycle/branch navigation gaps, then close HTML
interaction and implement both Pending capabilities; the full goal remains active.

## Review and carry a summary within TUI navigation (2026-10-02)

Choosing excerpts or a model draft in the TUI branch-switch offer now opens a
separate bounded multiline editor. The main prompt and attachments are retained.
The editor shows the departed session and pinned source boundary; Enter inserts
a line, Ctrl+U clears, Ctrl+Z/Ctrl+Y undo/redo, and Ctrl+S confirms the reviewed
text and continues. Ctrl+Enter is also accepted when the terminal reports it.
The review keys take precedence over global mouse selection while this editor
is open. Opening an offer, generating, editing or cancelling writes no summary.
An in-flight save cannot be dismissed and repeated from the same editor; errors
retain the edits and advise checking target history before an uncertain retry.
A committed reply from an older viewer reports its event without switching a
newly opened conversation. Existing Source/core/HTTP methods and the bounded
branch-summary Note format are reused; no stored structs or writers changed.

Real Windows TUI interaction found that opening a tree from history lost the
departed conversation and silently skipped the offer. All history entry points
now pass the actual open conversation separately from the inspected session,
including Calls and turn results. Unrelated keys keep a pending offer; cancelling
a pending model draft invalidates late replies without putting them into chat.

Focused editor/worker-state tests passed (exit 0): confirmation, multiline Unicode
edits, pre-copy paste and typed byte bounds, cancellation, late/wrong-source
drafts, source-change errors, stale save receipts and history entry points.
The oversized fixture asserts it exceeds 16 KiB. The existing CLI test for scoped
drafts locally and through the daemon passed (exit 0). An initial invocation
requested a nonexistent CLI library target and exited 1; the correct `--bin rook`
target passed. A new Unix PTY scenario covers cancelled review followed by one
confirmed local/daemon save, exact source/boundary, retained prompt and untouched
workspace; Windows executes no tests from that Unix-only file, so its exit 0
is not PTY evidence. The existing tree-navigation scenario now explicitly skips
the summary offer when continuing a different branch.

Actual fresh Windows PTY runs completed locally and through a scratch daemon
(exit 0). Both requested the model draft through navigation; captured requests
included source-only events and excluded the shared prefix. Local cancellation
followed by excerpt review saved one attributed multiline summary and retained
the unsent main prompt. In the daemon case, a concurrent source append made the
pinned save fail while retaining edits and leaving target history unchanged;
fresh excerpts then saved and continued with the main prompt intact. Actual HTTP
history verified exactly the two confirmed summaries and their different pinned
boundaries, no submitted draft, and unchanged workspace bytes (verification
script exit 0). Owned model/daemon helpers were stopped and their absence checked.
The scratch seed model initially returned an invalid completion verdict; its
fixture was corrected before the navigation checks. These checks use scripted
model replies, not a model-quality or cost benchmark.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 689.7
seconds), including the new editor/history-source checks, local/daemon scoped
draft and model-suggestion integration scenarios, existing queue/restart and
storage compatibility checks, and doctests. Its log is
`target/pi-navigation-review-ci.log`. `cargo xtask compaction` exited 0
(`target/pi-navigation-review-compaction.log`): 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end for the unchanged fixture. Published storage
claims remain unchanged. No scratch helpers remain running.

Branch navigation remains In progress for live switches during model output:
local busy-turn refusal, daemon observation switching while the departed turn
continues, and preserved prompt/attachments. Next verify those remaining live
scenarios and the queue's real TUI Stop retry across window/daemon restart.
Queue lifecycle, HTML download interaction, phase routing, declarative extension
UI and terminal experiments remain in the original scope; the full goal is active.

## Branch switches during model output and ordinary prompt admission

Local TUI navigation now refuses a busy turn before opening a transfer review,
generating a model draft or saving a summary. The current prompt and attachments
remain intact. Daemon navigation detaches the departed observer and advances the
connection epoch before attaching the target; queued old errors and snapshots
cannot end or replace the new view. Detaching leaves the original daemon turn
running. Ctrl+C in an idle daemon view exits instead of waiting for a turn ID.

Live testing exposed a retained original prompt blocking an explicit send in the
new branch: clients previously settled it only on terminal output, which the
departed observer no longer receives. Core now reports ordinary prompt admission
after its UserMessage and existing claim commit atomically. Optional `prompt_id`
on the existing `turn` JSON event carries that caller ID and survives bounded
replay eviction. Exact caller/session matching clears only the observed in-flight
frame. Starting an execution before prompt hooks is insufficient; a denied or
uncertain admission remains retryable. Legacy turn JSON still omits the field.
No postcard layout, stored receipt format or admission transaction changes.

The browser admits selection count and file metadata limits before copying File
references. At most four retained references survive replacing the chat view;
their bounded names stay visible beside an explicit clear button. Bytes are read
only for an explicit send. A session, busy-state or composer-view change while
reading attachments refuses that send and preserves the draft. Invalid selection
refuses navigation before the view is destroyed. Existing text/image byte limits
and historical attachment limits are shared by selection, navigation and send.

Focused core checks exited 0 for durable receipt/message visibility at admission
before a model request, and prompt-hook refusal without an admission signal.
Rust frontend, replay and protocol checks exited 0 for exact caller/session
settlement, uncertain disconnects, retained admission after eviction, successor
identity, oversized IDs and legacy JSON. Local busy-navigation, stale queued
daemon failure and idle Stop checks exited 0. Node module checks exited 0 for
pre-copy selection count, pre-read image/combined-text byte limits, reference
identity and durable/uncertain prompt retry. Fixtures exceed each asserted bound.
Initial targeted invocations used incorrect fork/loop/fixture APIs and failed;
they were corrected and rerun successfully. A build blocked by the owned running
scratch TUI exited 101; closing that process allowed the subsequent build to exit 0.

Fresh actual Windows TUI checks exited 0 in local and daemon modes. Local busy
refusal retained the main draft and next-turn text attachment, did not cancel the
original model reply, and allowed an explicit target send after completion.
Daemon navigation retained that draft/attachment while the source remained
running; the explicit target send succeeded before the source ended. Captured
model requests contained the retained text context. Actual HTTP history verified
the departed tail only in the source and no unsent prompt in the target before
the explicit send. The idle daemon TUI then exited with Ctrl+C (exit 0).

A real Edge check against the fresh scratch daemon exited 0: mouse navigation,
ordinary admission before a withheld reply, retained PNG/text selections despite
an empty replacement file field, departed turn continuing, no old tail in the
target, and no request after switching during asynchronous file reading. The
subsequent explicit send delivered both retained attachments to the chosen target.
An actual selection above the count cap refused navigation and preserved its
draft until Clear selected files. Workspace bytes remained unchanged. The first
browser harness compared a Promise directly to a session ID and exited 1 before
starting a turn; correcting its predicate produced the successful run above.
Its current-machine helper is `target/branch-admission-browser.mjs`, with artifacts
under the scratch root recorded in `target/branch-admission-root.txt`. These use
scripted model replies and do not measure model quality or cost. Owned model,
daemon, browser and terminal helpers were stopped and their absence verified.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 699.6
seconds), including the new admission/refusal, frontend/replay/protocol and
branch-navigation regressions, existing real daemon queue/restart checks and
doctests. Its log is `target/pi-live-branch-ci.log`. `cargo xtask compaction`
exited 0 (`target/pi-live-branch-compaction.log`): 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end for the unchanged fixture. Published
storage claims remain unchanged. Windows runs no Unix PTY tests; the actual
Windows terminal and browser evidence above supplies the live checks.

Branch navigation and optional reviewed summaries are Complete. Next return to
the queue's actual TUI Stop retry across window/daemon restart and remaining
lifecycle/frontend checks. Also audit switching away from an active `/goal`
while its initial caller frame is retained: the new ordinary-turn signal does
not acknowledge managed goal creation. HTML download interaction, phase routing,
declarative extension UI and terminal experiments retain their scope. The queue
row remains In progress; the full goal is active.

## Explicit Stop retry while its saved owner is still running

A fresh actual Windows TUI check exposed that `/retry-stop` refused every busy
view after rejoining, including the same original turn whose Stop had never
reached the daemon. The request was preserved, but the interface told the user
to wait for the turn they were trying to stop. Explicit retry now accepts a busy
view only when the observed session and ordinary turn or goal generation match
the saved owner. Unknown identity, a different session/turn, a promoted goal and
a replacement generation retain the request without sending it. Idle views can
still obtain an already-applied acknowledgement. Existing Stop frames, durable
daemon receipts and the bounded private sidecar format are unchanged.

The focused TUI regression exited 0 for same-owner ordinary/goal retry while
busy, unknown/mismatched identity refusal, exact frame reuse and successor
cleanup. A live loopback fault proxy withheld an ordinary Stop before delivery:
the source continued, the caller identity persisted before the write, and the
original interface's busy refusal was reproduced. After closing that window,
the corrected TUI loaded the same sidecar, rejoined the active turn and resent
the exact original frame. The daemon acknowledged its first application and
stopped the original turn; the acknowledgement cleared the sidecar. Actual
frame/health/sidecar verification exited 0, and the TUI exited 0.

A second live scenario delivered Stop but withheld all resulting view frames,
including its acknowledgement. The daemon's captured acknowledgement recorded
the first application, the source stopped, and the local sidecar stayed pending.
After closing the window and restarting the owned daemon on a new port, a new
TUI loaded the saved owner from disk and explicitly retried it. The daemon
returned the same caller ID with `already_applied=true`; the sidecar cleared,
no turn started, and the saved history boundary stayed unchanged. The verification
and TUI both exited 0. Owned proxy/model/daemon helpers were stopped and their
absence verified. Current-machine helpers are `target/stop-retry-proxy.mjs` and
`target/stop-retry-setup.ps1`; `target/stop-live-root.txt` records the fresh root
containing captured frames and before/after history. These are scripted-model
and transport checks, not model-quality measurements.

An initial held-reply fixture reached the normal 90-second stream idle limit;
the fresh fixture explicitly set 600 seconds before starting the daemon. The
proxy's first metadata read in each new window failed transiently; rejoining
the same session succeeded without changing the retained Stop identity. An
initial verification before successful rejoin exited 1 for a missing retry
capture; the final capture/sidecar/health checks above exited 0. This evidence
does not attribute those proxy metadata failures to a production daemon path.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 627.4
seconds), including the extended exact-owner TUI regression, existing real
daemon ordinary/goal Stop and restart checks and doctests. Its log is
`target/pi-active-stop-retry-ci.log`. Windows ran no Unix PTY tests; the actual
Windows terminal/frame/health/sidecar evidence above supplies the live checks.
No storage code, writer or format changed, so compaction was not rerun.

Queue remains In progress. Next audit initial managed-goal prompt acknowledgement
while leaving its active branch, then remaining queue lifecycle/frontend parity.
HTML download interaction,
phase routing, declarative extension UI and terminal experiments retain their
original scope; the full goal remains active.

## Initial managed-goal prompt acknowledgement during live branch switches

A fresh actual Edge check reproduced the remaining goal-specific outbox bug:
the daemon had saved the goal and begun streaming, but the tab retained its
initial `/goal` frame until the whole goal ended. Mouse navigation kept the
draft, then the target's explicit Send was refused by that old saved request.
The first-session goal claim was already admitted atomically in core; no new
store record or transaction was needed.

The existing `agent` event now has optional structured `admission` metadata
containing the caller ID and saved session. Only successful durable goal creation
or explicit retry of that admitted claim emits it. The daemon starts/joins the
goal before awaiting the acknowledgement and releases the admission lock before
delivery or rejoining an admitted claim. CLI, TUI and browser settle the exact
in-flight caller with its intended/observed session, without requiring `Started`
or final model output. The plain REPL also retains the acknowledged new session
ID. Unrelated, late or disconnected acknowledgements preserve the saved request;
explicit retry reuses its original frame. The acknowledgement does not replace
the view's goal generation, execution turn, metrics or draft. It is independent
of correction acceptance and does not invent a queue receipt or execution ID.
Caller length/ASCII validation precedes copying; old agent JSON omits the
optional field. Existing claim storage, JSON companions and postcard layouts
are unchanged.

Focused protocol, Rust retry/TUI dispatch and browser module checks exited 0 for
legacy JSON, exact caller/session settlement before Started, wrong/unobserved
acknowledgements, uncertain retry, successor protection and preserving the live
view/draft. Real daemon checks exited 0 for first goal confirmation while the
model reply is withheld, same-frame rejoin confirmation, conflicting text
refusal, unchanged generation and no duplicate goal event. A separate real
daemon refusal check exceeded the configured goal byte cap and exited 0: both
attempts produced no admission confirmation and retained one reserved session
with no transcript events. An initial targeted command used the nonexistent
`scenarios` test target; the actual `cli` target passed. One refusal invocation
could not rebuild rookd while the owned scratch daemon held its Windows exe;
stopping that daemon allowed the subsequent invocation to exit 0.

The corrected actual Edge check exited 0: goal confirmation before a withheld
reply, mouse branch navigation with a retained draft, explicit target send and
reply while the departed goal continued, and saved histories with no cross-branch
prompt or output. A Windows daemon TUI likewise confirmed the goal, switched
away, sent and received the target answer while the source still ran, and showed
no departed tail. API history/health verification exited 0; the terminal exited
0. After releasing each source reply, its tail was recorded only in its source;
the scratch goal was explicitly cancelled. The scripted verifier reports
`unproven`; these checks do not claim goal quality, successful independent
verification or cost savings.

An actual local Windows TUI check also exited 0: `/goal` remained metadata only,
navigation refused while its ordinary model reply was active, and explicit
target send succeeded after completion. Saved local histories separated the
standing goal/source tail from the target prompt; workspace bytes remained
unchanged. The browser harness was corrected to wait for replacement DOM nodes
and Edge's debugging port. A first fixed-browser fixture serialized model
requests through its default single slot and timed out waiting for the target
reply; the fresh fixture explicitly admitted parallel model requests. These
failed harness/build invocations are not counted as passing checks.

Current-machine browser helper: `target/goal-admission-browser.mjs`; final live
root: `target/branch-admission-918228bd633741cab5e16249f2a7bdda`, recorded in
`target/branch-admission-root.txt`, including browser screenshots and source/target
API histories, native daemon histories, and the local fixture under `local/`.
Owned model, daemon, browser and terminal helpers were stopped; absence verified.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 723.1
seconds), including the new protocol/frontend acknowledgement checks, real
daemon creation/refusal/retry scenarios, existing goal/follow-up/Stop restart
checks, storage compatibility tests and doctests. Its log is
`target/pi-goal-admission-ci.log`. Windows ran no Unix PTY tests; the actual
Windows terminal and Edge checks above supply the live evidence. No storage
code, writer or format changed, so compaction was not rerun.

Queue remains In progress. Next verify actual browser Stop retry after a lost
acknowledgement, tab reload and daemon restart: its current module and daemon
API evidence are separate, unlike the now-completed actual TUI retry path.
Then audit the remaining queue lifecycle/frontend requirements before changing
the table's state. HTML download interaction, phase routing, declarative
extension UI and terminal experiments retain their scope. The full goal is active.

## Actual browser Stop recovery and cancelled paused-stage retirement

Fresh actual Edge checks now connect the browser's saved Stop UI to the real
daemon across transport loss, tab reload and two daemon restarts. A bounded
loopback proxy captured the original caller, session and owner before dropping
an ordinary Stop request. The turn continued; reload preserved the exact frame;
the actual Retry saved Stop button delivered it once and cleared the saved
request on acknowledgement. A later ordinary Stop was applied but its
acknowledgement was withheld. After reload and daemon restart, explicit retry
returned `already_applied=true`, cleared tab storage, started no turn and left
the history boundary unchanged.

The goal scenario withheld a saved pause's socket acknowledgement, let the
current operation finish, then reloaded the tab and restarted the daemon. After
an explicit HTTP resume and rejoin, Retry saved Stop used the identified HTTP
route with the original caller and generation. Its already-applied response
cleared the saved request without pausing the resumed goal, starting another
turn or changing the history boundary. A later retained Stop for that generation
was refused after goal replacement: no HTTP control was submitted, the new goal
continued and the old frame stayed available for inspection. Explicit discard
followed by a fresh Stop used a new caller and the replacement generation.
Workspace bytes remained unchanged. All phases of the final fresh fixture
exited 0.

The first replacement attempt exposed a core bug: cancelling a paused goal
after its stage future had finished left `Saved.active` attached forever. The
daemon rejected a new goal with "resume or cancel" despite the cancelled state.
Cancellation now retires stopped context, including family work tags. Starting
another goal also cleans compatible older cancelled records left in that state.
Pause still retains context for resume. Execution and unknown-effect recovery
receipts remain intact, and unknown effects continue to block replacement.

Checking only the execution receipt would incorrectly clear a stage before its
agent was constructed or between execution and verification. A private guard
therefore owns the whole managed stage future. Admission and cancellation share
the existing writer lock; duplicate claims are refused. Active claims are capped
by the configured parallel-run limit per store before copying paths, and the
guard releases its slot when the future finishes or is dropped. No stored fields,
postcard layouts, control IDs or wire formats changed. Both bare and identified
controls use the same retirement rule; identified receipts and run state remain
one saved JSON record.

Focused direct-core checks exited 0 for paused context preservation, bare and
identified cancellation after the future finishes, retained legacy records across
store reopen, replacement generation isolation, cancellation before any running
execution receipt, refusal to replace a still-owned stage, parallel admission
bounds and slot reuse. Unknown-effect cancellation retained its active context
and execution receipt and refused replacement. Existing managed pause, goal
promotion, verification, budget and retry regressions also passed. Local goal
metadata still uses its existing ordinary-turn behavior; managed cancellation
is exercised directly in core and through the real daemon API.

The browser harness initially rejoined immediately after HTTP resume, while the
goal was still queued, and timed out in the idle view. Waiting for the actual
running operation before rejoining fixed that fixture race. This failed
invocation is not counted as a pass. The original cancelled-context failure
was reproduced against the unmodified core; the corrected final fresh fixture
passed through the entire replacement path. A real CLI cancellation was applied,
but PowerShell's `ErrorActionPreference=Stop` treated its normal stderr receipt
line as an error before its exit could be recorded. The explicit same-ID retry
then exited 0 with `already_applied=true` and cancelled status. Only that retry
is counted as a passing CLI check.

The first full `cargo xtask ci` ran with default test parallelism and exited 1
after 503.9 seconds. Its only failed target was the CLI suite: the existing
`killed_followups_resume_once_with_saved_settings_and_cancelled_ones_stay_stopped`
scenario timed out waiting for its model request after daemon restart. The
remaining workspace suites and doctests completed without failures. An exact
separate rerun with `RUST_TEST_THREADS=1` exited 0. The failure log is
`target/pi-browser-stop-ci.log`; the focused rerun is
`target/pi-browser-stop-recovery-recheck.log`. The timeout's cause is not established
by that successful rerun. The final full gate uses the same serial test setting
as previous Windows blocks; no test is excluded.

Current-machine helper: `target/browser-stop-live.mjs`. Final evidence is under
`target/stop-live-decf259339c44824b2ff35d41d6e165d`, recorded in
`target/stop-live-root.txt`: captured socket/HTTP receipts, before-restart
histories, screenshots and CLI retry output. The original failing replacement
fixture is retained under `target/stop-live-3df9011c00b14cac89e8eb506adff7c3`.
The replies are scripted; this is delivery/lifecycle evidence, not model quality
or cost evidence. All owned browser, proxy, model and daemon helpers were stopped
and their absence verified.

Final `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 621.6
seconds), including every workspace suite, the previously failed daemon
follow-up recovery scenario, managed cancellation/admission regressions,
storage compatibility checks and doctests. Its log is
`target/pi-browser-stop-ci-serial.log`. `cargo xtask compaction` exited 0
(`target/pi-browser-stop-compaction.log`): 4.02 MiB on disk, 37.1x dictionary
compression and 5.8x end-to-end for the existing fixture. Published measurements
remain unchanged. Windows runs no Unix PTY tests; actual Edge interaction,
captured protocol/HTTP receipts and direct-core execution supply this block's
lifecycle evidence.

Queue remains In progress. Next audit acknowledgement of an identified
`/continue` prompt while leaving a resumed goal's branch: the previous block
acknowledged initial goal creation, and resume uses a separate control path.
Include retry against a replacement generation in that audit: the existing
socket resume path reads the generation from the current run on each attempt.
Then finish the remaining queue lifecycle/frontend requirements before changing
the table's state. HTML download interaction, phase routing, declarative extension
UI and terminal experiments retain their scope. The full goal is active.

## Managed continuation acknowledgement and preserved generation

Actual Edge interaction reproduced the remaining continuation outbox bug:
`/continue` resumed the paused goal, but retained its caller frame until the goal
ended. After mouse branch navigation, that old frame refused the target's
explicit Send. Capturing the exact pending frame also reproduced a generation
bug against the real daemon: after cancellation and replacement, its retry
resumed the new paused goal using the old caller ID.

Identified continuation now admits the existing fixed-size chat claim together
with the resume control and current run JSON in one session-checked store
transaction. The claim's owner slot holds the goal generation. Validation,
control/claim caps and bounded serialization precede committing that state;
refusal saves no control or admission. A repeated admitted request can rejoin
its own live generation or report `already_admitted`. It never reapplies resume
to a later pause or replacement goal. The daemon sends the same structured
request acknowledgement used for goal creation after starting/joining the saved
goal, with the admission lock released before delivery. CLI, TUI and browser
already consume its exact caller/session without resetting the observed goal,
turn, metrics or draft. No frontend payload, stored layout or postcard format
changed. Prompts without caller IDs and legacy runs without generations retain
their bare resume behavior. Claims use the existing per-session message cap;
the managed record still has its bounded encoder and control cap.

Direct-core checks exited 0 for admission/control ownership, reopen, repeated
confirmation without another resume, rejection across replacement generations
even when a caller substitutes the current generation, the one-claim cap and
unchanged stored bytes after caller mismatch, an oversized claim, budget refusal
or full control receipts. The oversized fixture exceeds the actual 75-byte
record bound and checks the bound-specific error before any control is saved.
The existing managed lifecycle checks also passed.

Real daemon regressions exited 0 for acknowledgement while a resumed model reply
is held, exact same-frame rejoin with no second model request, changed-options
refusal, paused-state preservation across daemon restart, legacy unidentified
resume, and replay after replacement with unchanged generation/status/history.
The extended replacement test initially consumed an unread `work_paused` frame
from its separate legacy continuation. Giving replay a fresh socket produced
the successful check; that earlier invocation exited 101. An initial exact
filter omitted the module prefix and ran no tests; only the corrected executed
test is counted as evidence.

A fresh actual Edge check exited 0 after the fix: early continuation confirmation,
mouse branch navigation with a retained draft, target send/reply while the source
continued, no cross-branch prompt/output, and unchanged workspace bytes. Its
captured original continuation then received `already_admitted` against a
replacement paused goal, with no resume or history change (exit 0). Browser
evidence: `target/branch-admission-85614730fe1840a3a4610a9ebe35f1a9`, recorded in
`target/goal-resume-browser-root.txt`; helpers `target/goal-resume-browser.mjs`
and `target/goal-resume-replaced.mjs`. The original positive bug reproduction is
under `target/branch-admission-9c6851fd7d9844c79a5ccdbba2699e5c`.

An actual Windows daemon TUI also resumed the source, opened history/tree with
the configured F9 key, skipped optional summary transfer, retained its draft,
then sent/received the target answer while the source still ran. Source/target
history, generation, health and workspace verification exited 0; its idle
terminal exited 0. Native evidence is under
`target/branch-admission-d480881eafb24b6f853fd545f49025c5`, recorded in
`target/branch-admission-root.txt`, including `native-verify.json` and saved
histories. Initial injected control-key attempts did not open the overlay and
the held reply reached the standard idle timeout; those partial fixtures are
not navigation evidence. The fresh fixture used `[tui.keys]` and explicitly
set the agent's idle timeout to 600 seconds before starting the daemon. Earlier
`[keys]` and model-level timeout entries did not configure these settings.
The replies/verifier are scripted and do not measure model quality or cost.
All owned terminal, model, daemon and browser helpers were stopped and their
absence verified. Local goal metadata uses its existing ordinary-turn path;
the shared storage behavior is covered directly in core and through the daemon.

The first integrated `cargo xtask ci` with `RUST_TEST_THREADS=1` exited 1
after 1051.9 seconds (`target/pi-goal-resume-ci.log`). All 32 managed-work
checks passed, including the new continuation scenarios. CLI integration
finished with 93 passes and one failure:
`followups::killed_followups_resume_once_with_saved_settings_and_cancelled_ones_stay_stopped`
timed out waiting for the fourth streamed model request after restart at
`followups.rs:1462`. This is not a green gate. Its exact separate rerun exited
0 (`target/pi-goal-resume-followup-rerun.log`, one executed test, 122.20 seconds
including the daemon build). The cause of the first timeout is not established;
no assertion or timeout was weakened. The repeat full `cargo xtask ci` with
`RUST_TEST_THREADS=1` exited 0 (`ci: ok`, 1213.3 seconds;
`target/pi-goal-resume-ci-final.log`), including all CLI integration scenarios,
the managed-work checks, Clippy and doctests. `cargo xtask compaction` also
exited 0 (`target/pi-goal-resume-compaction.log`): 4.02 MiB on disk, 37.1x
dictionary compression and 5.8x end-to-end, matching the published measurements.

Queue remains In progress until the remaining lifecycle/frontend evidence is
audited against the original queue requirements. The reader audit found a
concrete remaining bounds gap: `chat/followups.rs::read` copies the entire
driver companion before checking 16 KiB; `message_queue.rs::read_from` and
the managed readers (`ids`, `read`, `for_session`, `read_identity`) use
unlimited `kv_get` although their writers already cap JSON at 8 MiB.
`execution.rs` also reads saved outcomes and evaluation caches with unlimited
`kv_get`; follow-up readiness reads a saved outcome. Next replace these reads
with admission against their existing writer bounds before copying, exercise
actual above-bound fixtures and unchanged state on refusal locally and through
the daemon, and retain legacy JSON/default compatibility. Then finish the queue
audit and actual HTML download/TUI interaction. Phase routing, declarative
extension UI and terminal experiments retain their original scope. The full
goal is active.
