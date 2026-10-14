# Browser control verification — 2026-10-11

The optional adapter uses pinned `@playwright/mcp@0.0.83` and its Chromium
1247 build. The [product guide](../browser.md) describes installation and
ordinary agent usage. Operator configuration and the installed daemon were
not changed; every run used scratch state and a local fixture application.

## Actual Chromium checks

The ordinary agent-loop fixture navigates, reads a snapshot, fills Message,
clicks Save, waits for confirmation, reads the resulting snapshot, captures
a PNG and closes. It verifies exactly one POST, the stored screenshot's
dimensions, complete paged call history, completed execution receipts and
an untouched workspace. Both delivery modes passed:

- Default text-only delivery: `/tmp/rook-browser-chromium-text-final.log`.
- Explicit image delivery: `/tmp/rook-browser-chromium-vision-final.log`.

The ignored lifetime check identifies Chromium from its exact MCP driver's
ancestry before cancellation or drop, then waits for that browser to end.
Cancelled page calls, cancelled close and engine drop passed in
`/tmp/rook-browser-chromium-cleanup-final.log`. The protocol fixture separately
checks cancellation uncertainty, context isolation, capacity, stale/foreign
refs, input/rendering bounds and descendant cleanup. The MCP client checks
process admission before spawn and capacity release on shutdown; sixteen
checks passed in `/tmp/rook-browser-mcp-lifecycle.log`.

Failures are retained. Early runs lacked the browser revision required by the
pinned package; installing that exact revision resolved startup. Subsequent
fixture failures assumed every action returned an inline snapshot and inspected
only the last transcript page. The corrected fixture explicitly reads snapshots
and traverses bounded pages. Native delegated calls also found an actual
parent-tool binding bug, fixed by replacing inherited browser registrations.
Abrupt MCP termination left a detached Chromium; graceful termination with
reserved bounded cleanup capacity fixed the owned-process checks. Context
generations now remain unique after engine restart, and cancelled close cannot
revive old refs.

## Native LAN model observations

`xtask/probes/browser/run.py` runs one native CLI task against a real local
application. Each run has independent scratch state and random submission and
confirmation values. It retains native stdout/stderr, binary fingerprint,
actual submissions, outcome and workspace contents. The fixed bounds are
twelve steps, 1,536 output tokens per request, 64,000 turn tokens including
auxiliary/checker work, and a 300-second native observation deadline. These are
functional observations, not paired performance comparisons.

All three runs used `huihui-ornith-1.5-35b-a3b-abliterated-i1` on
192.168.1.46. No run is reported as a successful complete native turn:

| Run directory under `~/.cache/rook-browser-agent-check/` | Native result | Observation |
|---|---|---|
| `20261011-native` | exit 1, 67.499 s | Exactly one correct submission; the next model request rejected screenshot pixels because the model is text-only. |
| `20261011-text-delivery` | exit 2, 133.670 s | Correct submission and confirmation, screenshot and close; automatic checking searched the workspace for a private temporary artifact and exhausted the total token budget. |
| `20261011-receipt-guidance` | exit 2, 160.205 s | Correct submission and confirmation, screenshot and close; checking recognized the parent receipt but still stopped at the fixed total token budget without a verdict. |

The image failure led to an explicit `browser.include_images=false` default.
Pixels stay in the result store; live requests, replay and compaction use the
same setting and preserve source envelopes. User attachments, ordinary MCP
images and explicit image retrieval retain their delivery behavior. The
separate default/vision Chromium checks and stored-pixel policy check passed.

The artifact-location mistake led to scoped checker guidance with a concrete
parent session/event anchor. A recorded PNG establishes a capture, not its
visual content; page facts and current-state requirements remain distinct.
This does not waive independent checking. Budget stops and missing verdicts
remain failures. The fixed budget was not increased, nor were identical failed
model attempts retried to obtain a pass. The second run's parent and checker
transcripts are retained beside its report.

## Regression gate

The final full gate `/tmp/rook-browser-ci-process-fixed.log` exited 0 in
841.4 seconds: fmt 1.1, all-target Clippy 11.6, frontend builds 9.0 and
complete workspace tests 819.7 seconds. Its predecessor
`/tmp/rook-browser-final-ci.log` exited 1 only on the process-launch layering
check: three test-only `ps` commands did not use the shared launch helper.
Those calls now use `rook_contain::quietly`; targeted layering checks and
the complete replacement gate passed. This does not convert incomplete native
LAN browser turns into successes.
Earlier gates were explicitly stopped when native checks identified required
product changes. Their partial test output is not a passing gate:
`/tmp/rook-browser-ci.log` and `/tmp/rook-browser-text-ci.log`.
All-target Clippy passed after the receipt guidance change in
`/tmp/rook-browser-receipt-guidance-clippy.log`. Seventy-two frontend checks
passed in `/tmp/rook-browser-web-tests.log`.
