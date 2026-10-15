---
name: tui-development
description: Use when implementing interactive terminal interfaces, input handling or terminal rendering.
version: 1.0.0
license: MIT
---

# Develop terminal user interfaces

Establish the terminal platforms, user journey and invocation modes. Inspect the
existing terminal backend, widgets, event loop and key bindings. Extend the current
toolkit; for a new application choose one suited to the language and deployment.
Ratatui/Crossterm, Textual and Bubble Tea are possible stacks, not dependencies to
install merely because this skill loaded. Check the installed version's lifecycle
API and capability support before using an example from another version.

Separate application state, input-to-action mapping, updates and rendering. Keep
focus, selection, scroll position, draft input, modal state and pending operations
explicit. Rendering should consume state without starting network calls or mutating
business data. Run slow work outside the drawing/input path and return attributable
results through bounded queues. Ignore stale completions after a view/task changed;
keep cancellation and worker shutdown visible.

Own terminal setup and restoration in one lifecycle boundary. Restore raw mode,
alternate screen, cursor, mouse capture and bracketed-paste modes on normal exit,
initialization failure and recoverable errors/panics. Use the toolkit's restoration
support rather than stacking conflicting panic hooks. Handle suspend/resume where
supported. `SIGKILL` cannot run cleanup. Emit diagnostics after restoration or to a
separate log; arbitrary stdout writes during drawing corrupt the display. Preserve
machine-readable stdout in commands that also offer interactive mode.

Route events through the focused widget and modal layer before global shortcuts.
Keep displayed hints/help and remapped bindings synchronized with the same action
definitions. Distinguish key press/repeat/release when the backend supplies them;
terminal protocols do not guarantee every modifier combination is distinguishable.
Provide alternatives for unsupported chords. Treat a paste as pasted text, not a
sequence of command/submit keys. Allow selection/copy or a discoverable way to hand
mouse handling back to the terminal.

Measure layout in terminal cells, with grapheme-aware editing and display widths.
UTF-8 byte length and Unicode scalar count do not equal rendered width. Cover
combining marks, CJK, emoji, wide glyphs and long unbroken lines using the toolkit's
text primitives. Clip within bounds and maintain scroll/selection validity when
content or terminal size changes. Recompute on resize, including tiny and zero-size
regions. Avoid arithmetic underflow and selecting an element from an empty list.

Render untrusted filenames, logs and model output as text; filter terminal control
sequences unless an intentional trusted parser handles them. Honor the project's
color policy and support a readable low-color/monochrome mode. Avoid conveying
status only through color. Keep plain output available when appropriate for pipes,
non-TTY use and readers that cannot use a full-screen interface.

Redraw when state changes or an active animation needs a tick. Bound retained logs,
transcripts and search results; virtualize long views. Do not spin-poll an idle
terminal or reset a draft/scroll position on every background event. Surface work
progress and recovery without stealing focus from the user's current edit.

Verify pure state transitions and representative rendered buffers, then exercise
the real binary through a PTY. Check typing, paste, modal focus, navigation, resize,
slow work, cancellation and exit/restored terminal flags. Buffer snapshots alone
do not establish actual key decoding or terminal cleanup. Use generous failure
deadlines; benchmark an actual redraw/input workload before optimizing it. Report
which terminal/OS/protocol combinations were tested and any unverified ones.

Lifecycle references: [Ratatui backends](https://ratatui.rs/concepts/backends/)
and [panic/restoration behavior](https://ratatui.rs/recipes/apps/panic-hooks/).
