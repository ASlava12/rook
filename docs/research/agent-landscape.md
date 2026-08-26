# What breaks in the agents people actually run

Research notes behind Rook's design. Every claim below is a real, public issue
on a real repository, read in August 2026. Star and reaction counts are from the
same reading. The point is not to criticise these projects — several are
excellent, and a few of these threads are already fixed — but to build against
the failure modes they exposed rather than rediscover them.

## The projects surveyed

| Project | Stars | Open issues | Language | Why it was read |
|---|---:|---:|---|---|
| [anomalyco/opencode](https://github.com/anomalyco/opencode) | 201k | 5.5k | TypeScript | Best-regarded TUI; large skills/memory discussion |
| [Significant-Gravitas/AutoGPT](https://github.com/Significant-Gravitas/AutoGPT) | 187k | 517 | Python | Long-running autonomous processes |
| [anthropics/claude-code](https://github.com/anthropics/claude-code) | 143k | 15k | — | The reference coding harness |
| [openai/codex](https://github.com/openai/codex) | 119k | 14k | Rust | Closest architectural relative |
| [google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli) | 107k | 869 | TypeScript | Large context, local-model demand |
| [openclaw/openclaw](https://github.com/openclaw/openclaw) | 388k | 5.7k | TypeScript | Persistent personal assistant |
| [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) | 237k | 36k | Python | Memory + skills + local models |
| [OpenHands/OpenHands](https://github.com/OpenHands/OpenHands) | 85k | 568 | TypeScript | Autonomous issue resolution |
| [cline/cline](https://github.com/cline/cline) | 67k | 1.1k | TypeScript | IDE agent, context management |
| [aaif-goose/goose](https://github.com/aaif-goose/goose) | 54k | 197 | Rust | Rust agent with extensions |
| [SWE-agent/SWE-agent](https://github.com/SWE-agent/SWE-agent) | 20k | 81 | Python | Research harness |
| [agent0ai/agent-zero](https://github.com/agent0ai/agent-zero) | 19k | 145 | Python | Docker-isolated general agent |

## 1. Storage grows without bound, and nobody planned for it

The clearest pattern in the whole survey.

**Codex writes trace logs into SQLite faster than the disk deserves.**
[codex#30236](https://github.com/openai/codex/issues/30236) reports `RUST_LOG=warn`
being ignored by the app-server's SQLite log sink: TRACE rows were ~90% of the
log, ~1,800 rows arrived in a 10-second sample, `logs_2.sqlite` reached 138 MB
with an 67–80 MB WAL alongside it. A companion thread —
[*"Codex SQLite feedback logs can write ~640 TB/year and rapidly consume SSD
endurance"*](https://github.com/openai/codex/issues/28224), 617 reactions —
extrapolates that write rate to hardware wear.
[*"Excessive SQLite WAL writes during streaming due to TRACE logs ignoring
RUST_LOG"*](https://github.com/openai/codex/issues/17320) (39 reactions) says the
same thing from the WAL side.

**OpenCode leaks memory until it is killed.** The
[Memory Megathread](https://github.com/anomalyco/opencode/issues/20695) (163
reactions, 137 comments) exists solely to collect heap snapshots from users whose
RSS passes 1–2 GB. The maintainers ship `OPENCODE_AUTO_HEAP_SNAPSHOT=1` to
capture it automatically.

**Agent Zero's backup fails once it matters.**
[agent-zero#1819](https://github.com/agent0ai/agent-zero/issues/1819): a 1.3 GB
workspace produces a 504 because `backup_create.py` zips synchronously inside one
HTTP request. The backend finishes — a 664 MB archive appears in `/tmp` — but the
gateway has already told the user it failed.

→ **In Rook.** Growth is bounded by construction, not by a later fix.
[`RetentionPolicy`](../../crates/rook-store/src/maintenance.rs) has non-`None`
defaults (180 days, 2,000 sessions, 4 GiB) and `Store::prune` enforces them.
Logging defaults to `warn` and the log directory has a byte cap in
[`TelemetryConfig`](../../crates/rook-core/src/config.rs). `rook store stat`
answers "why is this large" without a database client. Maintenance is a job with
a progress channel ([`ServerEvent::Progress`](../../crates/rook-proto/src/lib.rs)),
never a synchronous HTTP request.

## 2. Checkpointing by shelling out to git

[opencode#3176](https://github.com/anomalyco/opencode/issues/3176), *"Why is
OpenCode massively abusing git?"* (37 reactions): session snapshots run `git add .`
over the user's working directory. On a 45 GB, 54,000-file data-science workspace
that pins the CPU and stages datasets nobody asked to version — with, as the
reporter puts it, "no warning, no configuration, no permission".

Users want the *feature*: codex's
[`/rewind` checkpoint restore](https://github.com/openai/codex/issues/11626)
has 207 reactions and [*"Please make `/undo` back"*](https://github.com/openai/codex/issues/9203)
has 451. They do not want it
implemented by staging their whole disk.

→ **In Rook.** [`FileSet::capture`](../../crates/rook-core/src/fileset.rs) is
content-addressed and takes an explicit
[`CaptureLimits`](../../crates/rook-core/src/fileset.rs) budget: file count, total
bytes, per-file bytes, an exclusion list (`target/`, `node_modules/`, `.git/`, …)
applied *before* the counters, and `.gitignore` honoured. Exceeding a limit is an
error naming the limit, not a slow path. Two tests pin this behaviour:
`a_capture_refuses_to_run_away_instead_of_thrashing` and
`heavy_directories_are_excluded_before_they_count_against_the_budget`.

## 3. Tool and skill schemas are paid for on every single request

[hermes-agent#6839](https://github.com/NousResearch/hermes-agent/issues/6839),
*"Lazy Tool Schema Loading"*: with 50+ tools, full schemas cost ~3,500–5,000
tokens on every call regardless of need. The benchmark in the thread is the
striking part — on local models, tool-formatted prompts processed at **134 tok/s
versus 1,230 tok/s** for plain text with 8 tools. Roughly a 10× slowdown, paid on
every turn, to advertise tools the turn will not use.

Discoverability is the mirror image:
[opencode#7846](https://github.com/anomalyco/opencode/issues/7846) (117
reactions) asks for `/skills` because there is no way to see what is installed —
users must "know skill names beforehand and hope the LLM picks them up".

→ **In Rook.** Progressive disclosure is the default, not an option:
[`ToolBox::stubs`](../../crates/rook-tools/src/lib.rs) sends names and
descriptions, and [`SkillCard`](../../crates/rook-skills/src/index.rs) is what the
catalog is made of. A body arrives only when the model calls `load_skill`. The
test `the_catalog_is_one_card_per_name_and_stays_small` asserts a full catalog
costs under 100 tokens. `rook skills ls` is the discoverability answer, and it
prints what loading each skill *would* cost.

## 4. Context overflow is unrecoverable

[cline#4389](https://github.com/cline/cline/issues/4389): files over 300 KB are
refused outright even on million-token models; when a large read does land, the
task hits "prompt too long" with no recovery — "users can only 'Retry' or 'Start
New Task'", losing the work. Meanwhile visibility into the problem keeps getting
removed and re-requested:
[codex#33407](https://github.com/openai/codex/issues/33407), *"Regression: Codex
Desktop has no persistent context/token usage indicator"*, and
[opencode#6152](https://github.com/anomalyco/opencode/issues/6152), *"Session
context usage (similar to `/context`)"* (143 reactions).

→ **In Rook.** No hard file-size refusal: `read_file` pages by line offset and
tells the model how to get the rest. `run_command` caps captured output and keeps
the *tail*, where exit messages live. Compaction is checked *before* each request
via [`ContextBudget`](../../crates/rook-core/src/context.rs) rather than after a
rejection, and elision is always visible to the model. The full payload is in the
store either way, addressable by sequence number.

## 5. Skills and memory have no history

[hermes-agent#12238](https://github.com/NousResearch/hermes-agent/issues/12238),
*"Built-in Automatic Backup & Version Control for Agent Data"* (36 reactions),
states the problem exactly: "a single disk failure wipes months of agent
learning", and "there is no way to see how an agent's skills and memory evolved —
or roll back a single skill that started behaving incorrectly". It asks for
per-skill history, memory diffs, rollback granularity and experiment branches.

→ **In Rook.** This is a first-class feature rather than a backup script.
`rook skills capture` records a version, `rook skills history` lists them,
`rook skills diff` compares two, and `rook skills rollback` restores one —
capturing the current state first, so the rollback is itself undoable, and
reporting any file left on disk that the capture did not contain.

## 6. Cross-platform support decays, and a runtime is usually why

[codex#13802](https://github.com/openai/codex/issues/13802): FreeBSD worked via
npm until a commit restricted the platform list, after which the CLI refuses to
start. OpenClaw carries the same request. OpenCode's
[Bun segfault on Windows](https://github.com/anomalyco/opencode/issues/33742)
(47 reactions, 60 comments) forced users back a version, and
[Windows arm64 support](https://github.com/anomalyco/opencode/issues/4340) took 76
reactions to land. Cline's
[terminal integration across platforms and shells](https://github.com/cline/cline/issues/4356)
has 66 comments.

Note what is *not* the cause: Rust. Codex's CLI is Rust and its FreeBSD break came
from the npm distribution wrapper.

→ **In Rook.** No runtime to ship: two static binaries. The web UI is a single
hand-written HTML file with no build step, so `npm` is not a build prerequisite on
any platform. FreeBSD is built **and tested in a real VM** in CI, not
cross-checked — see [platforms.md](../platforms.md) for why, and for the two
dependencies that carry C code.

## 7. Destructive actions with no undo

[gemini-cli#26856](https://github.com/google-gemini/gemini-cli/issues/26856) is
hard to read: a user's Obsidian vault, "10000s of files", deleted with no
recovery. 166 reactions.

→ **In Rook.** Commands run through a deny list that is refused even with
interactive approval; `edit_file` rejects an ambiguous match instead of guessing;
checkpoints exist so there is something to go back to; and every tool result is
in the store, so "what did it actually do" is answerable after the fact.

## 8. Interoperability has settled, and it is worth adopting

Across goose, OpenHands, hermes and opencode, the same acronyms keep appearing in
feature requests: ACP for editor integration, MCP for tools, Agent Skills for
portable capability. goose has an open issue to
[adopt the Agent Plugins standard](https://github.com/aaif-goose/goose/issues/11043).

- **Agent Skills** — `SKILL.md` with YAML frontmatter, open-sourced by Anthropic
  in December 2025, adopted by OpenAI and Google, now governed under the Agentic
  AI Foundation. **Rook reads this format today**, so skills written for other
  agents work unchanged.
- **ACP** — JSON-RPC 2.0 over stdio, from Zed, v1 stable, adopted by JetBrains,
  Google and GitHub. *Planned, not implemented.*
- **MCP** — for consuming third-party tools. *Planned.*
- **Agent Plugins 1.0** — `plugin.json` packaging that defers to Agent Skills for
  the skill format. *Planned.*

Inventing a fourth format here would be a mistake, and the roadmap says so.

## Sources

- [anomalyco/opencode](https://github.com/anomalyco/opencode) · [openai/codex](https://github.com/openai/codex) · [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) · [cline/cline](https://github.com/cline/cline) · [agent0ai/agent-zero](https://github.com/agent0ai/agent-zero) · [google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli) · [aaif-goose/goose](https://github.com/aaif-goose/goose) · [openclaw/openclaw](https://github.com/openclaw/openclaw)
- [Agent Client Protocol](https://agentclientprotocol.com/get-started/introduction) · [agentclientprotocol/agent-client-protocol](https://github.com/agentclientprotocol/agent-client-protocol)
- [Agent Plugins 1.0](https://www.digitalapplied.com/blog/agent-plugins-1-0-open-standard-portable-ai-skills) · [Agent Skills / SKILL.md reference](https://www.webfuse.com/agent-skills-cheat-sheet)
- [vmactions/freebsd-vm](https://github.com/vmactions/freebsd-vm)
