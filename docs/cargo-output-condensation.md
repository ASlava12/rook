# Cargo output in foreground tool results

Direct local foreground Cargo build/check/test/clippy/doc/bench calls omit
ordinary `Compiling`, `Checking`, `Fresh` package-version lines and passing
`test ... ok` lines before the existing head/tail display cap. Compiler errors,
warning bodies, source spans, failing tests, test totals and unknown lines pass
through unchanged to that cap. It remains possible to elide a large diagnostic;
this is not a promise that arbitrarily large errors fit in one answer.

Recognized error, warning, note and help blocks protect their continuation
lines, even when the diagnostic text looks like Cargo progress. An empty line
or an unindented passing-test harness line ends that protection. Indented
`test ... ok` text inside a diagnostic remains data. An oversized line also
protects subsequent lines until one of those boundaries.

The `run_command` argument `output="raw"` keeps the original presentation.
`output="auto"` is the default. The selector recognizes direct Cargo commands,
including quoted executable paths and common global flags; compound commands,
pipelines, substitutions, unknown wrappers/options and other programs stay raw.
The selector is independent of authorization. Existing background, editor,
container and remote execution routes retain their own presentation.

Condensation requires the existing owned original-output capture to open
successfully. It never changes the child process, exit code, raw byte/activity
counters or command cleanup. Original captured bytes still pass through the
existing secret masking and `max_spill_bytes` cap. The response names that
capture for `read_result` with `source=output`; a short condensed result keeps
it too. A capped/failed capture is reported as incomplete rather than called a
complete original. Without an output capture, the normal presentation remains.

An omitted-line notice and typed counters say what was removed. A condensed
result is marked shortened and is not labelled as an unchanged complete command
result. Existing cycle detection can use the retained original when it is
complete, rather than treating two different original outputs as identical just
because their progress lines were omitted.

Each stream buffers at most 16 KiB of an incomplete line for classification;
longer lines pass through without accumulating. Partial lines at EOF, timeout
or pipe retirement are preserved. SGR colors are ignored only when recognizing
noise; kept bytes, other controls and non-UTF8 data retain the existing decoder
behavior. The existing display/capture/read-buffer limits still apply; this is
not a whole-process memory bound.

The design was prompted by Haico's Cargo output condenser at
`5b352f3c7e3b608e68ec49af65a66feb098e07d4`. This is an independent streaming
adapter for Rook's bounded capture and original-result receipts. It preserves
warning bodies and lock-wait notices. No upstream implementation was copied.

Local owned-process tests demonstrate the old middle-diagnostic omission on a
239,726-byte compiler-shaped output with a 2 KiB display cap. The fixture is an
owned executable emitting Cargo-shaped text, not a real compiler failure or a
model-quality benchmark. The [CLI/daemon fixture](../xtask/probes/cargo-output/README.md)
also checks normal model/tool plumbing and the explicit raw override.
A separate regression test invokes real `rustc` with a multiline
`compile_error!` message and requires every diagnostic byte to survive.
