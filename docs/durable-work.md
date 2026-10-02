# Session goals and scheduled tasks

[English](durable-work.md) · [Русский](ru/durable-work.md)

## Work now in the current session

In the shared `rook tui`, enter:

```text
/goal Implement the agreed plan and verify the result
```

The goal keeps this session's ID, transcript, model, effort and permissions.
It has no overall time, iteration or token ceiling. Each bounded stage continues
in the same session. Normal chat retains its limits. Messages steer the goal;
`✓ taken up` confirms inclusion in context, not execution. Ctrl-C pauses at a
safe boundary; `/continue` resumes. Session switching and closing the TUI leave
the daemon working. Restart recovers runnable goals; paused goals stay paused.
In `--alone` mode `/goal` sets metadata only.

## Tasks schedule future sessions

Press **F4**, or Ctrl-P → **tasks**, in `rook tui`. Tasks define when to start;
sessions hold the conversation, tools, approvals, result and verification.

- **n** creates a schedule. Enter advances through the fields and saves the last;
  Shift-Tab goes back, Ctrl-U clears a field, Escape cancels editing.
- Fields: goal, schedule, timezone, workspace, permissions, seconds, tokens,
  and iterations per run. The timezone is explicit; the TUI starts with `TZ`
  if set, otherwise UTC. The browser suggests its timezone.
- **r** runs now without changing the regular schedule (also works when disabled).
- **p / e** disable / enable future launches. Existing sessions continue.
- **[ / ]** select older / newer history; **Enter** opens that session.
- **x**, then **y**, cancels the latest session; completed operations are kept.
- **d** deletes a schedule once its last session is finished or cancelled.
  Session transcripts are retained under the normal retention policy.

Supported expressions (local calendar times in the chosen IANA timezone):

| Schedule | Meaning |
| --- | --- |
| `once 2026-12-01 03:00` | One future local date |
| `every 30m` | Every 30 minutes; `h` and `d` are also accepted |
| `daily 09:00` | Every day |
| `weekdays 09:00` | Monday through Friday |
| `weekly fri 09:00` | Every Friday (`mon` through `sun`) |

Use zones such as `Europe/Moscow`, `America/New_York` or `UTC`. A missing time
at the spring DST transition is skipped for recurring schedules; a repeated
fall time runs once, at its first occurrence. Ambiguous or missing one-time
local dates are rejected. Intervals are elapsed time, not calendar days.

Each occurrence opens a **new session**, tagged with its schedule ID. It starts
with the project configuration, skills and relevant memory, without importing
a previous run's conversation. The default per-run budgets are **one hour,
100,000 tokens and 100 stages**. All three must be positive. Tool and provider
requests already in flight can cross a budget; this is not a provider billing
cap. Tasks support readonly, assist and autonomous permissions. Assist is the
form's default; autonomous must be explicitly selected. Existing policy deny
and ask rules still apply. **Needs input** means open the session to answer an
approval or question; timed-out approvals are not automatically granted.

If a previous session is queued, running, paused, blocked or limited, the next
occurrence is skipped. Open that session to resolve it, or cancel it through
its goal control API (`POST /api/work/{session}/control`, JSON `"cancel"`).
The legacy `rook task cancel SESSION_ID` command also remains available.
When the daemon was offline, recurring occurrences more than one minute late
are skipped; the next future occurrence is scheduled. A missed one-time task
runs once when the daemon returns. Accepted launch reservations survive restart
and use the same session ID, so a crash during launch cannot start a duplicate.
There is no queue of every missed occurrence.

Schedules are stored in the daemon's store, with at most 64 schedules and 16
recent launches per schedule. Completed execution records are retired after
saving their status to history; transcripts have normal session retention.
Configuration/workspace errors disable the schedule and show a reason. A
schedule cannot guarantee exactly-once external effects: interrupted operations
still use the execution journal and may require recovery acknowledgement.

## API and optional CLI

`GET/POST /api/tasks` lists/creates schedules, `POST /api/tasks/{id}/control`
accepts `"enable"`, `"disable"`, `"run_now"`, or `"cancel_run"`; `DELETE /api/tasks/{id}` removes
a schedule. Creation requires a client-generated ULID and `spec` matching
`rook-proto::schedule::Spec`. Reusing the same ID and spec is idempotent;
conflicting settings are rejected. Run-now is an explicit new launch request,
not an idempotent retry: inspect history after an uncertain response.

