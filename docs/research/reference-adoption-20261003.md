# Further reference adoption, October 2026

Implementation tracker for [the October 3 review](reference-review-20261003.md).
Starting Rook: `ce66697`. The active goal is to implement the planned scope,
including the audits below; one finished block does not complete this goal.
Reference commits identify mechanisms, not code to copy wholesale.

| Requirement | State | Completion evidence |
|---|---|---|
| Responses streamed retry advice | Done | Bounded nested header decoding, transient/terminal classification, shared retry before content only, real HTTP delay/cancellation/partial-output/attempt tests; native CLI/daemon durable receipts; full CI exit 0 |
| Repetition in streamed text | Done | Bounded Unicode-aware detector, legitimate repetition/opt-out fixtures, attributed interruption, local/daemon physical accounting, clean reopen and actual compaction replay; CI and storage compaction exit 0 |
| Tool cycles without progress | Done | Bounded execution companions, content progress/live job and child checks, alternating calls/argument and background-ID churn, actual compaction/reopen and worker/new-instruction scopes; native CLI/daemon durable counts; CI and compaction exit 0 |
| Browser rendering batches | Done | Bounded frame batching, final/error/Stop flush, socket/session/turn/view ownership and history races; actual Edge 1,134 to 36 parses with full fenced text; drafts/questions/queue receipts, Node/assets tests and CI exit 0 |
| ACP child/compaction/error events | Pending | Explicit negotiated preview capabilities, legacy fallback, child IDs/ownership, duplicate/recovery and restricted cancellation, wire tests and CI |
| Model/price reference catalog | Pending | Explicit refresh and bounded cache, canonical/cloud identity, source/age, operator-applied rates, live/manual precedence, offline/account/corrupt-cache tests, frontend parity and CI |
| Delegated worktree recovery | Pending | Read-only diagnosis plus explicit restore, registered checkout identity, preservation of recreated/dirty files, leases/path/platform bounds, local/daemon checks and CI |
| Delegation progress persistence audit | Pending | Measured write/queue behavior under streamed updates, no full-history snapshots, durable critical receipts, bounded delivery and CI |
| Rooted runtime reuse audit | Pending | MCP/LSP/approval reuse, generation invalidation, execution-root trust isolation and delivery responsiveness during background preparation; fixes if needed and CI |
| Long-task compaction scenario | Pending | Actual before/compaction/replacement/next request with accepted correction, pending question and retained rejected approach; saved/reopen/opaque-state invariants and CI |

Each finished block updates this tracker and product documentation, checks actual
focused-test and `cargo xtask ci` process exits, and is committed. Storage changes
also require `cargo xtask compaction`. Preserve postcard/wire compatibility and
apply data admission bounds before copying. Controlled providers prove behavior;
real-model quality or speed claims require separate measurements.

## First block: Responses retry advice

Inspection found that `response_error` lost nested Retry-After,
and `Retrying::stream_attempts` only handled errors returned while opening the
HTTP stream. A 200 SSE response that fails before producing content escapes that
retry loop. Both paths must be addressed to implement the planned behavior.

Existing HTTP delay policy is reused: delta-seconds only, at most 120 seconds,
four attempts, terminal/auth/credit/context refusals retain their classification.
Retries after delivered text/reasoning/tool or completion evidence are forbidden.
Shared physical attempt observers must retain each failed retry and cancellation.
Implemented bounded nested Retry-After decoding (32 bytes before copying),
overload aliases, and lazy pre-content retries sharing the HTTP attempt counter.
The stream is returned before reading its first reply, preserving the engine's
first-token wait reporting. Failed streams and queue permits drop before backoff.
Cloned wrappers share learned effort/output refusal state.

Focused checks exited 0: `cargo test -p rook-llm` and the native CLI test
`responses_stream_retry_preserves_physical_receipts_locally_and_through_daemon`.
The native test verifies failed/completed durable receipts and their preservation
after shutdown, and leaves the destination branch untouched. Its first two runs
exited 101 because new test assertions confused later source ranges and failed
versus incomplete accounting; these assertions were corrected to the existing
contracts. The successful final run is retained in
`target/reference-retry-native-tests-final.log`; LLM tests are in
`target/reference-retry-llm-tests.log`. `cargo xtask ci` exited 0 in 559.2 seconds
(`target/reference-retry-ci.log`). No storage layout or configuration change is
part of this block, so a new compaction measurement was not required.

## Second block: repetition in streamed narrative

