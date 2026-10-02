# Extension reports

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
and browser Context view. Refresh the inspector after a hook returns. The hook
still runs once and exits; this contract does not stream progress during a hook.
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

## Remaining adoption work

Interactive forms, bounded typed answers, cancellation/reconnect behavior and
persistent live widgets remain unfinished. This report foundation does not
complete the declarative extension UI capability. Those additions must preserve
source ownership, text fallback and local/daemon/frontend parity.
