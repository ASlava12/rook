# ACP editor connection

Rook speaks stable ACP v1 over stdio (`rook acp`). An editor's file and terminal
capabilities are the same limits for parent and delegated work. MCP/LSP pools,
jobs and run grants are owned by each active session and reused across its
prompts. MCP initialization runs inside the cancellable turn, so a server that
holds its handshake cannot block the connection's close/cancel reader. The
installed daemon is not involved in this transport.

JSON-RPC `result: null` remains a present result. Defaultable editor write,
terminal wait and cleanup acknowledgements may use it instead of `{}`; it does
not certify an exit code. A missing result or an explicit error remains a
failed acknowledgement. File reads still require their text `content`, and
scalar/array write acknowledgements are refused. Once a write was submitted,
an unavailable, error or malformed acknowledgement retains an unknown-effect
receipt instead of certifying that nothing changed. That receipt survives
connection close; another process cannot resume mutations or model work until
the user inspects the editor buffer and explicitly acknowledges recovery.

For the defaultable `session/list` request, omitted parameters and `null` use
the same unfiltered request as `{}`. Non-null malformed requests remain errors;
required session/workspace/prompt fields are not defaulted.
The [owned native probe](../xtask/probes/acp-null/README.md) checks this exchange
against an actual CLI with local scripted replies and isolated editor buffers.

Session capabilities advertise `list`, `resume` and `close`. `session/list` accepts an
optional absolute `cwd` filter and a returned `cursor`. Each page returns at
most 64 sessions and scans at most 512 metadata records, so a filtered empty
page can still have `nextCursor`. Cursors use stable session IDs. `session/resume`
validates the stored session and returns its configuration without replaying
historical content. `session/load` streams the fixed saved root prefix before its
successful response: user/assistant messages, narrative thoughts, tool arguments,
results and retained user/tool images. Stable IDs contain the source session and
event; each content chunk retains its event/object/byte offset. Loading never
executes a saved command, sends a permission request or exposes opaque provider
companions. Historical admissions do not establish a current live command, and
a terminal tool status requires its saved measured outcome.

History uses bounded metadata pages, one verified object and small UTF-8 text
frames. It awaits the shared output leases rather than filling a second queue.
`server.acp_history_body_bytes` defaults to 16 MiB and validates 64 KiB–64 MiB;
it bounds one retained decoded object, with encoded input admitted separately.
Each object is verified once before its text is streamed, avoiding repeated
whole-object decompression per fragment. An oversized object/image or unreadable
source returns a failed load rather than successful elision. Increase the named
body/queue limit when the original source needs more space. A history producer
runs separately from the incoming reader; closing its session still cancels it
while output is blocked. The response's `_meta.rook.history` reports its fixed
`through` boundary and `rootComplete`. Optional child recovery retains its
separate best-effort limits below.

`session/close` validates the session before cancelling its owned turn and awaits
future destruction before acknowledging closure. A cancelled prompt receives one
`cancelled` response. Pending editor requests are released; known editor terminals
receive `terminal/kill` and `terminal/release` requests. Local process groups and
remote command leases are released with that session's jobs; its MCP/LSP pools
shut down and run grants disappear. Closing a different session leaves the current
turn running. Agent-created children remain restricted: close is a parent control.
Stored history remains listed and resumable; subsequent prompts use fresh equipment.

`server.max_acp_sessions` defaults to 64 and validates 1–256. It bounds session
runtime equipment per editor connection, admitted by requested MCP setup or the
first valid prompt.
Reaching the limit refuses another session before cancelling existing work; close
an active session to make room. Metadata-only new/load/resume operations with no requested client servers do
not start tools or count against the equipment limit. Explicit MCP setup owns
equipment under that same limit.

The standard `session/set_mode` and `session/set_config_option` target only the
named session. Mode and effort selections use bounded versioned JSON companions,
survive close/resume and leave other sessions unchanged. A mode update reaches
that session's current policy; effort is selected for its next prompt. Corrupt
or oversized preferences fail rather than silently selecting a more permissive
mode. The legacy `session/set_stance` remains a connection-wide default for
sessions without an explicit mode selection. Temporary run grants stay scoped
to the active session and disappear when its equipment closes.

Editor cleanup requests carry IDs but keep no pending waiter after cancellation.
They request remote cleanup without certifying that an editor process stopped.
A lost terminal-creation reply can leave no usable terminal ID; original execution
uncertainty remains in ordinary recovery. Neither close nor resume replays an
uncertain command or acknowledges its effects.

## Session setup and MCP

`session/new` selects the requested existing absolute `cwd` and saves its
canonical identity. The legacy `.` spelling selects the server's launch project.
Load/resume use the saved project when legacy callers omit cwd, and reject a
supplied different or unavailable directory. Files, commands, editor bridges,
LSP and MCP therefore use that session's actual project. No moved workspace is
silently replaced by the launch directory.

