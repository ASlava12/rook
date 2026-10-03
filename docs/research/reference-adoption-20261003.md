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
| ACP child/compaction/error events | Done | Negotiated v1 object capabilities, directed tasks/answers, child editor bridges/states and restricted controls, bounded recovery and durable compaction IDs; actual owned wire/core/transport and reopen fixtures; CI and compaction exit 0 |
| Model/price reference catalog | Done | Bounded explicit models.dev refresh/offline cache, exact direct-cloud identity and source/age, reviewed missing-rate application across CLI/terminal/browser/daemon, manual/live precedence and frozen costs; reached bounds/contention, account/environment/cache/alias/reopen tests; CI and compaction exit 0 |
| Delegated worktree recovery | Done | Read-only diagnosis and reviewed registered-index restoration across CLI/REPL/TUI/browser/daemon/tools, recreated/dirty file preservation, missing live lease and Windows/link/admission bounds, actual native/reopen and index-version fixtures; CI and compaction exit 0 |
| Delegation progress persistence audit | Done | Actual delayed-reader high-water 256 to 1, constant journal/receipt writes for 1 versus 1,024 fragments, exact critical observer and reopened CLI/daemon results, bounded coalescing in nursery/blocking/checker paths; CI exit 0 |
| Rooted runtime reuse audit | Done | Actual shared/worktree LSP processes, jobs and run approvals, HTTP MCP generation/cancellation, admitted detached daemon preparation with live delivery and publication ownership; CI exit 0 |
| Long-task compaction scenario | Done | Actual HTTP before/summary/first-next requests, pre-turn replacement replay, reached source boundary, accepted correction/open question/rejected approach, reopened old/new opaque-state invariants; CI exit 0 |

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

Eight rows are Done; the runtime audit and long-task compaction scenario remain.
The next block audits rooted runtime reuse and preparation responsiveness.
Do not mark the entire goal complete after one finished block.

The completed guard lives in `agent/tool_cycles.rs`. Its execution companion is
separate from prompt compaction and commits with operation completion; keep the
run/root/generation boundary and don't replace content proof with tool names or
timestamps in later runtime/delegation work. Worktree recovery is Done;
no audit is implicitly closed by ACP transport or catalog changes.

The ACP block uses the inspected v1 capability shapes:
`clientCapabilities.subagents` and `clientCapabilities.session.compaction`
are enabled by a non-null object, not a boolean. Missing/null retain the legacy
path. Upstream `59172ba`'s root post-admission error stop reason concerns v2;
ordinary v1 prompt error responses are unchanged. Its v1 change is the negotiated
subagent idle snapshot's error stop reason. Do not invent a v1 root capability or
silently apply the v2 prompt lifecycle to the existing stable v1 adapter.

The completed delegation audit measured ToolCall display progress, rather than
text fragments, and replaced the unbounded nursery/blocking/checker hint queues
with bounded coalescing. See [the measurements](../delegation-progress.md).
Shared-root children clone tools/context/server pools; worktree children
intentionally reconstruct local tools/LSP/jobs to avoid using parent-root MCP or
editor access. Runtime reuse must preserve that execution-root boundary.

## Fifth block: negotiated ACP child and compaction previews

Runtime observations carry the actual child session/execution workspace and
compaction boundary without extending stable Progress/ChatEvent enums. Child
associations precede directed task/answer messages, own tool/reasoning/context
events and editor requests. Optional child-scoped file, terminal and approval
wrappers share the existing rights/policy; worktree isolation still refuses
editor bridges and reconstructs its rooted equipment. Shared children no longer
borrow the parent's interactive `ask` tool. Individual cancellation is not
supported by the runtime, so capabilities are `{}` and child client operations
are refused without cancelling the parent. Actual child failure ends in an idle
error snapshot; dropped observation remains unknown. Ordinary v1 root errors
remain JSON-RPC errors. Non-null object previews are independent and only enabled
for explicit protocol v1. The pinned v2 post-admission lifecycle is not imported.

