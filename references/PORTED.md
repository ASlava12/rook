# Ported ideas

What Rook took from where, and at which upstream commit it was read. An entry is
an *idea* that was reimplemented, not code that was copied — see
[README.md](README.md#the-rule).

Add a row when you implement something after reading a reference. Add it to
[docs/roadmap.md](../docs/roadmap.md) instead when you decide *not* to yet.

| what | from | read at | where it landed |
|---|---|---|---|
| Session rewind that restores files as well as conversation | codex `/rewind` ([#11626](https://github.com/openai/codex/issues/11626)), `/undo` ([#9203](https://github.com/openai/codex/issues/9203)) | issues, not source | `Rook::rewind`, `rook session rewind` |
| Context-usage visibility | opencode `/context` ([#6152](https://github.com/anomalyco/opencode/issues/6152)) | issues, not source | `Rook::context_usage`, `rook session context` |
| Lazy tool/skill schemas | hermes ([#6839](https://github.com/NousResearch/hermes-agent/issues/6839)) | issues, not source | `ToolBox::stubs`, `SkillCard`, `load_skill` |
| `SKILL.md` format | Agent Skills specification | spec | `rook-skills` |
| Budgeted, content-addressed checkpoints | opencode's `git add .` failure ([#3176](https://github.com/anomalyco/opencode/issues/3176)) | issues, not source | `FileSet::capture`, `CaptureLimits` |
| Summarised compaction with a fallback ladder | goose `goose-context-management/src/summarize.rs` — took the structured summary and the "must not fail when needed most" framing, left the template engine | source | `AgentLoop::compact`, `Rook::last_compaction` |
| Three-way approval with per-command rules | goose `permission_inspector.rs` (AlwaysAllow/AskBefore/NeverAllow, defaults to asking) and its regex allow/ask/deny request; codex `approval_policy` | source | `rook-tools/src/policy.rs`, [ADR-0009](../docs/adr/0009-ask-before-acting.md) |
| Discovering models from the endpoint | opencode [#6231](https://github.com/anomalyco/opencode/issues/6231) *Auto-discover models from OpenAI-compatible provider endpoints* (234 reactions) | issues, not source | `Provider::models`, `rook models`, `rook doctor` |
| MCP over streamable HTTP | codex `rmcp-client/src/bin/test_streamable_http_server.rs` — confirmed the `mcp-session-id` header and that a POST may be answered as JSON or as an event stream | source | `rook-mcp/src/http.rs` |
| Bounded scanning of a stream with no separator | goose 8a1b836 "bound HTML comment scanning" — the same shape (rescan-from-the-start over unbounded input) was in our SSE parser | source, via `refs advance` | `rook-llm/src/openai.rs` |
| Bounded parallel sub-agents | codex `max_concurrent_threads_per_session` — took the per-session cap, kept delegation synchronous rather than adopting spawn/wait | source | `AgentLoop::delegate` |
| Hooks at points in a turn | codex `codex-rs/hooks/schema/generated` — took the allow/ask/deny decision and added-context ideas, left twelve events and a schema each for five events and one reply shape | source | `rook-core/src/hooks.rs` |
| Speaking ACP to editors | `references/acp` schema v1 — read the JSON schema for exact field names and enum values rather than the prose | source | `rook-acp`, `rook acp` |
| Delegating a sub-task to a fresh context | codex `codex_delegate.rs`, `session/multi_agents.rs` — took the `fork_turns` idea (how much parent context a child inherits), left the async spawn/wait protocol | source | `AgentLoop::delegate`, `rook session ls` |
| Durable memory with provenance and history | hermes ([#12238](https://github.com/NousResearch/hermes-agent/issues/12238)); read `hermes/tools/memory_tool.py` and `goose/crates/goose-mcp/src/memory` | source | `rook-core/src/memory.rs`, `rook memory` |
| Bounded logging and retention | codex SQLite growth ([#28224](https://github.com/openai/codex/issues/28224), [#17320](https://github.com/openai/codex/issues/17320)) | issues, not source | `RetentionPolicy`, `TelemetryConfig` |

## Triage log

`cargo xtask refs advance` prints what landed upstream since the pointer was last
moved; what follows is what was done with it, so a dismissal is a decision rather
than an omission.

**2026-08-27** — codex, goose, opencode and hermes advanced.

- goose `8a1b836` *bound HTML comment scanning* — **acted on.** The bug is a
  rescan-from-the-start over input that never terminates, quadratic against a
  hostile source. Our SSE parser had the same shape: no frame cap, and
  `buffer.find` restarting from byte zero on every chunk. Fixed and tested.
- goose `867a83c` *keep nested execute fences inert* — **does not apply.** It
  guards a local-inference mode where the model emits ```` ```execute ```` fences
  that get run. Rook takes tool calls from structured JSON only, and never
  executes anything it recognised in prose.
- codex `21ff2e8` *expose MCP provenance to tool lifecycle extensions* — **worth
  taking later.** An MCP tool result currently reaches the model with no marker
  of which server produced it. Recorded for the roadmap rather than done here.
- opencode `c2eacd7`, codex `daa3eaf`, hermes `15b673d` — **not applicable**:
  a Next.js redirect fix, a model-gating rule, and a provider change with no
  counterpart here.