Implemented a per-stream guard for main/checking/delegated replies and
limit-triggered final answers, using the same engine path in each frontend.
Text and narrative reasoning have separate Unicode-character tails; encrypted
or signed state and tool arguments are untouched. Checks begin at 16,000
characters and remain at most one configured tail apart; a single large delta
is consumed incrementally before any detector copy. The conservative criterion
requires a dominant exact periodic run and rejects distinct multiline data.
Short requested repetition is allowed; `agent.stream_repetition_guard=false`
allows deliberately large repetitive output. Tail capacity defaults to 64,000
characters per channel, with validation and defensive bounds of 16,000–262,144.

Interruption closes the physical stream, retains bounded redacted diagnostic
excerpts and an atomic durable neutral assistant marker, and returns a failure.
The raw diagnostic is excluded before history body loading and by the existing
context/compaction record filter. No tool from the unfinished response executes,
no opaque state from it is replayed, and unseen usage stays unknown. Existing
completed batches and ordinary connection-failure partial replies retain their
existing semantics. Auxiliary aside/summary streams keep their existing explicit
output limits and are outside this first narrative-guard block.

Focused checks exited 0: detector fixtures plus actual engine interrupted-text
and interrupted-reasoning cases, opt-out, local/native daemon interruption and
durable physical accounting, reopening and a real compaction request that includes
the neutral marker but excludes looped text. Logs:
`target/reference-repetition-unit.log`, `target/reference-repetition-core.log`,
`target/reference-repetition-native.log`, and
`target/reference-repetition-reopen-compaction.log`. Additional validation/state
fixtures and the tightened compaction exclusion assertion passed in the final
full CI gate. No stored struct or format version changes; the new note
policy required a fresh compaction measurement: `cargo xtask compaction` exited
0 (`target/reference-repetition-compaction.log`), with the documented figures
unchanged (23.31 MiB logical, 5.29 MiB distinct, 0.14 MiB warm, 4.02 MiB disk;
37.1x dictionary and 5.8x end-to-end compression). The first full CI exited 1 on
two new test Clippy findings (lock scope across await and Iterator::last), both
corrected. The second CI exited 1 on an older shortening fixture made of thousands
of identical thought fragments. It now supplies distinct lines while still
exceeding the shortening threshold; its focused test exited 0
(`target/reference-repetition-legitimate-thought.log`). The final `cargo xtask ci`
exited 0 in 625.4 seconds (`target/reference-repetition-ci-final.log`).

## Third block: tool cycles without verified progress

Replaced the turn-local full-string map with bounded SHA-256 observations in
`agent/tool_cycles.rs`. Arguments serialize directly into a hasher; typed text
and image results are hashed before hook/source decoration. Command titles and
descriptions, poll wait durations and fresh background job IDs cannot bypass an
otherwise identical result. Two equal results refuse a third identical call;
three refusals stop the turn. Changing arguments with equal outcomes warns at
10 observations and stops at 20, with distinct saved stop diagnostics for
repeated calls, argument churn and unknown tool names. The live warning also
reaches the next provider request. Refused calls retain matched durable call and
result entries; no operation is started and task completion is never inferred.

The default window is 30 fingerprints, configured within 20–128; admission checks
the 128 KiB JSON companion limit before copying/decoding. One companion slot per
session/run is scoped to the canonical execution root plus completion boundary
or managed generation. It commits in the execution journal's existing durable
operation-completion transaction. Explicit continuation, actual compaction and
reopening preserve it. Accepted corrections, new admitted prompts and goal
generations reset it, including recovery of an acceptance committed before live
delivery. Automatic managed iteration is explicitly distinguished from a new
human prompt. No postcard struct, wire field or format version is changed.

Successful command/file-tool names and modification times do not reset evidence.
Complete bounded content proofs admit 500 workspace files, 32 MiB total and 8 MiB
per file; declared read inputs admit 32 paths with matching byte limits. Hashing
also bounds a file growing after metadata admission. Incomplete/unreadable input
proofs skip the fast refusal and never certify progress; result-churn detection
still applies. Snapshot walks start with actual tool use, avoiding workspace
hashing on pure chat. Live jobs use a registry liveness query with no captured
output copy; live child polling uses the nursery. Polling does not evict history
or erase a stall in another tool. Foreign `running` text/metadata does not exempt
calls, and finished polls stay guarded.

