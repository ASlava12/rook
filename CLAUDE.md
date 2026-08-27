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
rook-store   ──►  (nothing internal)
rook-skills  ──►  (nothing internal)
rook-llm     ──►  (nothing internal)
rook-tools   ──►  rook-llm
rook-core    ──►  store, skills, llm, tools, proto
rookd        ──►  core, store, skills, proto
rook-cli     ──►  core, store, skills, llm, proto
```

`rook-store` must not learn what a skill or a checkpoint is. When GC needs to know
that a manifest keeps files alive, the caller passes an expander.

## Conventions that matter here

**Bounded by default.** Anything that accumulates — logs, captures, output capture,
context — has a limit in `Config` and a test that it is enforced. A new unbounded
accumulator is a bug, not a follow-up.

**Errors say what to do.** `CaptureTooBig` names the limit that was hit.
`StoreError::Locked` says which process is probably holding it and what to do
instead. `NoCompatibleVersion` lists every mismatch, not the first.

**Comments explain why, not what.** The code says what. Comments carry the reason a
non-obvious choice was made — usually a failure mode being avoided.

**Tests are named as claims.** `a_capture_refuses_to_run_away_instead_of_thrashing`,
not `test_capture_2`. Assertion messages print the actual values.

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

**A config field nothing reads is a lie, not a stub.** Four shipped that way —
`sandbox.allow`, `allow_outside_workspace`, `lazy_skills`, and
`storage.maintenance_interval_hours`, which `docs/storage.md` described as
working. `every_config_field_is_read_somewhere` now fails the build for a field
mentioned fewer than three times: its declaration, its default, and somebody
using it.

**A tool description opens with a sentence that stands alone.** Under lazy
loading only that first sentence is advertised; the rest is guidance on writing
the arguments, which only matters once the model has decided to call the tool.
`cargo run -p rook-tools --example schema-cost` prices both forms.

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
the two lands in the gap and a byte-triggered capture reads a blank screen.

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