Compaction start/terminal events share a saved optional `compaction_id` in the
existing Compaction JSON. Old records remain readable; replay source IDs for old
records are deterministic. Completion follows successful append. Generation
failure can still complete the existing neutral replacement fallback, while
failed persistence and cancellation cannot claim saved completion. Summary
streams cap combined narrative/text before assembly (64 KiB default, validated
1 KiB–1 MiB, 2,048 requested output tokens); opaque blocks are not assembled.
Fallback error display is also bounded before formatting. Display summaries and
directed tasks have UTF-8-safe prefixes, saved source attribution and explicit
shortening metadata.

Recovery bounds session/tree/event metadata, record reads and display copies.
Historical snapshots precede load/resume success; observed current states on
an existing connection are confirmed afterwards. Full child conversation replay
is not reconstructed; `_meta.rook.recovery` reports this and admission truncation.
Transport shares escaped-byte/event leases through flush, explicitly closes on
synchronous delivery overflow and caps pending editor requests. It cannot retain
an unbounded second queue or silently lose critical lifecycle/permission events.
See [the product contract](../acp.md) for exact limits.

Owned HTTP/ACP fixtures cover actual simultaneous and nested delegation, directed
participants, unsaved editor buffers, terminal requests and permission/tool IDs,
restricted controls, parent cancellation, failed children, v1 root errors,
independent/null/boolean capabilities, actual compaction and reopened store/ACP
recovery. Core fixtures exercise combined output admission, opaque-state exclusion,
saved ID/sequence, fallback, insufficient history, actual append failure after
summary generation, cancellation and configuration
bounds. Adapter/transport fixtures reach live-child, pending-request, in-flight
event and escaped-byte budgets; blocked output closes the view while preserving
the completed store mutation. Initial compiler errors in new fixture model fields
and a directionally ambiguous JSON-RPC-ID assertion were corrected. Clippy's
manual-async suggestions are locally explained: explicit `impl Future + Send`
boundaries prevent recursive run_with proof overflow without restricting public
progress closures. Focused logs are `target/reference-acp-tests-final.log`,
`target/reference-acp-compaction-tests-final.log`,
`target/reference-acp-delivery-tests.log` and `target/reference-acp-clippy.log`.
The first full CI exited 1 in 692.0 seconds: its only failed target was
`no_panics`, identifying two new production unwraps. Both are removed, and the
targeted rule plus actual append-failure fixture exited 0. Session-control
metadata read errors now fail closed instead of permitting an unchecked mutation.
The second full CI exited 1 in 913.3 seconds with only the existing native
tool-cycle fixture failing: a Windows accepted socket retained nonblocking mode,
so an immediate read raced the client's header write and returned WouldBlock.
The fixture now explicitly sets blocking mode, as the other owned HTTP fixtures
do; its actual native CLI/daemon check exited 0
(`target/reference-acp-native-cycle-fixture.log`), without relaxing limits.
The final full `cargo xtask ci` exited 0 in 873.8 seconds
(`target/reference-acp-ci-verified.log`), covering all existing frontend, daemon,
storage and replay checks as well as the new fixtures. `cargo xtask compaction`
exited 0 (`target/reference-acp-compaction.log`); corpus measurements remain
23.31 MiB logical, 5.29 MiB distinct, 0.14 MiB warm and 4.02 MiB on disk, with
37.1x dictionary and 5.8x end-to-end compression.

The catalog block extends the existing bounded endpoint catalog and configuration
editor rather than replacing them. `model_catalog.rs` and its runtime snapshot
preserve offline/live observation scopes; `model_route::Prices` still freezes
operator rates into physical receipts. Public references live separately in
`price_catalog.rs`, and cannot affect an ordinary turn until explicitly applied
to missing configured rates. No remaining audit is closed by this catalog work.

## Sixth block: explicit public model/price references

Added the bounded models.dev provider reference with a fixed public destination,
source/observation age, explicit refresh, private atomic cache and offline
inspection. No endpoint credentials or external credential helpers participate
in a public fetch. Byte, provider/model count and UTF-8 identity bounds apply
before retaining entries; unknown metadata remains borrowed raw data. Redirects,
invalid rates, duplicate/differing identities and unsupported cost schema refuse
refresh and preserve the previous cache. Corrupt/oversized/future-dated caches
remain unknown; stale references are visible but cannot be applied.

