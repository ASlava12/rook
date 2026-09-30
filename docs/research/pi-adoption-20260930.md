# Pi feature adoption

Implementation tracker for [the Pi review](pi-reference-review-20260930.md).
Source: Pi `ee602414c703be8da722ec56de7f2399e62581ac`; starting Rook:
`f62c540`. This tracks the full requested transfer, not just the first patch.

| Capability | State | Completion evidence needed |
|---|---|---|
| Bounded live delivery and snapshot recovery | In progress | Slow-reader byte/count bounds, snapshot convergence, current questions/approvals, terminal results, reconnect without cancelling work |
| Editable steering and follow-up queue | Pending | Core/CLI/API/TUI/browser, durable IDs, goal vs ordinary-turn boundaries, revoke/accept races, restart |
| Configurable keyboard actions and prompt undo | Complete | Shared registry/config/help, bounded Unicode edit tests, remapped-key and external-editor PTY checks, full CI passed |
| Branch navigation and optional branch summary | Pending | Existing session/event IDs, bounded tree/history, explicit workspace semantics, attributable summary |
| Inline tool cards | Pending | Compact/expanded results, errors/duration/diffs, bounded loading, TUI/browser verification |
| Context provenance inspector | Pending | Request-specific sources, discovered vs loaded skills, deferred tools, CLI/API/TUI/browser |
| Local HTML export | Pending | Selected history scope, bounded streaming, escaped content, no publication, cross-frontend access |
| Opt-in phase-based model routing | Pending | Explicit policy, continuity/capability constraints, actual route/cost reporting, comparison without claiming unmeasured savings |
| Declarative extension UI | Pending | Bounded status/progress/forms/result contract, trust boundary, text fallback and frontend parity |

Regular-scrollback TUI and terminal image preview remain experiments from the
review, to assess after the main capabilities. Existing MCP/OAuth, durable goals,
skills, editor launch and storage are reused. No replacement runtime is planned.

## First integrated block

The socket delivery queue admits encoded bytes and frame slots before JSON
allocation, including the in-flight frame. A slow view backpressures only its
relay. Config validation and editor help expose both limits. Focused tests cover
escaped JSON, paced readers, disconnects and terminal delivery. Engine/registry
guards are released before async error reporting.

Named keyboard actions and bounded prompt undo/redo use one registry for config,
palette, help and dispatch. Undo retains deltas, evicts before copying text, and
supports Unicode, paste, mention completion and external-editor replacement.
Both remapped-undo and external-editor undo/redo PTY tests passed on the integrated
tree. Pending-input cancellation cleanup is also included and verified.

The first full gate failed in doctests with missing dependent crates after
parallel worktree builds shared one target directory. Its ordinary tests passed,
but that was not a green gate. The combined tree subsequently passed
`CARGO_TARGET_DIR=/tmp/rook-pi-ci-target cargo xtask ci` with exit status 0,
including doctests. The gate reported 677.6 seconds. This isolated target must
not be reused by another checkout while it is validating this tree.

## Snapshot recovery in progress

Worktree `/tmp/rook-pi-snapshot-recovery`, branch `pi-snapshot-recovery`, has the
first integrated source changes staged as a baseline. Its **unstaged** diff is
the next snapshot change; export that diff rather than diffing against HEAD.
`/tmp/rook-pi-snapshot-in-progress.patch` is an intermediate export and must be
refreshed after further edits.

The implementation now has:

- Configurable current-input count/byte bounds and cleanup on answer, expiry or
  cancellation; reconnect reads the current requests, not historical controls.
- An atomic replay/subscription boundary, count/byte-bounded replay, sequence-only
  broadcast notifications and replacement snapshots when a view falls behind.
- Retained current metrics and terminal outcomes, with explicitly marked partial
  history and shortened oversized terminal details. CLI JSON exposes whether
  its live view was truncated.
- Input resolution notifications across windows, without waiting for more model
  output. The TUI keeps the current question while other pending requests wait.
- Snapshot handling in CLI, TUI and browser. The protocol is requested through
  `live_snapshots=true`; older clients receive ordinary events.

Server tests passed for lag convergence, terminal delivery, atomic joins,
current request replay, byte/count bounds and resolution across two views. The
current CLI reporting tests passed. A Chrome check using real browser modules
and injected protocol events passed for transcript replacement, prompt/form
preservation, duplicate input events, sending an answer, remote resolution and
terminal state. Its harness and log are `/tmp/rook-pi-browser-check/recovery.cjs`
and `/tmp/rook-pi-browser-recovery.log`. It does not replace a real-daemon TUI test.
Clippy found a test-only redundant clone; fix it and finish validation before
integrating this block.

Remaining work for this capability includes real-daemon TUI recovery checks,
configuration and compatibility checks, documentation and full CI after merge.
Also audit the terminal client's unbounded forwarding queues: server bounds alone
do not bound a paused TUI's local event backlog. Carry a byte/count lease through
remote reception and TUI event handling before calling end-to-end delivery done.
The other capabilities in the table retain their full scope.

## Validation environment

macOS showed long cold-start pauses before test code executed: a process sample
contained only `_dyld_start`. A list-only launch of a copied test binary took
about 25 seconds; its repeat took about 0.01 seconds. This did not establish the
OS cause. A parallel list-only probe also encountered long startup waits and was
stopped; only those diagnostic processes were stopped. No check was waived.
Use isolated build targets for simultaneously active Rook worktrees to prevent
artifact interference. The external shared target is also used by another project;
do not clean it or stop that project's builds.
