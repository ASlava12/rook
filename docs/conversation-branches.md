# Conversation branches

`rook session tree SESSION_ID` shows loaded ancestors, the selected session and
one page of its direct children. `--json` returns the same bounded page as
`GET /api/sessions/SESSION_ID/tree`. Pass `--after CURSOR` (HTTP `?after=CURSOR`)
to scan the next page of children. Children are ordered by session ID. Explore a
child to load its descendants, or its parent to find siblings.

The TUI opens this view with `/tree [session-id]`, `v` in the history viewer, or
`b` on a selected row in the sessions pane:

| Key | Action |
|---|---|
| Up/Down or k/j | Select a loaded node |
| Enter | Explore the selected branch |
| h | Read that branch's saved history |
| c | Continue that branch in the conversation pane |
| n | Scan the next page of direct children |
| u | Load earlier ancestors omitted by the page limit |
| r | Refresh the currently explored branch |
| Esc | Close the viewer |

Browsing and reading history leave the active conversation alone. Continuing a
branch preserves the unsent draft and does not submit it. In local `--alone`
mode, a running turn must finish or be stopped before switching conversations.
Through the daemon, switching changes the observed session; the previous turn
keeps running. In the browser, **Conversation branches** is available in both
Chat and Sessions, with separate Explore, Read history and Continue buttons.
The REPL `/tree [session-id]` prints a page; `/session ID` continues a session.

These actions do not restore files, change worktrees or transfer goals and
queued messages. Workspace recovery remains an explicit rewind/undo operation.
All sessions in a workspace see its current files, including changes made since
the conversation forked.

New ordinary forks record an exclusive boundary: `fork before #N` means events
before N were copied. The boundary stays fixed as the child grows. Older forks
without a saved boundary display **boundary unknown**. Delegated tasks are
labelled separately. Missing parents are reported, cycles encountered within the
loaded ancestry are rejected, and a bounded path is not presented as the complete
tree.

The default limits are editable in `rook config edit` under `branches`:

```toml
[branches]
page_entries = 64
page_bytes = 131072
scan_sessions = 256
ancestors = 32
```

They cap children returned, encoded page size, session IDs examined per child
scan and ancestor depth. Empty child pages can still have a continuation cursor
when unrelated sessions used the scan allowance. Cursors are exclusive session
IDs and still work if the cursor session is deleted. Pages reflect current
metadata rather than a frozen snapshot. Title/workspace previews are limited to
256/512 UTF-8 bytes and expose truncation flags. This limits a single tree read;
it does not change retention of sessions or history.

Optional summaries of the departed branch and editing a selected historical
message into a new branch remain part of the Pi adoption work. The tree view
does not generate or silently insert a summary into model context.