```sh
rook task schedule "Check CI failures" --when "weekdays 09:00" --timezone Europe/Moscow
rook task list
rook task show TASK_ID
rook task run TASK_ID
rook task disable TASK_ID
rook task enable TASK_ID
rook task delete TASK_ID
```

Creation supports `--stance`, `--seconds`, `--tokens`, `--max-iterations` and
`--id` for retrying a submission. The former immediate `task start` and its
steering/control commands remain for compatibility with existing runs; they
are not the Tasks panel. Use `/goal` for immediate conversational work.

For immediate work, `rook task pause|resume|cancel RUN_ID` prints a control ID
and goal generation to stderr before sending the request. If the response is
lost, retry the same action with both `--control-id ID --generation GENERATION`.
The daemon records the action and ID together; a duplicate returns the current
run with `already_applied: true`, even after restart, without changing its state.
A reused ID with a different action or a generation from a replaced goal is
rejected. `POST /api/work/{id}/control` accepts
`{"id":"...","generation":"...","action":"pause"}` and returns a control
outcome containing `run`. Bare JSON actions such as `"pause"` still return a
bare run for older clients. Identified controls are limited to 1,024 receipts
per run; inspect the run if that limit is reached. The chat socket also uses
these receipts for identified `/continue` and Stop requests.

An old stored run without a generation still uses the bare action route from
the CLI; inspect its state after an uncertain response.

When `/continue` resumes a paused goal over the chat socket, its prompt ID is
also the resume control ID. Retrying that exact prompt after a later pause
cannot resume the goal again, including across a daemon restart. A client that
omits the prompt ID retains the older behavior. Ctrl-C and the browser Stop
button send `{"type":"stop","id":"...","generation":"..."}` for a managed
goal. The daemon announces that generation in a `goal` event when a turn starts,
is joined, or switches goals. A Stop retry with the same ID cannot pause a
goal again after a later resume. If the client has not observed the goal
generation, Stop reports an error and leaves the goal running; reattach and
retry. Old clients can still send `{"type":"cancel"}`. An identified Stop
without a generation stops an ordinary turn, but refuses to pause a goal.
The TUI and browser retain a Stop ID for repeated presses during the same
turn. The daemon emits `stop_applied` with the same ID after saving the pause;
this acknowledgement resolves an uncertain Stop. The TUI prints the goal ID,
generation and exact `rook task pause ... --control-id ... --generation ...`
retry command before sending. If the socket disconnects before confirmation,
the TUI keeps that ID while rejoining the goal. The browser saves one bounded
goal Stop in tab storage before sending and offers **Retry saved Stop** and
**Discard saved Stop** after a reload. Explicit retry reads the current goal,
checks the saved generation and uses the identified HTTP control, including
when no live socket remains after a daemon restart. Ordinary Stop carries its
execution turn ID; the browser retains that scoped frame in tab storage and
retries it over a reconnected socket. The TUI keeps one pending Stop per workspace
and session in a bounded private journal under `ROOK_HOME`, including across
window restarts. Open the original session and use `/retry-stop` to repeat its
exact caller ID and owner, or `/discard-stop` after inspection. Retry is allowed
while the same observed turn or goal generation is still running. Unknown
identity, another session, a successor turn or a replacement goal cannot receive
the saved Stop. The daemon acknowledges an already applied request without
stopping a later owner. A resumed goal may report its
current paused status on a duplicate prompt; inspect the goal before choosing
a new continuation.

## Edit or withdraw queued messages

Open **message queue** from Ctrl-P, or type `/queue`, in the TUI. The panel shows
pending messages from both the ordinary session and its current goal. It remains
usable while a turn runs; one background worker performs queue requests.
While a turn runs, the first queued message is also pinned immediately above
the prompt. The pinned line remains visible as the model writes and while the
chat is scrolled back. It shows a short preview and pending count from the
bounded queue view; `/queue` opens the full, editable message. The view refreshes
after receipt changes and periodically for changes from another window. Before
the first session ID is available, a local pending message gets the same short
preview.

If a daemon-backed TUI or plain REPL reports **Prompt saved** after a connection
or turn failure, `/retry` resends its original ID, session, text and options.
`/discard` releases that saved prompt so another can be sent; the old one may
already have reached the daemon. The plain REPL finds the current daemon address
again on retry. Both saved prompts live only until their terminal process exits.

- **e** loads the selected message for editing. Ctrl-S saves, Enter adds a line,
  and Escape cancels the edit. A conflict retains the unsaved text.
- **d** withdraws the selected pending message.
- **q** withdraws it and appends its complete text to the prompt draft. Existing
  draft text is retained; this does not send a prompt. An accepted message cannot
  be withdrawn or restored through this action.
