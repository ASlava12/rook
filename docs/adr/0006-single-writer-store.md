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

## What routes

All of it. Every `store`, `session`, `skills`, `memory` and `checkpoint`
subcommand goes over the API when the store is held, and each is the same call
the daemon makes on its own store — one implementation and two ways in, which
is what keeps the paths from drifting. `rook doctor` and `rook models` do not
route because they no longer need to: the configuration is a file and the
environment is the machine, so neither opens a store at all.

Getting there found two commands refusing for nothing — `skills sources` and
`skills search` read no store and had only been sitting behind the check — and
one real inconsistency, `store gc` assembling its own collection options and
ignoring the configured `gc_grace_secs`.

What still needs the store here is a turn: `run`, `chat` and `acp` write as
they go and cannot be a request. A person who wants a turn while `rookd` runs
has the daemon's own chat, which is the same engine from the other side.

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
