# Ordinary terminal scrollback experiment

Scope: the separate terminal experiment in the pinned
[Pi review](pi-reference-review-20260930.md#7-более-поздние-эксперименты).
Pi distinguishes main-screen document updates from alternate-screen viewport
updates; changing its rendering mode affects scrolling, selection and overlays.
Rook already has bounded transcript retention, structured tool cards, a growing
editor, blocking controls, lower queued input and source-owned live reports.

The [finite Rust prototype](../../crates/rook-cli/examples/terminal_scrollback.rs)
uses the versions in [Cargo.lock](../../Cargo.lock): Ratatui 0.30.2,
ratatui-core 0.1.2 and crossterm 0.29.0. It inserts completed fixture rows above a
10-row inline viewport and compares the same footer with fullscreen layout.
It has no model, session, workspace mutation or new product runtime. The
[permanent driver](../../xtask/probes/terminal-ui/README.md) describes reproduction
and bounds.

## Observations

| Check | Observation | Evidence scope |
|---|---|---|
| Completed output | 80 rows actually reach TestBackend scrollback; each row occurs exactly once across saved and visible cells at widths 40/80/120 | Pinned terminal backend; not an operator-host copy/search test |
| Lower queue before resize | Queue content is at row 22 of a 24-row screen | Actual Windows native inline/fullscreen screens and TestBackend |
| Editing and append | `KEEP_DRAFT` retained; one further completed row added | Actual native input and final result: 81 rows |
| Overlay | Alternate-buffer overlay opens and returns without losing the draft or latest main-buffer row | Actual native inline and fullscreen screens |
| Inline shrink 80→40 | Accepted native resize moves the inline viewport to y=0; queue content moves from row 22 to row 8 | Actual Windows native run and independent TestBackend shrink |
| Fullscreen shrink | Narrow reflow keeps the latest row, draft and queue at row 22 in TestBackend | Native Win32 alternate-buffer resize was rejected (`The parameter is incorrect`); the native run remains width 80 |
| Host history | Win32 reports only a 24-row backing surface; old host scrollback is inaccessible through that API | Actual ConPTY buffer observation; not a claim that native host history was lost |

Native inline evidence is in
`target/scrollback-probe-inline-2ad1367155bf4ee1a41e21977396fb92`, with driver
exit 0 (`target/pi-scrollback-inline-corrected-proof.log`) and terminal exit 0.
Fullscreen evidence is in
`target/scrollback-probe-fullscreen-987b7b2149e14b2dbbed5884bf5f1186`, with driver
exit 0 (`target/pi-scrollback-fullscreen-proof.log`) and terminal exit 0.
`initial/draft/overlay/restored/appended/resized.json`, `run-result.json` and
bounded `tty-initial/tty-ending.json` retain the observations. Each root contains
only synthetic fixture data; both processes are stopped.

The first focused test exited 101 because its instrumentation used
`CompletedFrame::area`, which reports terminal bounds. It was corrected to
capture `Frame::area()` inside the drawing closure; the resulting tests expose
the shrink issue rather than counting a full-terminal rectangle as an inline
viewport. The first native helper failed C# compilation (ambiguous Math.Min);
the next driver timed out opening an overlay. The helper now uses an explicit
integer conversion and layout-independent synthetic letters with Ctrl modifiers.
Those failures are retained in `target/pi-scrollback-example-tests.log`,
`target/pi-scrollback-inline-proof.log` and
`target/pi-scrollback-inline-final-proof.log`.

## Adoption decision

The four focused example tests exited 0 (`target/pi-scrollback-four-tests.log`).
Mandatory `cargo xtask ci` exited 0 in 495.5 seconds, including fmt, Clippy,
build, workspace tests and doctests (`target/pi-scrollback-ci.log`). No production
storage changes were made, so this block does not require a new compaction run.

Retain the current fullscreen Rook interface. A direct replacement with
`Viewport::Inline` fails the required lower-queue position on an accepted width
shrink. The experiment demonstrates a viable bounded insertion mechanism and
overlay-buffer restoration; it does not provide a production conversation
adapter. No `rook tui` option or default was changed.

Any future production adapter must address:

- Explicit re-anchoring/reflow after shrink without duplicating committed rows.
- Which transcript rows are immutable: streaming Markdown, in-flight tool cards
  and later receipt mutations cannot all be appended as final text immediately.
- Saved-prefix and branch/reconnect replacement while old text remains in the
  terminal's own history; attribution must make the selected conversation clear.
- Current questions, approvals, queue, widgets and editor resizing together.
- Host mouse selection/copy/search in a real terminal UI, plus local/daemon
  interaction parity; Win32 backing-buffer reads do not prove those behaviors.

This closes the simple inline-renderer assessment with a measured rejection of
direct adoption. It does not claim that production scrollback mode was shipped.
The terminal image experiment and completed real-model phase comparison remain
separate work in the [adoption tracker](pi-adoption-20260930.md).
