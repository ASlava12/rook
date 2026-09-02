# Working in this repository

Rook is a local autonomous agent in Rust. Read
[docs/architecture.md](docs/architecture.md) before making structural changes, and
[docs/research/agent-landscape.md](docs/research/agent-landscape.md) to understand
why things are the way they are — most non-obvious decisions trace to a specific
public failure in another agent, and the ADRs cite them.

## Build and test

```sh
cargo xtask ci             # fmt + clippy -D warnings + test — the CI gate
cargo test --workspace
cargo xtask compaction     # re-measure the storage claims in README/docs
cargo xtask dist           # release build; also prints the binary sizes README quotes
cargo xtask targets        # supported target matrix
cargo xtask smoke --model ollama/qwen3:8b   # real turns against a real model
```

`cargo xtask clean` reclaims `target/` — incremental state and cross-target
artifacts first, `--all` for the rest. A full debug build with tests is ~800 MB;
if it is several gigabytes, something has re-enabled full debug info.

`ROOK_HOME` redirects all agent state, which is how tests and manual runs stay out
of a real store:

```sh
ROOK_HOME=/tmp/rook-scratch cargo run -p rook-cli -- store stat
```

## Layering

Dependencies run one way. Do not add an edge that reverses them.

```
rook-store  rook-skills  rook-llm  rook-lsp  rook-proto   ──►  (nothing internal)
rook-mcp                                                 ──►  llm
rook-tools                                               ──►  llm, mcp, proto
rook-core                                                ──►  everything below it
rookd  rook-acp                                          ──►  core and below
rook-cli                                                 ──►  acp, core and below
```

`crates/rook-core/tests/layering.rs` holds this, so an edge that reverses it
fails the build rather than being noticed later. It is ranks rather than a list
of edges: adding an ordinary dependency needs no edit there, and adding a crate
needs one line.

`rook-store` must not learn what a skill or a checkpoint is. When GC needs to know
that a manifest keeps files alive, the caller passes an expander.

## Conventions that matter here

**Bounded by default.** Anything that accumulates — logs, captures, output capture,
context — has a limit in `Config` and a test that it is enforced. A new unbounded
accumulator is a bug, not a follow-up.

**A limit is applied while the bytes arrive, not after.** `bytes.len() > MAX`
reads as a cap and is not one: by the time it is false the memory is already
spent, which is the whole thing it was there to prevent. Three shipped that way —
a command's output, a file `session diff` was deciding not to render, and a page
`web_fetch` had already downloaded. Read in chunks and stop, or ask the cheap
question first: a file's length before its contents, and a hash rather than a
comparison when it is too large to hold. Draining still matters where a writer is
on the other end — `hooks` deadlocked when it stopped reading a full pipe — so
bound the memory and read to the end.

**Errors say what to do.** `CaptureTooBig` names the limit that was hit.
`StoreError::Locked` says which process is probably holding it and what to do
instead. `NoCompatibleVersion` lists every mismatch, not the first.

**Comments explain why, not what.** The code says what. Comments carry the reason a
non-obvious choice was made — usually a failure mode being avoided.

**Tests are named as claims.** `a_capture_refuses_to_run_away_instead_of_thrashing`,
not `test_capture_2`. Assertion messages print the actual values.

**A timing constant in a test is not what the test claims.** Three CI failures
in a row were one: a deadline chosen to keep the suite quick, met on a laptop and
missed on a loaded runner. A wait that exists only to tell "failed" from "hung"
should be generous, and the test that is *about* a timeout sets its own — as
`a_hung_server_times_out_rather_than_blocking_the_agent` does. When raising it
twice has not helped, the number was never the problem: `tui_pty` needed
`one_at_a_time()`, not a third guess.

**A test for a bound asserts that the bound was reached.** Three passed here
while proving nothing: a scrollback test whose 5,000 short lines never came near
the megabyte it was capping, a step-limit test whose config never reached the
binary so the turn ran to the default of two hundred, and a compaction test whose
message sat under the threshold so nothing compacted either way. Each asserted
the outcome and not the precondition, and each passed with the code it was
testing removed. Assert the setup bit — that the total exceeded the cap, that the
limit in force was the one the test set — and then assert the behaviour.

**The front of a request must not vary per turn.** Prompt caching is a prefix
match, so anything interpolated into the system prompt — recalled memory, a
timestamp, a reordered tool list — invalidates everything behind it. Keep the
system block and the tool list stable, sort tools by name, and put per-turn
context next to the newest message instead.

**Expensive things belong to the front end, not the turn.** A new `AgentLoop` is
built for every turn, so anything constructed inside it is rebuilt every turn —
and anything dropped with it is torn down. The approval policy, the language
server pool and the MCP session are all built once by the front end and handed
in. Getting this wrong is invisible in the code and expensive in use: language
servers re-index, MCP servers respawn, and an approval granted "for the run" is
forgotten.

**Undo is a property of the workspace, not of a log.** Reversibility is not
something to weigh per decision — it is the floor the whole design stands on,
and a rewind that covers most of what happened is a rewind nobody can trust. It
was one session deep: a turn that delegated its writing left a child's
checkpoints in the child's session, so rewinding the parent restored nothing and
said `checkpoints_applied: 0` while doing it. `Rewind` follows what a turn
delegated now, and the test that found it asserts the parent wrote nothing
itself — the first version of it passed while the parent did the work, because a
scripted provider hands out replies in order and the child had taken a different
one.

