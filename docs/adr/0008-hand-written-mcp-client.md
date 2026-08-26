# 0008 — A hand-written MCP client instead of the SDK

**Status:** accepted

## Context

MCP is how an agent consumes third-party tools, and it is the single largest
capability multiplier available. Both Rust references solve it the same way: codex
and goose depend on `rmcp`, the official SDK.

## Decision

Implement the transports directly in `rook-mcp`. Four methods are used —
`initialize`, `notifications/initialized`, `tools/list`, `tools/call` — over
newline-delimited JSON-RPC 2.0 on a subprocess's pipes, or over HTTP.

The two differ in more than plumbing, which is why the abstraction is at the
level of a whole request rather than of writing a line: stdio needs a
pending-request table because answers arrive on a shared pipe, while HTTP answers
each POST directly and must handle a reply that arrives as either a JSON object
or an event stream.

## Why not `rmcp`

Measured rather than assumed. Against Rook's existing dependency graph, `rmcp`
with only the `client` and `transport-child-process` features adds **21 crates**:

```
chrono  dyn-clone  futures  futures-executor  futures-io  nix  num-traits
pastey  process-wrap  ref-cast  ref-cast-impl  rmcp  rmcp-macros  schemars
schemars_derive  serde_derive_internals  tokio-stream  uuid  autocfg  cfg_aliases
```

`schemars` is the clearest sign of the mismatch: it exists to *generate* JSON
Schema, and Rook never generates one. MCP tool schemas arrive as opaque JSON and
are forwarded to the model untouched. `nix` is unix-only and needs conditional
handling on Windows, on a four-platform target list.

The whole client is ~250 lines. codex's wrapper *around* `rmcp` is 15,613 — that
is the cost of the transports, retries and OAuth flows Rook does not use.

This is not a claim that `rmcp` is wrong. It is a good SDK, and a project using
its HTTP transports, sampling, roots or elicitation should take it. Rook uses
stdio and tools.

## What the SDK would have handled, and is handled here

These are the parts that are easy to miss when writing a client by hand, and each
has a test:

- **stderr must be drained.** An undrained pipe fills and blocks the server
  mid-write, which presents as a hang with no output anywhere.
- **Responses must be matched by id.** Concurrent calls otherwise receive each
  other's answers.
- **A dead child must fail its pending calls.** Otherwise every waiter hangs until
  its timeout.
- **Handshake and call timeouts are separate.** A server that never completes
  `initialize` must fail startup fast rather than stalling the agent.
- **Notifications and junk lines share the pipe** with responses and must not
  disturb a call in flight.

## Cost

- No sampling, roots, elicitation or resource subscriptions.
- The HTTP transport does not open the optional server-to-client stream, so a
  server that initiates requests of its own is unsupported.
- Protocol revisions have to be tracked by hand. The subset in use has been stable
  across revisions, and `initialize` reports the mismatch when a server disagrees.
