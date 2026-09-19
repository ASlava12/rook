# Remediating two audits, and the session that was investigated

Dated 2026-09-17. The reports behind it: `../agent_report_01.md` (2026-08-29)
and `../report03_agent.md` (2026-09-16). This is the state of the fixes, not a
fresh claim that no defects remain.

## The session it started from

In `01M2QHRRBA2FSHPTWEJ4829SRD` the investigation found messages from the person
that could not be read back, failing with a dictionary error. The old replay
skipped such a message in silence. Writing the same bytes again returned the
existing damaged object by its hash, so the second path was no better than the
first. Both are fixed: a replay shows a visible gap, and handing the original
over again repairs the object. The test
`unreadable_user_history_is_visible_and_the_new_instruction_survives` checks the
request the scripted provider actually received.

The live store was not touched. Nothing here can reconstruct a dictionary or
content that is gone without a backup or the original bytes. Claude stopping on
a provider limit is a separate event; none of this lifts a service limit.

## What the second report found

| ID | The fix | Where it is checked |
|---|---|---|
| F01 | Dictionary generations are kept until the new one is published atomically; the timer trains only the kinds that have none; an unreadable dedup hit is repaired; GC stops rather than sweeping past a container it cannot read | `rook-store/tests/store.rs`: generations and reopening, repair, scheduled training, GC. *(2026-09-19: maintenance and `store gc` now remove objects nothing can decode before the sweep, so that last case no longer arises in the ordinary path — GC still refuses if it meets one.)* |
| F02 | `rook-contain::files`: every operation goes through a directory handle; a dangling symlink is refused; a write replaces a file atomically; a restore validates the whole plan before writing; delete and move use the same file boundary | `rook-tools/tests/audit_regressions.rs`, `rook-core/tests/audit_regressions.rs`, and the existing file and rewind tests |
| F03 | A manifest and a variant are read relative to the skill bundle with a 1 MiB limit; leaving it by `..`, an absolute path or a symlink is refused | `rook-skills/tests/skills.rs` |
| F04 | Redirection, expansion and flags that execute or write require approval under `assist` | `shell_write_operators_and_executing_options_require_approval`, and the existing policy tests |
| F05 | The rewind plan is built before the fork is created; only delegated subtasks count; one checkpoint sequence is written in the same transaction, copied on a fork and deleted with the session | The parent/child chronology and rewind regressions |
| F06 | A tool-call index is checked before it resizes anything; at most 256 calls; a call with no name is an error | `rook-llm/tests/stream_limits.rs` |
| F07 | A file window keeps no more than the output budget, however many lines were asked for | The bounded-read regression and the existing file tests |
| F08 | Waiting for a process to end after EOF stays inside the timeout and the cancellation | The foreground and jobs EOF regressions |
| F09 | `tools/call` is not retried after the transport is lost; an unknown outcome is returned and the connection is rebuilt for the next explicit call | An MCP counter of the external effect, and the restart test |
| F10 | Project scope compares the components of normalised paths | Adjacent directories and `..` in the core regressions |
| F11 | Compare-and-swap publishes the memory head and its history entry atomically; a conflict repeats the read/modify/write | Sixteen simultaneous `remember` calls keep sixteen facts and sixteen versions |
| F12 | Every claim is checked; nested leases are counted; a stale guard does not release a new owner | The nested, conflicting and expired-guard regressions |
| F13 | An active engine is never evicted; when every place is taken, a new project is refused | The daemon's active-engine and capacity tests |
| F14 | An ACP JSON-RPC error reaches the peer and becomes a write error | The editor-rejection test |
| F15 | An empty symbol is refused before any search | `rook-lsp/tests/positions.rs` |
| F16 | LSP positions count UTF-16 code units | The Unicode position regression |
| F17 | The HTTP MCP timeout covers the headers and the body; the stdio deadline also covers writing the request | The stalled-JSON-body regression and the transport tests |
| F18 | A hook's stdin, stdout/stderr and wait share one deadline; the process group is ended on cancellation | A hook with a full stdin and a process that never reads it |
| F19 | A regex that will not compile takes its hook out of the list to run and returns a diagnosis | The invalid-matcher regression |
| F20 | The shared limit counts arguments, reasoning blocks, identifiers and the wire stream as they arrive rather than after | The stream-limit and streaming tests |
| F21 | Before a batch of edits changes anything, its paths are resolved and checked for naming the same file | The alias-batch regression |
| F22 | A command with named secrets in it never creates a raw spill file | The synthetic-secret regression |
| F23 | A separate terminal `Failed` event; the CLI exits non-zero on failed and cancelled; an ordinary `Error` stays non-terminal | Protocol, CLI and daemon tests; the TUI and web handlers were updated |
| F24 | The `Host` is checked against loopback or an explicit allowlist, and then `Origin` is checked against it; the check covers the API and the static pages | The tests that send a hostile `Host` and `Origin` |
| F25 | A reload takes a try-lock on every engine; if any one is busy the new configuration stays pending | The reader-held, reload and health regressions |
| F26 | A pending ACP request is removed on cancellation by RAII; ordinary peer requests have a 60-second deadline, and a terminal wait has the command's own | The cancellation-leak regression |
| F27 | Every answer to a prompt goes through one guard, including an early provider error | The failed-prompt, cancel and barrier integration test |
| F28 | A redirect is resolved with `Url::join` against the current URL | The relative-redirect unit test |
| F29 | rustls updated to 0.23.45 | `Cargo.lock`, `cargo audit`, and the TLS and provider tests |

