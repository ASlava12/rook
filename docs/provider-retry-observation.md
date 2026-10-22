# Provider retry status

A temporary provider refusal can require a long pause inside a model request.
That pause is distinct from a durable goal scheduler retry. The LLM wrapper now
reports its already sampled delay, next physical attempt and optional HTTP status
before sleeping. The notice has no prompt, credentials or provider response body,
and does not admit or bill a generation.

The core scopes notices to the current loop and session. A bounded latest-value
channel supplies local frontend status; runtime observers also receive the notice
directly. CLI, TUI, ACP and browser views show the scheduled provider retry.
Physical admission clears the status. Cancellation and owner termination clear
both root and child observations; stored observations never establish a live owner
or authorize a request after restart.

Delegated waits also reach the parent's bounded display-hint channel. A child's
deadline remains in that child's activity projection: it does not replace the
parent's own model deadline or disable the parent's ordinary inactivity checks.

Activity snapshots add optional provider_wait data: until_ms, next_attempt and
status. Older snapshots and clients remain readable. Existing scheduler retry_at
keeps its original meaning. The expected provider pause excludes that period from
inactivity notices without changing last_progress_at. If a retry remains overdue
for the configured threshold, it can still produce a notice. Repeating the same
deadline cannot postpone that check. New physical requests receive normal
no-output monitoring. Actual parallel tool progress does not discard a known
provider deadline.

This does not change retry eligibility, credit recovery, physical attempt counts,
usage accounting, request budgets, work-slot ownership or human-blocker controls.
The provider wait is advisory runtime state, not a checkpoint or resume command.

The distinction between provider dispatch/status and substantive progress was
reviewed against the pinned Hermes auxiliary-client progress hooks; this is an
independent Rook implementation, not a port of a Hermes retry-observation API.
The [owned runtime fixture](../xtask/probes/provider-wait/README.md) complements
deterministic tests for sampled waits, cancellation, overdue detection, legacy
snapshots and root/child isolation.
