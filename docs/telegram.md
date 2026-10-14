# Telegram conversations

The daemon can own Bot API long polling and map each authorized chat/topic pair
onto one ordinary Rook session. This transport is disabled by default. Session
history, tool policy, checkpoints, execution recovery and approvals use the same
engine as the CLI and browser. Closing a browser does not end the bot's turn.

Store the bot token using `rook secrets`, then name that secret in configuration:

```toml
[telegram]
enabled = true
secret = "my_bot"
workspace = "/absolute/path/to/project"
allowed_users = [123456789]
allowed_chats = [123456789]
stance = "assist"
max_turn_tokens = 65536
max_turn_secs = 1800
```

Both access lists are required. Every sender must be a human in `allowed_users`
and the chat must be in `allowed_chats`. A topic gets its own session. The first
accepted sender owns approval buttons in that route; another allowed sender may
submit text, but cannot answer that owner's buttons. Forwarded messages, media
and bot messages do not become human instructions. Unsupported media is skipped;
oversized authorized text stops polling before its offset is confirmed.
The bot identity is verified before input admission and cannot silently change
while existing routing history belongs to another bot. Rook does not remove an
existing webhook; polling refusal asks the operator to resolve ownership.

Text while a turn is running enters its ordinary durable steering queue. `/stop`
is captured against the observed live turn; retrying it cannot stop a replacement
turn. `/status` reports the mapped session and whether it is running. Approval
buttons offer allow once or deny, use opaque tokens and bind the sender, chat,
topic, request, process/turn epoch and expiry. After restart, old buttons cannot
authorize a new request. Single choice questions can offer buttons; free-form or
multiple questions point to the same session in the ordinary UI.

Bot turns have finite token/time ceilings. The trusted bot stance and the global
policy only tighten their permissions. No second agent loop is involved. If a
process is lost after a prompt was admitted, Rook retains its ordinary execution
receipt rather than replaying that prompt. New instructions remain explicit.

## Inspect and resolve deliveries

The same commands are available as `rook telegram '<JSON>'`, chat/TUI
`/telegram <JSON>`, POST `/api/telegram`, and the Telegram panel under Tasks.
An empty command reports status, including the last adapter error. Input inspection
retains the text and captured target even when the process stopped before prompt admission. Lists have 16-item pages and continuation keys:

```json
{"action":"routes","after":null}
{"action":"inputs","after":null}
{"action":"input","update":42}
{"action":"outbox","after":null}
{"action":"inspect","id":"retained-message-id"}
```

Outbound state is persisted before sending. A successful reply records Telegram's
message ID. A definite flood-limit refusal waits for `retry_after`, within the
finite `max_attempts` budget. A timeout, lost response or process loss during
sending leaves `uncertain`, which requires inspection and an explicit decision.
Telegram supplies no guaranteed send idempotency key: a reviewed retry can send a
duplicate. Decisions include the observed attempt count and a bounded note:

```json
{"action":"retry","id":"retained-message-id","attempts":1,"note":"Checked the chat before retrying"}
{"action":"discard","id":"retained-message-id","attempts":1,"note":"Keep the original uncertain receipt"}
{"action":"forget","id":"retained-message-id","attempts":1}
```

Sending or sent messages cannot be retried/discarded by these controls. Queued
messages respect their saved flood-wait deadline and can only be discarded. Forget
only accepts resolved `sent` or `discarded` receipts and frees outbox capacity.
Forgetting an output receipt does not resend its original session event: all
output parts and the event cursor were published in one transaction. Delivery
and source history remain in the ordinary session/store until normal retention.
Input receipts keep a bounded recent history; the confirmed offset prevents old
updates from creating another instruction after a receipt is evicted.

Responses use plain text and are split conservatively at 4,000 UTF-16 code units.
Large stored responses are shortened at `message_bytes` with the session/event
anchor for reading the full source. The adapter bounds poll batches, routing,
input receipts, outbox, callbacks, request/response bytes and retained state. Full
outboxes stop publication until the operator resolves/forgets deliveries; they
do not silently drop results. Credential URLs and upstream bodies are excluded
from transport error messages. HTTP is permitted only on loopback for fixtures;
production Bot API transport uses HTTPS and disables redirects.

## Verification scope

Local protocol fixtures exercise HTTP against owned loopback servers, including
byte limits, duplicate IDs, topic routing, callback identity, flood waits,
uncertain sends, restart, split publication and source/acknowledgement boundaries.
The daemon fixture additionally uses the actual CLI/API and ordinary tool engine.
These checks establish transport behavior, not model quality or delivery through
an operator's live bot. A live check requires a configured secret alias and
explicitly authorized user/chat IDs; no credentials are invented.
