# Native browser dialog responses

Source reviewed: Hermes pinned at
`22063fc099ec9ad8357d7f8f6e6248a0400680d3`, especially
`tools/browser_dialog_tool.py` and `tools/browser_supervisor_dialogs.py`.
Hermes offers explicit accept/dismiss responses to native JavaScript dialogs.
Its CDP supervisor, dialog IDs and browser bridge are not transferred to Rook.

Rook's existing optional browser adapter lacked an action for responding to a
native modal, although its pinned `@playwright/mcp@0.0.83` driver already offers
`browser_handle_dialog` with boolean `accept` and optional `promptText`.
The installed pinned package's `playwright-core/lib/coreBundle.js` was inspected
to confirm that contract before implementation. No upstream code is copied.

The new `browser` action `dialog` reuses ordinary dispatch, approvals, MCP result
attribution, call limits and process ownership. A required `context` from local
`status` binds it to the current conversation, browser generation and latest
browser call. A response consumes that context; intervening browser calls also
invalidate it, so a new response requires fresh local status. The
response addresses the driver's current dialog, not a persistent per-dialog ID.
`text` is bounded to 16384 UTF-8 bytes and applies only when accepting. A dialog
response cannot start a browser. Driver capabilities are retained from startup
so an unsupported optional action can fail before dispatch without poisoning
the admitted context. Uncertain effects still require explicit close.

## Verification ledger

- Platform-independent admission tests passed on Windows: strict boolean/text
  input, UTF-8 byte bounds, unknown fields, closed contexts, remote provenance
  and external action policy.
- The ordinary-agent native Chromium journey passed on Windows with the pinned
  driver. It covers alert acceptance, both confirmation choices and prompt
  answer, dismissal and explicit empty text. The first run exposed premature
  tool-cycle refusals for repeated snapshots/status: workspace bytes do not
  prove browser state unchanged. Those observations now use their actual
  answers to detect cycles; they cannot erase mutation history. An ordinary
  engine regression confirms that 20 identical observations still stop and
  repeated `click`/`dialog` mutations retain the two-result refusal boundary.
- Unix protocol fixtures cover stale/foreign context refusal, exact backend
  arguments, capability absence and cancellation without replay. Their checks
  passed in the Linux, macOS and FreeBSD gates on `a805dec`; final-revision
  platform checks remain pending.
- Windows browser dispatch/denial and observation-cycle regressions passed.
  Formatting and `cargo clippy -p rook-core --all-targets -- -D warnings` passed.
  All 10 selected ordinary-agent tool-cycle regressions and the previous native
  form/screenshot journey also passed. Integrated platform checks remain pending
  for the final feature revision. This work is isolated from the 0.20.0 release
  source.

An expanded native journey then exposed a second premature refusal: a third
identical confirmation response in one context looked like replay even after a
new dialog opened. Binding response contexts to the current browser call
distinguishes the new operation and rejects consumed contexts before dispatch.
The expanded seven-dialog journey passed on Windows together with all four
browser integration tests, including the previous form/screenshot journey.
Both admission tests and all-target core clippy passed again after this change.
The additional Unix protocol assertions for consumed and superseded response
contexts remain pending in platform CI.

The bundled browser QA guidance is updated to describe offered dialog actions
and the existing console/network/viewport controls accurately. A resized
viewport alone is not represented as mobile device emulation.

The first integrated feature revision (`a805dec`) passed seven required platform
jobs but failed the Windows command-result churn regression. That log did not
retain individual command results, so its precise variance is unproven. The
regression now includes bounded results on failure without changing its
20-result threshold.

A separate Windows reproduction established that shell undo receipt IDs made
five identical commands with identical output appear to make progress. Complete,
untruncated foreground command results now carry the UTF-8 byte boundary of
their exit status and captured output. Cycle comparison uses that prefix before
presentation diagnostics and appended undo receipts. Missing, invalid or partial
boundaries fall back to comparing the whole result. Diagnostics remain visible,
and cleanup, timeout and approval behavior is unchanged. The new engine
regression failed before this correction and passed afterward with all 11
selected tool-cycle engine regressions on Windows. Full-suite and final platform
validation remain pending for the corrected revision.

Configured execution can redact secrets after receiving a command result. Its
boundary is adjusted for that replacement, including UTF-8 and shorter/longer
masked values; a replacement crossing the boundary invalidates it. A failed
environment completion invalidates the boundary so the full error remains part
of cycle evidence. The focused boundary/redaction regression passed on Windows.
The six native command integration tests also passed.

On `16faf55`, the complete Windows ordinary-agent integration target passed all
280 tests in 400.58 seconds, and workspace/all-target clippy passed. A separate
diagnostic run of the long claim-verification test passed in 3.71 seconds; the
full target's passing result is the evidence for that suite. A requested native
Chromium cleanup test selected zero tests on Windows because its module is
Unix-only; this is not counted as successful native cleanup verification.

A subsequent regression exposed the same false-progress mechanism for truncated
output: each command also receives a fresh spill filename. When the complete
capture is available, cycle comparison now hashes its actual bytes and exit
status instead of its filename or appended receipt. Reading requires a regular
file in the engine's private output directory and is bounded to 8 MiB (or the
smaller configured spill cap). Incomplete, missing, oversized or foreign
captures retain the existing fallback rather than asserting full-output
equality. The hash is private cycle evidence, never tool metadata. The new
native regression failed before this fix and passed afterward with all 12
selected tool-cycle regressions. A focused fixture checks that changed hidden
middle bytes count as different results, while new filenames alone do not;
it passed, as did the renewed workspace/all-target clippy check.

On `9b07098`, the complete local workspace test preflight passed, including all
281 ordinary-agent tests. All eight required platform jobs passed in
[run 39600126761](https://github.com/ASlava12/rook/actions/runs/39600126761).
The optional model smoke remains separate evidence. Later scheduler guidance
and retry-observation changes are described with their own validation scope in
[managed retry guidance](../managed-retry-guidance.md); final candidate platform
and artifact checks are still required for the combined release.
