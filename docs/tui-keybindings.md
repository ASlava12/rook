# Terminal keys and prompt undo

`/turns` opens recorded results and token totals for the current session. In the
history viewer, `t` opens the same view, `n` scans older results, Enter opens the
full stored outcome, and `h` returns to history. Esc closes without changing the
draft. See [recorded turn results](durable-work.md#recorded-turn-results) for
accounting scope and pagination limits.

`/tree [session-id]` opens [conversation branches](conversation-branches.md).
The same view is `v` in history or `b` in the sessions pane. Enter explores a
node, `h` reads its history, and `c` continues it while preserving the draft.
In history, `Shift+B` branches from the selected event. A user message becomes an
editable draft with its attachments; other events continue after that point.
An existing draft must be saved or cleared first. Creating a branch does not
submit a prompt or restore files; lowercase `b` still navigates back in history.
The tree's `e` edits a branch name. In history, `m` labels the selected event
and `l` opens bookmarks; Enter jumps to one, `m` edits it, `x` removes it, and
`b` returns to history.
`/summary TARGET_SESSION reviewed text` copies an explicitly sourced historical
summary from the open conversation into another branch; it does not switch or
restore files. See [conversation branches](conversation-branches.md).

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

Long draft lines wrap to the input width, including pasted text without spaces.
The box grows to ten rows and then scrolls vertically to keep the cursor visible.
When rows are outside the box, its border shows how many are hidden above and
below. Palette, memory, checkpoint-name and history input fields also grow for
long pasted lines. A paste exceeding a history field's limit reports the limit
instead of silently dropping the text.
Up/Down move through the visual rows before entering prompt history. Resizing
reflows the view; these soft wraps do not add newlines to the submitted prompt.

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
