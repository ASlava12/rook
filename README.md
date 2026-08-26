# Rook

An autonomous agent whose memory you can actually read.

Rook is a general-purpose local agent — coding, research, automation — written in
Rust and shipped as two static binaries with no runtime. It stores everything it
does in a compact, content-addressed store, and it treats *inspecting* that store
as a feature rather than a debugging afterthought: a CLI, a terminal browser and a
web UI, all views over the same engine.

> **Status: early.** The storage layer, the skill system and the inspection tools
> are implemented and covered by 166 tests. The agent loop — tool dispatch, skill
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
rook doctor                                # environment, model reachability, skills
rook models                                # what the configured provider serves

rook                                       # talk to it
rook run "summarise what changed in src/"  # one turn, streamed, for scripts
rook tui                                   # full terminal UI: chat plus a store browser
rookd                                      # http://127.0.0.1:7717 — web UI + API
```

In a conversation, slash commands reach the same engine the subcommands do:

```
/btw <question> ask about the work without joining the conversation
/goal [text]    what this session is for; the agent is told
/context        what this conversation costs, and of what
/skills [name]  what applies here, or one skill's body
/undo           rewind past the last exchange, files included
/rewind <seq>   rewind to a point in the transcript
/session  /mcp  /new  /help  /quit
```

`/btw` answers from what the agent already knows — no tools, one call — and its
answer never enters the context the agent carries forward, though it is still in
the transcript. Ctrl-C stops the turn in flight without leaving; whatever it
already did stays in the log.

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

All three front ends run turns, stream them, and ask for approvals the same way:
`rook chat`, `rook tui`, and the web UI at `rookd`. Nothing is reachable from one
that is not reachable from the others.

### Code intelligence

When a language server is on `PATH`, the agent gets four more tools: what the
type checker thinks is wrong with a file, where a name is defined, what actually
refers to it, and where a symbol lives in the workspace. It asks by name — the
name it can read in the source — rather than by line and column:

```sh
rook lsp servers                              # what applies here
rook lsp diagnostics src/main.rs              # without running a build
rook lsp definition src/main.rs parse
rook lsp references src/main.rs parse
rook lsp symbol ObjectId
```

rust-analyzer, gopls, clangd, typescript-language-server and pyright are detected
automatically; `[[lsp]]` in the config overrides that. Servers start lazily, on
the first question that needs one. Auto-*installation* is deliberately not done:
downloading and running a binary on your behalf is a different decision from
using one you already have.

### Hooks

Commands that run at points in a turn, so extending the agent does not mean
changing it:

```toml
[[hooks]]
event   = "post_tool"                    # what it prints is appended to the result
match   = "/^(write_file|edit_file)$/"   # plain substring, or /regex/
command = "cargo fmt --all 2>&1 | tail -3"

