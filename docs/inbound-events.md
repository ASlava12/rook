# Tasks from inbound events

Inbound events are optional and disabled by default. A trusted operator configures
an authenticated route, literal task, workspace and finite budgets. The daemon
receives signed JSON and records a durable delivery; its existing work supervisor
then runs the task. No socket owns the task, and an HTTP disconnect does not
cancel it. Payload fields never select task instructions, workspace, permissions,
model or tool approvals.

Configure a secret with `rook secrets add inbound`. This asks for its value; use
a private signing key of 16–4096 bytes. Existing vault sources are supported.
The value stays in the vault, not in config, delivery records or model messages.

```toml
[inbound]
enabled = true
body_bytes = 65536
state_bytes = 16777216
max_routes = 8
max_pending = 32
max_receipts = 256
timestamp_window_secs = 300

[inbound.routes.issue]
workspace = "/absolute/path/to/project"
secret = "inbound"
task = "Review the issue in the event source and report a proposed fix."
autonomous = false
max_iterations = 8
max_tokens = 65536
max_seconds = 1800
```

Route and vault names use 1–64 ASCII letters, digits, underscores or hyphens.
The workspace is canonicalized at receipt. Task instructions are limited to
16 KiB; route budgets are positive and finite. Current work budgets and handler
budgets can only tighten the frozen delivery ceilings. An autonomous handler
cannot raise the workspace's current stance. A non-autonomous handler tightens
it to at most Assist. Unattended approvals grant nothing; inspect the ordinary
task and its sessions when it needs input or recovery.

Changing a handler does not rewrite an accepted task. Its original instructions,
workspace and ceilings are retained. Disabling or removing a handler leaves
pending deliveries available for inspection/cancellation. Invalid updated
settings refuse dispatch. Already admitted tasks use ordinary work controls.

## Signed requests

POST exact UTF-8 JSON bytes to `/api/inbound/<route>`, with:

- `X-Rook-Delivery`: a producer-generated ID, 1–128 ASCII letters, digits,
  underscores, hyphens, periods or colons.
- `X-Rook-Timestamp`: Unix seconds as decimal ASCII.
- `X-Rook-Signature`: `v1=` followed by 64 hexadecimal HMAC-SHA256 digits.

The HMAC input is this exact byte sequence (each `\0` is one zero byte):

```text
rook-inbound-v1\0<route>\0<delivery>\0<timestamp>\0<raw request body>
```

This protocol binds route, delivery, time and body; it uses no unsigned fallback.
The timestamp must be within the configured 30–3600 second window. Retries may
use a fresh timestamp and signature while retaining the same delivery ID and
exact body. Semantically equivalent JSON with different whitespace is different
body content. A reused route/ID with different bytes is refused.

For an independent producer, the signing expression in Python is:

```python
signed = b"\0".join([
    b"rook-inbound-v1", route.encode("ascii"), delivery.encode("ascii"),
    timestamp.encode("ascii"), body,
])
signature = "v1=" + hmac.new(key, signed, hashlib.sha256).hexdigest()
```

`key` is the same private value configured in the vault; `body` is the exact
JSON bytes sent. Signature verification uses the
[RustCrypto HMAC verifier](https://docs.rs/hmac/0.12.1/hmac/).

The daemon's existing trusted Host/Origin rules still apply. A reverse proxy must
use a configured allowed authority and explicitly route this ingress. The normal
daemon default remains loopback. This feature adds no separate public listener.

A successful response is HTTP 202 with receipt, route, delivery, source session,
state, task identity if admitted, and `duplicate`. It does not disclose configured
instructions, credentials or task output to the producer. Invalid authentication
returns 401; oversized bodies return 413. Authentication and JSON validation
precede any delivery/session/task effect. Body size is enforced while bytes
arrive, including chunked bodies. Body arrival and admission each have a
20-second deadline; two admissions may be active. A slow credential resolver
continues to own admission after its caller times out. Retry the same delivery
when an admission result is uncertain.

## Durable state and task recovery

Source session, immutable source Note and receipt are created together in one
store transaction. Starting work publishes its run/index and delivery transition
atomically. Repeated delivery or dispatcher ticks return the existing identity;
they do not create another task. A lost or deleted source cannot be silently
replaced. The source Note retains exact original UTF-8 JSON, signed timestamp,
route/delivery and body hash. JSON numbers and whitespace are not normalized.
New state uses a bounded versioned JSON companion; stored postcard types do not
change.

The task uses the ordinary journals, policy, output, progress, queues and recovery
controls. Its iterations are children of the source session. Existing work in the
same workspace prevents overlapping task admission; the event remains pending.
A bounded rotating dispatcher prevents a busy queue from hiding later entries.
Provider failure uses ordinary durable retry state. Unknown tool effects require
ordinary observed recovery and acknowledgement; event retries never replay them.

The model receives at most a 4096-byte source preview, explicitly marked as data
with omitted-byte counts. Event sessions alone expose `inbound_read` for bounded
4096-byte pages of their immutable source. The tool accepts only a byte offset;
it cannot choose another receipt/session or change delivery state. Source notes
remain data on replay and do not become human-message learning evidence.

The ledger defaults to 32 pending and 256 retained receipts within 16 MiB.
Configurable bounds are 1–256 pending, 1–4096 receipts, 1–32 routes, 1 KiB–1 MiB
body bytes and 4 KiB–64 MiB ledger bytes. Reached capacity refuses new deliveries,
while duplicates remain inspectable. There is no automatic deduplication expiry.
Cancel pending deliveries explicitly; forget resolved receipts to free capacity.
Forgetting permits that producer ID to be admitted again and preserves its old
source session. Source sessions stay protected from automatic retention until
cancelled/forgotten; task operation history follows ordinary retention.

## Shared inspection

Use `rook events [JSON action]`, `/events` in chat/TUI, or **Inbound events** in
the browser Tasks tab. Local and daemon views use the same core. HTTP inspection
uses POST `/api/events` with an 8 KiB request limit. Responses are bounded to
1 MiB, and historical inspection works while ingress is disabled.

```json
{"action":"list","after":null}
{"action":"inspect","id":"<receipt>"}
{"action":"read","id":"<receipt>","offset":0}
{"action":"cancel","id":"<pending receipt>"}
{"action":"forget","id":"<resolved receipt>"}
```

Lists page 16 receipt headers with a continuation cursor. Inspection shows the
frozen task/workspace, source identity and bounded current task status/output;
that output is an observation, not an independent completion certificate.
Source reads return Base64 byte pages, object identity and the next offset.
Open the source/task session to inspect original operations and the session tree.
Control already admitted tasks through ordinary work controls. Active, limited,
blocked or uncertain tasks cannot be forgotten until ordinarily resolved or
cancelled. Pending deliveries must be cancelled before forgetting.

## Verification

Core fixtures verify signature/body/ledger limits, concurrent duplicate receipt
admission, changed guards, exact original bytes, restart, queue conflicts,
finite budgets, read-only policy, large-source paging and actual provider data
envelopes. A real CLI/HTTP fixture uses an owned local model protocol server:
acceptance is held behind existing work, the daemon restarts, then a single
ordinary shell task writes the event code and its independent checker reads the
file. Redelivery keeps its run identity, and local/daemon source reads match.
Both native TUI modes preserve drafts and create no task while inspecting.
Browser fixtures verify source paging, departed views and explicit forgetting.
These are protocol/runtime checks, not a claim of local-model answer quality.
