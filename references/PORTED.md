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
| Approval scope outliving a single turn | codex `a57b398` *require approval for input to escalated terminals* — the fix does not apply (no persistent terminals here), but its subject, approval scope across turns, exposed the opposite defect in ours | source, via `refs advance` | `agent::policy_for`, [ADR-0009](../docs/adr/0009-ask-before-acting.md) |
| A budget on advertised tool schemas | hermes `3fd70ad1` cutting one tool from 924 to 518 tokens a call — ours are already lean, so the value was the lesson that schemas drift unmeasured | source, via `refs advance` | `the_advertised_tool_schemas_stay_within_a_budget` |
| Planning without a checklist tool | goose [#11172](https://github.com/aaif-goose/goose/issues/11172) — a benchmarked A/B showing the tool is not the active ingredient; adopted its V2 variant and skipped the tool every reference ships | issues, not source | [ADR-0010](../docs/adr/0010-no-todo-tool.md) |
| Code intelligence from a language server | codex [#8745](https://github.com/openai/codex/issues/8745) *LSP integration (auto-detect + auto-install)* (564 reactions) — took detection, left installation | issues, not source | `rook-lsp`, `rook-core/src/lsp.rs`, `rook lsp` |
| An aside that does not clutter the conversation | opencode [#16992](https://github.com/anomalyco/opencode/issues/16992) *add /btw command* (376 reactions), asking for what Claude Code shipped | issues, not source | `AgentLoop::aside`, `/btw` |
| Discovering models from the endpoint | opencode [#6231](https://github.com/anomalyco/opencode/issues/6231) *Auto-discover models from OpenAI-compatible provider endpoints* (234 reactions) | issues, not source | `Provider::models`, `rook models`, `rook doctor` |
| MCP over streamable HTTP | codex `rmcp-client/src/bin/test_streamable_http_server.rs` — confirmed the `mcp-session-id` header and that a POST may be answered as JSON or as an event stream | source | `rook-mcp/src/http.rs` |
| Bounded scanning of a stream with no separator | goose 8a1b836 "bound HTML comment scanning" — the same shape (rescan-from-the-start over unbounded input) was in our SSE parser | source, via `refs advance` | `rook-llm/src/openai.rs` |
| Bounded parallel sub-agents | codex `max_concurrent_threads_per_session` — took the per-session cap, kept delegation synchronous rather than adopting spawn/wait | source | `AgentLoop::delegate` |
| Hooks at points in a turn | codex `codex-rs/hooks/schema/generated` — took the allow/ask/deny decision and added-context ideas, left twelve events and a schema each for five events and one reply shape | source | `rook-core/src/hooks.rs` |
| Speaking ACP to editors | `references/acp` schema v1 — read the JSON schema for exact field names and enum values rather than the prose | source | `rook-acp`, `rook acp` |
| Delegating a sub-task to a fresh context | codex `codex_delegate.rs`, `session/multi_agents.rs` — took the `fork_turns` idea (how much parent context a child inherits), left the async spawn/wait protocol | source | `AgentLoop::delegate`, `rook session ls` |
| Asking the user structured questions | hermes `7d6c6ae4` *clarify: schema diet + single questions[] interface (880 → 335 tok/call)* — took the one-questions[]-shape and the rule that options must never be written into the question text, and cut further: no per-question id, and the answer's own text is the "Other" row | source, via `refs advance` | `rook-tools/src/ask.rs`, `AgentLoop::ask_via` |
| A workspace boundary that a symlink cannot cross | codex `2926014` *make filesystem policy matching URI-native* — their case was encoded and case-variant paths, ours was a symlink out of the workspace that lexical containment could not see | source, via `refs advance` | `ToolContext::resolve`, `through_symlinks`, `sandbox.allow_outside_workspace` |
| Widening a fact's scope instead of keeping the first | hermes `3b672a68` *delete-path drops every scope of a removed id* — the fix does not apply (identity here is the text, not an id per scope), but its subject exposed the same hazard in ours | source, via `refs advance` | `MemoryBook::learn`, `Scope::within` |
| Durable memory with provenance and history | hermes ([#12238](https://github.com/NousResearch/hermes-agent/issues/12238)); read `hermes/tools/memory_tool.py` and `goose/crates/goose-mcp/src/memory` | source | `rook-core/src/memory.rs`, `rook memory` |
| Bounded logging and retention | codex SQLite growth ([#28224](https://github.com/openai/codex/issues/28224), [#17320](https://github.com/openai/codex/issues/17320)) | issues, not source | `RetentionPolicy`, `TelemetryConfig` |

## Triage log

`cargo xtask refs advance` prints what landed upstream since the pointer was last
moved; what follows is what was done with it, so a dismissal is a decision rather
than an omission.

**2026-08-27 (eighth pass)** — hermes advanced 12 commits.

- hermes `3b672a68` *delete-path drops every scope of a removed id* — **does not
  port, and found ours.** They delete an id across scopes; here a fact's
  identity is its text alone, so the same sentence learned globally and in a
  project is one fact and the *first* scope won. Probed both directions: the
  harmless one keeps the wider scope, and the harmful one leaves a fact the
  model asked to remember globally scoped to a single project, answered
  "already remembered", and silently absent everywhere else. `learn` now widens
  to the containing scope, and reports the case where neither contains the other
  rather than picking one.
- hermes `50d3c53f` *count log growth as watchdog progress* — **already the
  behaviour**: the stream watchdog is reset by any frame, not only by content.
- hermes `7a1aafb4`, `dc91b6b5`, `80be4890`, `91c475bf`, `71823be9`, `03f5302a`,
  `266d8ce2`, `2f0cec8e`, `adb29d85`, `79b8703d` — **not applicable**: Windows
  desktop updater recovery, PowerShell line endings, and JS formatting.

**2026-08-27 (seventh pass)** — codex and openhands advanced.

- codex `7c37479` *reduce skill catalog prompts with path aliases* — **does not
  port; the design it optimises is one we do not have.** Their catalog carries a
  locator per skill, so a shared root repeats fifty times and is worth aliasing.
  Ours carries name and description and no path at all. It did prompt the right
  question, though: the catalog also carried a version, and `load_skill` takes a
  name while `resolve` picks the version from the environment — so it was ~100
  tokens per fifty skills the model could not act on. Removed from the card,
  kept on the heading of a body that is actually loaded, where it is provenance.
- openhands `a917e36` *remove unused random tip code*, `1d6dcaf`, `9c83fdf`
  *documentation* — **not applicable**: their own dead code and contributor
  guidelines.

**2026-08-27 (sixth pass)** — cline and hermes advanced.

- hermes `e1e72f10` *register stdio MCP helper children in the spawn ledger and
  reap orphans* — **probed, mostly already handled, and one gap left open on
  purpose.** Both spawners set `kill_on_drop`, and a probe confirmed it: a
  handshake that never answers, one that times out, and a dropped session all
  leave nothing behind, which is now asserted by
  `dropping_a_server_takes_its_child_process_with_it`. What is not covered is
  SIGKILL or an abort, where no in-process cleanup can run. A ledger of PIDs
  would need to prove identity before killing anything — the codex commits
  triaged in the fifth pass were about exactly that failure — and on the four
  platforms here that means four ways to read a process's start time or
  environment. Killing a user's own rust-analyzer by PID reuse is worse than
  leaving an orphan they can see, so this stays open rather than half-built.
- cline `b4fd4ee` *tunnel ProtoBus over the existing Host Bridge* — **not
  applicable**: an internal IPC consolidation for their extension host.
- hermes `6170fff1`, `65335549` *managed SSH remote update engine*, `bf5ff510`,
  `53057f2b` *config migration on update paths*, `89f32fe4` *contributors
  registry* — **not applicable**: desktop update machinery and repository
  bookkeeping.

**2026-08-27 (fifth pass)** — acp, cline and codex advanced.

- codex `2926014` *make filesystem policy matching URI-native* — **does not
  port, but found a hole in ours.** Their concern was case-variant and encoded
  paths; ours was worse. `ToolContext::resolve` compared paths lexically, so a
  symlink inside the workspace pointing out of it went straight through: a probe
  read a file from outside and planted another one there, while every path in
  the error-free output looked contained. `resolve` now canonicalises the
  deepest existing ancestor before checking containment, and the refusal names
  where the path really led. It also exposed `allow_outside_workspace` as a dead
  field — nothing set it — so the refusal advised something impossible; it is now
  `sandbox.allow_outside_workspace`.
- cline `89970ea` *sign Windows CLI binaries with Azure Trusted Signing* —
  **worth revisiting, cannot do here.** `cargo xtask dist` ships unsigned
  binaries, so Windows SmartScreen will warn on every download. Signing needs a
  certificate this repository does not have. Roadmap, not done.
- codex `e56e492` *standalone tool outputs in `turn/start`* — **not applicable**:
  a turn here starts from a prompt or a replayed log, and there is no out-of-band
  tool result to inject.
- codex `ae357e7` *attach verified access context to plugin MCP calls*,
  `f374188` *harden managed proxy listener handoff*, acp `ae596e1` *docs: update
  registry agents* — **not applicable**: ChatGPT account entitlements, managed
  proxy internals, and an upstream documentation list.

**2026-08-27 (fourth pass)** — hermes advanced 13 commits.

- hermes `7d6c6ae4` *clarify: schema diet + single questions[] interface* —
  **ported, and it found a gap.** Rook had no way for the agent to ask the user
  anything: it could only write prose and hope. Took the single `questions[]`
  shape and the rule that options belong in `choices` and never in the question
  text, dropped the per-question id (answers echo their question instead), and
  let a typed answer stand in for the "Other" row. Its own lesson applied to
  ourselves: `ask` came in at 227 tokens a call, the most expensive tool we have,
  and was trimmed to 191 before it shipped.
- The same commit's measurement exposed a second defect: `ToolSpec::stub` copied
  the whole description, so lazy loading was advertising every tool's argument
  guidance to save only its schema. A stub is now the first sentence, which took
  the lazy path from 340 to 152 tokens a call.
- hermes `df3d41ee` *sweep aborted-fetch tmp_pack debris before it corrupts the
  pack directory* — **already handled**: payloads are written to `tmp/` and
  renamed, and `gc` reclaims orphans left by a crash in between.
- hermes `5d7ed70e` / `2812d612` *guard the post-update fleet check against PID
  reuse* — **not applicable**: Rook does not outlive a process to re-check a PID
  it recorded earlier.
- hermes `790e1eb6`, `b2c01136`, `f9135c18`, `ccdd7f41`, `f0045c53`, `5609ccbe`,
  `de2a9de7`, `77001a6b`, `8fdda828` — **not applicable**: Windows gateway
  service supervision, venv update recovery, and JS dependency hygiene.

**2026-08-27 (third pass)** — codex, cline and hermes advanced.

- hermes `708f84c4` / `39a5838f` *claim a message before enriching it; release a
  failed handler's claim so the turn isn't swallowed* — **does not port, but
  found two of ours.** Rook has no message-dedup problem, but the subject — a
  turn duplicated or swallowed around cancellation — pointed at both cancel
  paths. In `rookd` a cancel aborted the task and sent nothing, so the browser
  sat in its working state forever. In ACP it aborted the task that owed a reply
  to the pending `session/prompt`, so the editor waited for a JSON-RPC response
  that would never come. Both fixed; the ACP reply is now claimed with an atomic
  flag, since aborting a task that was about to reply would otherwise answer one
  id twice.
- codex `528fd7a` *retained-image budgeting* — **not applicable**: tool results
  here are text, and oversized ones are already windowed.
- codex `0d654e6` *track window and fork positions in turn metadata* — **worth
  revisiting.** Our fork point is a sequence number and the compaction point is
  an event; recording both on the session would make `session ls` show where a
  fork diverged without reading the log. Roadmap, not done.
- codex `a98b946`, `b9c4b9a`, `6e00841`, `0340e12`, cline `ee0982c`, and the
  hermes Slack/desktop fixes — **not applicable**: Guardian internals, browser
  plugins, a window title bar, and product surfaces with no counterpart here.

**2026-08-27 (second pass)** — codex, goose and hermes advanced again.

- hermes `3fd70ad1` *skill_manage 924 → 518 tok/call* — **measured, no diet
  needed.** Our six built-in schemas cost ~489 tokens together, against 924 for
  their one tool. The transferable part was that a schema grows unmeasured, so
  there is now a budget test.
- codex `a57b398` *require approval for input to escalated terminals* — **does
  not apply, but found a defect.** Rook has no persistent terminals and closes
  stdin. Its subject — approval scope spanning turns — prompted a check of ours,
  which had the opposite bug: "allow for the rest of the run" was forgotten at
  the end of the turn it was granted in, because every front end built a fresh
  policy per turn. Fixed; measured at two prompts before and one after.
- codex `37a5149`, `d61ba72`, `102ae5e`, `07d260c`, goose `f812cbd`, hermes
  formatting commits — **not applicable**: Guardian internals, Windows telemetry,
  CI automation and `npm run fix`.

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
