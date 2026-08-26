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
| Delegating a sub-task to a fresh context | codex `codex_delegate.rs`, `session/multi_agents.rs` — took the `fork_turns` idea (how much parent context a child inherits), left the async spawn/wait protocol | source | `AgentLoop::delegate`, `rook session ls` |
| Durable memory with provenance and history | hermes ([#12238](https://github.com/NousResearch/hermes-agent/issues/12238)); read `hermes/tools/memory_tool.py` and `goose/crates/goose-mcp/src/memory` | source | `rook-core/src/memory.rs`, `rook memory` |
| Bounded logging and retention | codex SQLite growth ([#28224](https://github.com/openai/codex/issues/28224), [#17320](https://github.com/openai/codex/issues/17320)) | issues, not source | `RetentionPolicy`, `TelemetryConfig` |

## Not yet read

Submodule pointers that have never been advanced since being added are, by
definition, unread beyond their issue trackers. `cargo xtask refs status` shows
how far each has drifted.
