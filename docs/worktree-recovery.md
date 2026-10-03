# Recovering a delegated checkout

An isolated child retains its registered Git checkout and transcript. If the
checkout directory disappears, diagnosis is read-only; restoration is an explicit
operation. Select its original parent session and inspect the exact child:

```sh
rook session worktree PARENT CHILD --json
rook session worktree PARENT CHILD --restore REVIEW_TOKEN --json
```

The REPL and TUI expose `/worktree CHILD [restore REVIEW_TOKEN]`. The TUI uses
its bounded background history worker, leaving input and streamed replies live.
The browser Sessions view has **Delegated worktree recovery**, with diagnosis
and a separate reviewed restore button. The daemon exposes GET/POST
`/api/sessions/PARENT/worktrees/CHILD`; POST accepts only `review_token` and a
4 KiB body. The agent's `worktree` tool accepts `diagnose` and `restore`; restore
uses the normal write approval policy and is unavailable to a checker.

## Source and preservation

The source is the registered child's **Git index**, including staged edits, with
its HEAD, index SHA-256 and Git admin path reported explicitly. Its repository
and parent/child ownership must match the engine and the original owned location.
The source token binds that identity and the configured limits. Re-diagnose if
the index, HEAD, registration or configuration changes. Missing registration,
foreign `.git` markers, unresolved index entries and unsafe paths are refused.

Restoration publishes only absent files, then the registered `.git` marker.
Existing files, directories, final links and untracked entries are preserved.
Linked directory prefixes are refused, including links back inside the tree;
publication cannot replace an entry created concurrently. A present checkout
does not offer restoration: tracked deletions in a live checkout remain edits.
Raw Git blob bytes are copied without smudge filters or line-ending conversion.
Unix executable modes are preserved. Native symlinks retain their contents;
`core.symlinks=false` produces text files. Absolute native symlink creation on
Windows requires manual repair or a newly reviewed `core.symlinks=false` setting;
it is refused before destination writes. Sparse files and submodule checkouts
are not materialized. Split indices and sparse-directory indices need manual
repair because their external metadata has not been admitted by this operation.

Deleted unstaged/untracked data has no source here. Restore does not complete the
child's task, merge its changes into the parent, or certify current files/tests.
The stored worktree's `finished` marker releases an interrupted checkout for
ordinary review/removal; task outcome remains separate and unknown.

## Bounds and interruption

Before copying any blobs, source metadata, index entries and blob sizes are
admitted. The `[agent]` settings are:

| Setting | Default | Allowed range |
|---|---:|---:|
| `worktree_recovery_max_files` | 4096 | 1–65536 |
| `worktree_recovery_max_bytes` | 64 MiB | 1 byte–256 MiB |
| `worktree_recovery_max_file_bytes` | 16 MiB | 1 byte–64 MiB |
| `worktree_recovery_max_metadata_bytes` | 2 MiB | 1 KiB–16 MiB |

Session/companion admission and reports are bounded to 256 KiB, paths and link
targets to 32768 bytes, and diagnosis/restoration to a 60-second async deadline.
Binary index versions 2–4 are admitted before Git reads a private index copy;
Git outputs and both pipes remain bounded/drained. All raw blobs are staged in
an owned temporary directory before checkout mutation. No registered index,
HEAD, branches or Git admin records are rewritten or pruned.

Restoration exclusively leases the checkout, including live/resumed shared
children and a directory deleted after its lease was acquired. The daemon admits
two operations at once and performs them outside engine locks. A durable bounded
JSON companion records the source and start before destination mutation. Normal
failure/cancellation records `partial_or_cancelled`; process loss can leave
`running`, which is an uncertain result, not completion. Published files remain
available for inspection and a fresh diagnosis. Only a committed successful
restore records `completed`. Existing postcard and worktree storage shapes are
unchanged. Original Git semantics are documented in
[the Git index format](https://git-scm.com/docs/gitformat-index) and
[Git worktree](https://git-scm.com/docs/git-worktree).