Named inline/shared endpoints use the existing structural resolver and an exact,
conservative direct-cloud identity table. Local/custom/proxied sources and aliases
cannot inherit public prices by model name. Capabilities/context are display-only.
Tiered/context/modality rates and provider/experimental overrides cannot be
flattened into runtime accounting. Manual values (including zero) retain priority;
the operator can review and fill only missing input/output/cache rates. Unknown
cache rates remain unknown. Last application source/age/fields are attributed in
an optional configuration string, not used to select runtime rates.

Reviews bind the whole config revision, source/endpoint/passive-account scope and
cache revision. Inline and vault-backed environment-key rotations are included
without executing command/keychain helpers. Passive credential hashing streams
JSON into the digest instead of allocating a credential copy. The shared config
editor and `config set` take the same explicitly released write lock; the editor
checks external changes under it before atomic replacement. Config set also
admits its existing file and writes atomically. Literal dotted source names,
comments, manual values and the resolver's trimmed endpoint spelling are retained.
Saved store structs and wire enums are unchanged, and frozen physical cost
snapshots/old numeric receipts are not recalculated after a refresh or config edit.

The same core inspection/application is exposed by `rook prices`, the terminal
form (`--interactive` or `p` in a clean `rook config edit` draft), and the browser
Prices tab/daemon endpoints. Terminal refresh has a visible deadline/cancellation;
browser actions belong to their original view/review and discard late responses.
Daemon cache/config work runs outside engine locks, admits two catalog operations,
and limits apply request bodies to 4 KiB. The product contract and exact limits
are in [model-price-references.md](../model-price-references.md).

Owned fixtures cover actual finite HTTP header/chunk overflow and failed refresh,
count/string/alias/duplicate limits, corrupt/future/stale/offline cache behavior,
command-helper nonexecution, direct/proxy/local identity, actual CLI and daemon
application, credential/environment/config/cache rotations, preserved comments
and manual context/rates, reached cooperative write contention, physical cached
token costs and old rate snapshots. Browser fixtures cover plain-text untrusted
names, explicit/declined application, one in-flight action, stale review errors,
and late responses/detached buttons across new views and tab changes.
Focused checks exited 0: `target/reference-prices-core-verified.log`,
`target/reference-prices-native.log`, and `target/reference-prices-browser-fixed.log`.
Initial new-test compilation/Clippy findings (private Dispatch import, a needless
Usage update and literal format calls) and a mock-DOM text predicate were corrected.
A real explicit upstream refresh also exited 0 in an owned ROOK_HOME fixture
(`target/reference-prices-live-refresh.log`); it does not claim account prices or
live provider capabilities. The first full CI exited 0 in 912.6 seconds
(`target/reference-prices-ci.log`). Inspection then corrected trimmed endpoint
compatibility, avoided copying helper definitions during passive environment scope
inspection, and made terminal metadata show plain values/unknown. The reached
write-contention/trimmed-endpoint regression and other core checks exited 0.
The final full `cargo xtask ci` exited 0 in 911.8 seconds
(`target/reference-prices-ci-verified.log`), covering the final code and all
existing native/daemon/frontend/storage checks. `cargo xtask compaction` exited 0
(`target/reference-prices-compaction.log`); measurements remain 23.31 MiB logical,
5.29 MiB distinct, 0.14 MiB warm and 4.02 MiB disk, with 37.1x dictionary and
5.8x end-to-end compression. This closes the catalog row only.

## Seventh block: delegated checkout recovery

Implemented shared core diagnosis and explicit restoration from the registered
child Git index, including staged changes. Tokens bind parent/child/repository,
admin, HEAD, index bytes and configured limits. Existing recreated files and
untracked entries are preserved; healthy checkout deletions remain ordinary edits.
No registered index, Git admin, branch or format is rewritten. Raw blob sizes,
metadata and paths are admitted before copying; directory traversal does not
follow links, and atomic publication preserves concurrent entries. Live leases
keep their identity after directory deletion, including Windows canonical prefixes.
Bounded durable JSON receipts distinguish successful restoration from interruption
and uncertainty; task completion/current parent files/tests are never inferred.
See [the operator contract](../worktree-recovery.md).

