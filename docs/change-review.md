# Reviewing frozen changes

`review_changes` captures selected changes and runs an independent reviewer with
access only to the retained diff and source pages. Findings name their severity,
file, side and line range. The reviewer has no shell, editing, browser, MCP or
delegation tools, including when it fabricates an unadvertised tool call.

A review belongs to its originating session and workspace. Its source identity
covers the selection, resolved Git revisions and retained redacted source. Later
workspace edits do not retarget findings or line comments. A checksum mismatch
refuses the stored snapshot rather than reading current files in its place.

The reviewer receives an output schema. One complete JSON object is accepted
plainly, in an explicit `json` fence, or after explanatory prose. Competing or
malformed objects and invalid structured findings fail. Ordinary narrative may
use one tool-free request to repair its JSON representation; that request is
instructed to preserve facts and uncertainty, shares the turn's token/time budget,
and does not rerun the review. Source and line checks still apply to its result.
A completed review does not certify the accuracy of every model claim.

## Selecting changes

Ask the agent to use `review_changes` with one of these arguments:

```json
{"target":"working","paths":["src/main.rs"]}
```

```json
{"target":"commit","revision":"HEAD"}
```

```json
{"target":"base","revision":"main"}
```

Working reviews compare HEAD with current files and include selected untracked
files. Commit reviews compare a resolved commit with its first parent; root
commits are supported. Base reviews compare HEAD with its merge base against the
resolved revision, excluding later working edits. Paths are literal repository
relative paths, not Git patterns. The workspace must be the repository root.
Only bounded UTF-8 regular files are supported; links, submodules, binary files
and special files are refused. Git mode changes remain in the retained diff.

To review an existing snapshot, use only its saved review id:

```json
{"review":"REVIEW_ID"}
```

Every selected diff must be read through all its pages before the reviewer can
return a completed review. Completion confirms an orderly run with valid source
anchors; it does not prove that the model found every defect. Failure, a budget
stop and cancellation retain the source and an incomplete status. The linked
reviewer session keeps the ordinary journal and usage receipts.

## Inspecting and commenting

CLI commands below also route through a running daemon. In chat or TUI use
`/review` followed by the same JSON action; the current conversation owns it.
An empty action captures working changes without starting a model review.

```sh
rook session review SESSION_ID
rook session review SESSION_ID '{"action":"list"}'
rook session review SESSION_ID '{"action":"inspect","review":"REVIEW_ID"}'
rook session review SESSION_ID '{"action":"source","review":"REVIEW_ID","path":"src/main.rs","side":"after","offset":0}'
```

Source offsets are UTF-8 byte offsets. Follow `next` for another bounded page;
`first_line` gives its original line position. A missing side, such as the old
side of a new file, returns an error.

Copy `source` from the inspected review when adding a comment. Line numbers are
one based and belong to the selected before or after source, not the current
workspace file:

```sh
rook session review SESSION_ID '{"action":"comment","review":"REVIEW_ID","comment":{"source":"SOURCE_SHA256","path":"src/main.rs","side":"after","line":12,"text":"Handle the empty input here."}}'
rook session review SESSION_ID '{"action":"feedback","review":"REVIEW_ID","source":"SOURCE_SHA256"}'
```

Feedback produces attributed corrections for an ordinary subsequent prompt. It
asks the agent to inspect current code before editing. It does not run repairs.
Comments append transactionally; concurrent comments do not overwrite each other.
Deleting the owner session deletes its review companion too.

In the browser, open a session's **Change reviews** section. Select working,
commit or base changes and optional exact paths, then capture or open a saved
review. Read retained source, save line comments, or prepare a model-review or
feedback draft. These buttons prepare text for the user to send. A different
session with active work or a draft prevents feedback from being mixed into it.
Late replies and navigation cannot change the owner of a pending comment.

The HTTP surface is `POST /api/sessions/{id}/reviews` with the same action JSON.
Requests are limited to 64 KiB. At most two review operations run through HTTP at
a time, and the admitted worker retains its slot after a client disconnects.
Capturing source has a whole-operation deadline of sixty seconds.

## Limits and interrupted runs

`[review]` configures retained reviews, file count and bytes, snapshot/state
bytes, page size, comment count/bytes, findings and reviewer steps. The defaults
retain eight reviews per session, with at most 32 files of 64 KiB per side and a
512 KiB encoded snapshot. The independent reviewer also inherits the parent's
remaining time/token budget and shared sub-agent allowance.

A started status records admission; it is not a liveness probe. Cancellation of
an admitted reviewer records interrupted. After process loss a started record
may remain: inspect its linked session, and capture a new review rather than
assuming the old run completed. Source and comments survive reopening the store.
Narrow paths when source limits are exceeded. When the retained review count is
full, use a new session or configure a larger bounded `review.max_reviews`.
Known credentials are redacted before storage while preserving line counts;
this is the ordinary vault's known-value protection, not a detector for every
possible secret in source.
