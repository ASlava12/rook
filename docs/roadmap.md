# Roadmap

What exists, what is next, and what is deliberately not being built. Ordered by
what unblocks the most.

## Done

- **Content-addressed store** — blake3 addressing, per-kind trained zstd
  dictionaries, inlining, mark-and-sweep GC with a caller-supplied expander,
  retention policy with real defaults, integrity verification, format versioning.
  Measured at 20.5× end to end. 13 tests.
- **Skills** — Agent Skills `SKILL.md` parsing with unknown fields preserved,
  environment detection, `requires` gating, `variants` bodies, source-then-version
  resolution with full mismatch reporting, catalog cards for progressive
  disclosure. 13 tests.
- **Skill and workspace versioning** — capture, history, diff, undoable rollback,
  budgeted captures that refuse rather than thrash. 11 tests.
- **Agent loop** — tool dispatch, `load_skill`, pre-request context budgeting,
  mechanical compaction, automatic checkpointing before any mutating tool call,
  everything logged to the store. 12 tests against a scripted provider; not yet
  exercised against a live model in CI.
- **Rewind and fork** — `rook session rewind` restores workspace files as well as
  the conversation, deleting files the turn created, and forks rather than
  truncating so the rewound-past turns stay readable.
- **Context visibility** — `rook session context` breaks the cost down by kind and
  separates what a fresh turn would carry from what is merely stored.
- **Tools** — paged `read_file`, `write_file`, unambiguous `edit_file`, `list_dir`,
  regex `search`, `run_command` with timeout, output cap and deny list.
- **Compaction** — the model summarises the span it replaces, into goal / done /
  open sections, and the summary is recorded in the log as a durable checkpoint:
  later turns and later processes start from it instead of replaying the span.
  A failed summary degrades to a marker rather than wedging the turn. 4 tests.
- **Permissions** — three modes and regex allow/ask/deny rules over what a call
  would actually do, defaulting to asking, with denial beating everything
  ([ADR-0009](adr/0009-ask-before-acting.md)). 12 tests, including the shipped
  deny rules checked in both directions.
- **Delegation** — a `delegate` pseudo-tool runs sub-tasks in child sessions with
  their own context and returns only their conclusions, so bulk stays out of the
  parent while remaining readable in the children. Several tasks run concurrently
  under a configured cap; one failing does not lose the rest. Depth-limited, with
  optional inheritance of the recent exchange. 7 tests.
