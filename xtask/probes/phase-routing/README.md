# Live phase-routing probes on Windows

These probes drive the shipped executables and browser modules against a
controlled loopback HTTP provider. They verify UI attribution and saved-prefix
behavior; the usage and delay are imposed by the fixture. They do **not** measure
real model quality, savings or latency.

Run from the repository root, using Windows PowerShell, Node 24 or later and
Microsoft Edge at its standard installation path. Build the actual binaries first:

```powershell
cargo build -p rook-cli -p rookd
```

Check every command's exit status. A failed probe retains its scratch workspace,
store, raw requests, screen text and screenshots under `target/phase-live-*`.
Each setup uses its own `ROOK_HOME`, random loopback ports and browser profile;
it refuses to replace the pointer to a still-running fixture.

## Browser

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/setup.ps1 -Mode browser
node xtask/probes/phase-routing/browser.mjs
```

The browser probe uses actual mouse clicks, text insertion and page reload via
Edge CDP. It observes application state for synchronization, without replacing
the frontend or injecting model events. Text and PNG captures retain the pending,
completed and reloaded Context panels. It checks selected, dispatched and reported
identity, the effective next-request window, pending physical attempts, saved
costs and the warning that receipt and attempt estimates overlap.

## Local and shared TUI

For each `$taskMode` (`local`, then `shared`), run setup:

```powershell
$taskMode = 'local'
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/setup.ps1 -Mode $taskMode
$taskRoot = (Get-Content "target/phase-live-$taskMode-root.txt").Trim()
```

In a second terminal at the repository root, set the same mode and root and start
the actual TUI. Keep that terminal open until the driver quits it:

```powershell
$taskMode = 'local'
$taskRoot = (Get-Content "target/phase-live-$taskMode-root.txt").Trim()
$env:ROOK_HOME = Join-Path $taskRoot 'home'
$env:ROOK_LOG = 'error'
if ($taskMode -eq 'local') {
  target/debug/rook.exe --workspace (Join-Path $taskRoot 'workspace') tui --alone
} else {
  target/debug/rook.exe --workspace (Join-Path $taskRoot 'workspace') tui
}
```

When the chat pane is visible, run the driver in the first terminal:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/tui.ps1 -Mode $taskMode
```

The console helper attaches only to the uniquely identified fixture TUI. It opens
`CONIN$` explicitly so redirected stdin remains supported, writes real keyboard
events and captures the active screen rather than matching raw escape sequences.
Its input is capped at 256 UTF-16 code units and capture at 160 columns by 60 rows.
ASCII regex patterns avoid Windows PowerShell's interpretation of UTF-8 source
without a BOM. All waits have deadlines.

After the TUI exits successfully, prepare branches before the successful write
result and after the completed turn:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/recovery.ps1 -Mode $taskMode -Prepare
```

Start the TUI again in the second terminal with the same launch command. Once
the chat pane is visible, run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/recovery.ps1 -Mode $taskMode
```

This driver uses `/session` and `/context` to inspect both saved-prefix forks and
the reopened parent. Before the transition, the next window is 65536 and the
saved analysis subtotal is USD 0.00003800. After it, the window is 32768 and the
subtotal USD 0.00007000 includes the completion classifier. Branch selection
must cause no fourth model POST and must not rewind or modify workspace files.

## Evidence and cleanup

After all three modes and both native recovery probes have passed:

```powershell
node xtask/probes/phase-routing/verify.mjs
```

This checks retained artifacts with byte admission limits and writes a proof
index to `target/pi-phase-live-proof.json`. It cannot replace checking the actual
probe and TUI exit statuses. The model admits at most 4 MiB per request before
copying it and at most 48 requests; the second stream waits at most 120 seconds
for the driver to release terminal usage.

Always stop each owned fixture before building again, including after failures:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/cleanup.ps1 -Mode local
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/cleanup.ps1 -Mode shared
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/phase-routing/cleanup.ps1 -Mode browser
```

Close a still-running fixture TUI first. Cleanup closes only the isolated Edge
instance through its own CDP endpoint, validates each daemon/model PID's current
command line against the recorded root before stopping it and verifies that no
process for that root remains. Artifacts are retained for diagnosis; no installed
user daemon or store is targeted. These interactive Windows probes are separate
from the portable `cargo xtask ci` gate.

For independent real-model quality/cost/latency work, use the separate
[comparison runner](BENCH.md). The frontend fixture's prescribed usage and delay
must never be used as evidence for that comparison.