[[hooks]]
event   = "pre_tool"                     # may allow, ask, or deny
match   = "run_command"
command = "my-policy-check"              # {"decision":"deny","reason":"…"} on stdout
```

Five events: `session_start`, `prompt`, `pre_tool`, `post_tool`, `turn_end`. A
hook reads JSON on stdin and may answer with JSON; plain output is treated as
context for the model, so `echo` works. A `pre_tool` hook that fails blocks the
call it was guarding — a guard that cannot run is not approval — and no hook can
unlock what the deny list forbids.

### Models

The `provider/model` in `config.toml` picks the wire dialect:

```toml
[agent]
model = "anthropic/claude-opus-5"   # ANTHROPIC_API_KEY
# model = "ollama/qwen3-coder:30b"  # a local endpoint, no key
# model = "openai/gpt-5.5"          # OPENAI_API_KEY
```

Keys come from the environment, never from the config file or the store.
`rook models` asks the endpoint what it serves.

### From an editor

`rook acp` speaks the [Agent Client Protocol](https://agentclientprotocol.com) on
stdio — the same protocol Zed, JetBrains and Neovim already use — so no plugin is
needed per editor. Streamed output becomes `session/update`, and the permission
policy becomes the editor's approval dialog: the same decision, reaching the same
rules, whichever front end asks.

### What it is allowed to do

By default the agent asks before anything that changes the machine, and refuses
outright what the deny list forbids — no approval can override a denial:

```toml
[sandbox]
mode  = "ask"                      # auto | ask | readonly
allow = ["git status", '/^(ls|cat|rg)\b/']   # plain string, or /regex/
ask   = ["git push"]                          # prompts even in auto mode
deny  = ['/\brm\s+(-[a-zA-Z]+\s+)*\/(\s|\*|$)/']
```

`rook --yes` skips the prompts for one run. Unattended runs with no `--yes`
refuse rather than improvise, and say what would have made it possible.

### Delegation

A turn can hand self-contained sub-tasks to fresh agents and get back only their
conclusions. Each sub-agent runs in its own session with an empty context, so a
wide search or a long file survey never enters the conversation that asked for it
— and its full transcript stays readable afterwards. Several at once run
concurrently, bounded by `agent.max_parallel_subagents`:

```sh
rook session ls              # sub-tasks appear under ↳, linked to their parent
rook session show <child>    # everything the sub-agent actually did
```

Nesting stops at two levels, because past that the token cost compounds faster
than the work gets done.

### Memory

The agent can remember things across sessions, and you can read, correct and
audit what it believes:

```sh
rook memory ls                        # what applies in this workspace
rook memory search "how do deploys work"   # ranked, with why each matched
rook memory add --pin --global "never force-push to main"
rook memory since 7                   # what it learned or forgot this week
rook memory history                   # every recorded state
rook memory diff <objA> <objB>
```

Each fact carries where it came from — the session and turn that produced it —
so a wrong memory is traceable back to the turn that formed it. Every change
writes a new version, which is what makes `since`, `diff` and rollback possible.
Facts are scoped global or per-workspace, and only what matches the current
prompt enters the context, under a token budget.

### External tools

Any MCP server becomes a tool the agent can call. Declare it in
`~/.rook/config.toml`:

```toml
[[mcp]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[[mcp]]
name = "hosted"
url = "https://example.com/mcp"
headers = { Authorization = "Bearer …" }
```

A `command` is spoken to over its pipes; a `url` over HTTP, which may answer
either with JSON or with an event stream.

```sh
rook mcp ls                              # connect everything, report what it offers
rook mcp tools filesystem                # its tools and their arguments
rook mcp call filesystem read_file '{"path":"a.txt"}'   # no model in the loop
```

Servers connect concurrently and a failure is reported without stopping the turn —
one misconfigured server must not cost you the working ones. Tools are namespaced
`server__tool`.

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
  rook-llm      provider trait, OpenAI-compatible HTTP and the Anthropic Messages API
  rook-tools    read/write/edit/list/search/run, with the guards that keep a turn survivable
  rook-mcp      Model Context Protocol client: stdio and HTTP transports
  rook-lsp      Language Server Protocol client: diagnostics and navigation
  rook-acp      Agent Client Protocol server, so editors can drive it
  rook-proto    wire types shared by daemon, CLI and web
  rookd         HTTP backend, chat websocket, embedded web UI
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
- **Compaction summarises, but only the whole span at once.** There is no
  incremental or hierarchical summary of a very long session.
- **The CLI opens the store directly**, so it cannot run while `rookd` holds it.
  It says so clearly; routing the CLI through the daemon is
  [ADR-0006](docs/adr/0006-single-writer-store.md).
- **No structured plan state.** The agent is asked for a plan in prose and told
  not to keep a checklist — deliberately, on the strength of someone else's
  benchmark ([ADR-0010](docs/adr/0010-no-todo-tool.md)). There is nothing for a
  UI to render as progress.
- **Permissions are pattern matching, not a sandbox.** They raise the floor;
  `curl … | sh` is one obfuscation away from any rule. Not a jail.

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
