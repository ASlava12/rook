# Closing a local TUI with MCP servers

The local window closes its MCP connections before dropping its async runtime.
On Windows, pipe reads can otherwise keep the runtime waiting while the server
process is still alive and its owner has not yet been dropped. A closed-looking
window then retains the store and cannot finish exiting.

Window close cancels the local turn and connection/authentication workers,
then closes the session-owned MCP pool while the runtime can still drive
cleanup. Remote HTTP shutdown is limited to five seconds. Dropping a cancelled
stdio shutdown still transfers the owned process group to the existing reaper.

A routed window leaves the daemon's connections and turns running. Closing the
window does not send a remote Stop, replay a tool call or restart an MCP server.
Local initial discovery also runs in the background; closing the window cancels
its pending attempts. See [MCP startup](mcp-startup.md) for draft admission and
connection progress.
