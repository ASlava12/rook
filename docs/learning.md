# Reviewable knowledge after tasks

`rook session learning <session> '<JSON action>'`, chat/TUI `/learning`, the
browser's **Knowledge proposals** panel, and POST
`/api/sessions/<session>/learning` share one core contract. Empty CLI/chat
arguments list proposals and retained review attempts.

Nothing changes future memory or skills while a proposal is being generated or
edited. A proposal is a draft backed by quotations, not a certificate that a
procedure is universally correct. The final user review decides what to retain.

## Sources and review

Use `rook session turns <session>` to find a completed task's `result_seq`.
`{"action":"source","result":123}` returns its frozen source identities,
complete admitted user/tool excerpts, and explicit omissions. The exact adjacent
turn summary and result must belong to the selected session. Failed, unfinished,
recovery-blocked or unproven outcomes cannot supply a task source. A retained goal
check that did not hold also refuses learning. Truncated evidence cannot support
a proposal, and assistant prose is not a supporting source.

`{"action":"suggest","result":123}` requests a separate read-only model review.
Its child session, physical request accounting, streamed output cap and attempt
status are retained. It has no tools and cannot edit knowledge or the workspace.
A lost/failed review is inspectable; it is not replayed automatically.

You can also submit an explicit draft, or revise a generated one:

```json
{"action":"propose","result":123,"draft":{"target":{"kind":"memory","text":"Use Russian in reports.","supersedes":null},"reason":"The user explicitly requested this language","evidence":[117]}}
```

The evidence numbers must name complete retained source events from that task.
A skill target has `kind:skill`, an authored `skill` with name, description, body,
optional keywords/requires/files, and optional `remove` paths. Its body is capped
at 1,200 bytes; longer supporting instructions belong in bundled files.

`inspect` reads a saved proposal. `revise` names the old proposal and a replacement
draft. It retains the old revision, its exact sources and the link to its
successor. A stale token from the old revision cannot apply the new draft.
Exact duplicate targets return the existing proposal. Rejected and rolled-back
proposals remain in history.

## Application and rollback

`prepare` names a proposal and returns the memory/file diff and a token binding
the owning session, source, reviewed draft and current target. `apply` needs that
exact token. A changed target requires a fresh review. `reject` saves a note
without applying knowledge.

Memory changes publish the complete bounded memory head, history reference and
applied proposal receipt in one compare-and-set transaction. Corrections name an
exact previous fact and preserve its scope, tags and pinning. Its old version
remains reachable. `prepare_rollback`/`rollback` remove only the exact fact still
matching the applied proposal and restore any superseded fact. Unrelated facts
added later are preserved; another correction of the applied fact is a conflict.

Skill changes target only the selected user-owned version directory. System,
project and plugin skills cannot be overwritten. Declared versions are preserved.
All before/after objects and the review source are admitted before writes, with a
retained inverse version and durable per-file progress. File bytes and modes must
match the reviewed source. Additional user files stay outside the inverse unless
the proposal explicitly changed them. The common capability filesystem refuses
symlink traversal. Its existing-file compare/publication sequence is not an
atomic filesystem CAS against arbitrary external processes.

A partial or lost filesystem operation retains its change session, completed
paths, pending path and immutable original inverse. Stop any live owner, inspect
that session's `recovery` report and acknowledge unknown operations through the
existing execution-recovery interface. Then `recover` the proposal with an
inspection note. It observes files matching the saved before/after sources; it
never assumes unknown writes finished. A partial inverse can be inspected and
continued without swapping the original accepted change for its own inverse.
There are at most eight filesystem operations per proposal. Each accepted
application/inverse retains skill versions reachable through `skills history`.

## Automatic proposals and bounds

`[learning] automatic = true` enables one read-only proposal review after an
ordinary top-level completed task with no unresolved questions. Checking and
managed-work contexts do not trigger this hook. The default is off: no additional
model request or persistent knowledge change occurs when it is disabled.

`auto_apply_memory = true` is additionally restricted to exact literal user
assertions, with no superseded fact, and an allowing ordinary write policy. It
never automatically applies skill code or rewrites a remembered correction.
Other drafts require explicit review. A completed review is not repeated for the
same task result. The review has its own bounded allowance, separate from the
completed main response; model usage is recorded in its linked child session.

Defaults bound each owner's proposals and review attempts to 32, state to 2 MiB,
source scanning to 128 events/64 KiB, model output and reasoning to 16 KiB each,
and review time to 60 seconds. Current memory admission is capped at 2 MiB.
Skill snapshots admit 64 files/512 walked entries, 256 KiB per file and 1 MiB
combined. At most two reviewers and four HTTP operations run concurrently per
process. Source omissions and reached bounds refuse unsupported admission;
failed and unknown attempts remain visible rather than being silently evicted.
