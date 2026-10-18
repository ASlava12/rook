# Follow-up after 0.17.0

Source evidence and frozen reference pins are in the
[review](research/reference-review-20261018-followup.md).
This batch delivers five related capabilities; it does not change model-quality,
translation or compaction-output defaults.

## Session tool availability

`tool_status {}` returns up to 64 sorted rows from the current session's installed
catalog. `tool_status {"query":"recall"}` explains one named tool. When `next`
is present, pass it unchanged as `query`; it binds the continuation to a hash of
the names and availability states. Changed/unbound pages fail explicitly.
Names admit 64 bytes, continuation inputs 129 bytes, without control characters.
Inspection does not probe an endpoint, install equipment or request credentials.

Available means advertised in this context, not authorized for arbitrary
arguments. Calls still pass policy, arguments and approvals. Reasons distinguish
memory/web/checklist settings, question/private-input channels, delegation depth,
verification, domain network policy and recipe limits. Unknown/unloaded entries
do not establish a product limitation. MCP names include the complete installed
model-visible direct/deferred set, beyond its 16-name display sample. Deferred
entries require the offered `mcp_call` route; removed/restricted tools disappear
from that set, and refresh does not change older admitted call bindings.

The tool runs through the common engine in CLI, REPL, local/shared TUI, browser
and ACP turns where equipment permits it. Its saved ToolResult can be inspected
like other tool results; it is not an offline reconstruction of a live session.
The system guidance is static, preserving prefix caching. The initial three
paging arguments were merged into one opaque query continuation to reduce cost.

## Lifecycle observers

```toml
[[hooks]]
event = "post_compaction"
command = "my-context-index-notifier"
timeout_secs = 10

[[hooks]]
event = "session_delete"
command = "my-session-index-notifier"
timeout_secs = 10
```

`post_compaction` follows a successfully written and flushed compaction record,
including the source-retaining fallback after summary failure. Payload fields:
`event`, `session`, `cwd`, `model`, `event_seq`, `compaction_id`, `through_seq`,
`dropped_events`, `summarized`. No transcript or summary body is copied.
The matcher subject is `summary` or `source_fallback`. Manual, threshold and
overflow compaction share the same commit seam. A cancelled candidate or
insufficient history produces no callback.

`session_delete` follows explicit CLI or HTTP/browser deletion. Its subject is
`explicit`; payload fields are `event`, `session`, `reason` and removed `events`.
The deletion transaction claims the session's existence, including empty
sessions, so a missing-session retry cannot invoke the observer again. Internal
cleanup and retention do not invoke callbacks. The existing synchronous core
storage-only operation remains available for those uses.

Observer stdout is ignored, including decision/context/UI replies. It cannot
veto later observers or open an input form; UI/stream flags are disabled for
these invocations. Existing per-command timeouts, pipe-drain byte caps and owned
process-group teardown apply. Failures are warnings, not retroactive operation
failures. These are process-local best-effort observations: interruption during
delivery may lose a notification, and callbacks are not replayed on reopening.
An external index must support explicit reconciliation. Domain network policy
does not permit uncontained observers; explicit deletion rejects that unsupported
configuration before changing the store.

## Skill discovery and resources

Discovery admits at most 64 roots, 16,384 directory entries, 512 skill versions,
16 MiB of manifest reads and 64 diagnostics plus a final limit notice. Missing
optional roots remain ordinary absence; other I/O failures are reported. Both
flat and `<name>/<version>/SKILL.md` layouts share the budgets. No general
recursive project scan or filesystem watcher is added.

Source priority controls root admission and scan order (project before user,
plugin and builtin); equal-source root order remains stable. Limits are checked
before the next load. Individual manifests remain capped at 1 MiB while reading;
failed capped reads charge their full allowance, preventing oversized files from
evading the aggregate budget. Within a directory, partial results follow native
enumeration order and the limit notice explicitly says inspection was incomplete.

Bundled-resource enumeration rejects more than 4,096 entries, directory depth
32 or relative paths beyond 4,096 bytes. It skips `.git`, does not follow nested
symlinks and reports errors instead of advertising a silently truncated catalog.
The model sees that resource enumeration failed. Actual resource reads retain
their separate bounded source/hash/no-follow checks. The Rust `Skill::resources`
method now returns `Result<Vec<PathBuf>>`; there is no persisted format change.

## Interrupted managed stages

