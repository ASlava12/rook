# Opening a saved conversation in the TUI

Use `rook tui --session <id>` to open a saved conversation directly, or
`rook tui --session last` to select the latest conversation in the current
workspace. The selectors have the same spelling as `rook chat --session` and
`rook run --session`. The Sessions picker remains available inside the TUI.

```sh
rook tui --session last
rook -C /path/to/project tui --session 01M4KR4732S15ASVZWRCSXFYC3
rook tui --alone --session last
```

Without `-C`, an explicit session ID follows the session's recorded project.
An explicitly selected workspace must match that project; opening a conversation
does not move it. If its project is unavailable, move the session explicitly
with `rook session move` before continuing it. Unknown IDs, invalid selectors
and workspace conflicts are reported before entering the terminal screen.
If an older daemon cannot read the selected session's metadata, the error
explains that `rook daemon restart` loads the installed daemon build.

Opening a conversation shows its saved history. In a shared window it also
attaches to a turn already running in that session, using the existing live
progress and question handling. It sends no prompt and does not replace the
running turn's mode or effort settings. Model work starts only when a prompt
is submitted or an existing daemon turn is already working.

The same selection is available with `--alone` when this process holds the
store. Closing a shared window leaves its daemon running; `rook daemon stop`
uses Rook's normal daemon shutdown controls.
