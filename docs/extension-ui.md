# Extension reports and forms

An explicitly enabled hook may return display declarations alongside its
existing `context`, `decision` and `reason` reply fields:

```toml
[[hooks]]
event = "post_tool"
command = "my-extension"
ui = true
```

```json
{
  "ui": [
    {"kind":"status","id":"build","text":"Checking changes"},
    {"kind":"progress","id":"scan","label":"Scanning","done":1,"total":3},
    {"kind":"result","id":"checks","title":"Reported checks","body":"Extension output"}
  ]
}
```

Reports appear in `rook session context ID`, its JSON/API response, and the TUI
and browser Context view. With the ordinary JSON reply, refresh the inspector
after a hook returns. The optional streaming protocol below also saves updates
while the hook is running; refreshing Context can read them before it finishes.
Reports are text. They cannot run HTML, grant approval or enter model context.
Only an explicit `context` field contributes model context. Existing hook
decisions retain their existing policy limits.

Each report carries the host's hook event, original configuration position,
digest of its event/position/command, and saved event sequence. The command is
not displayed or saved in the report. These identify the producer; they do not
certify its claims. The inspector labels reports as saved branch history and
states that current files and tests are not verified.

Within one source, the latest declaration for an `id` replaces its previous
report. `{"kind":"clear","id":"build"}` removes only that source's report.
Different hooks own independent IDs. Reopening restores the saved reports;
forking restores only the selected saved prefix. Later parent updates do not
change the fork. These records use the existing Note format, with label
`rook:extension-ui:v1`, zero usage counters, and no store-format migration.

## Bounds and invalid declarations

| Setting | Default | Range |
| --- | ---: | ---: |
| `extension_ui.max_update_bytes` | 4096 | 1024–32768 |
| `extension_ui.max_entries` | 32 | 1–128 |
| `extension_ui.max_state_bytes` | 32768 | 4096–1048576 |

The raw UI array is admitted before decoding its strings; array entries are
admitted before allocating each next item. Persisted record reads inspect the
object size before copying. Retained state checks entry and encoded byte limits
before keeping an update. Omitted updates and invalid saved records are counted
explicitly; retained reports may consequently be older.

IDs contain 1–64 ASCII letters, digits, dots, underscores or hyphens. Status text
is at most 1024 UTF-8 bytes; labels and titles are at most 256; result bodies at
most 2048. Control characters and directional overrides are refused; bodies may
contain newlines. Progress requires `0 < total <= 9007199254740991` and
`done <= total`, retaining exact integer display in all frontends. Unknown item
kinds/fields are refused. Producer-supplied source fields are not accepted.
Configured secrets are redacted before recording, and redacted fields are
validated again before storage.

Hook input serialization stops at 8 MiB before copying excess bytes. Hook output
is drained with at most 64 KiB retained. With `ui = true`, truncated or malformed
JSON fails the hook; it cannot fall back to model context. Invalid UI arrays are
ignored while valid explicit context and decisions still apply. Without the UI
opt-in, the existing JSON/plain-text hook behavior remains available.

## Streaming protocol and typed forms

Add `ui_stream = true` to a hook already declaring `ui = true`. The host writes
the existing payload as one JSON line to stdin and keeps that pipe open. The
producer writes one JSON object per stdout line, each containing exactly one of
`ui`, `form` or `reply`. Flush each line. Diagnostics belong on stderr.

```json
{"ui":[{"kind":"status","id":"setup","text":"Awaiting configuration"}]}
{"form":{"id":"setup","title":"Project target","fields":[{"kind":"select","id":"target","label":"Target","choices":["local","remote"]},{"kind":"confirm","id":"continue","label":"Continue"}]}}
```

The form waits through the existing question channel. Read the next stdin line:

```json
{"form_answer":{"id":"setup","status":"answered","values":{"target":"remote","continue":false}}}
```

Finish with one terminal reply line and exit successfully:

```json
{"reply":{"context":"Explicit model context, if needed"}}
```

The reply accepts the existing `context`, `decision` and `reason` fields. A reply
is applied only after exit status 0. Additional non-whitespace stdout, incomplete
frames, unknown frame fields, invalid declarations and exhausted limits fail the
hook; they do not become plain model context. Reports already saved remain
attributed historical reports even if the producer subsequently fails.

| Field kind | Answer value | Declaration |
| --- | --- | --- |
| `text` | string | `id`, `label` |
| `select` | one listed string | `id`, `label`, `choices` |
| `multi_select` | array of distinct listed strings | `id`, `label`, `choices` |
| `confirm` | boolean | `id`, `label`; the question offers Yes/No |
| `integer` | integer within the declared range | `id`, `label`, `min`, `max` |