CLI, local/shared REPL and TUI, browser, daemon and approved agent tools use the
same implementation. TUI work runs in its background worker; daemon work is
admitted before a blocking worker and retains no engine lock. Focused native
CLI/daemon/REPL restoration and reopen checks exited 0, as did filesystem link,
atomic preservation and reached input-limit tests, session metadata admission,
core ownership/source/lease/bounds cases and browser late-view/token cases.
The native test found Windows Git's verbatim-path refusal and a large async
buffer causing stack overflow; both were corrected and the reached native path
then exited 0. Actual index versions 3/4, sparse/gitlink/symlink-text fixtures and
the split-index refusal test also exited 0. First complete CI exited 0 in 818.9
seconds (`target/reference-worktree-ci-verified.log`). `cargo xtask compaction`
exited 0 (`target/reference-worktree-compaction.log`): unchanged 23.31 MiB logical,
5.29 MiB distinct, 0.14 MiB warm and 4.02 MiB disk, 37.1x dictionary and 5.8x
end-to-end. Final review reached a browser regression with lowercase child IDs:
the server canonicalized the ID, but the button compared its unnormalized input.
The reached test failed, then passed after input identity normalization. Final
`cargo xtask ci` exited 0 in 828.2 seconds
(`target/reference-worktree-ci-final-code.log`), covering the corrected embedded
browser assets and all native/daemon/core/storage checks. Earlier CI attempts
exited 1 on two new Clippy findings (needless question mark and byte-char slice),
which were corrected before the successful full gates. No installed daemon,
operator store/configuration or browser profile was used. This closes row seven;
the three remaining audits/scenarios stay Pending.

## Completion and continuation notes

All ten planned rows are Done. The active implementation scope is complete;
the other observations in the review remain separate future proposals. The
sections below retain the per-block evidence and failed checks as history.

Progress audit is Done. `agent/delegation_progress.rs` supplies nursery/blocking/
checker delivery; `agent/stream.rs` consumes the nursery receiver. The configured
per-queue limit is `agent.delegation_progress_entries` (32, range 1–128), with
2 KiB admitted hint text before copy. Only intermediate display hints coalesce;
critical call/result/execution receipts and child outcomes remain exact. The
dedicated engine audit file explicitly uses `#![cfg(test)]`. Execution write
measurements are test-only and count successful `save_with_claim` updates,
rather than all database transactions. ACP has its separate bounded observer
transport; it does not replace the execution journal.

Runtime audit is Done. Shared children reuse
actual LSP processes/jobs/run approvals; worktrees rebuild rooted equipment and
drop parent-only tools. Daemon discovery uses an admitted publication entry and
a detached blocking worker, retaining no engine/project guard during discovery.
Focused child, MCP generation, cancellation/delivery and cache ownership tests
and full CI exited 0. The synchronous core `for_workspace` contract remains available.
The final long-task compaction scenario has been added to `provider_history`.
Its actual HTTP before/summary/replacement/next-request and reopened signed-state
checks and full CI exited 0 (`target/reference-long-task-focused-final.log`,
`target/reference-long-task-ci.log`). Product evidence is documented
in [long-task-compaction.md](../long-task-compaction.md).

## Eighth block: measured and bounded delegated display progress

The actual delayed-reader baseline retained 256 hints from one child reply.
Executing 256 real file reads with the same text in one versus 1,024 fragments
retained 787 child journal records in both cases, about 42 KiB logical bodies,
and no copied 28 KiB parent conversation. The corrected delivery retains one
latest hint for that child, with the last read naming a different file. Both
fragmentation cases perform 515 successful execution companion save-path writes,
about 322 KiB across all updates, with a maximum individual receipt of 737 bytes.
Critical observers still see all 256 calls and already-saved results plus the
child's start/end. The product contract and reproducible measurements are in
[delegation-progress.md](../delegation-progress.md).