**Three front ends, one engine.** New capability goes in `rook-core` and is
exposed by the CLI, the API and the TUI. A feature reachable from one front end
and not the others is a bug — the approver is shared for exactly this reason.

**A number in prose rots unless a command reprints it.** The README's
compression ratios and binary sizes are re-measurable by `cargo xtask compaction`
and `cargo xtask dist`, and are kept current. A test count in prose is not worth
the same discipline — it was wrong by a factor of two before anyone noticed — so
do not put one there.

**One question, one answer.** Which events reach the model is asked in three
places — the replay that builds a request, what compaction summarises, and what
`session context` reports as the cost — and all three drifted apart before
`context::reaches_the_model` existed. When two paths have to agree, give them one
function to agree through, not two lists to keep in step.

**Nothing here is exempt from being used.** Every crate is a library, so `pub`
turns off the dead-code lint: a function that exists and is called from nowhere
advertises an API that is not there, and an error variant nothing raises names a
case nothing handles. `ContextOverflow` was declared for a prompt too large to
send and nothing checked, so such a prompt went to the provider whole.
`every_public_function_is_called_somewhere` and
`every_error_variant_is_constructed_somewhere` fail the build for the next one.
Either wire it up or delete it.

Those two count every `.rs` file, so a function only a test reaches passes them —
used by something is not the same as used, and library API offered to nobody is
the thing being guarded against.
`a_public_function_no_production_code_calls_says_it_is_a_test_seam` asks the same
question of `src/` alone. A genuine seam — one that exists because the
alternative is filling a context window or sleeping for an hour — says so with
`#[doc(hidden)]`, and that is the whole exemption.

**A config field nothing reads is a lie, not a stub.** Four shipped that way —
`sandbox.allow`, `allow_outside_workspace`, `lazy_skills`, and
`storage.maintenance_interval_hours`, which `docs/storage.md` described as
working. `every_config_field_is_read_somewhere` now fails the build for a field
mentioned fewer than three times: its declaration, its default, and somebody
using it.

**A tool description opens with a sentence that stands alone.** Under lazy
loading only that first sentence is advertised; the rest is guidance on writing
the arguments, which only matters once the model has decided to call the tool.
`cargo run -p rook-core --example schema-cost` prices both forms — from core, because the loop adds six tools of its own and the two largest are among them.

**Verifying a change to `rookd`.** The daemon tests in
`crates/rook-cli/tests/cli.rs` run the binary, and they build it themselves —
so `cargo test -p rook-cli` after editing `rookd` can run against the previous
one and pass. `cargo xtask ci` is fine, because `cargo test --workspace` builds
every member first; a targeted run is not. Build `rookd` explicitly before
trusting one, which is also how to tell whether such a test bites at all.

**Verifying the TUI.** `crates/rook-cli/tests/tui_pty.rs` does it; add to that
rather than starting again. A pty capture is not readable as text: ratatui
places characters cell by cell, so a substring the screen clearly shows never
appears contiguously in the byte stream. Replay the cursor-positioning escapes
into a grid before asserting on it, give the pty a window size or the app draws
into a zero-sized terminal and emits nothing, and accumulate the stream across
frames — a redraw after a keypress emits only the cells that changed. Do not
wait for the output to go quiet: it redraws on a 60 ms tick whether or not
anything changed. Wait for a frame rather than for the first byte: entering the
alternate screen writes before anything is drawn, so work the app does between
the two lands in the gap and a byte-triggered capture reads a blank screen. And
after a keypress, wait for the content being asserted rather than for a settling
window — a window is a guess about redraw latency, and under a full `cargo xtask
ci` that guess is wrong often enough to look like a product failure. These tests
take `one_at_a_time()`: each starts a whole `rook` from cold before its first
byte reaches the terminal, and nine at once on the FreeBSD VM starved one past a
minute of having drawn nothing. Add to that rather than to the deadline.

**A test that starts a subprocess and waits for it takes `one_at_a_time()`.**
`tui_pty`, `rook-mcp`'s client and `rook-lsp`'s client all do. Thirteen mocks
spawned at once on a loaded machine timed out *together*, on the handshake, which
reads as a broken client and is a scheduler — the tell is that every test in the
file failed identically. Serially each gets the machine and the file still
finishes in seconds.

## Storage changes

- Bump `FORMAT_VERSION` in `crates/rook-store/src/schema.rs` for any change older
  builds cannot read. Opening a newer format must keep failing cleanly.
- **Adding a field to a stored struct breaks every record already written.**
  postcard is not self-describing, so a decoder reading an old record hits the
  end of the buffer looking for the new field. Put session-scoped extras in the
  `kv` table instead — that is where `goal/<session>` lives.
- Never change how an existing object decodes. Objects record their own codec
  precisely so encoding can evolve without rewriting history.
- Re-run `cargo xtask compaction` and update the numbers in `README.md` and
  `docs/storage.md` if they move. Those numbers are load-bearing claims.

## Adding a skill capability

Frontmatter fields go in `crates/rook-skills/src/manifest.rs`. Keep them in keys
the Agent Skills spec leaves free, and keep unknown fields round-tripping —
[ADR-0003](docs/adr/0003-agent-skills-format.md) explains why both matter.

## Platform work

Platform branches belong in `paths.rs`, `exec.rs` or the `userland` predicate —
not scattered through call sites. FreeBSD is a tested target; see
[docs/platforms.md](docs/platforms.md) for the two C dependencies that constrain
cross-compilation.
