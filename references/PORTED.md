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
| Editing a record without undoing what landed beside it | codex `0b45b17` *preserve permissions when updating session metadata* — naming a session read it, changed the title and wrote it back, putting the turn's own event counters back to the stale read | source, via `refs advance` | `Store::update_session`, `Rook::name_session_from` |
| Reading a reply whose text arrives as parts | hermes `23bae43cf` *normalize list-shaped streaming content deltas* — one field typed as a string made the frame unparseable, and an unparseable frame is skipped, so the text vanished without a word | source, via `refs advance` | `rook-llm/src/openai.rs`, `Text` |
| A layering that fails the build instead of describing itself | goose `a9060fd` *stop enumerating crates in AGENTS.md* — the opposite conclusion: the list here is a rule, so what it needed was enforcement, not deletion. It found the table already drifted | source, via `refs advance` | `crates/rook-core/tests/layering.rs` |
| Measuring context by what the provider counted | hermes `d3a1c4651` *context size anchors on provider-reported usage* — ours estimated the messages and not the tool schemas, so the budget was short by the size of the tool list on every request | source, via `refs advance` | `measured`, `AgentLoop::run_with` |
| Naming the shell the model actually has | codex `5ed294d` *match Windows shell guidance to the executor platform* — the environment block named the OS and not the shell, and `;` chaining sent to `cmd.exe` fails as silently as GNU flags sent to BSD | source, via `refs advance` | `AgentLoop::system_prompt`, `rook_core::SHELL` |
| Telling the model what day it is | codex `430d26b` *classify clock tools as built-in control tools* — taken as a fact beside the prompt rather than a tool, since a date needs no round trip and must not sit in a prefix that is supposed to cache | source, via `refs advance` | `AgentLoop::request_messages`, `rook_store::today` |
| Resuming a session where it belongs | codex `f5636bb` *restore thread cwd from owned settings snapshots* — ours took whatever directory the command was run from, so the conversation was one project's and the edits another's | source, via `refs advance` | `Rook::following`, `rook run --session`, `rook chat --resume` |
| A tool name a model will accept | codex `94cbbdd` *support package-style MCP server names* — theirs widens what a server may be called; ours already took such a name and sanitised it, but a namespaced name over sixty-four characters makes the provider reject every request | source, via `refs advance` | `mcp::namespaced` |
| A sub-task that did not finish, said as such | hermes' five delegation commits — theirs reported a failed child as completed; ours reported an unfinished one in the same shape as a finished one | source, via `refs advance` | `AgentLoop::delegate`, `finished` |
| Knowing a conversation was paused, not continuous | openclaw's per-message timestamps — theirs stamps every line, ours marks a gap of an hour or more, which is the part that changes an answer | source, via `refs advance` | `AgentLoop::history`, `gap_before` |
| Choosing how long a cached prefix lives | goose `fb15d4e` *configurable Anthropic prompt-cache TTL* — ours wrote the five-minute default, so a pause longer than that paid to reprocess the whole prefix | source, via `refs advance` | `[agent] prompt_cache_ttl`, `CacheTtl` |
| A rollback whose undo point is real | hermes `1315e65a5`, `154fd10af` *failed rollback keeps the skill and the snapshots* / *name the preserved snapshot path* — ours captured first and discarded the result, then claimed undoability either way | source, via `refs advance` | `Rook::rollback_skill`, `Rollback::undo` |
| The effort the user asked for reaching the wire | hermes `b954547e7` *hand-rolled effort map inverted the ladder* — theirs was inverted, ours was absent: the OpenAI dialect sent no `reasoning_effort` at all | source, via `refs advance` | `rook-llm/src/openai.rs`, `crates/rook-llm/tests/openai.rs` |
| Waiting out a provider that said "later" | codex `a73bf25` *decouple HTTP retry backoff from overload integration testing* — theirs refines a retry; here there was none at all, and a 429 ended a turn that had run for minutes | source, via `refs advance` | `rook-llm/src/retry.rs`, `from_spec_with` |
| A limit that says what to do instead of only what it took | hermes `585723126` *limits line teaches spillover instead of a bare 50KB cap* — the mechanism does not fit a capture that never holds the middle, the lesson does | source, via `refs advance` | `elide_middle` |
| A sub-agent's budget being its parent's, not its own | codex `4761851` *account subagent token usage toward root goals* — ours already charged the tokens; what it did not bound was the step budget the model writes into the call, or how many sub-agents one turn may start | source, via `refs advance` | `AgentLoop::delegate`, `agent.max_subagents_per_turn` |
| Auxiliary work billed at the turn's thinking budget | hermes `213ae08e7` *guarded fast summary lane* — they routed summarisation to a cheaper lane; here it was one request that had never been given an effort at all | source, via `refs advance` | `AgentLoop::ask_for_summary` |
| Keeping what a restore is about to overwrite | cline `89c2efa` *refuse checkpoint workspace restore when HEAD moved past the checkpoint* — they refuse where a store that cannot keep the current state has no better option; ours can keep it, so a rewind became reversible instead of merely safe | source, via `refs advance` | `Rook::rewind`, `Rewind::files_kept` |
| Reporting a search hit inside a captured file | codex `57e2edc` *encrypt sensitive history and notes tool arguments* — encryption does not fit a store whose point is that it is readable, but the question behind it exposed a search that scanned files and could not report them | source, via `refs advance` | `Rook::captured_as`, `Hit::file` |
| Summarising only what the model was shown | hermes `1341dfbd` *exclude operational notifications from the tail anchor* — the same mismatch: their notifications, our checkpoint manifests and asides | source, via `refs advance` | `AgentLoop::summarise_span`, `replayed` |
| A deny list anchored to command position | hermes *anchor the mkfs hardline pattern to command position* — the same half-anchoring, and the same reasoning: an unoverridable rule that fires on a mention takes a harmless command away for good | source, via `refs advance` | `config::COMMAND`, `deny_list.rs` |
| Durable memory with provenance and history | hermes ([#12238](https://github.com/NousResearch/hermes-agent/issues/12238)); read `hermes/tools/memory_tool.py` and `goose/crates/goose-mcp/src/memory` | source | `rook-core/src/memory.rs`, `rook memory` |
| Bounded logging and retention | codex SQLite growth ([#28224](https://github.com/openai/codex/issues/28224), [#17320](https://github.com/openai/codex/issues/17320)) | issues, not source | `RetentionPolicy`, `TelemetryConfig` |

## Triage log

`cargo xtask refs advance` prints what landed upstream since the pointer was last
moved; what follows is what was done with it, so a dismissal is a decision rather
than an omission.

**2026-08-31 (twenty-fifth pass)** — codex 12, goose 5, openhands 4, opencode 3,
acp 1. Nothing taken; one gap found and recorded rather than guessed at.

- goose `7d97fe1` *preserve thinking and redacted-thinking blocks across turns* —
  **a real gap here, and not one to close blind.** `EventKind::Reasoning` is
  logged and `context::reaches_the_model` excludes it, and within a turn the
  assistant message pushed back carries content and tool calls and no thinking at
  all. So a model that reasoned before calling a tool re-derives that reasoning
  on the next step. Whether that is merely a loss of continuity or a refused
  request is unknown, and this project of all things should not guess: nothing
  here has run against a live model, which is the first item in the README's own
  list of what is not done. Closing it means an opaque thinking block with its
  signature on `Message`,
  filled and echoed by one dialect, and a wire detail implemented against
  documentation rather than a live model is how every request breaks at once.
  `cargo xtask smoke` is what would settle it. Written down in the roadmap so it
  is a decision rather than an omission.
- codex's Guardian cluster — nine commits on preserving a review's evidence and
  the user's answers across compaction. Not applicable: a check here runs in a
  fresh session that is told the claim and nothing else, so the parent compacting
  cannot reach it.
- The rest are a native voice build recipe, a desktop proxy, a settings nav, an
  a11y shortcut alignment, deepseek stats and a registry listing.

**2026-08-31 (openclaw, first read)** — added at the user's request and read for
the first time. Nothing ported yet; one thing recorded.

- openclaw *secret egress host binding* — a secret is bound to the exact HTTPS
  hosts it may be substituted into, and an unbound one fails closed rather than
  going out in plaintext. [The roadmap's secrets entry](../docs/roadmap.md) had
  the first half of this shape already — named, never valued, substituted at the
  edge — and was open about not knowing what stops a tool from being asked to
  print one. Binding to destinations answers it: the question stops being about
  tools and becomes one about where a request is going, and only `fetch` and the
  MCP client go anywhere. Still at the concept stage, which is where the user
  asked for it to stay.
- openclaw *date and time* — **taken, in a smaller shape.** They stamp every
  inbound message with a local timestamp and an elapsed-time suffix. Here the
  replayed history read as continuous however long the gaps, so a session picked
  up a week later looked like one paused for a moment. A gap of an hour or more
  is now marked where it happened; stamping every line would pay tokens on every
  request to answer a question nobody asks except across a gap.
- The rest of their release is transports and surfaces of their own — Discord and
  Slack logins, a browser-extension CDP relay, Buzz rooms, ClickClack menus, a
  macOS app, a Control UI. The architecture it is all hung from is one gateway
  with untrusted execution behind it, which is a different shape from a local
  agent with a single-writer store.

**2026-08-31 (twenty-fourth pass)** — hermes 42, codex 3, goose 2, opencode 1.
Two taken. hermes' remaining thirty-odd are their own: group-chat transports
across gateways, Telegram polling, dashboard auth cookies, and a long run of
native-compaction leases and backoff rows, which Rook has no equivalent of
because it compacts itself.

- goose `fb15d4e` *configurable Anthropic prompt-cache TTL (5m/1h)* — **taken.**
  The cached prefix was written with the unnamed five-minute default, so a person
  who thought for six minutes paid to reprocess the whole of it. `[agent]
  prompt_cache_ttl` chooses, and the reasoning is why it is a choice rather than
  a new default: the hour costs more to write and pays off only when the
  conversation outlives five minutes, which a conversation does and a scripted
  `rook run` — one turn, never reading the cache it wrote — does not.
- hermes `1b6ea1a2c`, `ec02d5179`, `5ce8f7155`, `5a134383f`, `b4d517438` — five
  commits on one theme: *a sub-agent that failed was reported to the parent as
  completed*. Here the failure itself is reported as one, but a child that ran
  out of steps read exactly like a child that finished: the stop reason was in
  the line and nothing else distinguished it, and a parent skimming five uniform
  blocks reads them uniformly. It now says it did not finish, and why, with what
  it managed still following — that is usually most of the work.
- codex `a9519cbc` *make the update_plan tool opt-in* — nothing to take, and
  worth recording: their checklist tool is now off by default, which is the
  direction [ADR-0010](../docs/adr/0010-no-todo-tool.md) went further in on the
  strength of goose's own A/B.
- codex `b7cd519c` *mark history ingestion requests in turn metadata* — already
  separate here: what a turn puts beside the newest message is never logged, so
  the replay that rebuilds history cannot mistake it for something said.

**2026-08-31 (twenty-third pass)** — acp 2, openhands 1, all documentation: a
registry listing twice over, and a save button disabled when an edit is reverted.
Nothing to take. The pass was worth making anyway, because looking at what the
research notes claim about Rook found three claims that had gone stale — ACP, MCP
and Agent Plugins were still listed there as *planned*.

**2026-08-30 (twenty-second pass)** — codex 4, hermes 26, openhands 2, acp 1.
One taken, and it was their new feature meeting our old bug.

- codex `94cbbdd` *support package-style MCP server names* — **their feature is
  our bug.** They widened what a server may be called to
  `npm:@modelcontextprotocol/server-sequential.thinking`; here such a name was
  already accepted and already sanitised into the tool namespace, but nothing
  capped the length. Models take sixty-four characters, and a name past it is not
  one tool refused — the provider rejects the whole request, so every turn fails
  for as long as the tool list carries it. The server half now gives way, since
  the tool half is what tells two apart, and what is cut is replaced by a digest
  of the whole name so two long servers do not become one.
- codex `cefa060` *approve the first Node REPL execution without a Guardian
  wait* — dismissed; there is no persistent REPL here, and an approval that
  skipped the gate on a first call is what [ADR-0009](../docs/adr/0009-ask-before-acting.md)
  refuses.
- codex `88f7765`, `da23e13`, openhands' two and acp's one — test working
  directories, JediTerm rendering, a clipped menu, a CI heading, and a registry
  list.
- hermes' twenty-six are almost all one thing: negotiating *native* compaction
  with a provider, and carrying that capability across model switches, resumes
  and proxies. Rook compacts itself, so there is nothing to negotiate. Two of
  them are worth naming because they confirm ports already made:
  `be9270378` *keep tool-schema tokens in the unanchored fallback estimate* is
  the defect `measured` was written for, and `4f2254350` *exactly one auxiliary
  request per attempt* is what `worth_compacting` already enforces. And
  `ef71f2cad` *deny by default on unattended platforms* is what `Unattended` has
  always done.

**2026-08-30 (twenty-first pass)** — hermes 124 commits, codex 4, opencode 2,
acp 1. Nothing taken, and two of the dismissals took reading our own code to be
sure of.

- hermes `6b290b81d` *reduce false positives in exfil_curl/exfil_wget patterns*
  and `21e52c1fd`, its sibling — **the lesson is already the rule here.** Theirs
  is a threat scanner matching env-var names anywhere in a command, so
  `$TRILLIUM_ETAPI_URL` tripped a rule meant for `$API_KEY`; the fix anchors the
  match. Rook has no such scanner and does not claim one — the deny list is
  pattern matching over what a call would do, and `COMMAND` already anchors every
  shipped rule at command position for exactly this reason, so `grep -r mkfs
  docs/` is not refused for saying the word.
- hermes `7e95b67ad` *revision pinning, canonicalization, case-fold guards* and
  `86dda3cd3` *fetch explicitly linked same-directory siblings on install* —
  dismissed after checking ours for the same family. `write_skill` takes file
  names from the model, and `safe_relative` refuses anything that is not a
  sequence of normal components, so `../` cannot write outside the skill's
  directory. `catalog::install` proves the name can be a directory before the
  `remove_dir_all` it guards, and `copy_tree` skips symlinks rather than
  following them, since `read_dir` reports a link as itself while `fs::copy`
  reads through it.
- hermes `514707ff3` *system prompt always rebuilds at the commit boundary* —
  already true and for a different reason: an `AgentLoop` is built per turn and
  `system_prompt()` is called each time, so an edited `AGENTS.md` reaches the
  next turn. Compaction rebuilds the whole request rather than the tail, which is
  the same defect they were fixing.
- codex `0a12b855a` *preserve Guardian authorization across history compaction* —
  does not apply: an approval granted for the run lives on the `Policy`, which
  compaction does not touch, and nothing here keys authorization off the
  model-visible conversation.
- The remaining hundred and twenty-odd are their own surfaces: Telegram and
  Discord menus, cron schedule grammar, a real-profile browser, desktop MCP
  OAuth, a skills hub, per-provider `request_overrides`, and todo snapshots —
  which [ADR-0010](../docs/adr/0010-no-todo-tool.md) declines. codex's other
  three are Vim motions in their composer; opencode's two and acp's one are docs.

**2026-08-30 (twentieth pass)** — codex 4 commits, acp 1.

- codex `f5636bb` *restore thread cwd from owned settings snapshots* —
  **ported, and it was the same hole.** Resuming a session by id used whatever
  directory the command was run from, so the transcript named one project's files
  while the turn read and edited another's, and nothing said so. A session is
  bound to a project — its checkpoints restore into it, its memory is scoped to
  it — so `Rook::following` moves the engine to where the session belongs unless
  `-C` says otherwise, which is the user deciding. Their second half, a setting
  falling outside the replay window after compaction, does not apply: the
  workspace is on the session record, not in the log.
- codex `4210c08` *preserve turn lineage across goal continuations* — dismissed;
  a goal here is one line of text on the session, and there are no automatic
  continuations to attribute.
- codex `aaa7ed0` *harden diagnostic report uploads*, `b8c8637` grammar in a
  prompt — dismissed; nothing is uploaded from here, by design.
- acp `9c211e2` *update registry agents* — a list of agents that speak the
  protocol, not a schema change.

**2026-08-29 (nineteenth pass)** — hermes 3 commits.

- hermes `835a913ff` *arm the failure cooldown when codex compaction fails* —
  **dismissed, and it took reading ours to be sure.** Their failed compaction
  frees nothing, so the next turn tries again and pays for another call; the
  cooldown is what stops that. Here a summary that cannot be produced still
  records a position — the span is dropped from the request either way and the
  events are still in the log, so the note says where to read them. Context is
  freed, and there is nothing to retry.
  `a_compaction_whose_summary_failed_still_moves_the_session_on` is that claim.
  The `Err` paths that remain cost no model call, and adding a wait to them would
  be a configuration field guarding nothing.
- hermes `0ffad55e0`, `4209d371a` — validating `/model` against a vendor's
  recommendation endpoint. `rook models` asks the endpoint what it serves, which
  is the same question without a vendor in it.

**2026-08-29 (eighteenth pass)** — hermes 46 commits, cline 1.

- hermes `1315e65a5` *a failed rollback restore keeps the skill and the
  snapshots* and `154fd10af` *name the preserved snapshot path in the ROLLBACK
  FAILED payload* — **taken, and the same claim here was unbacked.**
  `Rook::rollback_skill` took a capture of the current state first and threw the
  result away with `let _ =`, then printed "the previous state was captured
  first, so this is undoable" whether or not it had been. The capture now decides
  the message: a skill that is on disk must be captured or the rollback does not
  happen, one that is not says plainly that there was nothing to take, and the
  CLI prints the command that undoes it rather than the claim that something
  would. A restore that fails part way now names that capture in the error, which
  is the half of their fix about the payload.
- hermes `b954547e7` *route effort through canonical clamp_effort — hand-rolled
  map inverted the ladder* — **their bug is not here; looking for it found a
  worse one.** The Google ladder is monotone and Anthropic passes the rung
  through. The OpenAI dialect sent nothing at all: `--effort`, `/effort`, the ACP
  session setting and `[agent] effort` all reached the request and were dropped
  on the way to the wire. `reasoning_effort` goes out now, to the families
  documented to take it and to no others — most of what speaks this dialect is
  not OpenAI, and a strict server rejects an unknown field rather than ignoring
  it, which is hermes' own `31f0336da`.
- hermes `11b98a142` *two-line conversation clock* — already here as of
  `856fb43`, and taken the same way: beside the newest message rather than in the
  system prompt, so the cached prefix does not change every turn.
- hermes `c87058983` *tolerate malformed ports in the Ollama URL heuristic*,
  `31f0336da` *omit Ollama-only `think=false` on strict OpenAI-compat endpoints*
  — dismissed as written: there is no Ollama-specific request shaping here. The
  second one's rule is what the effort mapping above follows.
- hermes `b6bd681e8` *nested subtasks via optional parent field* — dismissed;
  [ADR-0010](../docs/adr/0010-no-todo-tool.md) declines the todo tool on the
  strength of goose's own A/B, and nesting it is more of what was declined.
- The remaining thirty-eight are provider plugins for endpoints Rook does not
  ship (Nebius, Ramp Router, TokenPlan), Slack gateway routing and busy modes,
  prompt text for their own identity, and contributor mappings.
- cline `48d6385` *thread task id into hook runner creation so execution
  telemetry fires* — dismissed; hooks here are matched and run per event with the
  session already in the payload, and there is no telemetry to correlate.

**2026-08-29 (seventeenth pass)** — hermes 934 commits, codex 31, cline 5,
goose 3, acp, opencode and openhands two each.

- goose `a9060fd` *stop enumerating crates in AGENTS.md Structure* — **taken as
  the opposite conclusion, and it found a drift.** They deleted a list from their
  agent-facing document because it goes stale. The list here is a *rule* — which
  crate may depend on which — so deleting it loses the rule; what was missing was
  anything holding it. `crates/rook-core/tests/layering.rs` does now, by rank
  rather than by edge, and it immediately showed the table in `CLAUDE.md` had
  drifted: `rook-lsp`, `rook-mcp` and `rook-acp` were absent from it entirely,
  and `rook-tools` had gained two dependencies since it was written.
- codex `0b45b17` *preserve permissions when updating session metadata* —
  **ported, and it was the same shape here.** Naming a session read its record,
  set the title and wrote the record back, in two transactions. `append_event`
  edits the same record — the sequence number, the event count, the token totals
  — inside one, so anything it landed between the read and the write was put back
  to what the reader saw. Naming happens at the top of a turn, beside the events
  the turn is already writing. `Store::update_session` does the read and the
  write together, and is now the only way an existing record is edited.
- codex `c2abf86` *run executor hooks for interrupted turns*, `f9cdc90`
  *preserve context baselines across nested agent forks*, `f742dab` *per-tool
  MCP output limits* — **not taken**: a hook here is per tool call and per
  prompt rather than per turn, a forked sub-agent starts with an empty context by
  design, and one output cap covers every tool because the cost being bounded is
  the context, which does not care which tool filled it.
- codex: Guardian primitives, plugin catalogues, Seatbelt policy names, app-server
  notifications and release packaging — **not applicable**.
- hermes `23bae43cf` *normalize list-shaped streaming content deltas* —
  **ported, and the failure it prevents is a silent one.** The dialect says
  `content` is a string; several servers that implement it send a list of parts
  instead. A field typed as a string made the whole frame fail to parse, and a
  frame that fails to parse is skipped — so the reply arrived empty with nothing
  anywhere saying why. This is aimed at self-hosted servers, which is where that
  shape is likeliest.
- hermes `0dd0f6e64` *clamp authorization gate lock timeout to prevent
  OverflowError on macOS* — **not applicable, and checked rather than assumed**:
  `tokio::time::timeout(Duration::from_secs(u64::MAX), …)` saturates and returns
  normally here. Theirs is asyncio's arithmetic, not a shape this has.
- hermes `bf8b28f27` *route browser snapshot storage through the symlink-safe
  writer* — **already held**: every path a tool touches goes through
  `ToolContext::resolve`, which follows symlinks before deciding, and there is no
  second writer that bypasses it.
- hermes: 145 desktop fixes, 56 gateway, 28 JS formatting, and the rest across
  kanban, cron, auth, update and their OpenViking memory provider — **not
  applicable**. Triaged by subject, which is the honest way to handle a branch
  landing of this size; what was read closely is the thirty-odd under `agent`,
  `tools`, `skills`, `memory`, `compression` and `context`, and most of those had
  already been triaged in earlier passes because the merge replays them.
- cline `cea134b` *prevent hook spawn failures from crashing the core process* —
  **already held**: a hook that cannot be spawned is caught where it is invoked,
  and for `pre_tool` it becomes a denial rather than a crash or a silent pass,
  which is the direction that matters.
- codex, acp, opencode, openhands — registry docs, model catalogues, Linux ARM64
  artifacts, telemetry flags and console animations: **not applicable**, product
  surfaces and release plumbing this does not have.

**2026-08-28 (sixteenth pass)** — codex 11 commits, hermes 70, goose 3, cline,
opencode and openhands one each.

- codex `5ed294d` *match Windows shell guidance to the executor platform* —
  **ported, and this session had already paid for not having it.** The
  environment block names the OS, the userland, the arch and the toolchains, and
  the comment beside it explains that telling a model it is on BSD stops it
  reaching for GNU `sed -i`. It did not name the shell. `;` does not chain in
  `cmd.exe` and `$(…)` is not substitution there, and neither fails loudly — the
  line runs as something else, which is exactly what happened to four tests of
  ours on Windows this morning.
- codex `430d26b` *classify clock tools as built-in control tools* — **taken as
  the fact rather than the tool.** What a clock is for is a model knowing what
  "now" is instead of guessing from its training, and a fact does not need a
  round trip. It goes beside the prompt, not in the system block: a date is the
  example [CLAUDE.md](../CLAUDE.md) names when it says the front of a request
  must not vary.
- hermes `d3a1c4651` *context size anchors on provider-reported usage —
  estimation shrinks to the last turn* — **ported, and it was covering an
  under-count.** The estimate here is `len / 4` over the messages and does not
  count the tool schemas, which are ~750 tokens of every request by default. Both
  the compaction threshold and the overflow check turned on that number, so the
  budget was optimistic by the size of the tool list. The provider counted what
  it actually received; anchoring on it leaves only what has been appended since
  to estimate. Taken only when the report is at least what the text plainly
  weighs — several local servers report a constant, and under-counting is the
  direction that ends a turn with a limit error.
- hermes `c7761573f`, `93f4dc756`, `c6a426e9a` *prune positionally unanswered
  tool_calls before the send* — **already held**: `close_open_call` answers a
  call the log never answered, and a result with no call ahead of it is dropped
  rather than sent. Both directions, and for the same reason they give.
- hermes `b855f86bc` *413 recovery measures bytes, not token estimates* — **not
  applicable yet**: nothing here recovers from a 413, it refuses before sending
  with `ContextOverflow`, and the anchor above is what makes that refusal
  accurate.
- hermes `95cf7dc9e` *session temp root off tmpfs* — **not applicable**: the
  staging directory is under `ROOK_HOME`, not `/tmp`, and is swept by age.
- hermes `72874b067` *skill_manage operations[]*, `baa344dee`, `7b5e1911f`,
  `a9e72f1b5` schema diets, the desktop, gateway, install and profile commits —
  **not applicable**: batched skill operations are a shape this does not have,
  and the diets are the exercise this repository already runs as a test.
- goose `cfc0538` *parse OpenRouter nested cache_write_tokens* — **not
  applicable**: cache reads are what the budget and the cost line use, and they
  are read from the field every dialect puts them in.
- codex `39507ee` *reject NUL bytes in reviewed terminal input* — **already
  held, by the platform**: `Command::arg` refuses an interior NUL when it builds
  the argument, so the spawn fails rather than the shell seeing a truncated line.
- codex `7625343`, `92f887e` *preserve cached MCP tools during binding capture*;
  `1cc81ca`, `8faf725` *compression for shared rollout lineages*; cline's hub
  timeout tests; opencode's model list; openhands' profile stamp — **not
  applicable**: shared rollouts, hub sessions and profile stamps are shapes this
  does not have.

**2026-08-28 (fifteenth pass)** — hermes 40 commits, codex 4, cline, goose and
opencode one each; acp and openhands unchanged.

- codex `a73bf25` *decouple HTTP retry backoff from overload integration
  testing* — **ported, and the gap was total.** Their commit refines a retry
  that exists; here there was none. A 429 or a 529 — the two things a hosted
  endpoint says most when it is busy — ended the turn, and an autonomous agent
  is exactly where nobody is watching to ask again. Wrapped around the provider
  rather than written into each of the three dialects, and only the request is
  retried: every dialect checks the status before returning the stream, so a
  failure that reaches the wrapper has emitted nothing to duplicate.
- hermes `585723126` *limits line teaches spillover instead of a bare 50KB cap*
  — **ported as the lesson, not the mechanism.** Their fix saves the full output
  and hands back a path. Ours cannot: `Ends` keeps a head and a tail and
  discards the middle as it streams, which is what makes a runaway command cost
  bounded memory. What did apply is that a bare byte count leaves the reader to
  guess; the elision now says what to do instead.
- hermes `ae8c97603` *stdout spillover to cache with a read_file recipe* —
  **not taken.** Spilling to disk is a real feature with a real budget of its
  own, and it trades away the property the current design was built for. A
  roadmap entry, not a pass.
- hermes `c30ac90a9` *rebuild dynamic tool schemas at the compaction boundary so
  forever-sessions pick up config changes* — **not applicable**: an `AgentLoop`
  is built per turn and `tool_specs()` runs per request, so there is no schema
  cached across a session to go stale.
- codex `dc2ccc6` *make subagents follow the root service tier* — **already
  held, and deliberately not entirely**: a child inherits its parent's tools,
  policy, approver, hooks, language servers, step ceiling and sub-agent
  allowance. Effort is the exception and is set low on purpose.
- goose `dd8f5ed` *remove unused unstable ACP methods*; cline `ce71fe5` and
  opencode `1be9fd5` *model list updates*; hermes' image-corrupt retries,
  skills-guard tuning, cron profile hardening, bot-mode and remote kernels —
  **not applicable**: multimodal input, a Python skills linter, hosted profiles
  and remote execution backends are none of them shapes this has.

**2026-08-28 (fourteenth pass)** — all seven advanced: hermes 77 commits, codex
34, cline 21, opencode 11, openhands 4, acp 2, goose 1.

Four ports. Three are the same shape — a limit that exists but is not the one in
force at the point that matters — and the fourth is a cost nobody had priced.

- codex `4761851` *account subagent token usage toward root goals* and `2d929eb`
  *honor turn token budgets in Guardian review rollover* — **ported, twice, and
  the defect was worse than theirs.** Our children's tokens were already charged
  to the parent, so the accounting half was done. What was not: `max_steps` for a
  child was read straight out of the tool arguments, so the configured ceiling
  was whatever the model wrote — a probe asked for 50 against a configured 3 and
  got 9 steps. And the list of tasks has no length, so one `delegate` call was
  `tasks x max_steps` model calls with nothing bounding either factor. The step
  budget now only ever shortens, and the sub-agent count is capped per turn and
  shared with the children, so a child that delegates again spends the same
  allowance rather than opening a new one.
- cline `89c2efa` *refuse checkpoint workspace restore when HEAD moved past the
  checkpoint* — **ported as the opposite answer, for the same reason.** Their
  restore could destroy work committed after the checkpoint, so they refuse. Ours
  could destroy an edit made by hand, which no checkpoint holds; but a
  content-addressed store can keep it for the cost of a hash, so the restore now
  captures what it is about to write over, onto the fork it just made. Refusing
  makes a rewind safe; capturing makes it reversible.
- cline `9e7c1a3` *CLI crash when a remote MCP server is offline but enabled* and
  codex `124e560` *make the optional MCP startup grace configurable* — **already
  held**: `connect_mcp` connects concurrently, collects failures instead of
  propagating them, and each server's `startup_timeout_secs` is configurable.
- hermes `2c6938dc3` *retry a stalled summary on the fallback chain* — **already
  held**, by a different mechanism: the summarising call runs through the same
  provider as the turn, so `stream_idle_timeout_secs` ends a stall, and the
  fallback records a compaction that says where to read the span instead.
- hermes `213ae08e7` *guarded fast summary lane*, `372c4cdfc` *certify the
  effective fast route* — **ported as the one line it amounts to here.** They
  built a route; the idea under it is that condensing a transcript is mechanical
  work that should not be billed at the turn's thinking budget. A sub-agent here
  already runs at low effort for that exact reason, and the summarisation call —
  the other auxiliary request — asked for whatever the provider does by default.
  Now it asks for low, like the sub-agent.
- codex `94311d4` *forward history note images to the model* — **not
  applicable**: nothing here is multimodal, which is a gap in
  [docs/roadmap.md](../docs/roadmap.md) rather than a defect in this.
- codex `1932143`, `dc031d4` *propagate executor OS / PowerShell version into
  turn environments* — **not applicable**: one process, one machine; the
  environment a skill is matched against is this one.
- opencode `517ee73` *filter unreplayable Bedrock reasoning before caching* —
  **not applicable**: reasoning is logged for a person to read and is not
  replayed into a request, so there is nothing unreplayable to filter.
- codex `6be2a6c` *let the history backend enforce tool output budgets* —
  **already held**: `sandbox.max_output_bytes` bounds capture at the tool, not
  at the store, which is the earlier of the two places.
- cline `691fcb6` *discover global rules at ~/Cline/Rules*, `29530ca` *searchable
  session history*, `2208d18` *discovery boundary for Agent Plugins* — **already
  held**: user skills under `~/.rook/skills`, `rook search`, and `rook-core`'s
  plugin loader respectively.
- codex `7d6f808`, `f6726e8`, `e325e3a`, `8935ff1` and the Guardian, plugin
  catalog and telemetry commits; hermes' desktop, cron, kanban and computer-use
  work; opencode's Azure auth and console charts; openhands' Canvas frontend;
  acp's registry docs; goose's model list — **not applicable**: product surfaces
  and services that have no counterpart here.

**2026-08-27 (thirteenth pass)** — hermes moved 1509 commits, codex one.

A long-lived branch landed on hermes' default branch, so the pointer jumped by
more than a release. Triaged by subject rather than one by one, which is the
honest way to handle a bulk landing: 234 desktop fixes, 78 gateway, 49 JS
formatting, and the rest across Slack, Telegram, cron, kanban and their
OpenViking memory provider — none applicable. What was read closely is the
thirty-odd commits under `agent`, `tools`, `context`, `tui`, `cli` and `memory`.

- hermes `anchor the mkfs hardline pattern to command position` and *widen the
  command-position anchor to the whole hardline class* — **ported, and it was
  the same defect.** Our deny rules anchored their argument but not the command,
  so `grep -r mkfs docs/` and `echo 'never run mkfs on a live disk'` were refused
  outright — and nothing overrides a denial, so those commands were simply gone.
  The comment beside the rules already said a deny list that cries wolf gets
  turned off; it was half right about its own rules.
- hermes `fix(context): prefer max_input_tokens over max_tokens for Anthropic
  proxies` — **not applicable**: `agent.context_window` is configured or taken
  from the provider, and a proxy that reports both is a shape we do not meet.
- hermes `fix(agent): honor prompt_caching for custom providers`, `normalize
  custom provider route identity` — **not applicable**: one provider per spec
  here, with caching decided by the dialect rather than by a route table.
- codex `d5cacec` — **not applicable**: their own release tooling.

**2026-08-27 (twelfth pass)** — hermes advanced 26 commits.

- hermes `1341dfbd` *exclude operational notifications from the tail anchor* —
  **ported, and it was the same defect.** Compaction summarised the whole
  transcript while the replay that builds the model's messages uses five event
  kinds. So a checkpoint manifest, an aside and a failed skill load were
  summarised as if they were conversation, and the model was handed a summary of
  things it had never seen. Compaction now reads what the replay reads.
- hermes `11cf59d9` *adopt live compression config on the next turn* —
  **partly here already**: approvals and effort are live in every front end. The
  storage settings still need a restart, which is a smaller thing and stays open.
- hermes `0a4d3aba` *fail closed when a profile's state.db will not open* —
  **already the behaviour**: `Store::open` errors and names the path and the
  probable holder.
- The remaining 23 — Electron desktop model pickers, gateway dial guards, SSH
  update fleets, WSL2 keepalive windows, voice-note relay — **not applicable.**

**2026-08-27 (eleventh pass)** — codex and openhands advanced.

- codex `57e2edc` *encrypt sensitive history and notes tool arguments* — **does
  not port, and asked a question we had not answered.** Encrypting at rest is
  the wrong shape here: the store's whole claim is that it is inspectable, and a
  key would have to live beside it. But the question behind it — what happens
  when a secret reaches the store — turned up a real defect. A checkpoint
  captures `.env` because it must, and `rook search` scanned those objects and
  could never report them: only an object that was the body of an event became a
  hit. So the one tool for finding where a secret went could not find it. Fixed,
  and the exposure is now stated in the README and `docs/storage.md` along with
  the remedy that already existed.
- codex `72c9659` *preserve tool authority for TUI delegation prompts* —
  **already the shape here**: a sub-agent inherits the parent's approver and
  policy rather than being given its own.
- openhands `1697faa` *validate api key before advancing* — **already covered**:
  an empty key reads as unset and says so, and `rook doctor` probes the provider.
- codex `f1433fc` *developer instructions for persistent mode* — **not
  applicable**: their own contributor documentation.

**2026-08-27 (tenth pass)** — hermes advanced 25 commits.

- hermes `1dc552d5` *terminal: honest schema, pager defaults in the env
  (837 → 670 tok/call)* — **measured, nothing to change.** Their fix sets pager
  and colour variables so a command's output is not full of escapes. Probed ours
  with `TERM=xterm-256color` and `CLICOLOR_FORCE=1` inherited: `git log`, `git
  status` and `cargo` all returned clean text, because stdout is a pipe and they
  detect it. stdin is already `/dev/null`, so an interactive prompt gets EOF
  rather than hanging to the timeout. Their schema-size half we did in the
  fourth pass.
- hermes `1f4d095f`, `830e4a29`, `42046b45`, `f1d05ce7` and the rest of the
  browser series, plus the PROOF workflows — **not applicable**: copying a real
  Chrome profile with its cookie database, and the Windows file-locking that
  makes it hard. Rook has no browser tool.
- hermes `f780cb36` *drop the cua_browser_* route*, `791e2ae3` *JS formatting* —
  **not applicable.**

**2026-08-27 (ninth pass)** — codex and goose advanced.

- goose `caf5951` *enforce permission policy for app tool calls* — **does not
  port, and found ours.** They had a path where tool calls skipped the policy;
  here `write_skill` was one. It is implemented by the loop rather than the
  toolbox, and the loop handled every pseudo-tool before the gate — so in
  `readonly` mode, whose whole promise is that nothing changes the machine, the
  agent still wrote files into `~/.rook/skills` and captured them into the
  store. Probed and confirmed before changing anything: `write_file` refused,
  `write_skill` did not. The gate now takes a supplied risk so a pseudo-tool
  that changes the machine answers to it too.
- codex `81e1800` *scope extension capabilities to invocation lifetimes* —
  **already the shape here**: an approval granted "for the run" lives on the
  policy the front end owns, and a sub-agent inherits it rather than widening it.
- codex `5af6979`, `307ce6c`, `eed1dee` — **not applicable**: their exec-server
  test pin, Guardian analytics, and gRPC trace propagation.

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

**2026-08-27 (fourteenth pass)** — cline 5, hermes 4, codex 2, opencode 1,
openhands 1.

One of the thirteen was worth acting on, and it found a hole here rather than
something to port. codex #41072 forwards the model's confirmation policy to
tools reached through MCP; Rook did not have that policy to forward. `McpTool`
never implemented `risk`, so it inherited the trait's default — `ReadOnly` — and
`Policy::decide` returns `Allow` for read-only before it consults the deny list,
before it checks `readonly` mode and before any rule. Every tool of every
connected server ran unasked, in every mode. MCP tools are now their own risk,
matched by the namespaced name they are called by.

The protocol offers `readOnlyHint`, which is the obvious thing to trust and the
wrong thing to trust: it is the claim of the party whose behaviour is in
question. It is parsed, shown to the user in the approval prompt, and does not
decide.

Dismissed: cline's Windows Authenticode installer (#13607) is the certificate
problem already on the roadmap; its desktop work on scheduled-task output,
schedule listing and voice-badge tooltips (#13610–13613) has no counterpart
here. hermes' four are internals of their own compression path. opencode #45503
merges duplicate rows in a Go usage table. openhands #16931 skips onboarding
when a local backend already has a usable model — a good idea for a first run,
but Rook's first failure already names what to do, and detecting a running
Ollama is a separate piece of work rather than this one.

**2026-08-27 (fifteenth pass)** — hermes 21, codex 2.

hermes' approval work — clamping `approvals.timeout` at the config-read
chokepoint (#83220), failing closed when the deadline import is missing,
enforcing an explicit timeout on the guardian call, clamping the authorization
gate's lock timeout — is one concern in four commits: a question put to a person
must be bounded, and must fail closed when it is not answered. Rook was bounded
in two front ends and unbounded in the third. Over ACP an editor that opened the
permission dialog and never answered held the turn, its language servers and the
store's single write lock for the life of the connection. The bound was also a
number written out four times, twice as 300 seconds and twice as 600, and
settable nowhere.

Their `fix(hermes_cli): stop config set/unset from wiping user overrides on
invalid YAML` was checked here and does not apply: `Config::load` refuses a
malformed file and nothing writes over one it could not read — `rook init`
against a broken `config.toml` fails and leaves the file untouched.

codex #41094 requires synchronous review for sensitive MCP actions, a follow-up
to the #41072 acted on last pass. Rook has no deferred-approval path to make
synchronous: `decide` returns `Ask`, the approver is awaited, and the default
approver refuses. What it did lack was the bound above, which is the same
concern from the other end.

Dismissed: hermes' remaining seventeen are internals of their own desktop,
gateway, cron and conformance work. codex #41087 exposes usage metadata in
completion events; Rook carries it on `Delta::Done` and accumulates it in the
turn's outcome, but does not report a running total mid-turn — worth doing, and
not this.

**2026-08-27 (sixteenth pass)** — hermes 36, codex 4, opencode 2, acp 1.

Two were worth checking here and both found the behaviour already right, so what
came of them is tests rather than fixes — a property nothing pins is one that can
be removed by accident.

hermes' `fix(hermes_cli): scope hook timeouts and fail closed on pre_tool_call`
is two claims. Rook bounds a hook by its own `timeout_secs` rather than a shared
one, and treats a timeout as a failure like any other — which for `pre_tool`
means deny, since a hook that cannot answer must not become an approval. Pinned:
a `pre_tool` hook that sleeps thirty seconds against a one-second timeout refuses
the call and does not wait it out.

codex #41118 propagates a parent's trusted skills to delegated workers. A child
here resolves against the same index and the same environment — the index is
behind a lock on the shared `Rook`, not copied — so what the parent could load
the child can, including a skill written during the run. Pinned by delegating a
task that loads one.

Dismissed: codex #41117 freezes plugin roots in MCP tool attribution, which is
already true here for a different reason — plugins are discovered once when the
store opens, so a run's attribution cannot move under it. Guardian V2 metrics and
async test timeouts are their own subsystem. hermes' remaining thirty-four are
desktop, bots, gateway, cron and browser-daemon internals, an OpenRouter model
catalogue, and contributor mappings; their browser work on recycling a wedged
session after a timeout is about a daemon Rook does not have — the equivalent
here is killing the process group, which `exec` does. opencode's two are a model
mapping and generated code. acp's one is a documentation update.

**2026-08-27 (seventeenth pass)** — codex 9, openhands 2, and the four that were
already up to date.

codex #41152 fails closed on unbounded parent compactions, which is the same
shape as a defect found here hours earlier from the other end. Theirs is about a
parent compacting without a bound; here the bound was missing in a narrower way.
A span too small to summarise leaves the context exactly as full as it was, so
the check at the top of the next step is true again — and the step after that.
Measured: seven summarisation calls in one turn to stand still. A turn compacts
once per turn that it achieves something now.

Dismissed with reasons: #41165 requires explicit requests for spawn model
overrides, and a sub-task here shares the parent's provider — there is no
override to request. #41162 resolves token budgets from each step's active
model; one loop has one provider and one budget. #41143 limits inline diff
previews in the TUI, which shows a change summary rather than diffs and says why
in the code. #41159, #41158, #41151, #41150 and #41146 are Guardian V2 and their
own skill machinery. openhands' two are a model catalogue and a cloud-only
conversation rename.

**2026-08-27 (eighteenth pass)** — hermes 65.

The pointer was advanced during the seventeenth pass and the commits read, and
then the log entry was not written. Recording it late is worth doing: an
advanced pointer with no entry is exactly the silent omission this file exists
to prevent.

Acted on: `refactor(delegate_task): tasks-only interface + depth-derived
delegation (1,201 → 773 tok/call, −36%)`. Rook's `delegate` was already a
quarter of theirs, so the −36% was not there to take, but the shape was: it
advertised both a bare `task` and a `tasks` list, which is what led a live model
to fill both and run every sub-task twice. It advertises the list and still
accepts the bare field. Measuring it turned up the larger problem — the schema
budget test and the tool that prices it both lived in `rook-tools`, which cannot
see the six tools the loop adds, so they guarded 729 tokens of a list that costs
1,476.

Considered and not done, for now: `fix(mcp): signal reconnect from the mid-call
fast-fail site too` and `fix(mcp): correct inverted liveness check in
_stdio_children_dead`. Rook's stdio client has no inverted check — a timeout
removes the waiter, the reader awaits the lock rather than trying it, and the
pending map is cleared when the pipe closes. It also does not reconnect: a
server that dies takes its tools out for the rest of the run, and every later
call returns the transport error. That is honest but poor, and it is a piece of
work rather than a line, so it is named here rather than half-done.

Wrong within the hour, and the correction matters more than the paragraph: the
reconnect landed seven minutes after this was written, in the same pass.
`request_with` restarts a server whose transport failed and retries the call
once, bounded by `MOST_RESTARTS`, and leaves alone anything the server itself
answered — an rpc error is a working server. The paragraph stood for four days
saying the opposite, which is the one thing a log of decisions must not do.

Dismissed: `fix(sessions): don't let the empty-session sweep delete an archived
transcript` — there is no such sweep here; retention deletes by age, size and
tag. `fix(gemini): embed images in Gemini 3.x functionResponse.parts` — a tool
result is text throughout Rook, so there is no image to embed. The remaining
sixty are gateway liveness and launchd restarts, desktop STT and cloud auth,
cron booking, WeCom streaming, kanban connections, plugin handlers for their own
platform adapters, YAML provider-key coercion, and contributor mappings.

## Eighteenth pass — cline, goose, opencode, openhands, acp

Acted on: cline's `Clarify model-facing message when user rejects a tool call`.
Three front ends each wrote the literal `"the user declined"`, which reaches the
model as `refused: the user declined` — indistinguishable from a fault, and a
fault is something a model routes around. Theirs appends "NOT a tool or system
failure. Clarify with user before proceeding."; Rook now has one
`Approval::declined()` saying nothing failed, that no other tool or sub-agent
will be allowed the same thing, and to ask the person what they would rather —
the wording the unattended refusal already uses, with somebody present to ask.

Acted on, from goose's `fix(security): restrict extension tool dispatch`: their
hole was a fallback that split `server__tool` and dispatched by prefix when the
lookup missed, so a tool the server implements but never advertised — one
filtered out of the list — could still be called. Rook cannot do this: it builds
one `McpTool` per advertised descriptor and dispatches by exact registry lookup,
and `Registry::without` removes the tool rather than hiding it. But looking for
the equivalent found a real one next door. A read-only checker drops
`write_file` and `edit_file` by name, and `delete_file` was added to the toolbox
months after that line was written, so a checker could delete the work it was
judging. The list is now `CHANGES_FILES` beside `CHANGES_THINGS`, and a test
pins the set a checker is left with so the next tool cannot arrive unweighed.

Dismissed: `fix(gdk): preserve tool-call indices in streaming responses` — the
OpenAI dialect here already assembles by `call.index`. `fix(core): propagate
parent aborts to delegated subagents` — sub-tasks are futures awaited inside the
parent's, so dropping the parent drops them, and their processes die with
`kill_on_drop`; cline had to plumb what structured concurrency gives here. The
rest of the twenty-nine are desktop marketplace panes, locale bugs, auth
refresh, dependency bumps, registry docs and cloud UI.

Also acted on, from hermes: `fix(compression): take reasoning_content when the
summarizer leaves content empty`. Rook read only the text channel, so a
reasoning model that fitted the whole summary into one thought and left
`content` empty was treated as a summariser that produced nothing — and the span
it had just summarised was replaced by the note saying it could not be. And
`fix(agent): cap compaction threshold floor at 85% of the context window`, whose
underlying point is that the fraction is config: `ContextBudget::new` now clamps
it, because a threshold at the top of the window is tripped by a turn that then
has nowhere to put the tool results it is about to receive.

Dismissed from those two: hermes' `surface silent turn stalls with a bounded
turn-liveness watchdog` — every dialect here already has `stream_idle_timeout`,
which is the same watchdog one layer down. openclaw's `keep plugin surfaces
reachable over bracketed IPv6 hosts` — the daemon writes `http://{addr}` from a
`SocketAddr`, whose `Display` brackets v6 already; theirs joined host and port as
strings. `retain grep matches with byte-form paths` — search here is lossy from
end to end and drops nothing for being unrepresentable.

Also acted on, from openclaw's `fix(memory): preserve vector worker stderr`: a
stdio MCP server that fails explains itself on stderr and nowhere else — the
protocol reports only that the pipe closed. Rook drained that pipe (an undrained
one blocks the server mid-write) straight into a debug log, so the user was told
"the server exited" and had to go and find out why. The last few lines are now
kept, bounded in lines and in bytes, and appear in `Closed` and `Timeout`. The
part worth having is the ordering: two tasks drain the two pipes, and the one
draining stdout is what releases the waiters, so without waiting for its sibling
whether the error carries the explanation is up to the scheduler.

## Nineteenth pass — goose, codex, openhands, acp, opencode

Acted on, from goose's `fix(dictation): bound provider response bodies` and
`fix(desktop): reject oversized recipe files before reading`: both are the rule
this repository already wrote down after three instances, and the provider
client had nine more. Every dialect read a body with `response.text()` and then
called `truncate` on it — the cap paid before it was checked, with `truncate`'s
own doc comment saying "a body can be a megabyte of HTML". `base_url` is
configuration, so how much arrives is decided by whatever is on the other end.
`whole_text` refuses past the size one reply may be, `quoted_text` reads only
what an error message will carry, and the streaming cap became the same
constant as the non-streaming one, because it is the same question.

Found while reading goose's `fix(security): anchor execute shell extraction`,
which is about parsing a tool call out of prose: `Provider::supports_tools`
says the agent "falls back to prompt-encoded tool calls", and nothing reads it,
no provider overrides it, and there is no such fallback. It survives the
dead-API tests only because `Retrying` forwards it. That is the next piece of
work rather than a line here.

Dismissed: goose's `isolate Telegram voice temp files`, `isolate audio recorder
generations` and `fuse command scanner signals` — no dictation, no recorder, and
the command rules here anchor to command position already. codex's Guardian
retention, per-account app approvals and turn analytics are their hosted
product. openhands' cron editing and cloud settings, acp and opencode's registry
docs.

## Twentieth pass — openclaw

Acted on: `fix(agents): resolve Windows bare commands through PATHEXT`. Windows
is a tested target here, and `std::process::Command` searches `PATH` for
`foo.exe` and nothing else — it does not consult `PATHEXT`, which is a shell's
job. npm, uv and bun all install their runners as `.cmd`, so every MCP server
configured the way its README writes it — `npx -y @modelcontextprotocol/…` —
was "program not found" on Windows while working everywhere else. Three
spawners had it: `rook-mcp`, `rook-lsp` and the version probe behind a skill's
`requires`, which quietly reported a skill as inapplicable. The lookup is
ordinary code rather than a `#[cfg(windows)]` block, so all of it is reachable
from a test on any machine — the first shape of it was `cfg`-gated, and
deleting the lookup entirely did not fail a single test here.

Dismissed: `fix(ios): omit deep-link URLs from logs` — the Google key goes in a
header here and not the `?key=` the docs lead with, with a comment saying why.
The rest of the two hundred are their gateway, their plugin registry, their
desktop and their messaging bridges.

## Twenty-first pass — goose, codex, openhands, acp, opencode, cline

Acted on, from goose's `fix(openai): update stored tool call name when later
delta carries non-empty name`. Their bug was a name arriving late and being
ignored; the same line here had the worse direction. Some gateways repeat `id`
and `name` on every continuation chunk with nothing in them, and
`ToolCallBuffer` took an empty string as an update — after which `drain`
discards the call as nameless and the model's tool call has silently not
happened. Both directions are one condition: empty is not an update. The test
for it fails with zero calls where one is expected, which is what the defect
looked like from the outside.

Acted on, from codex's `Fix relative MCP server spawning on macOS`: a server
configured as `./bin/server` together with a `cwd` is resolved against the
parent's directory on some platforms and the child's on others — Rust's own
documentation says not to rely on it. Resolved before the spawn, in the same
function that already looked a bare name up on PATH, it is neither. `rook-lsp`
had it too, against the workspace root.

Dismissed: openhands' `patch DOMPurify sanitization bypass` — the web UI builds
every node with `createTextNode` and `setAttribute` and has no `innerHTML`
anywhere, so there is nothing to sanitise; it also builds no attribute from
data, which is where the other half of that class lives. opencode's `stop Azure
model discovery from logging to stdout` — `rook mcp serve` speaks JSON-RPC on
stdout and nothing in the library writes there; the diagnostics that exist are
`eprintln!`. The rest are their consoles, desktops and marketplaces.

Also from this pass, hermes' `fix(agent): stop the between-turns tool refresh
from forking the cached prefix` — twice bitten there, and here the order of the
tool list was pinned by a test while the list itself was not. A tool appearing
or changing between turns invalidates everything behind it just as surely as
reordering them, and the list is assembled from more places than the order is.
Now pinned, with the precondition asserted: a request carrying no tools at all
would otherwise pass it saying nothing.

And openclaw's `fix(mcp): size OAuth-authenticated request bodies` found the
same defect the provider client had, one crate over: `rook-mcp`'s HTTP
transport read a body with `response.text()` and called `truncate` on it
afterwards. A url is configuration, so what comes back is decided by whatever
answers it.

Dismissed from these two: hermes' compaction cooldown work, which still does not
apply — a failed summary here records a position, so context is freed and there
is nothing to retry. openclaw's `record empty subagent completion` — a turn that
says nothing already answers with the reason it stopped rather than an empty
string. The rest are their gateway, their multiplex and their desktop.