All three internal hint queues now share the bounded coalescing implementation:
fixed admitted child slots, 2 KiB UTF-8-safe hints before copy, fair position for
updates to an existing child, oldest pending hint eviction for a new child at
capacity, one text-free wakeup and producer closure/cancellation handling.
Reached fixtures cover excess children, stale hints, a byte limit inside a
multibyte character, closed readers and defensive configuration ceilings.
Durable operations/lifecycle and saved formats are unchanged. No storage-layout
or persistence-policy change is part of this block; compaction remeasurement
was not required.

Focused checks exited 0: `target/reference-delegation-baseline-verified.log`,
`target/reference-delegation-core-final.log`,
`target/reference-delegation-admission-final.log`,
`target/reference-delegation-engine-tests.log`,
`target/reference-delegation-checker-tests.log` and
`target/reference-delegation-native-final.log`. The native fixture drives actual
blocking and nursery children locally and through an owned daemon, then reopens
both stores and checks all call/result pairs, exact last-result sequences, clean
receipts and child outcomes. Early fixture attempts confused structured OpenAI
text content and checker requests with parent/child script routing; a subsequent
assertion expected `completed` instead of the actual `end_turn` receipt status.
Those fixture assumptions were corrected before the successful native run.

The first full CI exited 1 in 696.3 seconds with only `no_panics` failing:
the dedicated test file had no local `cfg(test)` marker, so the scanner treated
its fixture unwraps as shipping code. Adding the explicit file-level marker
preserves the rule; its targeted check exited 0. Final `cargo xtask ci` exited 0
in 863.2 seconds (`target/reference-delegation-ci-final.log`), including the
reached Unicode admission and all native/core/daemon/storage/frontends checks.
No installed daemon, operator store/configuration or browser profile was used.
This closes row eight only; the two remaining rows stay Pending.

## Ninth block: rooted equipment and responsive preparation

The actual delegated fixture uses one warmed LSP process and shared jobs/run
approval across two children. A worktree child starts a distinct LSP process with
its checkout root, executes local diagnostics and records the refusal of a
parent-only tool. Real HTTP MCP fixtures verify atomic generation replacement,
old in-flight calls, failed/cancelled candidates and bounded admission; the
existing native daemon reconnect fixture also passes in the full gate.

Inspection found synchronous plugin/skill discovery under both the daemon
project-map write lock and root engine read lock. `WorkspaceSeed` now captures
immutable inputs; the admitted cache entry owns preparation independently of
request cancellation, and a blocking worker performs discovery without those
locks. Ready and pending entries share the existing project cap. Publication
waiters protect a root before cloning its engine, and the current configuration
is applied at publication. Saved/wire formats and synchronous `for_workspace`
semantics are unchanged. See [runtime-reuse.md](../runtime-reuse.md).

Focused checks exited 0: `target/reference-runtime-child-tests-fixed.log`,
`target/reference-runtime-mcp-tests.log`,
`target/reference-runtime-preparation-config.log` and
`target/reference-runtime-project-tests.log`. The held-worker daemon fixture
exercises real API health/context/instructions and live notice delivery after
cancelling the first opening request, then observes one reused published engine.
Earlier fixture compilation errors concerned the transcript API arguments and
a `Debug` bound in a cancellation assertion; both were corrected.

The first CI exited 1 in 49.2 seconds on Clippy's question-mark suggestion.
After correction, final `cargo xtask ci` exited 0 in 936.6 seconds
(`target/reference-runtime-ci-final.log`). No storage policy/layout change is
part of this block, so compaction remeasurement was not required. No installed
daemon or operator state was used. Only the long-task compaction row remains.

## Tenth block: long-task compaction across correction and an open decision

Added a controlled real HTTP Responses scenario with a long journal, an accepted
user correction, an unanswered deployment question and a rejected deletion
approach. It observes the actual request before compaction, summary request,
replacement replay before another turn and first subsequent request. The live
history exceeds the configured threshold; the saved source boundary lies after
the question, so the facts must survive through the summary rather than only
through the retained tail. Actual read results reach the summary input, while
opaque state and executable tools do not.

