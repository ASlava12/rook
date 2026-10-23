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

## Local build notes

After a completed, unsuccessful direct local foreground Cargo command, Rook can
append advisory passages from documentation already kept under `rust errors` or
a related topic. It recognizes at most two plain `error[Edddd]:` codes in the
first 256 lines of an admitted visible-output prefix of at most 64 KiB. ANSI
colored and JSON diagnostics are not parsed. There is no extra model call or
network request. A citation describes the kept topic and version; it does not
establish the cause of the failure, identify a dependency, or certify a fix.

Timeouts, uncertain execution, wrappers, background commands,
editor terminals and remote execution do not get these notes.
The command's result may still report live descendants; advice quotes received
output and does not certify their termination. An absent diagnostic prefix is
never replaced by text from later receipts or hooks. The original capture may
remain incomplete after pipe retirement; a positive exit and an observed prefix
permit advisory citation without asserting a complete capture. Display
condensation can retain an observed diagnostic without making the original
capture complete. The diagnostic prefix excludes later receipts and hook prose.
Notes are added after cycle/progress comparison, and do not count as progress.
Each code is considered once per turn, with at most sixteen remembered codes;
a cache miss is silent. Notes are limited to 6,000 bytes.

`docs` also accepts `local_only: true`: it exits on a cache miss before source
configuration or approval, and rejects combination with `refresh`. Existing
domain-policy tool restrictions still apply. Explicit versions cannot reuse a
different version, including on a neighboring topic. The normal fetching mode
continues to work as before.

Local-only reads prefer an exact reference, admit at most 128 documentation
reference rows with one lookahead and admit at most eight neighboring sets. Names are limited to
512 bytes and reference values must be 32-byte object ids. Each set admits at
most 512 KiB encoded and decoded, before copying or decompression; an external
payload read permits one refusal byte. These are at most 4 MiB of each admitted
payload representation across neighbor candidates. Sets permit 32 pages, topic
and version fields of 128 bytes, URLs of 2,048 bytes and titles of 512 bytes.
Queries admit 2,048 bytes. The response is limited to 16 KiB. Oversized, invalid
or omitted entries produce an incomplete-coverage notice. These are bounded
reader and presentation contracts, not a whole-process memory or latency bound.

The design follows Haico's `src/tools/build_notes.rs` at the same pinned source
above, independently implemented against Rook's process receipts and kept docs.

The frontend contract can be exercised against a downloaded or development pair:

```sh
python3 xtask/probes/local-build-notes/run.py --bin-dir /path/to/bin \
  --tmp-root /owned/short/tmp --build-target /owned/helper-target \
  --output /owned/local-build-notes.json
```

It seeds three private documentation objects through a helper using the locked
store dependency, then drives both normal CLI and daemon WebSocket sessions.
All fifteen flags must pass. Its two Cargo invocations use an owned
compiler-shaped executable; the separate registered regression invokes real
Cargo. The same final fixture fails eleven feature conditions on published
0.21.14, with four request/exit/identity controls passing. Baseline refreshes
reach only the owned loopback server, never an external source. No model-quality
improvement is inferred from this differential.