## The first report

In the appendix of the first report most of R01–R14 was already marked fixed.
Those changes were kept and are covered by the ordinary test suite.

| ID | State |
|---|---|
| R01 | The origin guard now also checks a trusted `Host` (F24) |
| R02, R10 | The existing validation of skill names was kept; loading a variant is bounded further (F03) |
| R03 | The existing limits on ref history and retention were kept; the new checkpoint-order KV entries are deleted with their session |
| R04 | The existing transactional GC and the guard for fresh orphans were kept; a live container that cannot be read now stops GC |
| R05 | The existing health and maintenance checks were kept; a reload no longer puts readers behind a writer (F25) |
| R06 | The bounds now count every payload and the index (F06, F20) |
| R07 | The existing MCP line limit was kept; the transport deadlines were widened (F17) |
| R08 | A zero timeout already meant the configured default; the path taken after EOF was fixed (F08). An explicit non-zero timeout is still configurable |
| R09 | GC without an expander refuses to delete anything if a reachable container would have to be walked |
| R11 | The existing closed permissions on the state directories were kept; the raw secret spill was closed (F22) |
| R12 | The existing `git clone --` separator was kept |
| R13 | The existing copy budgets and the refusal to follow a symlink were kept; writing and restoring are bounded further by the capability filesystem |
| R14 | The existing drain of delegation progress was kept |

## Smaller remarks from the reports

`allowed-tools` is marked as informational; a non-empty value logs a warning. It
does not narrow the policy. Duplicate workspace members were removed, along with
an unreachable HTTP 304 branch and a doubled URL encoding. Two `rookd`
constructors are compiled for tests only. The textual no-dead-API and no-panics
checks were kept as lints and joined by checks of behaviour on external input.

`chacha20` was moved by the resolver from the yanked 0.10.1 to 0.10.2. The
unused `heapless` part of postcard is off through `default-features = false`;
the `use-std` feature that is needed stays on, and the dependency on
`atomic-polyfill` is no longer required. No serialised postcard structure
changed.

## Compatibility and limits

The store format is now 2. Opening an older store upgrades the marker under
the lock, after which older binaries refuse it — which is the point, because
they do not read dictionary generations and would overwrite the current one. New
code and an old daemon should not be mixed. Test setup now always builds `rookd`
before the CLI integration checks.

Older checkpoints with no shared sequence number can carry the same
one-second stamp in different sessions. Where that is ambiguous, a rewind
returns an error before touching any file, and restoring a named checkpoint is
still available. Applying a batch is not a filesystem transaction: an I/O error
part-way through can leave a plan half applied, though a rewind saves the
current contents first. Explicitly allowed external paths are still supported
through the sandbox setting.

