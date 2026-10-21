# User guidance during a managed retry wait

A provider or completion-check failure can leave a managed goal waiting for its
next attempt. Fresh explicit guidance now clears that ordinary retry deadline
and queues the next stage immediately. Editing a still-queued instruction also
admits one immediate attempt when its revision actually changes.

The message receipt and scheduler transition share the existing saved-state
transaction. A retried submission or edit with the same identity/revision does
not wake another attempt or reset idle accounting. Failure counts, spend, time
and iteration budgets, the active transcript and execution recovery are retained.
The receipt is acknowledged as applied only when delivered to the agent.

This is not a resume control. Paused, blocked, cancelled and limited goals keep
their state. An explicit `defer_work` deadline is preserved even when guidance
is queued for the next admitted stage. Existing generation and unknown-effect
recovery checks still precede work.

Live conversation goals now report an expected retry through the existing
`queued` activity phase and optional `retry_at` timestamp. CLI, TUI and browser
views describe the scheduled retry rather than an active model request. The
observation does not schedule work or establish ownership after a daemon restart.
Older saved activity records remain readable without the optional field.

Scheduling and its status announcement preserve the last physical progress time.
The expected wait is excluded from inactivity notices; a scheduler that remains
overdue for the configured inactivity threshold can still trigger a notice.
Starting the next stage clears the schedule, and actual model observations set
the model phase. Repeating the same deadline cannot postpone overdue detection.

Regressions cover an actual provider outage with a thirty-minute deadline,
same-session continuation after reopening the store, idempotent submissions and
edits, protected goal states and a real deferred-work checkpoint. All 54 managed
tests passed, together with six core activity tests, two Windows daemon activity
tests, workspace/all-target clippy and all 109 browser tests. The scheduled-retry
rendering case retains idle evidence without a false model-stall notification.
The clock fixture uses a margin around the whole-second wall-clock conversion.
The preceding browser/cycle revision passed the complete local workspace
preflight and all eight required platform CI jobs. Final candidate platform
validation and downloaded-package checks remain required before publication.

The owned native probe passed all 19 checks using locally built `rook` and
`rookd` 0.21.0. Its new conversation case establishes a thirty-minute ordinary
retry, observes the expected deadline through HTTP diagnostics and WebSocket,
then delivers fresh guidance to the same session in less than 20 seconds without
waiting for that deadline. Exactly two physical model requests occur: the
injected error and the successful guided stage. Earlier checkpoint, process-loss,
pause/cancel and batch-tail checks also pass. This is native Windows candidate
evidence; the extracted release package must pass the probe separately.