A form has 1–4 fields; selection fields have 1–4 distinct choices. Form/field IDs
contain 1–48 ASCII letters, digits, dots, underscores or hyphens; field IDs are
unique in their form. Titles, labels and choices have 1–256 UTF-8 bytes. Integer
bounds stay within ±9007199254740991. Unknown fields, control characters and
directional overrides are refused. Each received field answer has at most four
strings of 1–1024 bytes, admitted before copying into typed values. The encoded
answer must fit `extension_ui.max_update_bytes`, including JSON escaping.

The host never invents an answer for an absent or expired response. Forms reuse
the existing question controls: in the terminal, an explicit empty Enter on a
single-choice question accepts its first displayed recommendation. The browser
requires choosing an option or typing an answer. A complete valid response is `answered`;
invalid selections, numbers or oversized answers are `invalid`; skipped, missing
or expired responses are `unanswered`. A host without an asker, including ordinary
noninteractive CLI runs, returns `unavailable`. These three statuses have
`values: null`, without partially valid values. The producer decides how to
handle them. Typed values are sent only to the producer's stdin, without automatic
model or journal recording. An extension can explicitly include them in its
reply context, so this is not a guarantee against a producer echoing its input.

Saved form reports use the producer's source and ID `form.<id>`, retaining only
title/status. `answered` means the host received a valid answer; it does not
certify that the extension used it successfully. Cancellation removes the current
question, stops the owned producer group and saves `interrupted` while waiting.
Late answers cannot revive that question. These reports have the same branch
prefix, source ownership, redaction and storage rules as ordinary reports.

The streaming line, including its newline, must fit `max_update_bytes`; stdout
has a cumulative 64 KiB cap, including trailing whitespace. `max_entries` also
bounds the number of stream frames, including the terminal reply. Initial stdin
payload serialization retains the 8 MiB cap, and concurrent stderr draining
retains at most 64 KiB. Command reads/writes have `timeout_secs` patience reset by
arriving chunks. That command timer is suspended only while the asker waits;
the existing user-input patience, admission limits and cancellation still apply.
After the terminal reply, child exit and pipe closure must finish within the
command timeout.

## Input recovery in the frontends

Local and shared TUI show each field with the host-assigned source, form title
and typed hint, using the existing question controls. Long captions wrap. The
browser presents the batch as one form. On socket disconnect it keeps the draft
disabled and exposes Reconnect; controls become usable only after the current
request is authoritatively recovered with the same ID and declaration. Changed
declarations discard the older draft. Resolved requests and terminal turns clear
the controls. Reconnecting does not resend the producer's question or the prompt.

Question and approval IDs are opaque strings unique to their channel, including
new channels after restart. A late answer cannot bind to a new channel's first
request. Exact durable prompt admissions recovered after disconnect also clear
the saved prompt retry; unrelated completions and receipts do not. Existing
protocol shapes and storage formats remain unchanged.

## Live chat reports

Committed updates refresh a persistent panel in local/shared TUI and browser
chat while the model or a form is waiting. Status, progress, result and clear
keep the same producer/source ownership as saved Context. Panels remain after
the turn ends and restore from the selected conversation's saved prefix on
reopen or branch navigation. They explicitly describe historical extension
reports; they do not verify current files or tests. The compact TUI shows up to
two recent reports and links result bodies to `/context`; the browser shows
bounded plain text, including result bodies. On small terminals, the panel
yields space to current input and the next queued message at the bottom.

Notifications retain only a coalesced revision, without producer data. Each
observer folds its own saved session prefix. Local TUI reads that state directly
instead of adding full snapshots to its turn-event queue. Daemon observers use
their bounded delivery queue and restore display from saved Notes on attachment,
independently of evictable transcript replay. Live display
can omit whole reports to fit the smaller queue/replay budget; omissions are
explicit and full admitted saved reports remain available in Context.

The existing `agent` chat event has an optional `extension_ui` object with
`session`, `through` (exclusive saved prefix), and `state`. Older clients still
receive attributed `text`. New clients apply structured snapshots only to the
observed session and ignore older prefixes. Replacing transcript history does
not erase the panel. A cancelled producer's saved interruption can refresh an
idle observer even after the agent task was aborted.

`GET /api/sessions/{id}/extension-ui` restores the same snapshot without
executing hooks or model requests. CLI local/daemon Source uses this read for
restoration; browser response bytes are admitted while arriving before JSON
decoding. Receiver limits are 128 reports and 1 MiB of encoded display state.
Malformed optional reports are refused and shown as omissions/invalid records;
display-read failure does not stop the agent. No store schema changed.
