# R17 adoption — 2026-10-18

Scope: all eight accepted items in the [R17 review](research/reference-review-20261016-r17.md), using its pinned references and the v0.16.0 baseline. The reviewed snapshots remain recorded in the source review; the active October 18 cycle has since advanced the gitlinks again. The implementation evidence below precedes the final release; see the [October 18 cycle](reference-cycle-20261018.md) for integrated audit and publication status.

| ID | Status | Delivery |
|---|---|---|
| R17-1 | Implemented; verified | Model and optional effort commit in one versioned JSON receipt. Root admission freezes the pair; a legacy model-only pick preserves the selected effort. Standalone effort changes use the same revision ordering. CLI, REPL, TUI, browser presets and HTTP share the operation. |
| R17-2 | Implemented; verified | A shared admission fence drains existing turns, managed owners, jobs, workspace writers and admitted mutations. Pending answers, private input and Stop remain reachable. Exact request IDs support retry/cancel; timeout never forces restart. A separate CLI controller performs replacement after quiescence and saves its bounded handoff result. |
| R17-3 | Implemented; verified | Optional compaction phase ceiling includes provider admission/retries and is capped by the remaining turn deadline. Dropping the turn cancels its summary future. Deadline-expired results cannot commit; the attributed fallback retains saved-source references. Ordinary streamed replies keep their silence-based watchdog. |
| R17-4 | Implemented; verified | Main, compaction and errand providers resolve independent ordered backup lists. Omitted purpose inherits legacy global priorities; `[]` allows only its primary. A restricted compaction route cannot escape into the main chain after a construction error. Existing route-local credentials, windows and accounting remain in force. |
| R17-5 | Implemented; verified | Per-task schemas are admitted before child work. Delivered JSON is redacted and then validated. Bounded typed receipts retain status, validation, omissions and saved child/event references; interruption gets an unavailable result. Invalid schema output never reruns child tools. Parent receipts survive removal of a child and report missing/pruned sources. |
| R17-6 | Implemented; verified | Shared branch-aware saved-answer selection feeds CLI/API, explicit REPL/TUI `/copy` and browser actions. Internal compaction summaries require `--summary` or their separate button. UTF-8 excerpts are bounded; clipboard failure offers readable/selectable saved text. |
| R17-7 | Experiment pending: no completed comparison | An explicit experimental summary-output override and reproducible A/B harness are available. The default remains 2048. The configured API returned HTTP 429 (quota exhausted). The separately checked Qwen server was reachable, but its 2048-token baseline failed to finish within 90 seconds and then a diagnostic 480-second ceiling while another large request was active. No successful paired quality/token measurements were obtained; no improvement claim is made. |
| R17-8 | Implemented; verified | Per-source declarations enable only implemented dialect controls and bounded levels/budgets. Declarations decorate fresh route-local observations, expose configured provenance, and invalidate previous metadata scope without invalidating the saved model choice when only capabilities change. Explicit disable and refusal remove effort parameters. |

## Usage

Commit a model/effort pair for the next turn:

```sh
rook session model SESSION MODEL --effort high
rook session model SESSION
```

For an exact retry, supply the same `--request-id ID --revision N`, model and effort. Reusing an ID with different effort fails. Retried older selections return their original receipt without replacing a newer choice. Saved JSON v1 choices remain readable; stored postcard records are unchanged. `/model NAME --effort high` and presets use the same operation. A selected pair is distinct from the last admitted pair, and from the actual answering fallback/effort mapping reported for a request.

Drain the daemon after installing a build, or inspect/cancel a plan:

```sh
rook daemon drain --timeout-secs 300 --restart
rook daemon drain --status
rook daemon drain --cancel PLAN_ID
```

Without `--restart`, drain only waits and closes admissions. `--id PLAN_ID` makes Start retryable without resetting its deadline. `/daemon status|drain [seconds]|restart [seconds]|cancel ID` works in REPL/TUI; browser controls are under **Drain before updating daemon**. HTTP uses GET/POST `/api/daemon/drain`. A timeout or failed handoff leaves admission closed until an explicit cancel; it never silently aborts work. Answer or decline an outstanding question, or explicitly stop its work, to let it finish. Jobs left running also keep the drain open. Pending work that had not started stays pending. Ordinary `daemon restart` retains its earlier explicit immediate-restart behavior.