- **r** refreshes, **a** includes accepted/withdrawn receipts, **n** reads the next
  page, and Enter reads a selected message. Arrow keys select a receipt.
- PageUp/PageDown or the mouse wheel scroll the read-only message detail. The
  position resets when another receipt is selected; editing has its own cursor.

In the browser, expand **Message queue** under the conversation. Use **Edit
message**, **Withdraw message**, or **Withdraw to draft**. The prompt supports
multiple lines: Enter sends, Shift+Enter or Alt+Enter adds a line. A withdrawal
that finishes after switching sessions keeps its draft handoff for the original
session; it never appends to the other session while that session is selected.

Live daemon chat receipts show their opaque reference, revision and current
state. Acceptance metadata comes from the transaction that saved the accepted
text. Submission, editing and withdrawal through either the combined queue API
or the older session/work instruction routes update attached running views. The
older routes keep their original response shape. Late queued notifications
cannot overwrite accepted or withdrawn
state. These marks mean inclusion in context, not successful execution of the
instruction. The queue panel remains the authoritative read after reconnect or
when the retained live view is shortened; refresh it to inspect older receipts.

The WebSocket's existing `interjected` and `agent` events optionally carry
`receipt: {session, reference, revision, status}`. Older clients can keep showing
their text, and events from an older daemon have no receipt metadata. Merely
writing a message to the socket is shown as sent, not as accepted.

Ordinary corrections typed during a running turn now use scoped HTTP submission
with a caller-generated ID, in both the TUI and browser. The TUI also uses the
same durable submission path for local turns once their session ID is known.
The destination is resolved once, before the first write. A retry keeps that
destination and the original ID/text, including when the receipt has since been
edited, withdrawn or accepted. Replacing a goal invalidates its old destination;
the request is rejected instead of being redirected to the replacement.

A running agent also pins the goal generation it joined. Reading a pending
message ID does not authorize later acceptance from a replacement goal: the
generation and conversation are checked again under the receipt mutation lock.
An old consumer stops at a safe boundary if its goal was replaced or forgotten.
While a goal is active, pending session messages from before promotion are
retained until an agent has joined the current runnable goal. This does not interrupt an operation already
in flight or make legacy in-memory submissions generation-qualified.

There is one unconfirmed send per window. In the TUI, open `/queue`: **u** retries
the saved send and **c** forgets the local retry. That retry survives a daemon
restart while the TUI window stays open, but is not saved across TUI restarts.
In the browser, **Message queue → Retry submission** uses the saved request even
after reloading the same tab. The browser stores only that pending request in
`sessionStorage`, checks the configured message byte limit, and gives each queue
HTTP request a 30-second deadline. **Forget pending send** removes the local
retry, not a server receipt: inspect the queue before sending the text again.
Forgetting does not withdraw a message that already reached the server.

The chat socket accepts optional `id` and `target` on `prompt`. A correction
sent while a turn or goal is running reuses its receipt when the same ID and
original text are sent again; a different text with that ID is rejected. Save
`submission_target` from the queue page with the ID before sending: the daemon
then rejects a retry aimed at a replacement goal. Older socket clients can
omit both fields. The CLI and TUI generate an ID for each socket prompt.
For corrections to running work, use the queue controls and their saved ID and
target for an uncertain resend. Early TUI input before its first session ID is
not yet a scoped queue request. Initial socket prompts and `/goal` creation
also support caller IDs, as described in
[the architecture](architecture.md). Ordinary prompt admission is acknowledged
before completion, so leaving its running branch can release the exact saved
frame. A start notification alone leaves uncertain delivery retryable.

The CLI exposes the same operations locally and through the daemon:

```sh
rook session queue SESSION_ID
rook session queue SESSION_ID submit "new guidance"
rook session queue SESSION_ID submit --id REQUEST_ID --target TARGET "original guidance"
rook session queue SESSION_ID list --all
rook session queue SESSION_ID show REFERENCE
rook session queue SESSION_ID edit REFERENCE --revision 2 "revised guidance"
rook session queue SESSION_ID withdraw REFERENCE --revision 2
```

Use the opaque reference and revision printed by the queue; do not construct a
reference from a visible row number. Goal references identify the particular goal
run, so an editor left open across a new `/goal` cannot change that new goal's
messages. Refresh after a conflict. Read-only views do not start or resume work.