- **Memory** — durable facts with provenance, global or per-workspace scope,
  pinning, and a version per change so `memory since`, `memory diff` and rollback
  all work. Retrieval is term overlap with prefix matching, budgeted into the
  prompt; the agent edits it through `remember`/`forget`/`recall`. This is the
  shape [hermes-agent#12238](https://github.com/NousResearch/hermes-agent/issues/12238)
  asked for. 7 tests.
- **Planning** — one system-prompt line rather than a checklist tool, and a goal
  the user sets on a session. Chosen against the obvious design on the strength
  of goose's own A/B benchmark ([ADR-0010](adr/0010-no-todo-tool.md)). 3 tests.
- **Code intelligence** — an LSP client and four tools over it: diagnostics,
  definition, references and workspace symbols, addressed by symbol name rather
  than by position. Known servers are detected on `PATH` and started lazily, and
  a file edited since it was opened is re-sent before it is queried, so the
  answer describes the code as it is now. The pool belongs to the session, not
  the turn, so servers are not restarted between turns. 10 tests against a mock server, plus a
  real check-edit-check turn against clangd.
- **Hooks** — five events with matchers, able to allow / ask / deny a tool call
  and to add context the model sees. A failing `pre_tool` hook blocks; no hook
  overrides the deny list. 7 tests.
- **ACP server** — `rook acp` speaks v1 over stdio: initialize, session
  new/load/list/prompt/cancel, streamed `session/update`, and approvals through
  `session/request_permission`. Field names come from the schema in
  `references/acp`, not from memory. 8 tests over duplex streams.
- **MCP client** — stdio and streamable-HTTP transports, written directly rather
  than via the SDK ([ADR-0008](adr/0008-hand-written-mcp-client.md)). An HTTP
  answer may be JSON or an event stream and both are handled; the session id from
  `initialize` is carried on later requests. Servers connect concurrently,
  failures are reported rather than fatal, and their tools join the toolbox
  namespaced `server__tool`. 16 tests against mock servers.
- **Providers** — OpenAI-compatible HTTP: Ollama, LM Studio, llama.cpp, vLLM,
  OpenAI, OpenRouter. `rook models` lists what an endpoint serves and `rook
  doctor` says whether the configured one is among them; the context window can
  be set explicitly rather than assumed from the provider's name. Streaming over
  SSE with a configurable idle watchdog, so a
  dropped connection surfaces as a stall instead of looking like deep thought.
  Tool calls are emitted only once whole. Providers that cannot stream get a
  one-shot fallback, so no caller branches on it.
- **Three front ends** — an interactive conversation with slash commands and
  interruptible turns, a CLI with `--json` throughout, a ratatui browser, and a
  web UI that both reads the store and drives a turn over a websocket. All three
  run turns, stream them and answer approvals the same way, over one engine.
- **Asides** — `/btw` answers a question from the conversation without tools and
  without joining it, recorded as a note the history replay skips. 2 tests.
- **Conversation continuity** — every turn replays the session log, so `--session`
  and the chat both continue a conversation rather than starting one.
- **Four platforms** — Linux, macOS, Windows tested on hosted runners; FreeBSD
  built and tested in a VM.

## Next

**Triage the reference backlog.** `cargo xtask refs status` reports drift from
seven upstream agents; nothing has been read past their issue trackers yet.

**A live-model smoke test in CI.** An Ollama service container running a small
model, so the HTTP provider and one full turn are exercised for real rather than
against a stub.

**CLI through the daemon.** Detect a running `rookd` and route through its API,
falling back to direct access. Removes the single-writer papercut
([ADR-0006](adr/0006-single-writer-store.md)).

## After that

**Supervising sub-agents while they run.** Delegation fans out and waits for all
of them. codex's `spawn_agent`/`wait_agent` lets a parent send messages to a child
mid-run, which is a different feature rather than a deeper version of this one.

**Hierarchical compaction.** A session long enough to compact twice summarises a
summary. goose's ladder of progressively larger removals is the reference.

**Memory consolidation.** Nothing merges near-duplicate facts or ages out stale
ones yet; the identity check only catches exact repeats.

**ACP beyond the basics.** Modes, config options, `fs/*` delegation to the
editor's buffers, and terminals are all defined and unimplemented.

**Auto-installing language servers.** Detection is done; the other half of
codex #8745 asks for installation too, which means downloading and running a
binary on the user's behalf.

**MCP provenance in tool results.** A result carries the namespaced tool name but
nothing about which server produced it; codex exposes that to its lifecycle hooks
and it belongs in ours too.

**Agent Plugins 1.0 packaging.** `plugin.json` bundling skills and MCP servers.
Defers to Agent Skills for the skill format, so skills authored today package
unchanged.

**Anthropic and Google native providers.** The OpenAI dialect covers a great deal
but not extended thinking or native prompt caching.

**Real sandboxing.** Today's boundary is a workspace path check and pattern
rules ([ADR-0009](adr/0009-ask-before-acting.md)) — text matching, not containment.
Platform-native containment — seccomp, Seatbelt, Capsicum, Job Objects — is a
serious piece of work and platform-specific by nature.

**Interactive web UI.** Would trigger revisiting
[ADR-0007](adr/0007-no-js-build-step.md).

## Not planned

- **A hosted service.** Local-first is the point; there is no server to send
  transcripts to, by design.
- **A bespoke skill format.** [ADR-0003](adr/0003-agent-skills-format.md).
- **Telemetry upload.** The config field exists so the answer is discoverable, not
  because it is going to become true.
- **An IDE extension per editor.** ACP is how that gets solved once.
