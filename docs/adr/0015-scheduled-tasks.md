# ADR-0015: Tasks schedule session goals

Status: accepted

Separate immediate Tasks duplicated conversations after `/goal` learned to keep
working in the current session. Tasks now own time and invocation settings;
sessions remain the single conversation and execution interface.

The daemon polls a bounded schedule registry once per second. The core computes
calendar occurrences using chrono/chrono-tz and persists a session-ID reservation
before creating a session or managed goal. Retrying an interrupted launch uses
that ID. Each occurrence gets a fresh conversation with positive budgets and
explicit permissions. The existing chat supervisor handles approvals, progress,
verification, restart and user steering.

No overlapping occurrences of one task are allowed, including paused/blocked
runs. Recurring deadlines more than 60 seconds overdue are skipped, while a
missed one-time task runs once on return. Reservations already accepted survive
restart. DST gaps are skipped and folds run once; ambiguous one-time dates are
rejected. Calendar zones are explicit and independent of the daemon's zone.

The registry holds up to 64 schedules and 16 launch receipts each. Terminal
managed records are retired only after their status is copied durably into
schedule history; transcripts retain normal storage lifetime. This bounds live
execution records over indefinite recurring use. Existing `/api/work` and old
CLI commands remain compatible; `/api/tasks` and the Tasks pane show schedules.

This does not promise exactly-once effects in external systems. Existing
execution journals block blind replay of uncertain mutations. The host must
keep the daemon available; installing an OS service is outside this change.
