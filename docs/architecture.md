# Architecture

[English](architecture.md) · [Русский](ru/architecture.md)

## The shape

```
                    ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
                    │  rook (CLI)  │   │  rook (TUI)  │   │   web UI     │
                    └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
                           │                  │                  │ HTTP/JSON
                           │                  │           ┌──────┴───────┐
                           │                  │           │    rookd     │
                           └──────────┬───────┘           └──────┬───────┘
                                      │                          │
                                 ┌────┴──────────────────────────┴────┐
                                 │            rook-core               │
                                 │  Rook: config · env · agent loop   │
                                 │  context budget · file captures    │
                                 └──┬────────┬─────────┬──────────┬───┘
                                    │        │         │          │
                            ┌───────┴──┐ ┌───┴────┐ ┌──┴─────┐ ┌──┴─────┐
                            │rook-store│ │  -skills│ │ -tools │ │  -llm  │
                            └──────────┘ └────────┘ └────────┘ └────────┘
```

Three front ends, one engine. The CLI, the TUI and the web UI are views over the
same [`Rook`](../crates/rook-core/src/service.rs) façade, which is what keeps them
from becoming three products that disagree about what the agent did. Anything the
web UI can show, `rook … --json` can print.

## Why the pieces are separate

**`rook-store` knows nothing about agents.** It stores bytes by content hash,
appends to session logs, and reclaims space. It does not know what a skill or a
checkpoint is — when garbage collection needs to know that a manifest keeps its
files alive, the caller supplies an
[expander](../crates/rook-store/src/maintenance.rs). That boundary is what keeps
the store small enough to reason about and testable on its own.

**`rook-skills` knows nothing about the store.** It reads directories and resolves
versions against an [`Environment`](../crates/rook-skills/src/env.rs). Persisting a
skill's history is `rook-core`'s job, using the store. A skill system that could
only work against one storage backend would be much harder to reuse.

**`rook-llm` has no branch on vendor.** One trait, one HTTP implementation of the
chat-completions dialect that Ollama, LM Studio, llama.cpp, vLLM and OpenAI all
accept. Providers with their own wire format — Anthropic's Messages API, Google's
`generateContent` — get their own implementation of the same trait, and the agent
loop never learns which is answering.

The wrappers around that trait are the same trait again, which is what keeps the
loop from having to know about any of them: `Retrying` waits out what means
*later*, `Limited` counts how many requests one endpoint may have at once,
`Failover` holds the list from `[models]` and moves on from an endpoint that is
not there, and `Proxy` decides whether a request leaves through a proxy at all.
The order matters and is the shape of what was meant — each candidate carries its
own retries and the failover sits on top, so "later" is answered where it was
said and only an endpoint that has run out of "later" hands over. Which endpoint
a caller gets depends on what it is: a turn wants the one it was pointed at,
because moving it part-way throws away its cached prefix, and an errand wants
whichever has room.

**`rook-contain` is the floor.** Platform glue and capability filesystem operations,
with no internal dependencies, and the one place Win32 lives. Its external
dependencies are cap-std and platform bindings, without native C builds, so
`cargo check --target x86_64-pc-windows-msvc -p rook-contain` works from a Mac
while the rest of the workspace does not. Anything may reach for it: starting a
process without a console window is its answer as much as containing one is.

**`rookd` is a separate binary from `rook`.** A container, a headless box or an
editor integration should be able to run the backend without linking a terminal UI
into it. Its `/api/chat` websocket runs a turn and streams it back, including the
approval round-trip, so the browser is a way to use the agent and not only to
read what it did.

## The agent loop

[`AgentLoop::run`](../crates/rook-core/src/agent.rs) is deliberately small enough
to read in one sitting. Per step:

1. **Budget check first.** [`ContextBudget`](../crates/rook-core/src/context.rs)
   compacts *before* the request when the estimate crosses the threshold. An agent
   that discovers the limit by being rejected has already lost the turn.
2. **Build the request.** System prompt with the detected environment and the skill
   *catalog* — cards, not bodies. Tool *stubs*, not full schemas.
3. **Call the provider.**
4. **Dispatch tool calls**, including the `load_skill` pseudo-tool that pulls a
   skill body into context on demand.
