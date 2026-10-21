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
`status` binds it to the current conversation and browser generation. The
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
  are pending on a Unix runner.
- Windows browser dispatch/denial and observation-cycle regressions passed.
  Formatting and `cargo clippy -p rook-core --all-targets -- -D warnings` passed.
  All 10 selected ordinary-agent tool-cycle regressions and the previous native
  form/screenshot journey also passed. Integrated platform checks remain pending
  for the final feature revision. This work is isolated from the 0.20.0 release
  source.

The bundled browser QA guidance is updated to describe offered dialog actions
and the existing console/network/viewport controls accurately. A resized
viewport alone is not represented as mobile device emulation.
