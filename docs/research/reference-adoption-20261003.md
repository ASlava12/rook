# Further reference adoption, October 2026

Implementation tracker for [the October 3 review](reference-review-20261003.md).
Starting Rook: `ce66697`. The active goal is to implement the planned scope,
including the audits below; one finished block does not complete this goal.
Reference commits identify mechanisms, not code to copy wholesale.

| Requirement | State | Completion evidence |
|---|---|---|
| Responses streamed retry advice | Done | Bounded nested header decoding, transient/terminal classification, shared retry before content only, real HTTP delay/cancellation/partial-output/attempt tests; native CLI/daemon durable receipts; full CI exit 0 |
| Repetition in streamed text | Done | Bounded Unicode-aware detector, legitimate repetition/opt-out fixtures, attributed interruption, local/daemon physical accounting, clean reopen and actual compaction replay; CI and storage compaction exit 0 |
| Tool cycles without progress | Pending | Bounded run-scoped fingerprints, polling liveness, alternating calls/argument churn, compaction/restart/new-instruction behavior, observable warning/stop reasons and CI |
| Browser rendering batches | Pending | Frame batching, final/error/Stop flush, branch/reconnect ownership, actual long-response rendering measurements, preserved drafts/questions/queue and CI |
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

## Continuation notes for the remaining scope

The next behavioral block is the tool-cycle guard: the existing `repeated` map
in `agent.rs` is per turn and clears for `run_command`/file-changing tool names,
without proving a changed workspace. Replace retained full results with bounded
fingerprints and distinguish actual progress and live polling; keep new user
instructions and goal generations separate. Do not infer completion from a stop.

For the later delegation audit, `agent/delegation.rs` forwards ToolCall progress,
not every text delta, through two unbounded channels bounded indirectly by child
step/task limits. Measure queue high-water and durability before changing it.
Shared-root children clone tools/context/server pools; worktree children
intentionally reconstruct local tools/LSP/jobs to avoid using parent-root MCP or
editor access. Runtime reuse must preserve that execution-root boundary.