`mcpServers` accepts bounded stdio declarations (`name`, `command`, `args`,
`env` name/value pairs) and streamable HTTP declarations (`type: "http"`, `name`,
`url`, header name/value pairs). HTTP support is advertised; legacy SSE and
additional workspace roots are not. Invalid/oversized declarations and name
collisions with configured/plugin servers fail before starting a process.
`server.acp_setup_bytes` defaults to 64 KiB and validates 4 KiB–1 MiB. Names,
commands, arguments and environment/headers also have individual admission bounds;
the combined catalog remains under `mcp_connections` limits.

Successful setup waits for every explicitly requested server's initialization and
catalog. A failed server returns a failed request with its retained session ID,
without credentials or a false ready result. Startup and history run independently
of the incoming reader: cancel without an ID reaches an unacknowledged new session,
and close reaches a setup whose session is known. One pending setup is retained
per connection; a second must wait/cancel rather than spawn an unbounded queue.

A session reuses its unchanged MCP connection across ordinary prompts and resume.
An idle load/resume replaces the complete requested list; an empty list releases
old client connections. An executing session must finish/cancel before its list
can change. Close shuts down owned connections; subsequent setup reconnects only
the list sent again by the client. Declarations and credentials remain in memory,
not operator configuration or a persisted secret vault. Credential-shaped
headers/environment values are hidden by ordinary result/history redaction,
including delegated child contexts, without modifying source files.

## Optional previews

The October 2026 pinned ACP preview contract is opt-in. In an `initialize` with
`protocolVersion: 1`, the editor may send:

```json
{
  "clientCapabilities": {
    "subagents": {},
    "session": { "compaction": {} }
  }
}
```

Each capability must be an object. Missing, null and boolean values keep the
legacy behavior; the two capabilities are independent. These draft updates
are not required additions to stable v1. Rook does not implement the v2 prompt
lifecycle or change ordinary v1 JSON-RPC prompt error responses.

### Delegated sessions

`subagent_update` on the immediate parent announces a child before any event or
editor request naming it. Nested delegation preserves the immediate parent;
an association cannot move to another parent or form a cycle. Tasks use directed
`session_message` content; streamed child answers use `session_message_chunk`
with sender/recipient IDs. Reasoning, tools and context usage have the child's
own session ID. Message and tool IDs are scoped to its actual turn, including
checker retries in the same child. Context counts are local to the session and
do not invent a price or a combined tree bill.

Child file, terminal and permission requests use that child's ID while retaining
the parent's capability and policy restrictions. Shared-root children cannot
borrow the parent's interactive `ask` tool. Worktree children retain their
separate tools, LSP, jobs and execution root; editor bridges are refused there.

The runtime has no independent child cancellation handle. Child capabilities
are therefore `{}`: client prompts, configuration changes, load/resume and other
unadvertised operations are rejected. A child cancellation cannot cancel its
parent. Parent cancellation still ends the owned turn. Loss of observation is
`unknown`, not evidence that an external command stopped. Actual completion is
`idle` with its stop reason; actual failure is `idle`/`error` with a JSON-RPC error.
Future stop reason strings are retained.

### Compaction

`compaction_update` has one `compactionId` for `in_progress` and its terminal
`completed`, `failed` or `cancelled` update. Completion follows the successful
store append. Summary-generation failure may still complete a replacement with
the existing neutral fallback directing the agent to its saved source log.
Failed persistence never claims completed compaction. Without the capability,
live root compaction boundaries appear as ordinary attributed thought text.

Only an ordinary completed summary is displayed. Signed/encrypted provider
state is not copied to a summary or editor update. Model summary output admits
combined text/reasoning before assembly: `agent.max_compaction_summary_bytes`
defaults to 64 KiB, validates 1 KiB–1 MiB, and the request asks for at most 2,048
output tokens. Oversized output stops the summary attempt and uses the neutral
fallback; a provider's large error cannot become an unbounded fallback note.

The saved Compaction JSON gains optional `compaction_id`; existing `through_seq`,
`dropped_events` and `summary` meanings, postcard structs and format version stay
compatible. Old readers ignore the new field and old records remain readable.
Recovery uses saved IDs; records predating IDs get a deterministic source ID.

## Recovery and limits

Load sends historical associations and completed compactions before its
response. Historical running states do not establish current activity. A child
without a durable completed outcome is `unknown`. On an existing live connection,
observed current states are confirmed after a successful response. Replay never
reissues a recorded permission, file or terminal request. Full child conversation
replay is best-effort in the draft and is not currently reconstructed by Rook.

Recovery admits at most 64 sessions (63 children), 128 tree pages, 32 compactions
and the last 128 event metadata records per session. The response's
`_meta.rook.recovery` reports these limits, `truncated` and
`childHistoryReplayed: false`. A stored record is admitted by raw byte size before
reading JSON (6 MiB + 8 KiB maximum). Display summaries/tasks admit at most 64 KiB,
less with a smaller transport budget, with UTF-8-safe prefixes and saved source
IDs when shortened. Live associations cap at 1,024 per turn. Call display fields
admit 1 KiB before copying. Permission preview shortening is explicit; an
operation subject too large for informed approval remains unanswered.

