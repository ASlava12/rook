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
The index allows one writer at a time, and only `rookd` shares it: start that
first and every window works beside it, the browser included. A `rook tui`,
`chat` or `run` takes it alone — close that one, or start `rookd` before them.
```

## What routes

All of it. Every `store`, `session`, `skills`, `memory` and `checkpoint`
subcommand goes over the API when the store is held, and each is the same call
the daemon makes on its own store — one implementation and two ways in, which
is what keeps the paths from drifting. `rook doctor`, `rook models` and `rook lsp` do not
route because they no longer need to: the configuration is a file, a language
server is a file under the state directory, and the environment is the
machine — none of them opens a store at all. `lsp install` mattered: it refused
while `rookd` was up, which is exactly when somebody working notices a server
missing.

Getting there found two commands refusing for nothing — `skills sources` and
`skills search` read no store and had only been sitting behind the check — and
one real inconsistency, `store gc` assembling its own collection options and
ignoring the configured `gc_grace_secs`.

`rook tui` goes further: rather than taking the store when it is free, it makes
sure there is a daemon — starting one on a port the system picks if there is
none — and works through it. That is what makes a second window ordinary. It
was the first thing somebody who installed this hit, twice: two windows without
a daemon are impossible, because the window that takes the store serves nobody,
and the error said "`rook tui` works beside it" when what it works beside is
`rookd`. A window that starts one leaves it running and says so on the way out;
`--alone` is the single-process way back.

What still needs the store here is a turn started here: `run`, `chat` and
`acp` write as they go and cannot be a request. `rook tui` is the exception,
and shows the shape the others could follow — it holds the daemon's chat socket
and drives a turn through it, so a second window is another client of one
engine rather than a second engine that cannot exist. The browsing tabs read
over the API like every other command; what stays local there is the slash
commands, which write this process's store directly.

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
