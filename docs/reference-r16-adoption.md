# October 16 reference adoption

Scope: all eight candidates in the [dated source review](research/reference-review-20261016.md), against its ten advanced reference pins. All eight candidates are implemented. The complete local `cargo xtask ci` gate passes after the integrated workload exposed and helped fix the PTY-reader and secret-lock issues described below. The browser suite passes all 107 tests and the standalone SSH-worker filesystem suite passes all four tests. Live SSH verification remains unavailable. The original review remains a record of the pre-implementation gaps.

| Candidate | Production delivery | Verification |
|---|---|---|
| R16-1 Durable model selection | Shared core KV/CAS receipts, session metadata committed with application, CLI/REPL/TUI and HTTP/browser controls; ordinary, managed and follow-up turn boundaries | Reopen, A→B→retry-A, conflicting IDs/revisions, immutable active provider and changed-route refusal pass; complete gate passes |
| R16-2 Attention inbox | Bounded live-owner index, offline saved-hint scan, parent/child grouping and links, CLI/REPL/TUI and browser navigation | Saved observations do not become live controls; live-index pagination and child ancestry covered; complete gate passes |
| R16-3 Daemon delay diagnostics | Opt-in independent scheduler probe and fixed-name admission, engine-lock and blocking-work observations in existing support reports | Real blocked-executor probe and reached recorder limit pass; complete gate passes |
| R16-4 Share export profiles | Core renders all artifacts; shared omission manifest and text preview; explicit readable archive; local atomic no-overwrite save and browser preview/download | Profiles, ranges, hostile markup, Unicode/body bounds and omitted-event scan admission pass; browser suite passes; complete gate passes |
| R16-5 Per-model compaction | Source-scoped reserve, retained tail and trigger overrides across discovery, routing, overflow recovery and context inspection | Sparse/default policy and shrunken-window checks; complete gate passes |
| R16-6 Recipe equipment | Configured-name requirements, inherited tool/MCP intersections, rebuilt MCP discovery helpers and dispatch enforcement | Missing equipment spends no model request; empty tools, nested intersections and forced unadvertised calls covered; complete gate passes |
| R16-7 Terminal status | Opt-in OSC 7501 on stderr TTY only, root lifecycle and live pending-input states, bounded constant frames | Overlapping waits and secret-free frames covered; real PTY lifecycle/JSON suppression pass; complete gate passes |
| R16-8 Streaming SSH payloads | Negotiated version 2, fixed-size chunks, sender producer/receiver parser, hash/offset/size checks, private disk staging and atomic per-file publication | Binary/empty, bad order/hash/size, incomplete streams, legacy rejection, cancellation and real owned-filesystem failure checks pass; live SSH fixture unavailable |

Task-owned build and test scratch was removed after verification: 10.91 GiB of allocated space reclaimed. Compact verification logs are retained outside the repository.

## Reproducing the native frontend checks

After building `rook-cli` and `rookd`, a standard-library-only fixture exercises
an owned temporary daemon, a local fake HTTP model and real Unix PTYs:

```sh
python3 xtask/probes/reference-r16/run.py --bin-dir target/debug
```

Use `--tmp-root DIRECTORY` when the system temporary volume is full. The fixture
never opens the installed agent's store or changes its service. It holds model
A's response while selecting B, checks A→B→retry-A and a daemon restart, admits
the next root turn on B, compares API/CLI export artifacts, inspects the effective
compaction reserve and diagnostics, and verifies terminal working/done/clear
frames plus their suppression in JSON mode. The recorded runs passed; the busy
selection completed in 4–5 ms on this machine. This is controlled transport and
state verification, not a model-quality or latency benchmark.

Verification also covers two issues found under the integrated workload. The
PTY fixture retains incomplete UTF-8 characters across reads before replaying
redraws; every split of a Unicode frame is checked. Secret writes now explicitly
unlock their file on every exit, including errors. A deterministic duplicate-FD
check covers the fork/exec interval in which merely closing the parent's handle
can leave a shared lock held and make the next save report false contention.
An actually held lock still refuses a competing write.

## Model selection

`rook session model SESSION` reports `revision`, `selected`, `last_effective` and `pending`. `last_effective` describes the last admitted turn boundary, not proof that a model request completed or a turn is currently running. The existing model-request event remains the physical-request observation.

```sh
rook session model SESSION configured-name
rook session model SESSION other-name --request-id pick-42 --revision 1
```

An exact retry reuses both request ID and expected revision. The original immutable receipt says `queued`; reading status shows whether it has subsequently applied. The newest choice wins; replaying an earlier retained request cannot restore that earlier choice. The newest 64 receipts are retained in one 96 KiB bounded companion. An expired retry with its original revision is rejected as stale. No raw URL, credential or secret value is persisted in this receipt.