ACP uses the same `server.chat_queue_events`/`server.chat_queue_bytes` leases as
chat transport, including the frame currently being written. Escaped JSON byte
admission precedes encoding. Synchronous runtime observers cannot await a full
queue: exceeding a connection's budget closes that view explicitly, rather than
losing lifecycle or permission events. Durable execution remains available for
recovery. Pending editor requests cap at 256 and cancelled owners release them.
Once input closes, a blocked output writer has a five-second drain grace.

## Verification

Owned HTTP providers drive the actual engine and ACP wire in
`crates/rook-acp/tests/preview.rs`: concurrent/nested children, unsaved file
buffers, editor terminals, permission/tool identity, restricted controls, parent
cancellation, child versus root errors, capability fallbacks, recovery bounds and
actual compaction followed by store/connection reopening. Core fixtures verify
output admission, opaque-state exclusion, durable IDs, actual append failure
after generation and cancellation/error
observations. Actual editor-terminal and background-command fixtures verify
physical process death, closing another session without stopping its work,
run-grant isolation, fresh equipment after close and history retention. Nested
close retains unknown child outcomes and refuses independent child controls.
Socket fixtures reach the configured equipment limit before testing refusal and
release. A held owned HTTP MCP initialization request verifies that close cancels
startup before its unanswered handshake deadline. Adapter and shared delivery fixtures reach their request, child,
event and escaped-byte limits and check explicit failure.

The upstream contract is pinned in
[the reference review](research/reference-review-20261003.md); completion evidence
and remaining work are in [the adoption tracker](research/reference-adoption-20261003.md).


## Remaining interoperability work

Configured executors are available through the shared engine. The current full
regression result and any remaining verification are recorded in
[the functional adoption tracker](reference-functional-adoption.md).

## Configured external executors

A trusted operator profile can be selected explicitly through the ordinary
`delegate` tool. The model cannot supply an executable, arguments or credentials:

```toml
[acp_executor.profiles.helper]
command = "/absolute/path/to/an-acp-agent"
args = ["--acp"]
trusted = true

[acp_executor.profiles.helper.env]
# Optional trusted overrides, for example a separate home for another Rook.
ROOK_HOME = "/absolute/path/to/foreign-agent-state"
API_TOKEN = "secret:existing-alias"
```

Call `delegate` with `tasks` and `executor: "helper"`; do not combine `executor`
with `model` or `care`. Profiles without explicit trust are unavailable. Project
configuration cannot introduce trusted executable profiles. Launch admission uses
the inherited external-tool policy and approver. An ACP executable can run its
own tools independently: this opt-in is trust in that executable, not a sandbox.

Environment overrides are operator configuration, never model arguments. Names
are portable identifiers; at most 128 entries, 16 KiB per value and 64 KiB total.
`secret:alias` resolves from `rook secrets` only after launch permission, checks
the resolved bounds, and registers the value for output/history redaction. A
missing alias fails without starting a subprocess.

Each errand has an ordinary linked child session, selected workspace, journal,
progress and deadline. Explicit delegated context and hook context are sent as
bounded reference data alongside the task; the parent conversation is not sent
implicitly. Client callbacks support raw file reads, writes, native terminal
create/output/wait/kill/release and one-operation permission requests. Writes use ordinary dispatch, policy,
checkpoints and parent rewind. Raw reads preserve original line endings and optional one-based line windows.
`acp_executor.file_bytes` bounds source reads (default 1 MiB, 4 KiB–16 MiB);
encoded windows still obey the frame limit. Editor buffers stay authoritative,
and oversized editor text is refused before an owned text copy.

Native terminals use ordinary background commands and job receipts. Literal
arguments/environment values are shell-quoted; unsupported literal shapes on
Windows fail rather than executing a different argument. Output has its own
selected byte limit, and capture omission is distinct from secret redaction.
Terminal IDs are scoped to the external child, reject foreign/released IDs, and
retain completed output until release. Owned cancellation cannot stop parent
jobs. If this frontend owns an editor terminal bridge, native terminal capability
is not advertised; external requests cannot override that authority.
Foreign tool updates are observations of external work, never instructions to
execute the operation again. Duplicate callback IDs and wrong-session messages
fail without repeating an effect.

The client bounds frame bytes while receiving, serialized requests before
writing, cumulative response text, message/callback counts and stream silence.
Diagnostic stderr is drained into a tail bounded by the smaller of the frame
limit and 16 KiB, redacted and retained after cleanup. Cancel ends its owned
subprocess group. A missing completion response retains
unknown execution for inspection, including already completed native callbacks;
no external prompt is retried automatically. Output fragments are retained as
notes before the final external reply. Context-size updates are not billing:
external usage/cost remains explicitly unverified and stops further family model
requests, including completion checking/repair, rather than assigning native
model prices or creating another allowance. Inspect the saved child report and
actual workspace changes before a new request.

Actual Rook-to-Rook stdio, local CLI, daemon WebSocket, restart/source inspection,
both live TUI modes and browser outcome/source checks are covered by owned
fixtures. These prove transport and engine behavior, not model-quality gains or
external pricing. Local and daemon-backed TUI also display engine-produced final
reports when no model text delta carried them, without repeating ordinary streamed
answers. See the adoption tracker for the final combined regression gate.
