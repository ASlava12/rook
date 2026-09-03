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

Every read the CLI has goes over the API when the store is held: `store stat`,
`store ls`, `store cat`, `store refs`, `session ls`, `session show`,
`session diff`, `session context`, `skills ls`, `skills show`, `search` and
`memory ls`. Four writes route too, being the ones the daemon's API already
serves: `session goal`, `session rewind`, `memory rm` and `store maintain`.
Each is the same call the daemon makes on its own store, so there is one
implementation and two ways in.

The rest still refuse — `store gc`, `store train`, `session rm`, `skills
capture` and `skills install` — because they have no endpoint, and inventing
one per command is how the two paths start to drift. Adding an endpoint is
what makes each of them route; the honest error is what they get until then.

One of them is not identical routed. `store cat` over the API gets a windowed,
text-decoded payload, because the endpoint that serves it also serves a browser
and must not be the thing that takes it down; it says so when it happens.

## Why it is still not finished

The end state is that every command routes. Getting there is a meaningful
amount of work per command — an endpoint, a client path, and the two kept in
step — so it is done where the endpoint already exists and left honest where it
does not. Shipping the error is better than shipping a silent divergence
between two code paths.

## Alternatives rejected

**Multi-process access via file locking.** redb does not offer it; bolting it on
around an embedded B-tree is how corruption happens.

**Daemon-only, no direct access.** Would mean `rook store stat` cannot run without
starting a background service — a poor first experience, and wrong for scripting.

## Cost

Until this is done, running the daemon and the CLI at once needs the daemon
stopped, or the API used directly.
