# Native extension form and live report probes

These Windows probes run the actual CLI/TUI, daemon, browser modules and hook
process against a controlled HTTP model fixture. They verify interaction and
data flow, not real-model quality, cost or speed. No operator home/store is used.
`hook.ps1` and its POSIX counterpart `hook.sh` produce the same bounded form.
The core/native integration tests also exercise the protocol on both platforms.

From the repository root, first build `cargo build -p rook-cli -p rookd`.
Run `setup.ps1 -Mode local`, `shared` or `browser` with PowerShell; it creates a
fresh owned root in `target/` and a `target/extension-live-MODE-root.txt` pointer.
It refuses to replace a pointer whose fixture processes are still live.
Daemon/browser setup also launches the owned daemon and isolated headless Edge
profile. All background processes are hidden and their PIDs are retained.

For each TUI mode, open the actual CLI in a terminal/PTY:

```powershell
$taskMode = 'local' # or 'shared'
$taskRoot = (Get-Content "target/extension-live-$taskMode-root.txt").Trim()
$env:ROOK_HOME = Join-Path $taskRoot 'home'
$env:ROOK_LOG = 'error'
# Local: append --alone. Shared: omit --alone.
target/debug/rook.exe --workspace (Join-Path $taskRoot 'workspace') tui --alone
```

While that terminal is live, use another shell to run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/extension-ui/tui.ps1 -Mode local
```

Use `-Mode shared` for the daemon TUI. The driver attaches only to the single
owned CLI console, captures bounded actual screen text, answers text/select/
confirm/integer fields and opens Context. It checks source/title/field visibility,
typed producer values, persistent waiting/source panels, live progress/results,
source-owned clear, saved status and absence of the private fixture value in
model requests. It exits the TUI before reporting success. Retain the terminal's
actual exit status as well as the driver's exit status. Do not rebuild the CLI
while that executable is running on Windows.

For the browser, run `node xtask/probes/extension-ui/browser.mjs`. It uses actual
CDP mouse/text events, records screenshots, disconnects/reconnects the actual
chat socket and checks draft retention, typed submission, saved Context and
Stop while a second form is pending. It verifies waiting widgets before answering,
their recovery after reconnect/navigation, progress/results/clear after completion
and the saved interruption reaching an idle chat panel. A successful second prompt also proves the
recovered admission cleared its uncertain delivery retry. Failure captures are
retained; absence of `browser-proof.json` is not a pass.

Each root retains model request captures, producer `workspace/hook-answer.json`,
PID/log files, actual console or PNG/text captures and a MODE-proof.json. The
admitted request files are inspected before copying; fake private values are
never operator data. Capture logs with `*> target/NAME.log` and check
`$LASTEXITCODE`, including when PowerShell formats native stderr as an error.

Finally run `cleanup.ps1 -Mode MODE` for each started mode. It closes only the
isolated browser through its own CDP endpoint, verifies PID command lines still
contain the exact owned root before stopping daemon/model processes and refuses
success if any owned process remains. It retains all artifacts. Quit the TUI
first; cleanup deliberately does not kill an unrelated user window or daemon.
The console and browser-close helpers are shared with the phase-routing probes;
their original default pointers continue to work.
