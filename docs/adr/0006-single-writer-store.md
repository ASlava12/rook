# 0006 — One writer process; the CLI opens the store directly, for now

**Status:** accepted, to revisit

## Context

redb allows a single writer process. The design has three front ends and a daemon
over one store, so contention is not hypothetical: `rook store stat` while `rookd`
is running is an ordinary thing to do.

## Decision

For now, the CLI and TUI open the store directly. When the store is already held,
the error says exactly that and points at the alternative:

```
Error: the store at …/index.redb is already open in another process (probably `rookd`).
The index allows one writer at a time. Stop the daemon, or read through its
API at http://127.0.0.1:7717 instead.
```

## What routes so far

`store stat`, `session ls`, `session show`, `session diff`, `skills ls`, `search`
and `memory ls` go over the API when the store is held. What is left is
`store ls`, `store cat`, `session context` and the skill detail — each needs
either an endpoint it does not have or a printer that works from the API's shape
rather than the store's types, which is the work below and not a different
decision.

## Why not fix it now

The fix is for the CLI to detect a running daemon and route through its HTTP API,
falling back to direct access. That is the right end state, and it is a meaningful
amount of work: every command needs a client path as well as a direct path, and the
two must not drift.

Shipping the honest error first is better than shipping a silent divergence between
two code paths. It is on the [roadmap](../roadmap.md).

## Alternatives rejected

**Multi-process access via file locking.** redb does not offer it; bolting it on
around an embedded B-tree is how corruption happens.

**Daemon-only, no direct access.** Would mean `rook store stat` cannot run without
starting a background service — a poor first experience, and wrong for scripting.

## Cost

Until this is done, running the daemon and the CLI at once needs the daemon
stopped, or the API used directly.