`[work] restart_limit = 5` (1..100) snapshots a limit into each new goal.
Before a managed stage can reach a model/tool/check, a durable `restart_guard`
marks it unfinished. Returning normally, including handled provider errors,
retires the marker and resets the consecutive-interruption count. Dropping an
unfinished stage or losing its process leaves the marker for recovery.

Each subsequent owner counts that interrupted stage; reaching the admitted
limit parks the run as `blocked` before constructing another model. For limit
2, two interrupted physical stages are followed by a parked recovery attempt.
Provider outage backoff and token/time/iteration limits keep their own counters
and deadlines. Closing ownership does not rewrite a failure's retry timestamp.

An explicit existing `resume` clears the guard. Identified control retries never
erase interruptions that happened after the original resume; generation checks
remain in force. A paused, cancelled, not-yet-due or currently owned stage is not
counted as another interruption. Old JSON runs without the field read with the
five-stage default. No postcard schema or store format version changes.

CLI task inspection and HTTP run JSON expose the count/limit. Shared terminal
and browser goal delivery uses the existing blocked/open-question reason, with
the explicit inspect-and-resume action. A persisted unfinished marker is never
reported as proof of a live executor. This guards daemon-managed stages, not
arbitrary commands or an independent external agent's process supervisor.

## Verification

The final frozen-source `cargo xtask ci` completed successfully: fmt 1.5s,
workspace/all-target clippy 22.5s, current CLI/daemon build 33.2s and the complete
workspace/doctest run 1123.4s (1180.6s total). The corrected compaction-pressure
fixture and interrupted-stage budget-priority regression are included in that
run. All browser module tests passed.

The native CLI/daemon/Chrome/PTY probe passed, including `--followup` checks for
offline and daemon deletion, actual tool dispatch over HTTP and two isolated
daemon process kills/restarts followed by a parked recovery. The final targeted
catalog and managed-work checks, clippy/fmt, frontend rebuild and repeated native
probe also passed. Protocol/model fixtures establish engine behavior, not a
model-quality improvement or a live SSH/Telegram deployment.

The release workflow uses committed version-specific release notes when present,
with generated notes as the fallback; its YAML parses. Required platform CI must
pass on the exact source commit before tagging. Publication additionally checks
all five archives/checksums and runs the host probe on published binaries.
Current remote evidence: [CI](https://github.com/ASlava12/rook/actions/workflows/ci.yml),
[release builds](https://github.com/ASlava12/rook/actions/workflows/release.yml)
and [0.18.0 artifacts](https://github.com/ASlava12/rook/releases/tag/v0.18.0).

## Audit and corrections

- Exact installed descriptors, filtered MCP entries and current recipe/input/depth
  state supply availability; inspection never grants authority. An optional
  subrecipe name now uses the actual engine constant rather than a phantom alias.
- Observer output is ignored before decisions/UI can act. Domain policy rejects
  unsupported deletion callbacks before mutation; callback payloads omit source
  text and summaries. Empty-session existence is claimed inside the deletion
  transaction, so the removed-event count is never used as a duplicate detector.
- Managed ownership is claimed before counting an interruption; an already owned
  stage cannot be reclassified as a crash. Generation-bound resume retries cannot
  erase newer markers. Returned failures retain their original backoff timestamp,
  and a known exhausted saved budget takes priority over a resumable crash block.
- Both skill directory levels share admission. Failed capped reads consume their
  allowance; root priority is applied before the root cap, and `.git` does not
  inflate a bundled-resource listing. Truncation/error information stays visible.
- The first full local gate found the old single-message compaction fixture
  exceeding its 2976-token usable window (2999 estimated tokens) after the new
  prompt/tool guidance. The fixture now separates compaction pressure from
  overflow and verifies all six distinct calls plus the final work reply,
  pressure on every observed step and exactly one compaction attempt. The
  corrected targeted check passes; production compaction behavior was not changed.
- The same gate's ACP rustdoc reported E0463 while additional checks/rebuilds
  were updating the shared artifact directory. That run is recorded as failed,
  not green. Subsequent verification in the same local target directory must be sequential
  on frozen Rust sources; a fresh complete gate remains required before publication.

The isolated ACP rustdoc check and the corrected pressure test both passed.
The subsequent frozen-source full gate completed successfully, including ACP
rustdoc; no concurrent local Cargo invocation was started during that rerun.