HTTP clients use `GET /api/sessions/{id}/model-selection` and POST a JSON object with `id`, `expected_revision`, and `model` to the same endpoint. Browser controls retain an unconfirmed request for explicit retry/discard. Ordinary legacy socket settings still work and use a generated durable request when attached to a session; legacy clients cannot supply an exact retry identity. Selection before a session exists remains a connection preference.

A choice binds its primary configured source to a route/credential-source fingerprint. Existing configured fallback priority remains in force; fallback providers are resolved fresh at admission. A changed address or credential source requires an explicit new choice; no old key is carried to a renamed route. Applying the selection edits existing session metadata and its bounded JSON companion in one store transaction, without a postcard layout change. Building an auxiliary model or inspecting status must not count as starting a new root turn.

## Attention inbox

```sh
rook session attention --limit 64
rook session attention --after SESSION_CURSOR
```

`/attention [cursor]` is available in the REPL and both TUI modes. The browser session view has a waiting section with refresh/next-page navigation and parent links. `GET /api/attention` pages the current waiting-owner index, so a recent question is not hidden behind historical sessions. It reads at most 256 session metadata records per request and one extra ID to decide continuation. Parent/child groups are rendered together when both are on the page; a parent link remains available otherwise. Offline views instead scan bounded saved metadata pages: an empty filtered page can still have a continuation.

Live counts come from the current daemon/local TUI channels, excluding closed reply senders. Saved activity observations are hints in offline views; daemon views do not revive them after restart. Opening a session reattaches to its current owner and existing generation/request binding. The inbox itself sends no answers and exposes no secret values or question bodies. Declining, skipping and silence remain supported outcomes, and do not grant permission.

## Daemon timing observations

```toml
[telemetry.runtime_diagnostics]
enabled = true
interval_ms = 1000
slow_ms = 100
max_samples = 8
```

These settings apply at daemon startup. Disabled remains the default. Reports contain a fixed set of operation/metric names, numeric aggregates, at most `max_samples` recent slow observations and omission/coalescing counters. No prompt, command body, secret or per-request metric label is recorded.

An independent owned thread queues at most one tiny executor probe. It can observe scheduler delay even when the runtime cannot poll its own watchdog; repeated ticks coalesce instead of accumulating tasks. Blocking admission queue time and elapsed blocking work are separate observations. Elapsed blocking work may include lock waits; an explicit lock-wait sample does not claim CPU time or establish a causal culprit. Existing model/tool phase timings and question/approval stages remain the evidence for those waits. A crossed threshold never retries or kills work.

CLI `session diagnostics`, `/diagnostics`, the API support report and browser activity inspector expose the same process-local snapshot. A disabled or non-daemon recorder is represented as unavailable, not as zero measured delay.

## Export profiles and preview

```sh
rook session export-html SESSION --profile share --preview
rook session export-html SESSION --profile share-tools --from 2 --through 8 --output review.html
rook session export-html SESSION --profile archive --output readable-archive.html
```

`share` is the default and selects only user/assistant prose. `share-tools` also includes tool calls/results and their saved display details. `archive` explicitly includes readable internal events, but excludes opaque provider-history companions; it is not a raw store backup. Retained prose can itself contain private information: none of these profiles proves an artifact anonymous.

`/export-html [share|share-tools|archive] [FROM..THROUGH] NEW_FILE` works in the REPL and TUI. `/export-preview` accepts the same JSON request as the API, for example `{"profile":"share-tools","from":2,"through":8}`. Its escaped text preview includes at most eight 512-byte excerpts and an omission manifest. No file is written by preview. The browser previews the exact prepared, sandboxed artifact and then downloads it on a separate click; changing the selection retires the prepared download.

`POST /api/sessions/{id}/export` returns the prepared artifact, profile, selected range, omission counts and preview. Core uses a fixed saved prefix, a 512-scanned-event cap (including omitted events), 8192 bytes per rendered body and a 16 MiB output cap enforced while writing. HTML uses literal escaping and a script-free CSP. CLI publication stages beside the destination and never replaces an existing file. Nothing is uploaded or published by exporting.

## Compaction overrides

```toml
[models.small.compaction]
compact_at = 0.6
reserve_output = 2048
keep_recent_tokens = 3000
```

Each optional field inherits current behavior when omitted. The policy follows the configured source, including recipe-selected and implementation-phase models. It feeds proactive compaction, discovered/shrunken-window recovery and context inspection. A dynamically smaller window caps the explicit reserve to half its window and the retained-tail target below the trigger threshold. Complete call/result/provider batches and unseen images keep their existing preservation rules. The separate summarizer still budgets its own full request.

