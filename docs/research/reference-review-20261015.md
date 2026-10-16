# Reference review — 2026-10-15, follow-up after v0.14.0

The eight candidates below were authorized on October 16 and implemented in
[the follow-up adoption record](../reference-followup-adoption.md). This review
retains the pre-implementation findings and exact reference snapshots.


Baseline: Rook `90b70e005a150b5d6c006c6cc3e4488750bfc91c` (v0.14.0).
This pass refreshes references and recommends work; it does not implement the
candidates or demonstrate a model-quality/performance improvement.

## Reviewed snapshots

Fetched each branch configured in `.gitmodules` with `git fetch --depth=200
origin <branch>`, checked out the fetched commit detached, and staged all ten
gitlinks. All reference worktrees were clean before checkout. The pointers below
identify the source actually inspected, rather than an upstream release promise.

| Reference | Branch | Previous pointer | Reviewed pointer | Incoming commits |
|---|---|---|---|---|
| acp | main | `1cebe1c01837379abf34dd62b71e9c75bf140669` | [`7855babf8bee9db299ce668dfa911289f4d1cf4e`](https://github.com/agentclientprotocol/agent-client-protocol/commit/7855babf8bee9db299ce668dfa911289f4d1cf4e) | 3 |
| codex | main | `bb64b894fd67158be0f874872e6d7bfe717e7368` | [`97ebf2d94d62e2ff2045f3ea17b6e11eae9a21fb`](https://github.com/openai/codex/commit/97ebf2d94d62e2ff2045f3ea17b6e11eae9a21fb) | 29 |
| goose | main | `dd17bf2021f3fb3b326c471593c352e1b0d62bbd` | [`0df818175bc854229746c81f01ea154b1c6c5e9e`](https://github.com/aaif-goose/goose/commit/0df818175bc854229746c81f01ea154b1c6c5e9e) | 2 |
| opencode | dev | `37df62f4a1b6d89187a1169187a6cef10557c0ca` | [`412b2b1d8563b665b5e7fed56cb46f7c333f34d1`](https://github.com/anomalyco/opencode/commit/412b2b1d8563b665b5e7fed56cb46f7c333f34d1) | 42 |
| cline | main | `789f79e46d3ba579717e8dc8be35fe28af935b06` | [`c8f808b51e2ae80ac7fb7b41c6a780a0bdbdec89`](https://github.com/cline/cline/commit/c8f808b51e2ae80ac7fb7b41c6a780a0bdbdec89) | 6 |
| hermes | main | `525dbcad8d744710a43d01eb7366500e46eacbf4` | [`60f56a6b13c8eb9a9e088c6fc1165257c92f7e11`](https://github.com/NousResearch/hermes-agent/commit/60f56a6b13c8eb9a9e088c6fc1165257c92f7e11) | 139 |
| openhands | main | `1c75a05d587bcb502cd3f63d7e13eaa98ca84d53` | [`cb835dd3283b1974b8dc88efe98793eb4c4fd2d0`](https://github.com/OpenHands/OpenHands/commit/cb835dd3283b1974b8dc88efe98793eb4c4fd2d0) | 4 |
| openclaw | main | `4ba08fadfddd85ddf73dcad683c9aeb512f296b2` | [`0ee67b8da3820cf33035d7612bbfb98516280e59`](https://github.com/openclaw/openclaw/commit/0ee67b8da3820cf33035d7612bbfb98516280e59) | at least 200; shallow boundary |
| openresearch | main | `c28897716bdb4b0cf5f1ec255d6649ca84bdcf83` | [`816ce27a6b25bb1291c9df98e0e6782e9640fb91`](https://github.com/alphaXiv/OpenResearch/commit/816ce27a6b25bb1291c9df98e0e6782e9640fb91) | 3 |
| pi | main | `b06c1f2cf1dea6af8748ddeee18c23587d2cb7c6` | [`4954d760504c11df9b2cbdc040346107b6a8153d`](https://github.com/earendil-works/pi/commit/4954d760504c11df9b2cbdc040346107b6a8153d) | 19 |

Counts include merges. Nine old pointers are confirmed ancestors of the new
snapshot. OpenClaw's 200-commit fetch does not reach the old pointer: its count
is a lower bound. Its old/new trees are available for comparison, but the full
intervening history was not audited. Incoming commit subjects were screened for relevant changes; selected
implementations, diffs and tests were read. This is a selective functional review, not an audit of every changed line.
CodeGraph exceeded a 25-second inspection budget; focused source inspection was
used instead. No third-party builds, model requests or live service changes.

## Recommended order

P1 means an existing correctness/access problem deserves attention first. P2
means a useful product extension. P3 is optional and substantially broader.
Effort is a relative estimate, not a delivery promise.

### 1. P1 — Keep skill updates bound to their installed source (small/medium)

**Benefit:** a skill called `docx` installed from Rook must not silently become
a different author's `docx` when source ordering changes.

**Evidence:** Pi distinguishes packages by repository URL/local path and documents
collision precedence: [package identity](../../references/pi/packages/coding-agent/docs/packages.md#understand-scope-and-identity),
[collision regression](../../references/pi/packages/coding-agent/test/suite/regressions/2781-skill-collision-precedence.test.ts).
This supplies a design reference, not a claim that Pi has the same updater.

**Rook gap confirmed in source:**
[`refresh_skill`](../../crates/rook-core/src/service.rs) reads the recorded source,
then chooses `offered.iter().find(|o| o.name == name)` without checking that source.
The current source-order workaround does not fix this selection rule.

**Adaptation:** resolve by source identity plus skill name; retain a revision and
content receipt; distinguish an unavailable origin from an absent skill. Make
switching origin an explicit action. Prepare a replacement in a bounded temporary
directory before changing the installed copy; retain the existing history/undo.

**Acceptance:** two sources offering the same name, reversed source order,
unreachable original source, modified personal copy, failed copy, and explicit
migration all preserve the correct bytes and origin. No fallback substitution.

### 2. P1 — Read a skill's bundled resources through a scoped tool (medium)

**Benefit:** references, templates and scripts bundled in an installed skill become
usable even when the skill lives outside the workspace.

**Evidence:** Hermes [`skill_view(name, file_path)`](../../references/hermes/tools/skills_tool.py)
serves a relative file from the selected skill. Pi also documents resolving bundled
resources against the skill directory in [skills](../../references/pi/packages/coding-agent/docs/skills.md).
These are existing upstream capabilities, not necessarily new incoming commits.

**Rook gap:** [`bundled`](../../crates/rook-core/src/agent/prompt.rs) lists at most
20 resource paths; [`load_skill`](../../crates/rook-core/src/agent/tools.rs) loads
the body. Ordinary file reads remain workspace scoped; listing a path does not
grant access to installed resources. Recent adapted skills avoid this with
self-contained bodies, limiting future larger workflows.

**Adaptation:** a paged, byte-bounded `read_skill_resource` addressed by resolved
skill ID and relative path, with origin in the result. Initially read-only;
executing a bundled script is a separate permissioned action, never automatic
when loading a card. Do not widen access to the whole agent home directory.

**Acceptance:** read an installed reference from outside the workspace; refuse
absolute paths, traversal, symlink escapes and stale identities after an update;
enforce per-page/total limits and preserve ordinary source trust handling.

### 3. P1 — Scope replayed reasoning to the producing model (medium)

**Benefit:** switching between GLM, Qwen and other models should preserve the task
and tool history without presenting the prior model's thoughts as its visible reply.

**Incoming evidence:** Pi `911309429175254550837167baee245d5f9dbcb6`,
[`transformMessages`](../../references/pi/packages/ai/src/api/transform-messages.ts),
with cross-model thinking regression coverage. It drops foreign-model thinking
while retaining text/tool calls and same-model signed state.

**Rook gap:** [`history`](../../crates/rook-core/src/agent/history.rs) reconstructs
`EventKind::Reasoning` as `[thinking]` text on an assistant message; this path has
no producing-model comparison. Anthropic's opaque signed blocks already have
separate scope checks in [`anthropic.rs`](../../crates/rook-llm/src/anthropic.rs);
those protections must remain. This finding concerns readable reasoning replay.

**Adaptation:** associate reasoning with the physical provider/model receipt;
exclude foreign or unknown-origin reasoning from requests, retaining it in the
inspectable transcript. Preserve normal assistant text and tool-call/result pairs.
Use compatible metadata storage rather than changing existing event decoding.

**Acceptance:** same-model continuation, model/provider switch, restart/resume,
old unlabelled records, delegation and compaction; no orphaned tool results and no
foreign signature replay. Source inspection identifies the gap; live impact has
not been measured.

### 4. P1 — Fit compaction to the summarizer's actual context window (medium)

**Benefit:** a smaller auxiliary model can summarize a long main-model session
without repeatedly rejecting input or discarding the summary opportunity.

**Incoming evidence:** OpenCode `ae8d59f8b56c8034623d36ec514f9d9d4dc7cd5a`,
[`fitOutput`](../../references/opencode/packages/core/src/session/compaction.ts)
recomputes the output allowance after adding the actual summary prompt/reminder.

**Rook gap:** [`compaction.rs`](../../crates/rook-core/src/agent/compaction.rs)
prepares material using the main loop's budget, permits a separate
`compaction_model`, and requests 2,048 output tokens. The request is not explicitly
fitted there to the selected summarizer's window including its instructions and
source envelope. Provider retry/output-ceiling adaptation already exists; this
proposal concerns preparing the request before a provider refusal.

**Adaptation:** budget the serialized summary request against the chosen provider's
window, reserve output, bound source pages, and distinguish a request that cannot
fit from an empty summary. Preserve the previous summary and source pointers.

**Acceptance:** large main window/small summarizer window, long carried summary,
Russian/Unicode text and wrapper overhead; assert estimated input plus reserved
output fits before sending. Compare completion quality and compaction frequency
before enabling any change to default behavior.

### 5. P2 — Reusable prompt and session-setting presets (medium)

**Benefit:** save repeated requests such as review, release or translation along
with an explicitly selected model/effort, while retaining the current draft.

**Incoming evidence:** Codex `dcd9e954598cd5e9f80570204896d779c60c1ecd` and
`36803b18`, [`quick_dials.rs`](../../references/codex/codex-rs/tui/src/chatwidget/quick_dials.rs).
Upstream sends recalled input through normal queues and confirms replacement.

**Rook fit:** keyboard remapping, prompt undo, editable queues, recipes and
execution profiles already exist. A reusable input/settings preset is a smaller
interaction than a recipe and a different concern from a keybinding.

**Adaptation:** one core preset format consumed by CLI/TUI/web; show effective
settings before applying them. Saving text/model settings must not implicitly
widen permissions or overwrite the live draft.

**Acceptance:** save/replace/delete, failed persistence, active-turn queueing,
draft restoration, unavailable model and explicit permission-profile selection.
A missing/declined answer must leave settings unchanged.

### 6. P2 — Responsive diff viewer attached to saved review sources (medium)

**Benefit:** inspect an agent's edits in a file tree with split/unified patches,
then comment on the exact saved version from the same screen.

**Incoming evidence:** OpenCode `0478ba64` and `09a075d9`,
[`diff-viewer.tsx`](../../references/opencode/packages/tui/src/feature-plugins/system/diff-viewer.tsx)
selects a compact layout on narrow screens and only offers split view when it fits.

**Rook fit:** change summaries, frozen review sources, paged source reads,
findings/comments and selective undo already exist in
[`review.rs`](../../crates/rook-core/src/review.rs) and
[web review UI](../../web/dist/reviews.js). Extend their presentation; do not
introduce another review engine or imply diff inspection is currently absent.

**Acceptance:** narrow terminal, wide terminal, resize during inspection, long
lines, deleted/binary files and bounded large patches. Comments remain anchored
to saved hashes/line identities after the live workspace changes.

### 7. P2 — Browser diagnostics and viewport controls (medium)

**Benefit:** UI work can verify mobile layouts and explain failed interactions
using console errors and failed requests alongside screenshots.

**Evidence:** OpenClaw [browser inspection/control](../../references/openclaw/docs/tools/browser-control.md)
exposes console/errors/requests and resize. This is broader existing functionality,
not a claim that all of it arrived in the new 200-commit history window.

**Rook gap:** the native [`browser`](../../crates/rook-core/src/browser.rs) has
navigate/snapshot/click/type/wait/screenshot/status/close and a fixed initial
viewport; it does not expose these diagnostic/resize actions. A separately
configured general Playwright MCP server is already an alternative.

**Adaptation:** add bounded observation buffers and explicit viewport dimensions
to the existing conversation-owned context. Redact credential-bearing headers and
URLs. Arbitrary JavaScript evaluation is not required for this first extension.

**Acceptance:** reproducible mobile/desktop screenshot checks, failing API request,
console error, redaction, cancellation, session isolation and buffer limits.
Measure whether UI tasks find more real defects, rather than merely calling
more browser tools.

### 8. P3 — Optional MCP Apps renderer (large)

**Benefit:** an MCP tool can return an interactive table, chart or form within
the browser frontend rather than requiring a long textual interaction.

**Evidence:** Goose [`McpAppRenderer.tsx`](../../references/goose/ui/desktop/src/components/McpApps/McpAppRenderer.tsx),
[`resource.rs`](../../references/goose/crates/goose/src/goose_apps/resource.rs)
and [`app.rs`](../../references/goose/crates/goose/src/goose_apps/app.rs)
handle sandboxed app UI, CSP/permissions metadata and `ui://` resources.

**Rook fit:** resource reads and elicitation already exist, but there is no MCP
Apps host/renderer. This needs deliberate UI and protocol work; it is not solved
by rendering arbitrary HTML in chat.

**Acceptance:** sandbox/CSP, precise server/session identity, permissioned tool
calls, navigation/network limits, teardown and static CLI/TUI fallback. No implicit
secret forwarding; declined or unanswered requests stay declined/unanswered.
Start only with a concrete server/application that benefits from it.

## Decisions by reference

| Reference | Incoming work inspected | Decision |
|---|---|---|
| ACP | Three registry updates; protocol tree comparison | Update registry snapshot only; no new protocol feature. V2 remains a separate draft/negotiation project, not a reason to break v1. |
| Codex | Presets, permission selection, generated-image references, SQLite contention, scoped remote credentials | Presets are useful (#5). Rook uses a different store, so do not copy SQLite tuning. Existing vault/SSH ownership and image storage need comparisons, not replacement with Codex's remote executor. |
| Goose | Preserved compaction message visibility and ACP compaction notice | Regression reference: Rook already has durable compaction and progress notices. Existing skill/app mechanisms inform #8; no duplicate compaction UI project. |
| OpenCode | Compaction request sizing, responsive diff layout, protocol inference URLs, capability-shaped MCP discovery | Recommend #4 and #6. Check capability negotiation against Rook's existing resource-only support if touching MCP; do not re-propose resources/elicitation. Provider URL changes are dialect-specific. |
| Cline | Request IDs, base-URL normalization, auto-compact settings layout | Correlated request IDs already implemented. URL normalization is a compatibility regression reference; adding `/v1` blindly would break valid custom endpoints. |
| Hermes | Gateway admission/ownership corrections, cron attempt-output pairing, live loop countdown, model-switch routing | Existing scheduler receipts, activity and shared engine cover much of this. Preserve exact attempt/session attribution and no-consent semantics in future regressions. Skill resource access is useful (#2). No new gateway/kanban subsystem. |
| OpenHands | Preserve profile with per-launch model override; theme contrast fix | Preserve effective profile when adding #5. Use contrast regression ideas in existing UI; no separate profile architecture port justified. |
| OpenClaw | Task shutdown preservation, live local-model wait, session/model catalog performance, browser docs | Rook already has watchdog/activity and owned-work recovery. Browser diagnostics remain useful (#7). No general actor/cache rewrite without a local performance measurement. History review is explicitly partial. |
| OpenResearch | Windows whole-tree termination via Job Object, CMD batch execution | Rook already has Windows Job Object support in `rook-contain`; use teardown-completion as a regression reference if executor changes. Not a new user feature by itself. |
| Pi | Cross-model reasoning, error tool-result images, package identity, namespaced SQLite | Recommend #1 and #3. Inspect image compatibility if adding that dialect. Namespaced SQLite does not directly fit Rook's existing store. |

## Deliberately excluded

Already implemented: session trees, watchdog/activity, cancellation/owned jobs,
MCP resources and elicitation, private secrets, PDF text/pages, correlated traces,
SSH-native files, domain network policy on supported containment, loop guards,
model metadata cache, durable delivery and input queues, learning proposals,
review/experiment trees and optional skill packs.

Do not add automatic request translation, Code Mode, semantic memory or cache
rewrites on feature parity alone. Translation still requires the user's requested
measurable improvement. User questions/secrets can be declined or ignored; timeout
or absence is never permission and must not cause repeated prompting.

No runtime code, model configuration or installed skills were changed in this
review. Candidates belong on the roadmap, not in the implemented-feature ledger.