A restart controller runs the sibling installed `rook`/`rookd`. `run/drain-restart.json` records `restarting`, `restarted` or `failed`; a controller failure can leave no daemon if the old process has already stopped. Inspect that receipt and daemon logs, then start the daemon after correcting the error. There is no implicit forced shutdown on a drain timeout.

Purpose-specific routes and phase ceilings:

```toml
[agent]
compaction_timeout_secs = 60
# Experimental; omit to retain 2048 until an A/B result justifies a change:
# compaction_output_tokens = 4096

[agent.fallbacks]
main = ["backup"]
compaction = []
errand = ["local", "backup"]
```

Each list contains at most 16 unique configured source names. The selected purpose's primary precedes its backups. Unavailable optional credentials do not prompt for secrets or authorize a different route. A user may decline or ignore any pending question or private credential request; absence is never consent.

For an unfamiliar configured model alias:

```toml
[models.local.reasoning]
control = "openai-effort"
effort_levels = ["low", "medium", "high"]
```

Other implemented controls are `responses-effort`, `anthropic-effort`, `google-level`, `google-budget`, each restricted to its matching API. `supported = false` disables reasoning parameters and cannot be combined with enabling controls. An empty `effort_levels` suppresses effort; omitted levels inherit observations. Google budget controls accept `budget_min`/`budget_max` (1..131072, defaults 512/32768) and leave 128 tokens of visible output room. A request too small for the declared minimum fails before generation. Accepted request status reports the actual clamped budget, including the configured-source metadata wrapper and retry output-limit changes; an HTTP regression compares that status with the captured Google request. Doctor, model metadata and generic config surfaces show the declaration/provenance. `config set` accepts TOML arrays for these lists; the editor exposes optional nested reasoning fields and distinguishes unset/inherited lists from explicitly empty lists. Capability-only edits refresh observations without requiring the saved model choice to be selected again; physical route/credential changes retain the existing source-binding check. A declaration does not guarantee endpoint support: the existing named-parameter rejection retry still applies.

Delegate with a common schema or individual task contracts:

```json
{"tasks":[{"task":"Inspect src/main.rs and return findings","output_schema":{"type":"object","required":["findings"],"properties":{"findings":{"type":"array","items":{"type":"string"}}},"additionalProperties":false}}]}
```

`output_schema` at the top level is the default; a task's own schema takes precedence. Each schema is limited to 8 KiB, depth 16 and 1024 nodes; external schema resolution remains disabled. A result is validated up to 64 KiB/depth 32 after redaction, projected inline up to 8 KiB, and stored in a receipt up to 16 KiB. Larger answers retain their saved-source reference and explicitly unavailable validation. Schema repair retries are disabled for these children. `wait:false` uses the same contract. Read recent typed receipts with `rook session results SESSION`, `/results`, the browser **Delegated results** action, or GET `/api/sessions/SESSION/delegated-results`. The reader examines the latest 512 events and returns at most 64 receipts; older receipts remain in history.

Read without changing the clipboard:

```sh
rook session last-answer SESSION
rook session last-answer SESSION --summary
```

`/copy [--summary]` invokes the frontend clipboard helper explicitly. Browser buttons make the same saved-source selection. At most 512 event headers and 64 KiB of selected text are read; larger answers return a marked UTF-8 excerpt. Nothing is copied from live drafts, reasoning events, tool payloads or other branches.

## Verification

Targeted checks passed for durable pair/retry migration, branch/UTF-8 answer selection, explicit-empty chains, phase timeout/cancellation and configured/default summary output budgets. Real HTTP fixtures exercise all five declared controls across the four APIs, unknown aliases, explicit disable, refusal retry, and distinct main/summary/errand fallbacks, including inherited primary selection. Typed config setters and editor lists cover None versus explicit empty fallback semantics. Delegation checks exercise schema success/failure without child tool replay and missing saved sources. The final `cargo xtask ci` run completed formatting, clippy, CLI/daemon builds and the full workspace test run. Its sole failure was an existing PDF cancellation fixture reading an empty PID file between creation and writing; the fixture now waits for a complete PID line. The complete `rook-tools --lib` suite passed after that test-only correction. Formatting and workspace/all-target clippy also passed after the correction. The full gate is therefore not recorded as a green rerun. Native Chrome/PTY checks and JavaScript/Python syntax checks passed locally on macOS with owned provider fixtures; live-model quality is the separate pending experiment below.

