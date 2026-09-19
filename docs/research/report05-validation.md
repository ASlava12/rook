# Validating report five, and the changes that were not yet committed

Dated 2026-09-18. Starting HEAD: `f08928c`. What was checked: a local
`scratch/report05_agent.md` and the whole accumulated diff against that HEAD,
new sources and tests included. A copy of the original diff was kept before
anything was changed.

Git cannot say who wrote an uncommitted line, and among these were fixes from
earlier tasks as well. So each decision was taken from the code and its tests
rather than from a guess at the author.

## What was decided about the report

| ID | What the check found | What was done, and what shows it |
|---|---|---|
| R-01 | Confirmed. `disk_path` makes filesystem calls too, and in `read_window` the open was still outside the blocking task. | The disk branches of `read_text` and `write_text`, the open in `read_window` and the copy in `move_file` run in `spawn_blocking`. A test with a FIFO on a single-threaded runtime checks that the timer fires while a read is blocked. |
| R-02 | Confirmed, but the version of `cap_std` in use has no `fs::Dir::sync_all`. | On Unix the directory is synced through a cloned standard file handle after the rename, and so are the ancestors of a newly created nested destination. For a move, the destination is published and synced before the source is removed. A sync failure was returned to the caller. **Reversed on 2026-09-19:** it is not returned any more, because a cap-std directory handle is opened for lookup rather than for reading — `O_PATH` on Linux, the same idea on FreeBSD — so `fsync` on it is `EBADF`, and every `rook skills rollback` on those platforms reported a write that had in fact completed as a failure. A directory sync decides what survives a power cut and nothing about whether the write happened, so it returns nothing at all now. On Windows no such directory flush is claimed. |
| R-03 | Confirmed in part: a `SIGKILL` leaves the temporary file behind. A move to the store can return `EXDEV`, and deleting by a pattern could take a file somebody else is using. | The temporary file stays beside its destination. An occupied name is skipped, and when the attempts run out another process's file is not removed. The exact private name `.rook-write-<pid>-<serial>` is excluded from capture, `capture_paths`, `list_dir` and `written_since`. The regression checks occupied names, that the original survives, and that a failed write cleans up its own temporary. What a `SIGKILL` leaves is not removed automatically. |
| R-04 | The reasoning was partly wrong: the previous walk already returned `ENOTDIR` for the example given. The real gaps are elsewhere — a directory in the final component, conflicts within one plan, and duplicates that normalise to the same path. | The type of every existing component and the whole restore/rewind plan are checked before anything is written. Four scenarios check that the first file survives a refusal on a later path. This is a check before the fact and not a filesystem transaction. |
| R-05 | The double read is real; an unacceptable slowdown was not shown. | Caching every decompressed file was refused: it raises peak memory to the size of the snapshot. The readability check before the first write and applying one object at a time were kept, with the reason written in the code. |
| R-06 | Confirmed, and the whole call path was worse than described: `resolve` expanded the final link and could move the target itself. | The parent is resolved and the final entry is examined without following it. A symlink as the source is refused; an existing destination is not replaced, a dangling link included. The low-level helper checks the source type as well. The test keeps the link, its target and an occupied destination. Moving links is not supported at all: snapshots hold ordinary files. |
| R-07 | Ending the group on every exit deliberately takes a hook's children with it. The proposed `alive()` does `kill(-pgid, 0)` and does not check the owner. | Refused: it does not close PGID reuse and adds one more check-then-use race. The cleanup on exit, cancellation and timeout stays, and a test checks the timeout of a hook that never reads its stdin. The theoretical risk of a reused Unix PGID is not claimed to be eliminated. |
| R-08 | A defensive defect, confirmed; an ordinary release build sets `panic = "abort"`, so the path is less reachable than the report implies. | A poisoned mutex is recovered from in `redact`, `value` and `also_hide`. A test that poisons it deliberately checks that both already-registered and new values are still masked. |
| R-09 | Parsing conservatively is a deliberate boundary for allow rules. The claim that confirmation is always asked for is not accurate under `autonomous`. | The limit is explained in the risk description and in both READMEs, including how `assist` differs from `autonomous`. The rules were not loosened. |
| R-10 | A latent contract; today's callers all pass absolute paths. `strip_prefix("")` is not by itself the reason for the refusal, as the report claims. | An explicit error for a relative disk path. The regression checks that the diagnosis is readable with `allow_outside_workspace = true`. |

