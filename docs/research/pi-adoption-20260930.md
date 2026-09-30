# Pi feature adoption

Implementation tracker for [the Pi review](pi-reference-review-20260930.md).
Source: Pi `ee602414c703be8da722ec56de7f2399e62581ac`; starting Rook:
`f62c540`. This tracks the full requested transfer, not just the first patch.

| Capability | State | Completion evidence needed |
|---|---|---|
| Bounded live delivery and snapshot recovery | Complete | Queue/replay/input limits, WebSocket backpressure, atomic recovery, current controls, PTY reconnect/goal checks, browser form preservation and full CI passed |
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

## Snapshot recovery completed

The second integrated block provides:

- Current-input count/byte bounds and cleanup on answer, expiry or cancellation.
  Revision notifications clear resolved controls across windows without further
  model output; reconnect reads current requests rather than historical controls.
- Atomic replay/subscription, count/byte-bounded replay and sequence-only
  broadcast notifications. Missing events trigger replacement snapshots.
- Retained current metrics and terminal outcomes. Shortened oversized endings
  force a partial snapshot even for an already attached reader. CLI JSON exposes
  `live_view_truncated`; its retained Unicode text tail is also bounded.
- Shared transport leases held through terminal reception and TUI forwarding
  until actual processing. A separate socket reader allows cancellation and
  answers while the display queue is full; disconnect aborts that reader.
- CLI/TUI/browser handling and `live_snapshots=true` negotiation. Legacy clients
  receive ordinary events and a partial-history text marker.

Focused checks passed for queue/replay/input limits, atomic joins, lag convergence,
terminal results, config validation, legacy protocol behavior and a real WebSocket
whose consumer holds its queue full while sending Cancel. A real-daemon PTY check
closed a window on a pending approval, rejoined with a typed Unicode draft and
answered the recovered request. The existing steering PTY found an unnecessary
reattachment on every ordinary steering message; keeping the same live subscription
fixed the erased local receipt, and that scenario then passed.

A Chrome check using actual browser modules and injected protocol events passed
for transcript replacement, prompt/form preservation, duplicate input events,
sending an answer, remote resolution and terminal state. Its temporary harness
and log are `/tmp/rook-pi-browser-check/recovery.cjs` and
`/tmp/rook-pi-browser-recovery.log`. This is not a live-model browser benchmark.
The first integrated gate exposed a missing reattachment status in the goal
PTY scenario: the snapshot erased the earlier Attached notice. The TUI and
browser now restore that status, and the existing goal switch/pause/resume PTY
passed on the correction. Fresh turns also keep the current conversation and
submitted prompt; only reattachment or an actual replay gap replaces the view.
The browser scenario passed again after this change.

The final integrated `CARGO_TARGET_DIR=/tmp/rook-pi-ci-target cargo xtask ci`
passed with exit status 0, including the full TUI suite and doctests. The gate
reported 501.9 seconds; output is in `/tmp/rook-pi-snapshot-final-ci.log`.
The earlier gate finished with exit status 1 for the missing goal reattachment
notice; it is retained as failure evidence, not counted as a pass.

The preparation worktree `/tmp/rook-pi-snapshot-recovery` retains the first block
as a staged baseline, with this feature in its unstaged diff. It must not be
merged as a diff against its old HEAD. Main is now the authoritative integrated
tree; subsequent fixes belong there. The other capabilities retain their scope.

## Queue investigation

CodeGraph located `Interjections` but resolved no caller edges; directed source
reading established the two existing paths. Ordinary turns use an in-memory
`Vec<String>` and take the whole batch; durable work stores IDs but reads pending
text before extracting the receipt ID back from a rendered prefix when marking
acceptance. Neither path supports editing/revoking
an unaccepted message or separating steering from follow-up. The next block must
make acceptance atomic with its durable receipt and transcript admission, preserve
IDs through retries/restart, and keep ordinary turns distinct from `/goal`.

## Validation environment

macOS showed long cold-start pauses before test code executed: a process sample
contained only `_dyld_start`. A list-only launch of a copied test binary took
about 25 seconds; its repeat took about 0.01 seconds. This did not establish the
OS cause. A parallel list-only probe also encountered long startup waits and was
stopped; only those diagnostic processes were stopped. No check was waived.
Use isolated build targets for simultaneously active Rook worktrees to prevent
artifact interference. The external shared target is also used by another project;
do not clean it or stop that project's builds.
