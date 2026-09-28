# 0014 — Durable tasks belong to the daemon

Status: accepted.

## Problem

A single model turn has finite context, step, time and token allowances. The
foreground work loop depends on a client process and a project scorecard; two
failed/no-change iterations stop it. Repeatedly raising those allowances cannot
give it reliable operation across outages and restarts. Volatile interjections
also cannot tell a reconnecting client whether a correction was delivered.

## Decision

Keep the bounded AgentLoop. Add a durable task state machine in `rook-core`,
scheduled by `rookd` and exposed to all frontends through `rook-proto` and HTTP.
Retain the foreground `rook work` interface for scripts.

Persist the goal, cumulative budgets, active iteration session, completed turn
receipt, retries and bounded history. Publish the registry and a new record in
one store transaction. Serialize record updates so a worker cannot overwrite a
correction or pause saved while it awaited a model. Only one worker owns a task,
and only one nonterminal task owns a workspace.

Steering IDs are client-generated idempotency keys. Acceptance saves a pending
receipt. At safe boundaries AgentLoop logs the instruction, places it into
context, and saves its acknowledgement. Previously applied instructions carry
forward across iterations/retries; replaying context does not authorize replay
of effects. Final verification is fenced by the instruction revision, so a late
correction invalidates completion. This is durable delivery, not exactly-once
execution of the business operation described in a prompt.

Provider failure retains the session and schedules a bounded retry. Unknown
operation effects use the existing execution recovery gate, including harness
commands. Completed checks are reused through their witness-aware receipt cache.
Completion requires independent evidence for the current goal as well as any
configured project scorecard. Saved model verdicts are not trusted after a
restart without checking current evidence again.

## Costs and limits

A running operating-system process, available machine and working model are
still required. Boot/restart supervision belongs to systemd, launchd or the
operator; Rook resumes tasks when its daemon starts. Safe-boundary cancellation
waits for the current operation. Context, record counts, message sizes, retained
summaries and retry windows are bounded/configurable. Budget checks are not
provider-side billing enforcement. Independent model verification can still be
wrong, and unrelated external edits can invalidate evidence after it was read.

Tests simulate elapsed days and exercise actual process restarts, pending
delivery, late corrections, retry accounting and unknown-effect recovery. They
do not substitute for a real multi-day soak with a production model.


## Session goals

`/goal <text>` uses the existing chat session as the run identity and keeps every
stage in that same transcript. An optional conversation descriptor records the
session, model, effort, stance and turn options as JSON; existing records default
to standalone runs. Conversation goals have no overall budget by default, while
the individual AgentLoop stages and ordinary chat keep their bounds. A saved
stage's starting event sequence prevents an earlier stage's completion receipt
from being mistaken for a newly admitted stage; token accounting starts at the
existing session family's current spend.

The daemon reuses its live-chat registry for streaming, approvals, questions and
session switching. The supervisor restores missing live conversations after a
restart. Steering is durable, Ctrl-C pauses at an operation boundary, and closing
or switching a view never cancels a goal. No second task ID or task pane is needed.
