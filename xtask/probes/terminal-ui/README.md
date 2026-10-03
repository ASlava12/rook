# Ordinary scrollback mechanism experiment

This finite Rust example uses the same pinned Ratatui/crossterm dependencies as
Rook. It compares `Viewport::Inline(10)` plus `insert_before` with fullscreen
rendering. Completed fixture rows are separate from a mutable tail, draft and
lower queue. An overlay uses the alternate buffer temporarily and returns to
the main buffer. This is a renderer experiment, **not a new Rook mode**: it
does not run models, use an agent store, or claim local/daemon conversation parity.

Build and run its behavioral tests from the repository root:

```powershell
cargo test -p rook-cli --example terminal_scrollback
cargo build -p rook-cli --example terminal_scrollback
```

For each `inline` and `fullscreen` mode, create a fresh evidence root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/terminal-ui/setup.ps1 -Mode inline
$taskRoot=(Get-Content target/scrollback-probe-inline-root.txt).Trim()
target/debug/examples/terminal_scrollback.exe --root $taskRoot --mode inline
```

Run the executable in an actual terminal/PTY. In another shell run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/terminal-ui/scrollback.ps1 -Mode inline
```

Repeat with `fullscreen`. The driver attaches only to the owned executable
whose command line names that root. It types `KEEP_DRAFT`, opens/closes an
overlay, appends a completed row, requests a 40-column console size, captures
actual screen/buffer text and quits. Retain both driver and terminal exit codes.
`proof.json` distinguishes an accepted resize from a rejected request. A queue
row recorded after a rejected resize does not prove narrow-screen behavior.
`run-result.json` reports the actual `Frame::area()`; `CompletedFrame::area`
describes the full terminal and cannot measure an inline viewport.

No mouse capture is enabled. Native host copy/search/scroll remains outside
the program. Win32's 24-row ConPTY backing surface does not expose the host's
native scrollback: absence of an old row in `history` is not evidence that the
host lost it. Preserve bounded PTY traces separately if examining that boundary.
The TestBackend verifies actual insertion into its scrollback and that each
completed row occurs exactly once across scrollback and visible cells.

Admission precedes buffer insertion: terminal size 24..160 columns and 14..60
rows; at most 200 fixture rows, 800 inserted wrapped rows, 2048 draft bytes,
2048 input events and 16 overlays. The run/overlay deadlines are 300/60 seconds.
Console captures have at most 160 columns, 60 visible rows and 1200 backing
rows. Existing result files are refused; no operator configuration is changed.
Helpers use only the named workspace `target` root and record unsupported
console operations rather than silently counting them as successful checks.

The measured adoption decision and remaining adapter requirements are in
[the research note](../../../docs/research/terminal-scrollback-20261003.md).

## Image capability assessment

`cargo test -p rook-cli --example terminal_images` and
`cargo build -p rook-cli --example terminal_images` build a separate finite probe.
Run it with `--root FRESH_OWNED_DIRECTORY --alternate` in an actual terminal.
It queries one dummy pixel without storing or placing an image, captures at most
1024 bytes for three seconds and writes `image-query.json` without overwriting.
No response is inconclusive; DA without a graphics response is negative evidence
for that transport. Success proves the query only, never pixels/crop/deletion.

On Windows with WezTerm installed, use fresh isolated mux configurations:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/terminal-ui/images.ps1 -Capability default -Alternate
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/terminal-ui/images.ps1 -Capability kitty -Alternate
powershell -NoProfile -ExecutionPolicy Bypass -File xtask/probes/terminal-ui/images.ps1 -Capability kitty
```

The driver records the native probe exit independently of the persistent server,
uses an owned short socket/config and stops that exact server. It does not open
a GUI or change an operator configuration. Its Windows console-to-mux result
does not prove that the graphics query reached the mux parser. See
[the image assessment](../../../docs/research/terminal-images-20261003.md) for
observations, retained failures, adoption decision and production requirements.
