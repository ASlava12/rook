# Agent-to-agent messages

Rook's native agents can exchange messages inside one active root turn and its
delegated descendants. The same engine tool is available through CLI, TUI, HTTP
chat and the ACP editor adapter. Each agent keeps its own transcript, tool
permissions and token allowance.

For independent agents on different computers, use
[remote agents over authenticated HTTP(S)](remote-agents.md). That transport
admits durable tasks and messages rather than extending a volatile local inbox.

Start independent work with `delegate` and `wait:false`. The parent's ordinary
model requests and `subagents` collection continue to poll the children. Use
`agent_message` to discover recipients and exchange findings:

```json
{"action":"peers"}
{"action":"send","to":"SESSION_ID_FROM_PEERS","text":"The parser already handles empty input; inspect parser.rs:84."}
{"action":"send","to":"parent","text":"The failing case is isolated; the existing test covers it."}
{"action":"status","id":"MESSAGE_ID_FROM_SEND"}
```

These are arguments to `agent_message`, not CLI subcommands. `peers` returns the
caller's `self` and `parent` session IDs, plus up to 32 participants with task
previews and `active`/`closed` status. For another page, pass its `next` value as
`after`. IDs are session IDs, not the parent's local `task01` names. Siblings and
grandchildren can communicate directly. A guessed ID from another root turn or
an independent conversation has no mailbox in this family.

`send` returns a message ID and `queued`. Delivery occurs before the recipient's
next model step, after tool results are complete. `status` returns:

| Status | Meaning |
|---|---|
| `queued` | Accepted into this recipient's bounded runtime inbox. |
| `delivered` | Saved in the recipient's transcript and admitted as context data. This does not prove that the model acted on it. |
| `recipient_closed` | The recipient ended or was cancelled before admitting this queued message. |

Only the sender and recipient can query a receipt. Sending to a closed recipient,
yourself, an unknown ID, a full inbox, or an exhausted family quota returns a tool
error. Sending neither starts a new turn nor revives a completed agent. There is
no automatic reply, broadcast or ping-pong loop.
Message IDs include a fresh ULID so a receipt from an earlier turn cannot alias
new mail when that conversation resumes.

The engine supplies `from`, `to` and message IDs. Callers cannot supply a sender.
Configured secrets are redacted before delivery. Received envelopes are stored
as `rook:agent-message:v1` Notes and replayed inside the ordinary `rook_source`
data boundary; they are not user instructions or approval grants. They also enter
compaction and the checker's bounded saved-history excerpt. `rook session show`
and the existing frontend transcript views retain the recipient's record.

Queues, receipt states and participant discovery last for this root turn only.
Accepted recipient records persist and replay after restart; pending messages
are not durable work and are not resent. An independently resumed child gets a
new turn family. External ACP executor processes are not endpoints of this
mailbox transport; existing ACP delegation remains their explicit integration.
Independent Rook daemons communicate through the `remote_agent` transport above.

## Restrict individual tasks

Task objects in `delegate.tasks` accept `tools`:

```json
{
  "tasks": [
    {"task":"Read parser.rs and send findings to the parent.","tools":["read_file","agent_message"]},
    {"task":"Implement the parser fix and run its tests."}
  ],
  "wait": false
}
```

Omitting `tools` inherits the parent's equipment. An explicit list intersects
the inherited tool allowlist; `[]` exposes no tools. This controls both advertised
schemas and dispatch, including a model attempting a tool it was not offered.
It applies to waiting/background delegation and worktree children, and continues
through nested delegation and recipes. Include `delegate`, `subagents` or
`agent_message` only where that task needs them. Existing sandbox policy still
judges each allowed operation. Conflicting lists for duplicate tasks, malformed
names, and lists longer than 128 entries fail before children start. External ACP
executors cannot enforce this allowlist and reject the combination.

## Limits

Operator configuration under `[agent]`:

```toml
max_agent_message_bytes = 8192
max_agent_inbox_messages = 32
max_agent_messages_per_turn = 256
```

The byte cap applies to UTF-8 input before and after redaction. Hard ceilings are
64 KiB per message, 256 pending messages per recipient and 4096 accepted messages
per family. Zero in any setting disables sends. Delivered or discarded messages
still count toward the family quota. Participant retention also uses
`max_subagents_per_turn`, with a hard mailbox ceiling of 4096 participants
including the root. A membership guard closes inboxes on normal completion,
failure and cancellation. Peer pages and task previews are bounded separately.

Deterministic engine tests exercise sibling delivery, child-to-parent reporting,
attributed replay and both task-allowlist execution paths. Mailbox tests cover
size/queue/family limits, failures before append, closed recipients, discovery
pages, receipt visibility and redaction. These tests use scripted providers;
they do not establish the quality of a real model's collaboration decisions.
