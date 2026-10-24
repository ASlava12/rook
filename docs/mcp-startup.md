# MCP startup in the terminal interface

The local TUI opens while its MCP servers initialize and discover their tools.
Draft editing, commands and the MCP connections panel remain available. The
panel shows each pending server as `connecting`; ready servers publish their
catalog independently while retaining configuration order. Existing declaration
limits, parallel connection limits, timeouts and trusted plugin sources apply.

Until all initial connections finish or fail, submitting a model prompt keeps
the draft and explains that MCP startup is pending. No conversation entry or
model request is admitted. When startup finishes the window announces its
connected and failed counts. Send the draft again to start the turn; finishing
startup never submits or replays the draft automatically. Commands such as
`/mcp` remain available during this wait.

Initial connection attempts must settle before reconnecting a server or changing
its authentication. Afterwards the existing explicit reconnect and sign-in
controls work with the same session-owned pool. Active turns keep their original
tool bindings when a later connection is replaced.

Closing the window cancels its initial connection attempts. A routed TUI uses
the daemon's connections and does not start another copy of its servers.
This change covers local TUI MCP startup; plain CLI and daemon startup keep their
existing admission behavior.

The first-render and background connection design was informed by Pi's MCP
integration at `482c1381a00d8d00bc81697e4a980c593db6803e`. Rook's pool lifecycle,
progress publication and draft admission rules were implemented independently.