Replacement replay retains the existing `rook_source` data authority and is
identical after closing/reopening the store before the next turn. The subsequent
request carries the accepted correction, unanswered status and reason against
the rejected approach once, without the discarded signed batch. The journal
retains one original question and readable old events. A new signed response is
saved, then a second reopen verifies new-state replay without reviving old state.
Human-readable transcripts expose neither opaque value. The hidden read-only
history seam exposes the engine's actual replay for this stage comparison.
No production compaction algorithm, storage policy, layout or stable wire format
changed; new storage remeasurement was not required.

The first focused run exited 101 because the assertion searched a twice-escaped
JSON string for the summary. Parsing the existing source wrapper allows exact
content/kind/authority assertions. Both the new scenario and existing signed
reopen/fork/interruption/compaction fixture then exited 0
(`target/reference-long-task-focused-final.log`). Final `cargo xtask ci` exited 0
in 902.1 seconds (`target/reference-long-task-ci.log`). The owned endpoint checks
the actual summarizer input before returning a controlled summary; this proves
engine mechanics, not the quality of a particular model on real multi-day work.
No installed daemon or operator state was used. This closes the final row.

## v0.11.0 release validation follow-up

The first full hosted matrix reached Unix-only TUI fixtures with stale expectations:
local `/schema-retries` settings were expected in model steering, daemon `/context`
was expected to refuse the store, and branch review text was mistaken for a durable
summary receipt. The fixtures now assert the current behavior and wait for the
actual save acknowledgment. Ordinary Stop identity checks wait for withheld model
requests; timeout diagnostics retain a bounded last socket frame.

FreeBSD exposed a cancellation race in interactive hooks: dropping stdin before
the process-group guard allowed EOF to be treated as an answer. One owning input
struct drops its group before its pipe. The existing cancellation and answered-form
tests both exit 0 (`target/release-hook-focused.log`); the focused ordinary Stop
scenario exits 0 (`target/release-stop-focused.log`). Hosted job deadlines are
45 minutes because cold Windows/macOS builds plus the expanded suite exceed 25.
Full local and hosted gates are being repeated before tagging. Store formats,
stable wire shapes and operator state are unaffected.

The full local follow-up gate exited 1 in 647.8 seconds solely because the shipping
code scan rejected new unchecked stdin unwraps. The input borrow now returns an
I/O error instead; the focused `no_panics` suite exits 0
(`target/release-no-panics-focused.log`). All five dry-run archives at `4b85aee`
pass checksum/layout/skill-count verification; Unix binaries retain executable
permissions. A new complete gate and archive build follow the stdin-borrow fix.

The hosted Linux follow-up confirms ordinary Stop, local slash commands and
daemon context now pass. The summary fixture's transient acknowledgment is
cleared while rehydrating the selected branch, so it now waits for the saved
`branch-summary` event loaded into target history before closing the process.
That displayed persisted event also verifies the source-attributed recall.

The crash/reopen assertion then exposed a real missing durability boundary:
manual summary carry appended a buffered event between ordinary turns. Carry
now flushes before returning its acknowledgment; the existing core assertion
first reaches buffered events and then verifies none remain after saving. The
Unix PTY fixture retains abrupt local/daemon shutdown and checks the reopened
record, rather than weakening the scenario to a graceful exit.

Windows hosted failures also exposed fixture assumptions: the delegation mock
matched a quoted nursery report as a fresh child task, the hook scenario used
a five-second PowerShell startup guard, and worktree setup did not persist its
LF policy for the production Git invocation. The mock now matches an exact
task line across folded/structured user content; hook patience is 90 seconds;
owned worktree repositories set `core.autocrlf=false` before checkout creation.
Production hook patience and Git behavior are unchanged.

Focused delegation, native form, worktree recovery (with an owned temporary
global `autocrlf=true`) and summary tests exit 0. The initial strict whole-message
mock matcher exited 101 because provider folding prepended context; the corrected
task-line matcher passes with exact parent/child request counts. `cargo xtask
compaction` exits 0: 4.02 MiB on disk, 37.1x warm-object compression, 5.8x overall
(`target/release-summary-compaction.log`). Formats and measurement claims remain
unchanged. Full CI and release archive checks follow these final corrections.