5. **Append everything to the session log**, bodies stored by content hash. The
   flush to disk is not per event — see
   [storage.md](storage.md#what-a-power-cut-can-take) for where it happens
   instead, and what that cost before it moved.

Two behaviours are structural rather than optional.

**Progressive disclosure.** A hundred skills cost a few hundred tokens per turn
instead of tens of thousands, and on local models a tool-heavy prompt is roughly an
order of magnitude slower to process than plain text. Cards and stubs are the
default; `lazy_skills` / `lazy_tools` in config turn them off, not on.

The two are not the same trade. A skill card defers the *whole body*, and
`load_skill` fetches it — the model asks by name. A tool stub defers only the
prose: the first sentence of the description, and every argument's name and type
without the guidance around them. There is nothing to fetch, because a tool
advertised without its shape could not be called at all.
`the_whole_advertised_tool_list_stays_within_a_budget` holds both numbers — the
full schemas under 2,500 tokens a request, the stubs under 1,100, which is what
is actually paid because lazy loading is the default — and asserts that stubs
cost less than half of full, or the deferral buys nothing. It lives in
`rook-core`, because the loop adds tools of its own on top of `rook-tools`, and
the two largest advertised are among them: the test lived in `rook-tools` first
and guarded 729 tokens of a list that cost 1,476. `cargo run -p rook-core
--example schema-cost` prints where they stand today. Each time the cap was
raised the commit says which tool did it and what was cut first.

The skill catalog is capped by `agent.max_skill_cards`, and what does not fit is
named as a count rather than dropped silently: `load_skill` answers an unknown
name with the skills that match it, so a skill off the end of the list is still
reachable by description. It is sorted by source, nearest first, with the name
breaking ties — for two reasons that happen to agree. What the cap cut used to be
whatever came last alphabetically, so a project's own skill lost to one Rook ships
with, for no reason anybody chose; and the list is the front of the request, so an
ordering that shuffles between turns invalidates the cached prefix of everything
behind it.

**The environment in the system prompt.** The model is told the OS, arch and
userland it is operating in, and which toolchains exist. This is cheap and it stops
the most common cross-platform failure in agent transcripts — reaching for GNU
`sed -i` semantics on a BSD box.

## Where data lives

```
~/.rook/                 (or $ROOK_HOME)
  config.toml            everything tunable, with bounded defaults
  secrets.toml           0600, and never in the store: a fork would copy it
  format.json            store format version; a newer one is refused, not corrupted
  rookd.addr             where a running daemon is listening; removed on shutdown
  store/
    index.redb           metadata, session logs, refs, and inlined small objects
    objects/aa/bb/<hex>  payloads too large to inline
    dicts/<kind>.zdict   trained zstd dictionaries
    tmp/                 staging; anything left here is crash residue
  skills/<name>/SKILL.md user skills
  plugins/<name>/        Agent Plugins: skills and MCP servers together
  servers/<name>/        language servers `rook lsp install` fetched
  running/               one file per turn in flight; one left behind is a turn that died
  output/                the whole of a runaway command's output
  cache/sources/         skill sources between searches; deleting it costs a download
  logs/
```

`running/` and `output/` are outside the store deliberately. The store takes one
writer and what is kept there has to outlive that writer's death, and a command's
output is the agent's record rather than the project's — in the workspace it
would be in every checkpoint and every `git status`.

One root directory rather than the platform-idiomatic split across config, data
and cache locations. An agent's state is one thing people back up, sync and
inspect together, and scattering it across three OS-specific paths turns "where did
my agent's memory go" into a support question.

Project-local skills live in `<workspace>/.rook/skills` and shadow user skills of
the same name — a skill vendored into a repository is there on purpose.

## Failure handling, on purpose

- **One broken skill does not empty the catalog.** Discovery collects errors and
  keeps going; `rook doctor` lists what failed to load.
- **A capture that exceeds its budget is an error naming the budget**, not a slow
  path that stages 45 GB.
- **The payload file is written before its index entry.** A crash between the two
  leaves an orphan file that `gc` reclaims; the reverse order would leave an index
  entry pointing at nothing.
- **GC is mark-and-sweep, not refcounting.** Refcounts drift after a crash or a
  manual edit, and a store that miscounts deletes live data silently.
- **Every object is verified on read.** The hash is recomputed after decoding.

## Reading further

- [storage.md](storage.md) — the on-disk format and where the compaction comes from
- [skills.md](skills.md) — authoring, versioning, variants
- [platforms.md](platforms.md) — the four targets and what actually constrains them
- [adr/](adr/) — the decisions, with their alternatives
- [research/agent-landscape.md](research/agent-landscape.md) — what this is built against

### Source authority

`rook-core::sources` labels retrieved material as JSON data with harness-owned
provenance. AGENTS files, skill catalogs/bodies and hook context travel outside
the system role; live tool results, replayed history, memory and compaction
inputs preserve the same boundary. Content-pinned sources can carry scoped
instructions, but never tool permissions. Replayed skills recheck the current
pins; old records without provenance remain data. `ask` separates actual chosen
answers from questions and appended hook data. The completion classifier records
refusals/blockers as incomplete (`blocked`), without forcing a refused task to
continue. Neither this classifier nor prompt framing is a deterministic security
boundary; tool policy and containment remain independent enforcement layers.
