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
The HTTP controls are available now; an interactive queue editor and follow-up
execution after a complete turn or goal are still being implemented.

## Keep the daemon available

The machine must be awake with `rookd` running and model access available.
Rook does not install an OS boot service automatically. Configure systemd,
launchd or another host supervisor if schedules must survive logout/reboot;
only one daemon may own a given `ROOK_HOME`.

Automated tests exercise timezone/DST boundaries, missed dates, non-overlap,
durable reservations, daemon restart with a model request in flight, and the
TUI form. No real multiday model soak is claimed.