`GET /api/sessions/{id}/queue` returns the combined pending view. Query parameters
are `include_finished=true` and `after=<next reference>`. Page size, byte budget
and preview size use `transcript.page_entries`, `page_bytes` and `body_bytes`.
Full text is available at `GET /api/sessions/{id}/queue/{reference}`. To edit or
withdraw, POST to the combined queue with JSON matching one of these forms:

```json
{"action":"edit","reference":"REFERENCE","revision":2,"text":"revised guidance"}
{"action":"withdraw","reference":"REFERENCE","revision":2}
```

To submit, retain `submission_target` from the page together with your generated
ID and original text, then POST:

```json
{"action":"submit","target":"TARGET","id":"REQUEST_ID","text":"original guidance"}
```

Keep both the target and ID when retrying; do not resolve the target again.
The CLI prints them on stderr before submitting, including in JSON mode, so they
remain available if the process is interrupted before receiving its receipt.
Its `--id` and `--target` options must be supplied together. Submitting by this
route never starts a new turn or resumes a paused goal. A new client refuses an
older daemon that does not advertise a submission target; restart that daemon
with the current build.

The target, reference and revision are rechecked under the mutation lock. The
older scope-specific routes below remain available for compatibility; new clients
should use the combined view and mutation routes for generation protection.
The same panel also manages follow-ups, described below.

## Follow-up turns

A follow-up waits for the entire ordinary turn or `/goal` to complete and then
starts a fresh turn in the same session. Use `/followup <text>` in the TUI or
REPL, **Queue follow-up** beside Send in the browser, or:

```sh
rook session queue SESSION_ID submit --follow-up "Then document the result"
```

The HTTP operation is `{"action":"follow_up","target":"TARGET","id":"ID","text":"..."}`.
Use the page's `follow_up_target`, retaining it together with the ID, original
text and mode for retries. The target identifies the preceding execution or goal
generation. Follow-up references remain `session.ID`. Text is bounded by the same
receipt and byte limits as steering. This first integration accepts text only;
attachments, recipes and output contracts belong to an ordinary prompt.

During live execution, successful completion drains eligible follow-ups in order.
A limit, error, pause or cancellation does not count as completion. A goal's
individual stage ending does not count either. Editing and withdrawal use the
same queue controls; edits stop once execution is reserved. Each follow-up gets
fresh turn limits and temporary secrets while retaining shared policy, approval
channels and MCP/LSP connections. A `follow_up` stream event marks the new turn;
the observer remains attached, and terminal `done` is sent when the chain stops.
The final summary describes the last turn; earlier output stays in the transcript.

The daemon scans idle sessions for eligible follow-ups, including after restart.
It checks at most `work.followup_scan_sessions` session IDs per one-second tick
(default 128, range 1..4096), continuing from its cursor; active work respects
`work.max_parallel_runs`. Sessions previously opened for daemon execution retain
their workspace, selected model, effort and stance. Frontend setting changes are
saved immediately and used at the next follow-up boundary. Rules come from the
current configuration on restart; temporary approval grants are not persisted.
Tool-initiated stance changes are saved at turn boundaries. A session run only by
a local CLI has no daemon settings snapshot: send an explicit prompt through the
daemon to establish one. The daemon does not invent settings for those sessions.

After a lost process, a reserved follow-up can resume with its original execution
ID and admitted prompt. Saved session and prompt hook context is restored without
running those hooks again. If its outcome was already recorded, recovery reuses
that outcome. Unknown side effects block recovery and appear in the queue status;
inspect and resolve them through `/recovery` before proceeding. Older admissions
without saved hook context, or setup effects completed before admission, require
explicit inspection. Recovery starts a fresh turn allowance, not the remaining
wall-clock time from the lost process.

Explicit cancellation pauses automatic follow-ups across restart. Use `/continue`
or another explicit prompt to resume the session; changing a setting alone does
not resume it. A failed recovery also pauses the driver and reports its reason in
the queue. An unaccepted stopped reservation can be withdrawn and resubmitted;
accepted messages remain immutable.

For ordinary work, `/continue` (also `/go on` and `/carry on`) keeps the original
completion boundary across execution IDs and daemon restarts. Messages queued
before or during a continuation wait until that work finishes; another step or
time limit does not release them. A new unrelated prompt starts a separate
boundary. Execution IDs, prompt receipts and outcomes remain distinct: an optional
JSON `continuation` field holds one root ID, not a growing ancestor list.

If `/goal` promotes a running follow-up, the daemon keeps its observer, approvals
and settings through the first supervised stage. That stage uses the new goal's
options, including its attachments. A replacement goal never inherits the old
goal's pending follow-ups. Inspect and withdraw/resubmit those messages explicitly
if they still apply. The final stream summary describes only the last turn;
recorded outcomes have a separate report below. Remaining parity work keeps the
queue capability in progress in the adoption tracker.

