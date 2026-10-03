# Terminal image assessment

Scope: the optional image experiment in the pinned
[Pi review](pi-reference-review-20260930.md), not a promise of a new production
terminal renderer. Rook's existing attributed text/export fallback and explicit
bounded browser image reader remain the supported interfaces.

## Rendering contract

Pi `ee602414c703be8da722ec56de7f2399e62581ac` documents Kitty placement cropping
in fullscreen and iTerm2 text fallback there, because its iTerm2 path cannot
delete/crop placements during repaint. Its main-screen renderer accepts iTerm2.
See [the pinned README](../../references/pi/packages/tui/README.md) and
[terminal image implementation](../../references/pi/packages/tui/src/terminal-image.ts).
Pi's environment detection selects Kitty for `WEZTERM_PANE`; that hint alone
does not verify the active console transport or enabled graphics configuration.

The [Kitty protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/#querying-support-and-available-transmission-mediums)
specifies a query that loads a dummy pixel without retaining it, followed by
primary device attributes. A DA reply without the preceding graphics reply is
negative evidence on that transport. An empty/incomplete reply is inconclusive.
[WezTerm's tracking issue](https://github.com/wezterm/wezterm/issues/986) describes
its explicit `enable_kitty_graphics=true` option. Neither an installed executable
nor a terminal brand establishes successful fullscreen image behavior.

## Finite experiment

[The example](../../crates/rook-cli/examples/terminal_images.rs) emits the
documented one-pixel query and DA request in a fresh owned console. It stores at
most 1024 input bytes, uses a 64-byte bounded channel and a three-second deadline,
and refuses existing evidence files. A raw-mode/alternate-buffer guard restores
the console before writing JSON. No image placement, model request, agent store,
network, image decode or user configuration is involved. An idle console reader
is terminated with this finite process; it is not a proposed production input
reader. Outcome parsing requires the matching complete success reply, keeps
truncated replies inconclusive and does not infer unsupported graphics from
silence or capture saturation.

[The Windows driver](../../xtask/probes/terminal-ui/images.ps1) starts an owned
headless WezTerm mux with a fresh Lua config/socket, no automatic config reload
or update checks, and 32 scrollback lines. The example runs through its Windows
console path. A wrapper records its actual exit code; mux lifetime is separate.
The driver has a 20-second deadline, caps proof/exit reads before loading and
stops only the exact process naming its owned config. It does not open a GUI or
attach to an operator pane. WezTerm version: `20240203-110809-5046fc22`.

| Actual path | Native/driver exit | Reply | Evidence root under `target/` |
|---|---|---|---|
| Current ConPTY, alternate buffer | 0 | DA, no Kitty reply | `terminal-image-conpty-413c0da68e3648a4b73eab3986a3722e` |
| Fresh WezTerm config, alternate buffer | 0 / 0 | DA, no Kitty reply | `terminal-image-default-alternate-2b37bf282139494499c9bb42df3638ee` |
| Kitty explicitly enabled, alternate buffer | 0 / 0 | DA, no Kitty reply | `terminal-image-kitty-alternate-36537a9835a44899bffb3227d3f0838a` |
| Kitty explicitly enabled, main buffer | 0 / 0 | DA, no Kitty reply | `terminal-image-kitty-main-b5704a5f8c734624a3b60bc1e725027b` |

Each root retains `image-query.json`; corrected mux roots also retain exact
configuration, bounded response bytes, native exit report and driver summary.
The mux cases returned 28 bytes of DA and no matching graphics response.
All owned processes are stopped. Exit 0 means the finite experiment ran; it
does not turn these negative protocol observations into successful rendering.

The Windows console may consume the query before it reaches the mux parser.
These are observations of the complete console-to-mux path, not proof of parser
arrival or a general claim that WezTerm cannot render Kitty images. No actual
pixels, cropping, stale-placement deletion, resize, branch/reconnect lifecycle
or production local/daemon image panel were verified by this query. There is no
rich-terminal screenshot evidence in this block.

## Failures and checks

The first parser test exited 101 because the rejected-reply matcher used an
incorrect fixed prefix length. It now uses the byte slice's length; both focused
tests exited 0 (`target/pi-terminal-image-final-tests.log`), including wrong ID,
partial reply, error response, DA, silence and saturated-capture cases. The
example build exited 0 (`target/pi-terminal-image-build.log`).

The first mux driver exited 1 because the deeply nested socket path exceeded
Windows' Unix-socket path limit. The corrected socket is short, unique and still
inside `target`. The next driver exited 1 because it awaited the persistent mux
server's exit rather than the probe's exit. The wrapper now captures the probe
exit independently, then cleans up its server. Failure logs are retained in
`target/pi-terminal-image-kitty-alternate.log` and
`target/pi-terminal-image-kitty-alternate-final.log`; corrected runs are in
`target/pi-terminal-image-kitty-alternate-corrected.log`,
`target/pi-terminal-image-default-alternate.log` and
`target/pi-terminal-image-kitty-main.log`.

An initial `wezterm cli list` discovery auto-started its default mux; discovery
should use `--no-auto-start`. That exact newly created server had no connected
clients; its pane and server were stopped after verifying PID, creation time and
command line. Subsequent experiments use only explicit owned configs/sockets.

Mandatory `cargo xtask ci` exited 0 in 573.0 seconds, including fmt, Clippy,
frontend builds, workspace tests and doctests; output is retained in
`target/pi-terminal-image-ci.log`. No production storage changes were made, so
this block does not require a new compaction measurement.

## Adoption decision

Keep attributed text/export in the fullscreen TUI on the tested Windows path.
Do not select a graphics backend solely from Pi's terminal-name heuristic. This
assessment supplies a reproducible transport query and explains why automatic
pixel adoption is not established here; it does not ship or certify a preview.

A future opt-in renderer requires positive end-to-end protocol evidence and
actual pixels in a supported host. It must own and release placements on hide,
overlay, resize, branch switch, reconnect and shutdown; crop only within the
image region; preserve the lower queue/editor; and bound both encoded and decoded
data before copying/allocation. Reuse Rook's source-bound image companion reader
and explicit image action, with identical local/daemon semantics. Test those
interactions in the real host, including pixels and stale placements, before
calling production preview complete. Neither a successful query nor browser
pixels prove terminal placement behavior.

The optional experiment is assessed on the current Windows transport. Rich
terminal preview remains unverified future work; the outstanding main-table
requirement is the completed real-model phase comparison in the
[adoption tracker](pi-adoption-20260930.md).