Defaults are unchanged. Additional knobs do not establish translation, model-quality or general latency improvement; local model experiments remain a separate task.

## Recipe equipment

```toml
version = 1
prompt = 'Inspect the requested files.'

[equipment]
tools = ['read_file', 'list_dir', 'search']
mcp = []
require_tools = ['read_file']
require_mcp = []
```

Omission inherits the parent's allowed set; an explicit empty list permits none. Child allowed sets intersect with their parent's, and requirements are checked against that effective set before hooks or a model request. Missing requirements explain the unavailable prerequisite; preflight does not install servers, create credentials or ask repeatedly for a refused secret.

MCP names refer to configured, already connected servers. Scoping rebuilds both direct advertisements and deferred call/resource discovery routes, using the same frontend-owned connection lifetimes. Tool restrictions apply to built-ins and dynamic dispatch, including the actual target behind `mcp_call`; allowing that helper alone does not permit every target. For deferred tools prefer a required server plus explicitly allowed helpers/target names. An external ACP executor is refused when equipment requirements/restrictions cannot be enforced there. This adds no authority beyond the existing policy.

## Terminal status

Set `ROOK_PROGRAM_STATUS=on` to enable OSC 7501 in a supporting terminal. Both stdout and stderr must be TTYs, text mode must be active and `TERM` must be set and must not be `dumb`. It is off by default and emits nothing into JSON or redirected streams. Frames contain only literal application/state/kind fields; even private input uses the generic question kind. Existing visible status and `ROOK_NOTIFY` bell remain available.

Root work, terminal outcome and current pending permission/question/private-input states drive the reporter. Child progress does not turn the root into a completed program or erase another pending question. Normal shutdown clears the status. Abrupt process termination cannot execute cleanup code.

## SSH transfer version 2 and verification limits

The embedded worker and host require streaming version 2. There is no silent unbounded compatibility buffer; an incompatible peer is refused. Binary data is carried outside model messages in base64 chunks of at most 64 KiB encoded (48 KiB raw), each checked against its expected raw offset. File-start metadata admits declared size/mode/hash; file-end and whole-source certificates must match. Frame reads stop at their cap, and per-file/total/file-count/entry limits remain configured in `execution_environment`.

Upload staging is private to the owned remote resource. The receiver writes chunks to bounded temporary sources, verifies the complete source before workspace publication, rechecks each staged file during copy and replaces a file only after its temporary output is complete. Partial/failed input never produces a prepared acknowledgement. Normal cancellation removes owned staging. After a killed receiver, explicit resource release removes bounded, certified orphan staging only after its old process has been confirmed stopped; symlinks and unexpected entries are refused. Existing writeback preimage checks, operation receipts and explicit interrupted recovery remain in force. This is atomic **per file**, not a transaction over the entire workspace: a later failure can retain earlier completed path changes for inspection/recovery.

The host no longer creates a complete upload frame vector or complete dump response vector. Retained before/after snapshots still use their existing bounded base64 representation for review/recovery and therefore consume memory proportional to the configured snapshot limit. This change does not claim constant memory for all SSH execution.

The standalone owned-filesystem probe is reproducible without an SSH service:

```sh
python3 crates/rook-tools/tests/ssh_stream_worker.py
python3 crates/rook-tools/tests/ssh_stream_worker.py --bench
```

It executes the actual production parsing/staging/publication functions, excluding the Linux process supervisor. Checks cover binary and empty files, partial input, old versions, wrong offsets/hashes/sizes, traversal, changed staging, orphan cleanup/symlink refusal and an injected disk-full copy error. Existing destination bytes and temporary-file cleanup are checked on a real scratch filesystem. Rust transport checks also exercise an actual over-limit pipe and a killed/reaped timed-out process.

A local one-run parser comparison at 8 × 8 MiB raw data against v0.15.0 measured 128,629,426 bytes of peak auxiliary Python allocations for the buffered v1 parser and 451,364 bytes for staged v2. Parser elapsed times were 0.278 s and 0.184 s respectively. Prebuilt input buffers, Rust retained snapshots, SSH and kernel buffers are excluded, so these numbers establish a parser allocation reduction, not whole-agent RSS or network throughput improvement.

Full live SSH authentication/process-lifecycle verification was unavailable during this pass: local Docker was stopped and the previously authorized `192.168.1.72:22` was unreachable (initial timeout; a later read-only connection reported no network route). No model service was restarted or modified. The native SSH fixture remains a required environment-specific follow-up before claiming an end-to-end live deployment result.