### Recorded turn results

`rook session turns SESSION_ID` lists saved outcomes newest first; `--json`
returns the same report as `GET /api/sessions/SESSION_ID/turns`. Pass the returned
`before` as `--before NUMBER` (HTTP: `?before=NUMBER`) to scan older results.
In the TUI, `/turns` opens this view; `t` also opens it from conversation history.
`n` scans older results, Enter reads the complete saved outcome, and `h` returns
to history. The REPL accepts `/turns [before]`. In the browser history panel,
choose **Turn results**, **Older results** or **Latest results**. Reading leaves
the draft and running work intact.

Each outcome retains its execution ID, prompt event, follow-up/continuation
identity, stop reason, token counts, steps, timestamps and reply preview. The
complete outcome is a byte-pageable history event. Totals count recorded outcomes
in this session, including turns stopped by a limit. Successful completion is
counted separately. Cache tokens are a subset of input, not extra tokens to add.
Elapsed seconds sum execution wall-clock spans, including downtime on recovery;
they are not CPU time. Token counts are provider reports, not currency estimates.

The report states its first covered prompt. Historical turns without outcome
records, attempts that never saved an outcome, and usage before a lost process
resumed are not reconstructed. Inherited branch outcomes remain readable in
history but do not count as new executions in the child. These limits mean
recorded outcome totals are not a complete accounting of every attempted model
request. The latest execution receipt remains the place to inspect interruptions.

Results, the latest recovery outcome, queue readiness and a constant-size ledger
commit together. Replaying that execution's saved outcome does not count it again.
Pages reuse `transcript.page_entries`, `page_bytes` and `search_events`; an empty
page can still return a cursor through tool-heavy history. Complete result bodies
use the existing 8 MiB recovery encoding limit. These notes do not enter model
context. Session retention and deletion cover these records too.

## Steering receipts

Messages sent while an ordinary daemon-owned turn is running are saved in its
session queue. Saving does not start another turn or create a goal. If the turn
stops before accepting them, they remain queued for an explicit continuation.
Goal corrections keep their work-run queue, including while paused. Both queues
use the same revision and withdrawal rules; only pending messages are mutable.
A message is accepted at a safe model-request boundary, after tool results. The
accepted text and its receipt commit together before the acknowledgement.

| Operation | Ordinary session | Goal or standalone work |
| --- | --- | --- |
| Read receipts | `GET /api/sessions/{id}/instructions` | `GET /api/work/{id}` → `instructions` |
| Submit | `POST /api/sessions/{id}/instructions` | `POST /api/work/{id}/steer` |
| Edit | `PUT /api/sessions/{id}/instructions/{message}` | `PUT /api/work/{id}/instructions/{message}` |
| Withdraw | `DELETE /api/sessions/{id}/instructions/{message}` | `DELETE /api/work/{id}/instructions/{message}` |

Submit JSON is `{"id":"client-generated-id","text":"guidance"}`. Edit sends
`{"revision":0,"text":"revised guidance"}`; withdrawal sends `{"revision":0}`.
Read the current receipt before changing it. A successful mutation increments
`revision`; a stale revision is rejected. Repeating that same successful edit or
withdrawal confirms its receipt. Retrying an original submission with its ID
returns its latest receipt even if edited, withdrawn or accepted, rather than
queuing another message. A new correction needs a new ID.

Receipts contain `applied_at` and `withdrawn_at`; both null means queued.
Acceptance is immutable and cannot recall actions already performed. A queued
`/goal new text` changes the goal only when accepted, so editing or withdrawing
it before that point has the expected effect. Corrections submitted before
promotion remain in the session queue and are consumed by the promoted session.
New submissions during a goal use the work queue.

`work.max_messages` bounds retained receipts per queue, including accepted and
withdrawn ones needed to recognize retries. At the limit, use a new session or
raise the setting. `work.max_message_bytes` limits each message before copying it
into a receipt. Deleting or pruning an ordinary session removes its queue too.

## Keep the daemon available

The machine must be awake with `rookd` running and model access available.
Rook does not install an OS boot service automatically. Configure systemd,
launchd or another host supervisor if schedules must survive logout/reboot;
only one daemon may own a given `ROOK_HOME`.

Automated tests exercise timezone/DST boundaries, missed dates, non-overlap,
durable reservations, daemon restart with a model request in flight, and the
TUI form. No real multiday model soak is claimed.
