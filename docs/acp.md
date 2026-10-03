# ACP editor connection

Rook speaks stable ACP v1 over stdio (`rook acp`). An editor's file and terminal
capabilities are the same limits for parent and delegated work. MCP/LSP pools,
jobs and approval policy are kept for the connection, rather than rebuilt for
each prompt. The installed daemon is not involved in this transport.

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

Load/resume sends historical associations and completed compactions before its
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
observations. Adapter and shared delivery fixtures reach their request, child,
event and escaped-byte limits and check explicit failure.

The upstream contract is pinned in
[the reference review](research/reference-review-20261003.md); completion evidence
and remaining work are in [the adoption tracker](research/reference-adoption-20261003.md).
