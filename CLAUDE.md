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

**Three front ends, one engine.** New capability goes in `rook-core` and is
exposed by the CLI, the API and the TUI. A feature reachable from one front end
and not the others is a bug — the approver is shared for exactly this reason.

**Verifying the TUI.** A pty capture is not readable as text: ratatui places
characters cell by cell, so a substring the screen clearly shows never appears
contiguously in the byte stream. Reconstruct the grid from the cursor-positioning
escapes before asserting on it, and set the window size with `TIOCSWINSZ` or the
app renders into a zero-sized terminal and emits nothing.

## Storage changes

- Bump `FORMAT_VERSION` in `crates/rook-store/src/schema.rs` for any change older
  builds cannot read. Opening a newer format must keep failing cleanly.
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
