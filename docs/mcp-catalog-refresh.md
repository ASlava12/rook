# Live MCP tool catalogs

Set `refresh_tools = true` on an approved existing `[[mcp]]` server to opt in.
The default is false. Tool-list notifications require the server to negotiate
`tools.listChanged`; replacing a known transport also invalidates its list.
Resource-only servers are never probed with `tools/list`.

```toml
[[mcp]]
name = "example"
command = "your-approved-server"
refresh_tools = true

[mcp_connections]
refresh_timeout_secs = 30
```

The phase ceiling is 1–300 seconds, additionally capped by the remaining turn
deadline. Each server retains its existing `catalog_timeout_secs` and byte/tool/
page limits. Default limits are 8 MiB, 4096 tools and 64 pages. Refresh coalesces
readers, runs at most four sources concurrently and attempts at most three
consistent complete listings when notifications arrive during pagination.
Failure or cancellation retains the previous complete snapshot and applies a
five-second cooldown before another automatic attempt. Progress identifies the
catalog phase rather than reporting a wait for the language model.

New descriptors enter the next model/discovery request in the same turn. An
already admitted tool batch keeps its original connection and descriptor binding.
Recipe restrictions and removed tools continue to apply through deferred
`mcp_call` dispatch. `mcp_tools` replies return a `source` token: continuations
with `offset > 0` must repeat it and the same tool name or query. A changed source
requires restarting at offset zero. This token certifies read continuity; it
never grants execution permission.

HTTP servers may provide an optional owned GET notification stream at their MCP
URL. It uses the same proxy, auth, session and negotiated protocol headers.
404/405 falls back to notifications in POST response streams. Disconnect recovery
makes at most three opening attempts, honors SSE retry hints up to 300 seconds,
and stops automatic recovery for longer hints. Reconnection invalidates the list
to cover missed events. Shutdown and last-owner drop close the listener. GET
server requests cannot borrow a concurrent call's human-input authority; bounded
unowned requests receive a static refusal. Stdio preserves its serialized
interactive lease, so refresh may wait or retain the old list beside a long call.
These wire contracts follow the [MCP transport specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

MCP Apps confirmations are retired when the catalog is dirty, its generation
changes, or the connection is replaced. Open the current declaration and grant
fresh explicit consent. An old confirmation cannot authorize a replacement.

CLI/REPL MCP status, the TUI MCP panel, API and browser report configured and
negotiated refresh, generation, stale/current state, notification channel and
cooldown/error. Reading status performs no probe and makes no endpoint-liveness
claim. A server without list-change negotiation still exposes refresh failures.

Verification: `rook-mcp` client and `catalog_changes` suites exercise stdio and
native HTTP notification/retry/cancellation behavior. `rook-tools`' `mcp_catalog`
and `mcp_refresh` suites exercise pagination and retained bindings.
`rook-core`'s `reference_cycle` exercises an actual agent turn (direct and deferred
discovery), Apps consent invalidation and resource-only connections. This is
fixture evidence; it does not certify a particular third-party server.