The full validation also exposed a pre-existing race in the scheduler-probe test: its live producer could legitimately enqueue a new probe between snapshot and assertion. The regression now stops the producer before waiting for the final queued probe to drain; shipping sampler behavior is unchanged.

The existing network-proxy retirement test also observed a child connection closing before its owner finished dropping the listener on another runtime worker. It now waits for the owner task's completion before checking the closed port; shipping network behavior is unchanged.

The owned native probe is reproducible:

```sh
cargo build -p rook-cli -p rookd
python3 xtask/probes/reference-r17/run.py --bin-dir target/debug
# Optional actual browser controls:
python3 xtask/probes/reference-r17/run.py --bin-dir target/debug \
  --browser '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
```

It uses temporary state, a held HTTP model fixture and sibling built binaries. It checks busy paired selection (4–7 ms observed here; not a performance guarantee), A→B→retry-A, unchanged active effort, reopen/restart, CLI/API answer parity, cancellation and an actual drain restart. Chrome checks one paired preset mutation, selected pair after reload, late-acknowledgement ownership across session changes, explicit saved-answer copy, owned clipboard transport and readable clipboard fallback, and drain/cancel. Native PTY regression checks use fake clipboard utilities and never modify the operator's clipboard.

## Summarizer experiment

The harness `crates/rook-core/examples/compaction-ab.rs` compares 2048 against `clamp(summarizer_window / 16, 2048, 8192)`, in alternating A/B and B/A order. It accepts an explicitly prepared JSON array of `{name, entries:[{kind,label,body}], checks:["required fact"]}` exported from real saved conversations (maximum 4 MiB, 8 fixtures, 512 entries and 64 checks each). Source text is redacted through the configured vault. Run under an isolated `ROOK_HOME`/`ROOK_CONFIG_DIR` with only the selected model's configuration and explicitly supplied credentials; do not use the working daemon/store as a fixture. The harness does not run agent tools. Its phase ceiling defaults to 90 seconds and honors an explicit `agent.compaction_timeout_secs`, bounded to 1..600 seconds for this experiment. It stops after a failed baseline/candidate rather than comparing fallback source markers as summaries.

```sh
ROOK_HOME=/path/to/owned/home ROOK_CONFIG_DIR=/path/to/owned/home \
  cargo run -p rook-core --example compaction-ab -- /path/to/real-transcripts.json \
  > /path/to/owned/results.jsonl
```

On 2026-10-18 two real saved conversations were exported with bounded bodies (85 and 64 entries). The cloud route exhausted its quota and configured local fallbacks timed out. A read-only check of the previously authorized Qwen host found a healthy authenticated API and a 262144-token slot context. An owned SSH tunnel then carried the experiment without exposing the API credential on the LAN or changing server configuration.

The first Qwen baseline did not finish within 90.017 seconds. Existing server timing lines showed roughly 24 generated tokens/s and roughly 90 prompt tokens/s, so a 90-second experimental ceiling could exclude a legitimate response after prefill. A diagnostic retry used an explicitly configured 480-second ceiling; it also failed to finish (480.030 seconds). Slot diagnostics showed another active request growing to roughly 97000 prompt tokens. That is evidence of shared load, not a proven cause of this request's delay. No complete summary or final usage receipt was obtained, and remaining comparison arms were not sent. Temporary transcripts, credential copies and the tunnel were removed.

This is an inconclusive experiment, not evidence that the larger output budget helps or hurts. Keep it pending and the default unchanged. A later evaluation must record input/output usage, latency, truncation, goal/decision/open-work/refusal preservation and human review of semantic losses. Literal check-term presence is only a screening aid, not a quality score. Raw transcripts, credentials, summaries and build caches are temporary local evidence and must not be committed.
