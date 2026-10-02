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
The TUI refuses that switch before generating or saving a transfer summary.
Through the daemon, switching changes the observed session; the previous turn
keeps running. Its late output and connection errors cannot replace the new
conversation. Prompt attachments remain selected for the next explicit send;
the browser names retained files even when its replaced native file field is
empty, and **Clear selected files** releases them. Switching while the browser
is preparing attachment bytes cancels that send and preserves the draft.
An ordinary prompt acknowledged as durably admitted stops blocking a later
prompt in another branch, even while its original answer continues. A prompt
whose admission is uncertain still requires explicit retry or discard.
In the browser, **Conversation branches** is available in both
Chat and Sessions, with separate Explore, Read history and Continue buttons.
The REPL `/tree [session-id]` prints a page. `/session ID` on another branch
first offers a summary review; `/summary-draft ID` loads excerpts and
`/summary-suggest ID` requests a model draft. Review and save with `/summary ID
TEXT`, or repeat `/session ID` to continue without saving a summary. This works
locally and through the daemon. `/session` without an ID shows the current one.
In the TUI tree, `c` on a different branch offers `d` for recorded excerpts,
`s` for a model draft, `c` again to continue without a summary, and Esc to
cancel. Choosing `d` or `s` opens a separate bounded multiline review editor;
the main chat prompt and attachments stay intact. Enter adds a line, Ctrl+U
clears the review, Ctrl+Z/Ctrl+Y undo/redo, and Ctrl+S saves the reviewed text
and continues the selected branch. Ctrl+Enter also confirms in terminals that
report it. Esc cancels without writing. While saving, the editor waits for the
reply so a second key cannot repeat the write. A failed save keeps the edits;
check target history before retrying an uncertain response. If the source
changed, cancel and request a fresh draft. The source session and event boundary
are pinned, including when the tree was opened from historical Calls, history
or turn results. Opening the offer never saves or requests a model draft.
The explicit `/summary-draft` and `/summary-suggest` commands still print to
chat for saving separately with `/summary`.
In the browser, **Continue in chat** on another branch first offers a reviewed
summary. **Review summary** opens the existing editor, **Continue without
summary** switches directly, and **Cancel** leaves the current conversation
selected. Opening the offer does not generate a model draft or write history.

To prepare a transfer, run `rook session summary-draft SOURCE TARGET` or
`/summary-draft TARGET` in the REPL or TUI with the source conversation open.
The browser's **Carry reviewed summary** editor has **Load recorded excerpts**.
This reads at most 128 recent source events and includes at most 12 bounded
user/assistant excerpts with event numbers. It is a starting point for editing,
not a model-generated conclusion. Earlier events and attachments can be absent.
When saved fork boundaries identify a shared prefix, excerpts start at the
source's first event after that prefix. For unrelated sessions, the whole source
is eligible. Older forks without a known boundary are labelled as unscoped;
their excerpts may include shared history. The draft response includes
`source_from`, `common_ancestor`, and `scope_known`. If the bounded scan finds
no text after a known boundary, write a summary manually.
For a model-written starting point, add `--suggest` to the CLI draft command,
type `/summary-suggest TARGET` in the REPL or TUI, or choose **Generate suggested
summary** in the browser editor. The model receives only those bounded
historical excerpts as data, and its answer is capped at 16 KiB before being
kept as an editable draft. Generating it does not write to the target branch.
Review and rewrite it, then use `rook session summary SOURCE TARGET TEXT`,
`/summary TARGET TEXT`, or **Save summary and continue**. To pin the source
boundary printed with the draft, add `--source-through EVENT` to the CLI save
command or use `/summary-at TARGET EVENT TEXT` in the REPL or TUI. The browser
editor and TUI/daemon REPL draft flows pin it automatically; a changed source
branch makes the save fail until a new draft is reviewed. The saved history
identifies the source session and event boundary and tells the next turn to
verify historical file and test observations in the current workspace. A
browser draft is rejected if the source branch changed after it was loaded.
The HTTP draft route is `GET /api/sessions/TARGET/summary-draft?source=SOURCE`;
model suggestion is `POST /api/sessions/TARGET/summary-suggest` with
`{"source":"SOURCE"}`. Both return the same draft metadata.
`POST /api/sessions/TARGET/summary` accepts optional `source_through` with the
existing `source` and `text` fields. Both source and target must belong to the
same workspace. The summary body is limited to 16 KiB UTF-8 bytes.

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

To carry a reviewed summary when leaving a branch, use **Carry reviewed summary**
on the target node in the browser. In the CLI, run
`rook session summary SOURCE_SESSION TARGET_SESSION "summary text"`; in the
TUI or REPL, open the source conversation and enter
`/summary TARGET_SESSION summary text`. This saves a `branch-summary` event in
the target, then the browser continues there. CLI/TUI/REPL can switch separately
with their existing session controls. The HTTP equivalent is
`POST /api/sessions/TARGET_SESSION/summary` with
`{"source":"SOURCE_SESSION","text":"summary text"}`. The response gives the
saved event number. An uncertain response must be checked in target history
before retrying, since another save creates another event.

The text is explicitly user reviewed, at most 16 KiB of UTF-8, and both sessions
must belong to the same workspace. The saved record identifies the source
session and its last saved event when the summary was submitted. History shows
that attribution. The next model request sees the summary as source data, with
an explicit warning that historical file observations and test results need
verification in the current workspace. Browsing and ordinary branch switching
do not create or carry a summary. TUI navigation can generate a scoped model
draft after choosing `s`, then review and save it before continuing. Browser
navigation still requires opening the review editor and then selecting its
explicit generation button; the REPL prints the offered draft for a separate
save command. Local busy refusal and daemon switches during model output have
been verified with retained prompts and attachments; evidence and remaining
queue lifecycle work are in the [adoption tracker](research/pi-adoption-20260930.md).

## Saved tool images

History identifies a retained tool image by its original session, result and
image-companion event. In the browser, expand the result and select **Show saved
image**; use **Previous image**/**Next image** for multi-image results and
**Hide image** to release it. Pictures load only on that action, and only one is
retained across cards. They describe historical tool output, not current files
or test results. A copied branch can read its copied pictures; a branch ending
before the associated result cannot attach them to a different answer.

For a local raster file, use:

```sh
rook session image SESSION RESULT_EVENT --index 0 --output capture.png
```

The index starts at zero; each result can contain at most four images. The command
works locally and through the running daemon, reports the source, and creates
a new file without replacing an existing one. TUI history shows an export
command pinned to the displayed session/result; Calls retains the image-source
number as a text fallback. File payloads are limited to 2 MiB and 4096 pixels
per side. Ordinary history, live delivery and HTML export keep captions and
descriptors without embedding encoded pixel data. Terminal pixel rendering
remains a separate experiment.
