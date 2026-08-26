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
- **MCP client** — stdio transport, written directly rather than via the SDK
  ([ADR-0008](adr/0008-hand-written-mcp-client.md)). Servers connect concurrently,
  failures are reported rather than fatal, and their tools join the toolbox
  namespaced `server__tool`. 10 tests against a mock server.
- **Providers** — OpenAI-compatible HTTP: Ollama, LM Studio, llama.cpp, vLLM,
  OpenAI, OpenRouter. Streaming over SSE with a configurable idle watchdog, so a
  dropped connection surfaces as a stall instead of looking like deep thought.
  Tool calls are emitted only once whole. Providers that cannot stream get a
  one-shot fallback, so no caller branches on it.
- **Three front ends** — an interactive conversation with slash commands and
  interruptible turns, a CLI with `--json` throughout, a ratatui browser and a
  read-only web UI, all over one engine.
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

**A conversation pane in the TUI.** The browser is read-only; running a turn
means dropping to `rook chat`.

**CLI through the daemon.** Detect a running `rookd` and route through its API,
falling back to direct access. Removes the single-writer papercut
([ADR-0006](adr/0006-single-writer-store.md)).

**Model-summarised compaction.** Today's is mechanical — head plus recent tail,
elision marked. A summarising pass over the elided span is strictly better; the
full transcript stays in the store either way.

**Memory beyond the transcript.** `Kind::Memory` exists and nothing writes it yet.
Wanted: durable facts with provenance, a diff of what a session learned, and the
same capture/rollback treatment skills already get — the shape
[hermes-agent#12238](https://github.com/NousResearch/hermes-agent/issues/12238)
asked for.

## After that

**MCP over HTTP.** The stdio transport covers local servers; hosted ones need the
streamable-HTTP transport.

**ACP server.** JSON-RPC over stdio, v1 stable, adopted by Zed, JetBrains, Google
and GitHub. One implementation replaces a plugin per editor.

**Agent Plugins 1.0 packaging.** `plugin.json` bundling skills and MCP servers.
Defers to Agent Skills for the skill format, so skills authored today package
unchanged.

**Anthropic and Google native providers.** The OpenAI dialect covers a great deal
but not extended thinking or native prompt caching.

**Sub-agents.** Delegating a bounded task to a fresh context. Requested across
[every](https://github.com/openai/codex/issues/11626) surveyed project. Worth doing
only once streaming and compaction are solid.

**Real sandboxing.** Today's boundary is a workspace path check and a deny list.
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
