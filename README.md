# Rook

An autonomous agent whose memory you can actually read.

Rook is a general-purpose local agent — coding, research, automation — written in
Rust and shipped as two static binaries with no runtime. It stores everything it
does in a compact, content-addressed store, and it treats *inspecting* that store
as a feature rather than a debugging afterthought: a CLI, a terminal browser and a
web UI, all views over the same engine.

> **Status: early.** The storage layer, the skill system and the inspection tools
> are implemented and covered by 56 tests. The agent loop — tool dispatch, skill
> loading, budgeting, logging — is tested against a scripted provider; it has not
> yet been exercised against a live model in CI. Streaming, MCP and ACP are on the
> [roadmap](docs/roadmap.md) and are not implemented. Nothing below describes
> something that does not exist — see
> [what is not done](#what-is-not-done-yet).

## Why another one

Every design decision here traces to a specific, public failure in an agent people
actually run — an SQLite log that writes terabytes a year, a checkpoint feature
implemented as `git add .` over a 45 GB workspace, tool schemas that cost 5,000
tokens a turn, a context overflow with no recovery. The research is written up with
citations in **[docs/research/agent-landscape.md](docs/research/agent-landscape.md)**.

Three things follow from it.

**Memory is compact by construction.** Content addressing, zstd dictionaries
trained per object kind, and small objects inlined into the index. On a synthetic
transcript of 3,000 turns and 300 tool results:

```
logical bytes written by the agent :    21.88 MiB
  after dedup (distinct objects)   :     2.54 MiB
  cold store, standalone zstd      :     0.60 MiB   ratio  4.3x
  warm store, trained dictionaries :     0.12 MiB   ratio 20.7x
  on-disk total (index + objects)  :     1.07 MiB
  end-to-end (logical -> on disk)  :     20.5x
```

Reproduce it yourself: `cargo xtask compaction`.

**Memory is inspectable.** `rook store stat` tells you what your history costs and
where it went. `rook session show` prints any transcript by sequence number.
`rook store cat` prints any object. The TUI and the web UI show the same data.

**Skills are versioned and environment-aware.** A skill declares the environment it
is valid in — OS, userland, arch, language and tool versions — and can carry
platform-specific bodies instead of forking into `deploy-linux` and
`deploy-windows`. Every edit can be captured, diffed and rolled back.

## Install

```sh
git clone https://github.com/ASlava12/rook && cd rook
cargo build --release          # rook 4.4 MiB, rookd 2.8 MiB — no runtime, no shared libs
```

Requires a Rust toolchain and a C compiler (two dependencies vendor C — see
[docs/platforms.md](docs/platforms.md)). No Node, no Python, no Docker.

## Use

```sh
rook init                                  # create ~/.rook, config, store
rook doctor                                # what was detected, and what it means for skills

rook run "summarise what changed in src/ this week"   # streams as it arrives
rook                                       # terminal browser over everything stored
rookd                                      # http://127.0.0.1:7717 — web UI + API
```

### Reading what the agent remembers

```sh
rook store stat                # size, compression ratio, breakdown by kind
rook store ls --kind file      # objects, newest first
rook store cat 4f2a9b          # any object, by short hash
rook store gc --dry-run        # what is unreachable
rook store prune --dry-run     # what the retention policy would drop

rook session ls
rook session show 01JQ… --from 0 --limit 50
rook session show 01JQ… --json | jq '.[] | select(.kind=="tool-call")'
rook session context 01JQ…            # what the conversation costs, and of what
```

### Undoing a turn

The loop checkpoints every file a tool is about to modify, so a rewind puts the
workspace back as well as the conversation — and forks rather than truncates, so
the turns you rewound past stay readable in the parent session.

```sh
rook session rewind 01JQ… --to 12               # conversation and files
rook session rewind 01JQ… --to 12 --keep-files  # conversation only
rook session fork 01JQ… --at 12                 # branch without touching files
```

### Skills

```sh
rook skills ls                 # what applies here, and what loading each would cost
rook skills why deploy         # which version was chosen, and why the others were not
rook skills new my-skill -d "…"
rook skills capture my-skill -m "first version"
rook skills history my-skill
rook skills diff <objA> <objB>
rook skills rollback my-skill <obj>
```

A skill is a directory with a `SKILL.md` — the
[Agent Skills](https://www.webfuse.com/agent-skills-cheat-sheet) format, so skills
written for other agents work unchanged. Rook adds two optional blocks:

```yaml
---
name: in-place-edit
description: Edit files in place across platforms.
version: 1.2.0
requires:                        # gates the whole skill
  language: { rust: ">=1.85" }
  tool: { git: ">=2.30" }
variants:                        # swaps only the body
  - when: { userland: [bsd] }
    body: variants/bsd.md
  - when: { os: [windows] }
    body: variants/windows.md
---
```

`requires` is why `rook skills why` can tell you a skill is inert because you are
missing Docker 27, instead of leaving you to guess. See
[docs/skills.md](docs/skills.md).

## Layout

```
crates/
  rook-store    content-addressed store: redb index, zstd dictionaries, gc, retention
  rook-skills   SKILL.md parsing, environment detection, version + variant resolution
  rook-core     the engine: config, agent loop, context budget, file captures
  rook-llm      provider trait + OpenAI-compatible HTTP (Ollama, LM Studio, vLLM, OpenAI)
  rook-tools    read/write/edit/list/search/run, with the guards that keep a turn survivable
  rook-proto    wire types shared by daemon, CLI and web
  rookd         HTTP backend + embedded web UI
  rook-cli      `rook`: commands and the terminal browser
web/dist        the web UI: one hand-written HTML file, no build step
docs/           architecture, storage format, skills, platforms, ADRs, research
references/     upstream agent sources as shallow submodules, to read from
```

`references/` is not fetched by a normal clone. `cargo xtask refs status` shows how
far each pinned pointer has drifted from upstream — that gap is the backlog of
upstream work nobody has looked at yet. See [references/README.md](references/README.md).

## Platforms

Linux, macOS, Windows and FreeBSD. FreeBSD is built **and tested** in a real VM in
CI rather than cross-checked, because the two dependencies that vendor C are
exactly what a cross-check cannot exercise. Details and the target tier list:
[docs/platforms.md](docs/platforms.md), or `cargo xtask targets`.

## What is not done yet

Being explicit, because a roadmap presented as a feature list is how these projects
lose people's trust:

- **The agent loop has not been run against a live model in CI.** Its logic is
  covered by tests against a scripted provider, and the SSE parser by tests over a
  real socket, but no real model has been driven end to end in CI.
- **No MCP client, no ACP server.** Both are planned; see [docs/roadmap.md](docs/roadmap.md).
- **Compaction is mechanical**, not model-summarised: it keeps the head and the
  recent tail and marks the elision. The full transcript is never lost.
- **The CLI opens the store directly**, so it cannot run while `rookd` holds it.
  It says so clearly; routing the CLI through the daemon is
  [ADR-0006](docs/adr/0006-single-writer-store.md).
- **The web UI is read-only.**
- **No sandboxing beyond a deny list and a workspace boundary.** Not a jail.

## Development

```sh
cargo xtask ci             # fmt + clippy + test, the gate CI runs
cargo xtask targets        # the supported target matrix
cargo xtask compaction     # measure the storage claims above
cargo xtask clean          # report what target/ costs and reclaim it
cargo xtask refs status    # how far the reference pointers have drifted
cargo test --workspace
```

A full debug build with tests is about 800 MB of `target/`. Debug info is
line-tables-only and dependencies carry none, because full DWARF for the
dependency graph costs several gigabytes and is never stepped through.

## License

MIT or Apache-2.0, at your option.