Alongside R-02, publishing an external store object was checked: before this
there was no `sync_all` of the file or of the directory ahead of committing its
metadata. One helper now does `write → sync file → rename → sync directory`,
and `objects`, where the hash-prefix directory is created, is synced too. The
external-object test checks that it reads back after reopening and that no
temporary is left. This checks the publishing path, not what a power cut does.

## Checking the edits that had accumulated

| Area | What survived the check |
|---|---|
| rook-store | Dictionary generations and their atomic publication, repair of an unreadable dedup hit, GC refusing to run on incomplete reachability, the timer training only the dictionaries that are missing, the checkpoint clock, and format 2. No existing postcard structure was extended. |
| rook-core: rewind, memory, claims | One checkpoint order across a parent and its children, ordinary forks excluded from delegations, the destination checked, memory updated by compare-and-swap with its history in the same transaction, leases and the guard against a stale one, project scope compared by path components. |
| rook-core: agent | A visible marker where unreadable history was skipped; a separate check of whether a final answer without tools was meant. Two continuations for a promise with no action, then an explicit `incomplete`; spend, timeout and fresh instructions accounted for. This classifies an answer; it does not prove the task was done. |
| rook-core: hooks and secrets | One deadline across input, output and the wait, the process group cleaned up, a refusal for a matcher that will not compile; named secrets never reach a raw spill file. Poison recovery was added in this round. |
| rook-tools and rook-contain | Capability I/O, the parent-substitution guard, a bound on what a read holds in memory, an edit batch refused when two paths are the same file, a deadline on a command and a job stopped once stdout and stderr close, conservative allow rules, relative HTTP redirects resolved correctly. |
| rook-llm | Bounds on the wire stream, on arguments, on reasoning and on the number of blocks; a bounded tool-call index, synthetic ids where they are missing, an error for a call with no name. |
| rook-mcp | The deadline covers the HTTP body and writing a stdio request; a pending entry is removed on cancellation; `tools/call` is not retried automatically after an answer is lost, and the connection is rebuilt for the next explicit call. |
| rook-acp and rook-lsp | An editor's error is not counted as a successful write, pending entries are cleaned up, an early prompt refusal does not produce two answers, the disk fallback uses the shared boundary; snake_case stop reasons; UTF-16 positions and a refusal for an empty symbol. |
| rookd, CLI, web | A trusted `Host` and then `Origin`, a cap on engines that never evicts an active one, a reload that does not lock out new readers; a separate terminal `Failed` and a non-zero CLI status for failed and cancelled, with `Error` staying non-terminal. |
| Skills and dependencies | Reading a skill or variant bounded inside its bundle and by size, `allowed-tools` as informational metadata; rustls 0.23.45, the unnecessary `heapless` feature of postcard turned off, cap-std, duplicate workspace members removed. |
| Tests and documents | The regressions were kept; mock providers answer the new completion request, and the CLI tests build the current `rookd`. `rook-contain`'s dependencies and the limits of shell allow rules were made precise. A stale `agent-loop-assumptions.md` was replaced with a checked description of what the code does now, including the limits of token accounting when a provider reports no usage. Scratch reports, logs and test stores stay local. |

## Limits

A multi-file restore or rewind, and copy-then-remove, are not atomic
transactions: a disk failure or a concurrent change while they are being applied
can leave a partial result. If `fsync` fails after a rename, the new name may
already be published — the error does not mean the write is guaranteed not to
have happened. A move between filesystems can leave two copies if it is
interrupted after the copy. Checking the type of a source is not a defence
against every concurrent replacement inside the permitted boundary.
`spawn_blocking` keeps the runtime responsive, but a system call that has
already started is not cancelled along with the future waiting on it.

## What was run

On the final version `cargo xtask ci` passed: fmt, Clippy with `-D warnings`,
1,195 tests, 0 failures, 0 skipped; the whole run took 330.6 seconds. That
included all 28 PTY tests, which the author of the original report could not run
in their sandbox. The machine was macOS on arm64.

`cargo check --target x86_64-pc-windows-msvc -p rook-contain` passed, and so did
the same check for `x86_64-unknown-freebsd`. That checks compilation; running
the tests on those systems is not claimed here.

`cargo xtask compaction`: 23.31 MiB logical, 5.29 MiB distinct, 0.63 MiB with
standalone zstd, 0.14 MiB with dictionaries (37.1×), 4.02 MiB on disk (5.8×).
The figures matched the README and the storage documents, which had already been
corrected.

`cargo audit --json`: no known vulnerabilities and no warnings; the RustSec
database was current to commit `0765f6f611cb93c5ab2681b9c97fb06e5df9e1d1` of
2026-09-18. `node --check web/dist/chat.js` and `git diff --check` passed too.