`[server] allowed_hosts` holds exact external authorities (`host:port`) for a
proxy or remote access. Loopback addresses and `localhost` are allowed by
default. Changing the allowlist needs a restart. This is a `Host`/`Origin`
defence; token authentication is not introduced here. The list of project
engines is capped without evicting an active connection.

## What was run

`cargo xtask ci` passed in full: fmt, clippy with `-D warnings`, 1,177 tests, 0
skipped. The whole run took 274.9 seconds. `cargo audit --json`: no
vulnerabilities and no warnings, against the RustSec database as of
2026-09-17. `node --check web/dist/chat.js`, `cargo fmt --all --check` and
`git diff --check` passed as well. The regressions for the lost instruction
the investigation started from, for a sub-agent fork, and for a directory
substituted after `resolve`, are all in that run.

`cargo xtask compaction`: 23.31 MiB logical, 5.29 MiB distinct objects, 0.63 MiB
with standalone zstd, 0.14 MiB with dictionaries (37.1×), 4.02 MiB on disk
(5.8×). An isolated copy of the original HEAD gave the same figures, 4.02 MiB
included: the fixes added no size. The README and the storage documents were
updated from that re-measurement.

The checks ran on macOS arm64. `rook-contain` also cross-checked for
`x86_64-pc-windows-msvc` and `x86_64-unknown-freebsd`; running the tests on
those systems needs their own CI runners.

## An early `end_turn` in session 01M2RE836AS8XHMA43XDTA428Y (2026-09-18)

A second session stopped normally after 37 steps with `stopped=end_turn`. Its
last answer promised to delegate three checks and contained no tool calls. Under
`assist` the loop took any non-empty answer without tools as the end of the
turn. Nothing was written and nothing was delegated in that session; this is a
separate defect in how a turn ends, not the storage damage happening again.

Before accepting a non-empty `end_turn`, the loop now makes one small request
without tools: it tells a final answer, a question or a blocking problem apart
from a promise of the next action. The original task and the newest instruction
are both taken into account. This checks what the turn meant by stopping; it is
not proof that the report is true or that a file exists. It works under `assist`
and in nested tasks, and a checking agent with its own verdict protocol does not
check itself.

On `continue` the loop carries on with the same task under the same limits. Two
consecutive attempts to correct itself are allowed, and a real tool call resets
that count. Repeated promises give `stopped=incomplete`; a check that is
unreachable or malformed gives `completion_unchecked`, with the reason recorded
and an open question. What the check spends counts towards the turn; it waits at
most 60 seconds and no longer than the turn has left, and an exhausted budget is
not worked around. A fresh instruction arriving during the check hands control
back to the main loop. The price is one extra small request before a final
answer.

The regressions cover the exact last sentence of the session that started this,
followed by a report actually being written, and: repeated promises, final
answers and questions, a verdict that is unreachable or malformed, the timeout,
the budget, read-only being preserved, and instructions arriving late. Against
the configured `qwen3.8-flash-next`, seven control answers were checked
separately: the original sentence, a Russian promise to continue, a finished
result, a clarifying question, a requested plan, a blocking problem, and an
offer of optional further work. All seven were classified as expected. These are
control cases, not a guarantee that a model classifies every answer correctly.

Checking that piece of work: `cargo xtask ci` — 1,188 tests, with fmt and
Clippy passing; `cargo xtask dist` built the release. The resulting binary ran
one real short request against the configured model in a separate temporary
store:
`end_turn`, one recorded completion check, 2,883 input and 85 output tokens.

`rook` and `rookd` were installed into `~/.local/bin` and the daemon was
restarted on the same `127.0.0.1:54302`. A backup of the binaries and the
details of the installation are in
`~/.rook/backups/completion-fix-20260917T210412Z/`. The package version stayed
`0.4.0`; the SHA-256 of the installed `rook` was
`c8842da6cd70495fd8ed35a33478e640d541a7ac6cce6df6dc5f5fec3e80810b` and of
`rookd`
`618e2ddd74fb1e54b1ad1bd1b9dfc77b532fee0e5a298b43ba5889c47bec23e1`. The
configuration, the secrets and the session metadata were identical before and
after. The session with 207 events was kept; its audit was not resumed
automatically.
