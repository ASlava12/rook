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

The session investigation also exposed stale `model` activity while the scheduler
waits for a retry. That observation/reporting fix is separate and remains pending;
the wake-up change does not claim to correct its display.

Regressions cover an actual provider outage with a thirty-minute deadline,
same-session continuation after reopening the store, idempotent submissions and
edits, protected goal states and a real deferred-work checkpoint. Their result
is pending for the new revision; the preceding browser/cycle revision passed the
complete local workspace test preflight and the Linux CI gate.
