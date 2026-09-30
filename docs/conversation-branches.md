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
| e | Edit the selected branch's name |
| n | Scan the next page of direct children |
| u | Load earlier ancestors omitted by the page limit |
| r | Refresh the currently explored branch |
| Esc | Close the viewer |

In the history viewer, **Shift+B** creates a branch from the selected event.
For a user message it copies events before that message and loads the complete
message into the editor. For other events it copies through the selected event
and continues with an empty editor. Enter submits the edited prompt in the new
branch; creating the branch does not call the model. Save or clear any existing
draft and selected attachments first: the editor will not silently replace them.
Lowercase `b` keeps its existing history back action.

`rook session rename SESSION_ID TITLE` changes a branch name. In the TUI's
history viewer, `m` labels the selected event and `l` opens saved bookmarks;
Enter jumps to the selected bookmark, `m` edits its label, `x` removes it, and
`b` returns to history. Branch names and bookmarks can also be edited in the
browser's Conversation branches and History panels. The CLI equivalents are
`rook session bookmarks SESSION_ID`, `rook session bookmark SESSION_ID EVENT LABEL`,
and `rook session unbookmark SESSION_ID EVENT`. Use `--json` for structured
output. The HTTP routes are `POST /api/sessions/SESSION_ID/rename` with a
`title`, and `GET` or `POST /api/sessions/SESSION_ID/bookmarks` with an `event`
and `label` for the mutation. An empty bookmark label removes the mark.

Labels are saved against exact event numbers. A deleted or pruned event leaves
its label visible as unavailable until removed. Forks inherit labels only for
events they actually copy. These names and labels aid navigation; they are not
inserted into model context.

The browser history panels offer **Edit in new branch** for user messages and
**Continue after event in new branch** for other entries. Retained historical
attachments are named beside the prompt and can be cleared explicitly. They
remain attached when moving between Chat and Sessions and are consumed on
submission. The source conversation is unchanged.

`rook session branch SESSION_ID EVENT_NUMBER` creates the same branch and always
prints JSON containing its node and optional draft (`text`, `attachments`,
`notice`), suitable for another editor or client. The HTTP counterpart is
`POST /api/sessions/SESSION_ID/branch` with `{"event": EVENT_NUMBER}`. This is a
mutation: repeating it creates another branch. After an uncertain response,
inspect the parent's tree before retrying.

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
edit_bytes = 1048576
name_bytes = 256
bookmark_entries = 64
bookmark_bytes = 16384
```

They cap children returned, encoded page size, session IDs examined per child
scan, ancestor depth and complete editor text respectively. Empty child pages
can still have a continuation cursor when unrelated sessions used the scan
allowance. Cursors are exclusive session
IDs and still work if the cursor session is deleted. Pages reflect current
metadata rather than a frozen snapshot. Title/workspace previews are limited to
256/512 UTF-8 bytes and expose truncation flags. This limits a single tree read;
it does not change retention of sessions or history.

`name_bytes` caps the full UTF-8 branch name. A bookmark label is limited to 128
UTF-8 bytes; `bookmark_entries` and `bookmark_bytes` cap the number and encoded
size of saved labels for one session. Concurrent edits are committed atomically.

Oversized or invalid editor content is rejected before creating a branch; it is
never replaced by a shortened history preview. Attachment records and the full
draft response additionally have a 16 MiB encoded limit. New messages with
attachments retain optional editor metadata in the existing JSON record, so
branching recovers the admitted prompt, filenames, text files and images.
If a recipe expanded the prompt, this is the expanded text. The recipe invocation
and output settings are not restored into the editor.
Older records still open: their complete prepared text (including embedded file
context) goes into the editor with an explicit notice, and their images remain
attached. Older Rook readers ignore the added JSON metadata and replay the same
model message. No postcard record layout changes.

Optional summaries of the departed branch remain part of the Pi adoption work.
The tree view does not generate or silently insert a summary into model context.
