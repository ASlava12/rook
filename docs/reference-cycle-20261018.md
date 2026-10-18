# Reference delivery cycle — 2026-10-18

Delivery record for Rook 0.17.0: ten runtime capabilities, shared refactoring
and a security/contract/behavior audit with corrections. The latest
[source review](research/reference-review-20261018-cycle.md) records all ten
advanced reference pointers and the three additions to the seven R17 ports.
This checkout records local verification; [CI](https://github.com/ASlava12/rook/actions/workflows/ci.yml),
[packaging](https://github.com/ASlava12/rook/actions/workflows/release.yml) and the
[release](https://github.com/ASlava12/rook/releases/tag/v0.17.0) record publication.

| Work | Status | Evidence / remaining work |
|---|---|---|
| R17-1/2/3/4/5/6/8 | Implemented; integrated checks passed | [R17 delivery and native checks](reference-r17-adoption.md). The PDF fixture correction is included; the complete final gate exited zero. |
| C18-1: live MCP catalogs | Implemented; integrated checks passed | [Configuration and contracts](mcp-catalog-refresh.md). Stdio/native HTTP, cancellation/coalescing, safe pagination, direct/deferred actual turns, Apps invalidation and resource-only servers pass. Final integrated gate passed. |
| C18-2: Gemini malformed recovery | Implemented; integrated checks passed | Native HTTP buffered/streaming recovery, bounded retries, no replay after partial delivery, cancellation, and physical-attempt counters. An actual `AgentLoop` regression verifies no failed-frame tool dispatch and persisted usage, with the completion-check request accounted separately. Workspace/all-target clippy passed. |
| C18-3: background-work overview | Implemented; integrated and native checks passed | [Usage and coverage](background-work.md). Shared lazy family reader, session-owned metadata-only jobs, actual child executor observations and CLI/API/TUI/browser. |
| Refactoring and audit | Completed for this delivery | Findings, corrections, source ownership, permissions, cancellation, bounds, persisted compatibility and frontend evidence are recorded below. |
| Final validation and release | Local release gates passed | Final `cargo xtask ci` exited zero; browser modules and owned native daemon/Chrome/PTY checks passed. Version and release notes are prepared. Exact remote CI and artifact publication are recorded by the linked GitHub workflows/release. |
| Temporary build/state cleanup | Executor follow-up outside the checkout | Owned target/tmp data lives under `/Users/vart/.cache/rook-cycle-20261018`; remove after verification/publication and process exit, retaining compact evidence. |

R17-7 is an inconclusive quality experiment and remains pending. API quota and
Qwen timeouts yielded no successful comparison; the default summary budget stays
2048. It does not block unrelated available work or justify asking again for
secrets the user has declined to provide.

## Current evidence and next mechanisms

C18-2 is exercised by `crates/rook-llm/tests/google_recovery.rs`, the existing
Google/retry/library suites, and
`malformed_google_frames_never_reach_engine_tool_dispatch_and_failed_usage_stays_saved`
in `crates/rook-core/tests/reference_r17.rs`. Failed generations use a typed
`IncompleteResponse` error without copying provider response payloads. The shared
attempt guard preserves explicitly reported counters and refuses to mark them
completed; dollar estimates retain the existing conservative incomplete-usage
rules. Direct HTTP and actual-engine checks pass. This is fixture evidence, not a
live Gemini quality or availability measurement. The final integrated gate also passed on the complete cycle sources.

C18-1 implementation and audit: transport notifications advance a shared catalog
clock. The server publishes complete, coalesced snapshots; ToolBox refreshes at
safe request boundaries and retains the Arcs of admitted calls. Source tokens
prevent mixed search/schema continuations. Failure/cancellation retains the
previous snapshot with a cooldown, and operator surfaces expose stale state.
Apps declarations bind exact connection and catalog generations. The guide and
audit below record native stdio/HTTP, actual-engine and authority checks.

C18-3 implementation: ordinary background commands acquire their session owner
at admission; existing private registries retain scope. The shared family page
uses output-free metadata and bounded branch navigation. Current root/child
observers can attest their own work; saved state remains unverified. Display
pagination does not replace complete billing/recovery traversal. See the
[coverage guide](background-work.md) for per-owner child and job bounds.

### Verified provider audit finding

The existing Google finish mapper prioritized `called_tools` over refusals. A
buffered candidate containing `SAFETY` and a function call was accepted as an
executable response; a regression reproduced that before the fix. A later
refusal frame could also leave earlier function calls in the stream assembler.
`google::invalid_finish` now applies one finish contract to both native response
paths, ahead of exposing failed-frame contents. All documented refusal reasons
remain refusals; combining them with calls fails without retry. Malformed or
unexpected function-call finishes, the provider's call ceiling and missing
thought signatures also fail without replay. Only `MALFORMED_RESPONSE` is eligible
for the bounded pre-content retry. A later refusal aborts the stream, preventing
its assembled earlier calls from reaching engine dispatch. Reported error usage
continues through the shared attempt guard; absent counters remain unknown.

The native HTTP regression was observed failing on the old refusal behavior and
then passing after the correction. The actual-engine matrix covers both a refused
call in the final frame and one already delivered by an earlier frame; it checks
no tool journal entry, no file effect, no retry and the failed attempt's saved
counters. Successful malformed-response recovery also completes the separate
completion-check request. The complete cycle audit below also covers R17, live MCP catalogs
and session background work.

Final checks for this provider increment: the Google/recovery/retry/library suites
and the complete `reference_r17` integration binary passed on the final sources.
Workspace/all-target clippy passed with warnings denied, formatting passed, and
`git diff --check` passed. Reproduction uses `cargo test -p rook-llm --lib --test
google --test google_recovery --test retry` and `cargo test -p rook-core --test
reference_r17`. This does not replace the pending full integrated release gate.

## Delivery audit

The review covers the ten runtime ports (seven R17 items plus C18-1/2/3).
Reference worktrees stay frozen at the recorded October 18 pins during delivery.
The quality experiment is separate and does not count as a shipped runtime port.

| Boundary | Finding and correction | Evidence |
|---|---|---|
| Durable choice/admission | Model and effort must share one receipt/revision; a preset cannot issue two independent mutations or change a running root's admitted pair. Legacy receipts migrate without discarding selected effort. | `model_selection.rs`, R17 integration and native held-generation/restart/preset checks. |
| Drain/replacement | Admission, pending interactions and controller handoff need distinct ownership. A timeout cannot force replacement; exact retries and cancellation retain source identity. | Shared drain/controller tests, held native turn, explicit drain cancellation, successful restart and failed-controller probe. |
| Compaction/routes | Phase cancellation/deadline must include provider queue/retries and refuse late commits. A compaction construction failure must not escape a restricted chain. | `agent/compaction.rs`, models and R17 tests; ordinary stream watchdog semantics retained. |
| Delegated output | Validation before redaction can certify bytes different from delivery; raw errors/results need bounded redacted receipts and recoverable source references. | `delegated_results.rs`, actual delegated-turn schema/redaction/omission checks. No child tool replay. |
| Saved answer | Branch summaries/internal messages cannot silently replace the answer copied by the user. Clipboard refusal requires a readable fallback. | Last-answer tests, native PTY and Chrome owned clipboard controls. |
| Reasoning | Configured capability is not provider acceptance. Report source provenance, rejected combinations and the actual clamped wire mapping. | Native API adapter fixtures, catalog/config/doctor and R17 tests. |
| Failed Google generations | A refusal containing a function call, or arriving after an earlier call frame, could reach dispatch. Apply finish validation before exposing failed contents and abort late refusals; retry only malformed pre-content generations. | Failing-before/fixed native HTTP case and actual-engine no-dispatch/no-file-effect/failed-usage matrix. |
| MCP coalescing/cancellation | Publishing cooldown after releasing the refresh lease lets a queued reader issue a second failed list. Publish failure/cancellation under the lease; cancelled futures cannot install late catalogs. | Concurrent native HTTP owner cancellation, queued reader and exact list-count checks. |
| MCP binding/restrictions | Deferred invocation could bypass an immediately removed direct tool; paging could combine bytes from different catalogs. Rebuild restricted helpers immediately, retain source Arcs and require content/query-bound continuation tokens. | Two actual HTTP endpoints with equal names, frozen old boxes/calls and changed-catalog/query continuation tests. |
| MCP Apps/progress/retention | Dirty or replaced sources invalidate prior confirmations; a separate report lookup could stamp old declarations with a replacement connection's generation. Bind exact connection plus catalog snapshot, remove obsolete startup descriptor copies, and identify refresh progress accurately. | Apps consent/reconnect/failed-refresh matrix, direct and deferred full agent turns, resource-only no-probe regression. |
| HTTP listener authority | An idle notification stream must not borrow a concurrent call's human-input handler, leak endpoint payloads or survive its owner. GET/POST Accept and response status differ; SSE retry hints govern recovery. | Held interactive POST beside GET, unowned-RPC refusal, OAuth canary, 404/405 fallback, shutdown/drop and native reconnect timing tests. |
| Background ownership/privacy | Shared jobs lacked admission ownership; metadata polling cloned output. Record the session at admission, preserve private scope, project only bounded metadata and keep saved observations unverified. Actual child observations are bounded and end with their owner. | Ordinary RunCommand owner/filter/omission regression, core lazy paging, daemon saved/current owner tests and busy TUI draft preservation. |
| Preset acknowledgement | Typing while paired settings await an API acknowledgement could be overwritten by the late preset. Guard draft ownership before loading and after acknowledgement. Busy next-turn choice remains supported. | Browser module regressions for dismissed/refused settings, confirmed busy choice and newer draft; native Chrome exercises the real controls. |

The shared family reader reuses branch display bounds; complete billing/recovery
walks are unchanged. Daemon metadata polling joins already-created project
registries, including registered worktrees, and never creates equipment. Private
ACP job registries remain private. Child inactivity can include human waits, so
child stall alarms are disabled rather than making an unsupported causal claim.
The [coverage guide](background-work.md) states these limits and the lack of
persisted child observations. Conversation-goal scheduling state is saved state,
independent of whether a current daemon executor can attest ownership.

## Final local verification

The final local `cargo xtask ci` process for implementation commit `e55024d`
exited **0** and printed `ci: ok` on frozen runtime/test sources. Reported timings: fmt 1.3s, clippy 21.3s, frontend build
30.6s, tests 1083.9s, total 1137.0s. This includes the new private-registry scope,
metadata owner/omission and actual 128-child projection-bound regressions. A
preceding complete preflight also exited zero; it does not substitute for the
final run. Runtime/test sources stayed frozen throughout that run. The subsequent
compiler-compatibility correction is recorded below and has its own checks;
remote CI verifies the exact final release revision.

`node --test web/tests/*.mjs` exited zero on the same frontend sources. The owned
native probe exited zero using the final built `rook`/`rookd`, an isolated
`ROOK_HOME`, a held HTTP model fixture, actual Chrome and native PTYs. It verified
busy API/CLI owner parity; background-panel draft/session preservation; one paired
preset mutation; saved-answer copy and readable clipboard refusal; reload and
late-acknowledgement source ownership; successful/cancelled drain and restart;
failed-controller preservation; export parity; and text/JSON terminal lifecycle.
No operator clipboard or production daemon was used by the fixture.

Reproduction:

```sh
cargo xtask ci
node --test web/tests/*.mjs
python3 xtask/probes/reference-r17/run.py --bin-dir target/debug \
  --browser '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
```

These are protocol/engine/frontend fixture results, not a live-model quality
comparison. R17-7 remains inconclusive and its production default stays unchanged.
Remote platform verdicts and packaged artifact provenance are the GitHub records
linked at the top, rather than predictions embedded in this checkout.


## Remote compiler compatibility correction

The first pushed CI used Rust **1.99.1**, newer than the local **1.97.1** toolchain.
Its Linux/macOS/Windows clippy steps rejected the newly introduced atomic
`fetch_update` because current Rust renamed/deprecated it. MSRV 1.90 and all three
Linux cross-checks passed. Adopting `try_update` would break the supported MSRV.
The MCP revision clock instead uses the equivalent saturating
`compare_exchange_weak` loop, available on both toolchains. A regression verifies
concurrent invalidations do not lose increments and that saturation never wraps
an old snapshot back into currency. Existing unrelated MSRV compatibility
annotations are unchanged.

A separately installed, pinned Rust 1.99.1 passed workspace/all-target clippy
with warnings denied, formatting, MCP library/native HTTP/stdio client tests,
ToolBox catalog/refresh tests and the actual-agent `reference_cycle` suite.
Rust 1.90 passed `cargo check -p rook-mcp` on the changed source. The full local
integration run above is for the implementation commit; this narrow equivalent
atomic correction has targeted checks rather than another unrelated PTY rerun.
The full exact-revision platform gates and publication remain the linked GitHub
records. The local default toolchain is not changed by this verification.


## CI cache completion budget

On final runtime commit `4e9a46a`, Linux, macOS, FreeBSD, MSRV and all cross-checks
passed. Windows also completed `cargo xtask ci` successfully (`ci: ok`, 2581.9s)
and parsed `install.ps1`. Its job reached the 45-minute job deadline afterward,
while the cache action compressed the build cache; no failed test was reported.
The workflow now gives Windows 60 minutes for setup, checks and post-job cache
work, with an explicit unchanged 45-minute ceiling on `cargo xtask ci` itself.
Other platform job limits remain 45 minutes. This correction changes CI metadata,
not runtime/test sources. The subsequent workflow verifies successful completion.

The non-gating `ollama/qwen2.5-coder:3b` smoke on `4e9a46a` passed six scenarios
and failed two: it chose `read_file` instead of the explicitly requested command,
and emitted a pseudo tool call as assistant text instead of recalling a memory.
Both transcripts were read. They show model choices rather than a newly observed
execution failure; no text-to-tool execution or automatic replay was added to
turn them into passes. This is not evidence of a model-quality improvement.
