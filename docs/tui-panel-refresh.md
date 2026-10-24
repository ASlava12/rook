# Refreshing data panels

The sessions, tools, skills, memory, checkpoints, documentation, models and
store panes read their data in the background. Their loading status appears
inside the pane. Press `r` to request another refresh.

A refresh reads the lists and the selected conversation, skill versions and
documentation pages. Keyboard input and streamed conversation events keep
using the main window while those reads wait. Repeated requests retain the
latest requested selection; an older response cannot replace a newer one.
Changing the browser selection does not change the active conversation targeted
by Stop.

`/session <id|last>` resolves the requested session directly, even before the
first panel snapshot arrives. The subsequent refresh keeps that session selected
instead of substituting the first row from the session list.

If a read fails, successful previous data for that panel remains visible, with
the failed panel named in the refresh status and the conversation's display log.
Changing the selected conversation, skill or documentation set clears details
belonging to the previous selection rather than presenting them as new data.

Only the panel reader uses these bounds: each daemon GET has a 15-second
deadline and a 32 MiB response limit. Accepted panel results share a
64 MiB serialized-payload budget. Use the paged conversation history view for
larger retained output. An obsolete read already in progress finishes or reaches
its deadline; the remaining obsolete reads are skipped. One reader executes
work, and repeated requests replace one pending refresh.

These controls apply to panel reads. Operations that install, edit, restore or
otherwise change data keep their existing routing and deadlines. Local MCP
initialization and separate session continuation reads are outside this refresh
worker. Local TUI MCP connections use their own [background startup](mcp-startup.md).

The reference for cancellable loading and stale-response checks is Pi's
session selector at `482c1381a00d8d00bc81697e4a980c593db6803e`.
Rook's worker, data retention and request bounds were implemented independently.
