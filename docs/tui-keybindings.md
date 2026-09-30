# Terminal keys and prompt undo

Open `rook config edit`, enter `tui`, then `keys`. Each named action has its own
description and list of shortcuts. Missing actions keep their defaults; an empty
list disables a shortcut. Conflicting shortcuts, unknown action names and invalid
key spellings fail offline validation before save. Reopen the TUI to apply edits.

For example:

```toml
[tui]
undo_events = 256
undo_bytes = 1048576

[tui.keys]
"prompt.undo" = ["ctrl+x"]
"prompt.redo" = ["alt+x"]
"prompt.editor" = ["alt+e"]
```

Keys use `ctrl`, `alt`, `shift`, or `super`, followed by a key name. Examples:
`ctrl+x`, `alt+left`, `shift+enter`, `f5`. Use `plus` for the plus key and `space`
for space. Modified keys depend on the terminal: for example, Super usually
requires an enhanced keyboard protocol and some terminals reserve Alt+Enter.
Choose a combination the terminal actually delivers. `ctrl+j` remains a default
newline shortcut that works without an enhanced protocol.

The command palette lists named actions with their **active** shortcuts and can
execute an action even when no key is bound to it. The help pane also lists the
active map; PageUp/PageDown scroll it. Global actions work across ordinary panes;
prompt actions apply while the composer has focus. Modal approval answers and
picker navigation retain their own keys.

Defaults preserve the existing palette, editor and history gestures:

| Action | Default |
|---|---|
| Command/action palette | Ctrl+P |
| External prompt editor | Ctrl+E |
| Transcript search | Ctrl+F |
| Tool calls | Ctrl+O |
| Undo prompt edit | Ctrl+Z |
| Redo prompt edit | Alt+Z |
| Submit | Enter |
| Newline | Ctrl+J, Shift+Enter, Alt+Enter |

Undo and redo change only the draft. `/undo` and session rewind continue to
restore workspace changes. Pasting a paragraph, completing a mention, clearing
the prompt or returning text from the external editor creates one undoable edit.
Adjacent word typing is grouped; moving the cursor starts a new group. A new edit
after undo discards the redo branch. Submission clears the draft's undo history.

The limits apply to the combined retained undo and redo edits. The history stores
text deltas, so typing into a large prompt does not copy the whole prompt each
time. An individual edit larger than the configured history budget is applied
without retaining an undo record and clears older history, whose offsets would
otherwise refer to a different draft. The limits do not truncate the prompt.

If a hand-written configuration contains invalid key settings, TUI reports the
errors and uses built-in shortcuts. `rook config check --offline` explains the
configuration errors without starting a provider or opening the store.
