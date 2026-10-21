# Execution capacity during ordinary retry waits

An ordinary future retry keeps its live control/observation owner but releases
its managed work slot. An unrelated project can therefore use the available
executor while the first goal waits. Workspace ownership and project/run bounds
continue to apply.

When the deadline is due or fresh guidance clears it, the continuation claims
a slot under the same admission lock used by other managed starts. It rereads
the run's generation, status and deadline after admission, before entering the
engine. Both automatic deadlines and user guidance wait behind an occupied
executor. Configuration is read after the admission lock; an engine read guard
is dropped before waiting for that lock.

Scheduled launches use the same due-readiness predicate as worker recovery.
Future waits no longer suppress a ready schedule merely because their saved
status remains runnable. Follow-up and Telegram admission count occupied live
slots together with active legacy workers, rather than all live observers.

Activity remains queued with the retry's deadline. A newly ready continuation
blocked by capacity reports when it first became eligible; repeated admission
checks do not move that timestamp. Slot changes and status messages do not reset
physical progress. The watchdog keeps observing unexpected overdue scheduling
even while the owner holds no executor. `owner_live` describes the control
owner, and `turns_running` continues to count live turn/control futures. Managed
admission applies `max_parallel_runs` to reserved execution slots; explicit
interactive chat starts retain their existing admission policy.

The admission/wait branches share one clock reading per pass, preventing a
deadline crossed between two wall-clock reads from bypassing slot acquisition.
The slot flag is runtime-only. Run persistence, failure counts, budgets, queued
message receipts, generation controls and unknown-effect recovery retain their
checks. Pausing can retire the waiting owner; explicit resume still passes
capacity admission. An explicit `defer_work` checkpoint retains its separate
persisted deadline and owner-handoff behavior.

## Reference and verification

The design independently adapts the distinction between active invocations and
not-yet-due sleeping tasks in Pi's durable scheduler at
`482c1381a00d8d00bc81697e4a980c593db6803e`,
`packages/durable/src/harness/scheduler.ts` (`wake()` and reservation passes).
No upstream code is copied and the reference pointer is unchanged.

The published Windows 0.21.0 package reproduced the gap: with one managed slot,
a failed conversation waited almost 1800 seconds while unrelated work remained
queued and never reached its local model within the six-second observation.
The same owned reproduction passed with the updated daemon.

All 97 Windows daemon tests passed. The new HTTP/API scenario uses four provider
permits against one managed slot so provider serialization cannot mask a faulty
admission. It verifies a reserved schedule starts during another goal's backoff,
forces the saved deadline due while the other model is held, queues guidance,
then pauses/resumes and confirms the same session receives that guidance only
after capacity is available. Controlled-clock coverage confirms slot release
does not invent progress and an overdue retry still emits a queued-stage notice.

The expanded owned HTTP/CLI/WebSocket probe passed all 22 checks with the final
debug daemon, the published 0.21.0 CLI and the tagged 0.21.0 skill text. It covers
legacy-worker capacity, same-session guidance, attached observers, process
restart, skipped batch tails, pause/cancel and explicit human-blocker resume.
This is local Windows candidate evidence; combined-source platform gates and
downloaded release-package verification remain required before publication.
Scripted providers do not establish live-model task quality.