Focused evidence so far: the full agent-loop suite exited 0
(`target/reference-tool-cycles-all-engine.log`), worker iteration/accepted
steering/new-generation checks exited 0 (`target/reference-tool-cycles-goals.log`),
and native CLI/daemon continuation checks exited 0
(`target/reference-tool-cycles-native-verified.log`). The native fixture verifies
actual saved execution counts `[2, 0, 1]` for the initial loop, continuation and
fresh instruction after reopening. Additional targeted fixtures cover live versus
finished/foreign polling, no-op/failed writes, changed file bytes, background job
identity churn, corrupt/oversized receipts and explicit opt-out. An added mtime
fixture initially used the real built-in ahead of its substitute in the toolbox;
its precondition correctly failed. Removing the original before registering the
substitute made the actual mtime-only test pass
(`target/reference-tool-cycles-mtime.log`). The first full CI exited 1 on an
orphaned doc comment and a redundant let-and-return; both corrected. Early native
fixture failures incorrectly counted checker requests as main work, assumed the
same CLI/daemon JSON shape and decoded a saved turn tuple as a naked outcome;
the fixture now checks the actual existing shapes and durable outcomes.
The final `cargo xtask ci` exited 0 in 680.1 seconds
(`target/reference-tool-cycles-ci-final.log`), including the added background-ID,
mtime, configured-window and large-input fingerprint fixtures and all existing
frontend/storage/replay tests. `cargo xtask compaction` exited 0
(`target/reference-tool-cycles-compaction.log`); the existing corpus
measurement is unchanged: 23.31 MiB logical, 5.29 MiB distinct, 0.14 MiB warm,
4.02 MiB disk, 37.1x dictionary and 5.8x end-to-end compression.

## Fourth block: browser rendering batches

Browser chat now owns pending work by socket/session/turn/viewport, flushes
versus discards at explicit boundaries, releases completed source strings and
admits text before copy. It keeps one bounded prefix, one frame and one background
fallback, with no token-chunk queue or full source attribute. Per-answer admission
is 1,048,576 UTF-16 units; rendered scrollback is at most 4,194,304 model-source
units plus the existing 2,000-block bound. Shortening preserves surrogate pairs
and points to the source session's saved history. Equal final replies avoid a
duplicate parse; authoritative changes replace the current partial answer.
Historical answers render synchronously and late successful/failed overlapping
loads cannot overwrite the selected view.

The complete Node suite, syntax checks and embedded-module tests exited 0
(`target/reference-browser-node.log`, `target/reference-browser-assets.log`).
Actual Edge measurements reduced 1,134 Markdown parses to 36 on the same fully
verified fenced answer. The final browser probe exited 0
(`target/reference-browser-edge-final.log`) for error, Stop, snapshot, branch
selection/old socket, disconnect, queue receipt revisions and private
question/composer drafts. See [the measurements](browser-rendering-20261003.md)
and `xtask/probes/browser-rendering.mjs`. No installed daemon/profile was used;
owned scratch process termination is checked. Final `cargo xtask ci` exited 0
in 540.7 seconds (`target/reference-browser-ci.log`). Storage and daemon wire
formats were unchanged, so a compaction remeasurement was not required.

## Continuation notes for the remaining scope

Four behavioral rows are Done; six rows remain, including the required audits.
The next block is ACP child/compaction/error previews, followed by the catalog,
worktree recovery and the three audits. Do not mark the entire goal complete
after this finished browser block.

The completed guard lives in `agent/tool_cycles.rs`. Its execution companion is
separate from prompt compaction and commits with operation completion; keep the
run/root/generation boundary and don't replace content proof with tool names or
timestamps in later runtime/delegation work. The catalog, ACP previews and
worktree recovery are still Pending; no audit is implicitly closed by this block.

The next ACP block must use the inspected v1 capability shapes:
`clientCapabilities.subagents` and `clientCapabilities.session.compaction`
are enabled by a non-null object, not a boolean. Missing/null retain the legacy
path. Upstream `59172ba`'s root post-admission error stop reason concerns v2;
ordinary v1 prompt error responses are unchanged. Its v1 change is the negotiated
subagent idle snapshot's error stop reason. Do not invent a v1 root capability or
silently apply the v2 prompt lifecycle to the existing stable v1 adapter.

For the later delegation audit, `agent/delegation.rs` forwards ToolCall progress,
not every text delta, through two unbounded channels bounded indirectly by child
step/task limits. Measure queue high-water and durability before changing it.
Shared-root children clone tools/context/server pools; worktree children
intentionally reconstruct local tools/LSP/jobs to avoid using parent-root MCP or
editor access. Runtime reuse must preserve that execution-root boundary.
