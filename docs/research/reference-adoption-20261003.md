# Further reference adoption, October 2026

Implementation tracker for [the October 3 review](reference-review-20261003.md).
Starting Rook: `ce66697`. The active goal is to implement the planned scope,
including the audits below; one finished block does not complete this goal.
Reference commits identify mechanisms, not code to copy wholesale.

| Requirement | State | Completion evidence |
|---|---|---|
| Responses streamed retry advice | Done | Bounded nested header decoding, transient/terminal classification, shared retry before content only, real HTTP delay/cancellation/partial-output/attempt tests; native CLI/daemon durable receipts; full CI exit 0 |
| Repetition in streamed text | Pending | Bounded Unicode-aware detector, legitimate repetition fixtures, attributed interruption and replay semantics, partial/reasoning/tool-state accounting, local/daemon checks and CI |
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
