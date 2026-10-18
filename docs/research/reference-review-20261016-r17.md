# Reference review — 2026-10-16, after v0.16.0 (R17)

Rook baseline: `88b295fd5ed8383eb53209a5fee06cf85dab02f0` (`v0.16.0`).
The eight R16 candidates are delivered; see the [adoption record](../reference-r16-adoption.md).
This review proposes additional work. No runtime implementation or release is part of this pass.

On 2026-10-16 the user accepted all eight candidates into the
[R17 backlog](../roadmap.md#next). Implementation was requested on 2026-10-18;
current delivery and verification are recorded in the [adoption record](../reference-r17-adoption.md).
This source review retains the original rationale and acceptance scope.

## Snapshots and coverage

All ten clean reference worktrees were fetched from their configured branches.
Eight pins advanced; Cline and OpenResearch already matched their fetched tips.
The changed worktrees were checked out detached and their gitlinks staged.
No reference source files were edited. Pins describe the snapshots fetched during
this review, rather than an assertion about future branch tips.

| Reference / branch | Previous pin | Reviewed pin | Incoming commits |
|---|---|---|---|
| acp (`main`) | `9bec5b0975238dabafaa84d53a6c0aebbd5fac75` | `00c5a02d7ab96b9d6aa214002d408ce9d972b4e4` | 8 |
| cline (`main`) | `01b8a9724da905423e636396d7fb4acdf47796bb` | `01b8a9724da905423e636396d7fb4acdf47796bb` | 0 |
| codex (`main`) | `6ae8b8aa7c608730d4315d59c5c081897ae02780` | `b5115cdd669d37a8635a9742851e97ba4fb1d42c` | 4 |
| goose (`main`) | `ea4ad800d12c7c6cbb44b738921ce3e888aa4448` | `fe55e842fb8d92f16c45d04c83f07120cc4fa258` | 1 |
| hermes (`main`) | `34e8635cb9cfa6e55ee31b337f005697e2db3d37` | `f974134f367bc0a4c4cd24afd90494bbceaa86ee` | 124 |
| openclaw (`main`) | `51042e8c1a27c1361ad5f270c89fecc86acd432b` | `58d7e878feb0b27f85c4ecb48595c38b534553ab` | 236 |
| opencode (`dev`) | `50d4280dbdb2a8e5d9c89eb6c893d12c7a37f69f` | `ed772f5009755b5262805a7ba07e66e1417f140a` | 10 |
| openhands (`main`) | `ccdb08783cf83ffe4d72d8c41298c700fe7fda94` | `9d02893bc96aa256a4c0ece316bdb992247bc918` | 1 |
| openresearch (`main`) | `8fc90cf355cda1e89a6aabc02d49a098f159f463` | `8fc90cf355cda1e89a6aabc02d49a098f159f463` | 0 |
| pi (`main`) | `0fbdbd3f8ab1876ac260c0f08226a400759323b8` | `4dd2b149d155d7369de16ea4dc77c84e9f5ffb24` | 4 |

All previous pins are proven ancestors. The incoming ranges contain 388 commits
in total, including merges. Fetches used `--no-tags --depth=200`; OpenClaw was
deepened to 800 at its already selected SHA to reach the previous pin and cover
its complete 236-commit range. Every incoming subject was screened; selected
implementation and test paths were inspected. This is not an exhaustive audit
of every changed line. No third-party builds, live-model evaluations or performance
measurements were run. CodeGraph was available, but the bounded repository query
exceeded 35 seconds; focused source inspection was used. No existing Graphify
graph was available, and a new corpus graph was not built for this review.

## Recommended adoption order

P1 is the proposed implementation order, not a vulnerability rating. M/L express
relative scope, not a delivery estimate. These are proposals, not delivered features.

| ID | Priority / scope | Capability | Source | Additional value in Rook |
|---|---|---|---|---|
| R17-1 | P1 / M | Commit model and reasoning effort as one durable choice | Hermes | A preset or busy-session choice cannot apply only half of the requested settings |
| R17-2 | P1 / L | Drain before daemon restart, retaining access to pending input | Hermes | Update after current work, with a way to answer or decline a question holding the drain |
| R17-3 | P1 / M | Explicit compaction phase cancellation and time budget | OpenClaw, Hermes | Bound an auxiliary phase independently of the ordinary answer stream and prevent late commits |
| R17-4 | P2 / M | Separate fallback chains for compaction and errands | OpenClaw | Choose permitted backup endpoints for each purpose instead of one global priority rotation |
| R17-5 | P2 / L | Typed delegated results with validation and bounded projections | Hermes | Parent receives a verifiable structured result and a reference to the complete saved answer |
| R17-6 | P2 / M | Copy the last assistant answer explicitly | OpenCode | Copy an answer from a long session without selecting the whole transcript |
| R17-7 | P2 / M, experiment | Size summarizer output independently of the current fixed cap | Pi | Test whether reasoning-heavy summarizers need a different output allocation before changing defaults |
| R17-8 | P2 / M | Explicit reasoning capability overrides for configured models | OpenCode | Describe a new/local model whose endpoint omits or misreports reasoning metadata |

### R17-1 — One durable model-and-effort selection

**Evidence.** Hermes [b9a86d07e74e](https://github.com/NousResearch/hermes-agent/commit/b9a86d07e74e)
puts the reasoning level into the model mutation, including deferred application.
The pinned [busy-selection test](../../references/hermes/tests/gateway/test_session_mutation_model_busy.py)
checks that the next turn observes both the selected model and effort.

**Existing Rook / gap.** R16 already provides durable model receipts and next-turn
application. [Request and State](../../crates/rook-core/src/model_selection.rs)
carry the model but not effort. [Browser preset application](../../web/dist/chat.js)
sends model and effort separately. [Settings](../../crates/rookd/src/chat.rs) also
treats effort as a separate setting. Existing presets are not missing; the missing
contract is one durable transition for their model/effort pair. This review did
not reproduce Hermes's exact busy error in Rook.

**Adaptation and acceptance.** Extend the shared selection contract with optional
effort, preserving legacy model-only behavior. Commit the chosen pair and retry
receipt together; apply only at root admission and keep the active request fixed.
Preserve standalone effort changes with a documented ordering rule. Test busy
and idle choices, two clients, restart, persistence failure, and A→B→retry-A with
different efforts. Expose selected versus last-effective pairs in CLI/API/TUI/web.
Migrate the versioned JSON companion explicitly; do not alter stored postcard records.

### R17-2 — Restart after draining, without hiding the parked question

**Evidence.** Hermes [da76391fdc7f](https://github.com/NousResearch/hermes-agent/commit/da76391fdc7f)
keeps a draining owner's endpoint discoverable while a restart waits on a parked
approval/question. The pinned [reachability tests](../../references/hermes/tests/gateway/test_restart_wait_prompt_reach.py)
cover allowed reads, parked input and rejected unrelated writes.

**Existing Rook / gap.** [Daemon commands](../../crates/rook-cli/src/commands/daemon.rs)
already install/start/restart the daemon. [Shutdown API](../../crates/rookd/src/api.rs)
refuses a busy non-forced stop; force interrupts turns. [The updater](../../crates/rook-core/src/upgrade.rs)
replaces binaries and skills and asks the operator to restart. There is no
after-current-work drain state that closes new admissions while preserving the
current owner and a way to answer its pending request.

**Adaptation and acceptance.** Add an explicit drain/restart mode shared by the
daemon and clients. Keep status, attention navigation, cancellation and the exact
live input reply paths usable. Refuse or explicitly hold new work; never silently
start it on the draining owner. Include descendants, managed follow-ups, jobs and
writer ownership in the quiescence decision, rather than checking only the root
turn counter. Publish progress and allow cancelling the planned restart. A timeout
reports what remains and requires an explicit force decision; it never implies
approval. Test a parked permission, private-input refusal, a disconnected client,
active child work, competing drain requests and replacement startup failure.

### R17-3 — Compaction has its own cancellation contract

**Evidence.** OpenClaw [d942bc5b1fb3](https://github.com/openclaw/openclaw/commit/d942bc5b1fb3)
distinguishes progress-reset silence from a finite phase ceiling. Inspect the
pinned [compaction watchdog](../../references/openclaw/src/agents/embedded-agent-runner/compaction-safety-timeout.ts),
not just the first commit message. Hermes
[6fb41c9eabf9](https://github.com/NousResearch/hermes-agent/commit/6fb41c9eabf9)
and the pinned [compression owner](../../references/hermes/agent/conversation_compression.py)
also carry cancellation/deadline state through fallback and commit fences.

**Existing Rook / gap.** [Compaction](../../crates/rook-core/src/agent/compaction.rs)
already budgets input/output, caps combined summary bytes, emits observations and
falls back to a log-reference marker on failure. Providers have an idle watchdog.
The auxiliary summary await has no explicit phase budget or remaining-turn-deadline
wrapper; it does not use the ordinary narrative repetition guard. Existing byte
and transport-silence bounds do not define a complete compaction phase contract.

**Adaptation and acceptance.** Give auxiliary compaction an explicit, configurable
time policy, constrained by the remaining turn deadline and shared across permitted
attempts. Preserve transport silence reporting and distinguish timeout from user
cancellation. After cancellation wins, a late summary cannot commit. A failed
summary keeps the saved source prefix and an honest fallback; queued/steering
messages outside that prefix remain pending in order. Test trickling output,
admission waits, deadline/finish races, concurrent user input and restart. Keep
ordinary model replies governed by their existing silence watchdog: do not restore
the old global response timeout. Phase-ceiling defaults require measurements on
large, reasoning-heavy compactions; do not copy upstream seconds or multipliers.

### R17-4 — Purpose-specific fallback chains

**Evidence.** OpenClaw [6861671cda97](https://github.com/openclaw/openclaw/commit/6861671cda97)
introduces independent utility fallback selection. The pinned
[setting resolver](../../references/openclaw/src/agents/utility-model-setting.ts)
distinguishes absent, disabled and configured choices; the
[completion runner](../../references/openclaw/src/agents/utility-completion.ts)
reuses ordinary fallback classification without inheriting the main chain.

**Existing Rook / gap.** [Agent configuration](../../crates/rook-core/src/config.rs)
already has `compaction_model` and `errand_model`; this is not a request for a
second utility-model selector. [endpoints_for](../../crates/rook-core/src/models.rs)
adds all sources with a global `priority` to each selected endpoint. There is no
separate ordered allowlist for each purpose.

**Adaptation and acceptance.** Resolve bounded, configured-name chains for main
work, compaction and errands through one shared route builder. Define omitted
versus explicit-empty settings; preserve today's configuration when omitted.
Keep purpose and actual-attempt receipts, current context admission, credential
binding, network restrictions and cost ceilings. A fallback cannot silently gain
permission to send content to a new endpoint. Retry only eligible failed requests,
never an already completed answer or side-effecting work. Test unavailable local
models, explicit empty chains, duplicate/cyclic names, credential refusal, different
context windows and reported physical route/cost. No cheaper or faster default
routing is established by this source review.

### R17-5 — A structured result contract for delegated work

**Evidence.** Hermes [9ed387ce2301](https://github.com/NousResearch/hermes-agent/commit/9ed387ce2301),
[24dfa8a19e8b](https://github.com/NousResearch/hermes-agent/commit/24dfa8a19e8b)
and [322a249c252f](https://github.com/NousResearch/hermes-agent/commit/322a249c252f)
project/redact results before consumers and budget trimming. The pinned
[redaction owner](../../references/hermes/tools/delegation_redaction.py) preserves
JSON structure; [result handling](../../references/hermes/tools/delegate_tool_results.py)
keeps a complete projection before making head/tail excerpts. The
[tests](../../references/hermes/tests/tools/test_delegated_output_redaction.py)
cover scalar JSON, changed schema verdicts and complete-output spill sources.

**Existing Rook / gap.** Rook already records complete child sessions, returns
child IDs and accounting, inherits known secret redactions, and validates root/
recipe output through [the existing contract](../../crates/rook-core/src/agent/output.rs).
[Generic delegation](../../crates/rook-core/src/agent/delegation.rs) returns prose
blocks and offers no per-task schema plus a typed validation/projection receipt.
There is no need to duplicate complete child transcripts in a new spill cache.
This review found no confirmed child-secret leak in Rook.

**Adaptation and acceptance.** Allow a bounded per-task output contract using the
existing supported validator, with result status, schema verdict, source session/
sequence and omission information. Validate the delivered projection after any
redaction; a changed answer cannot retain an earlier positive verdict. Keep JSON
valid for object/array/scalar answers. Read the full saved result through existing
scoped session/result APIs instead of passing it all into parent context. Bound
schema size/depth and repair requests; failed validation does not rerun child tools
or certify unfinished work. Cover foreground/background results, cancellation,
large outputs, secrets spanning chunks and missing/pruned source records.

### R17-6 — Copy the last answer

**Evidence.** OpenCode [b0d762154c31](https://github.com/anomalyco/opencode/commit/b0d762154c31)
assigns `/copy` to the last assistant message. Its immediate follow-up
[bd902fcb467d](https://github.com/anomalyco/opencode/commit/bd902fcb467d)
also allows a completed compaction summary. The actual selection is in the pinned
[session route](../../references/opencode/packages/tui/src/routes/session/index.tsx).

**Existing Rook / gap.** Rook already has transcript views, normal terminal text
selection and profiled HTML export. CLI/TUI chat commands and the browser do not
offer a dedicated last-answer copy action. Copying the whole rendered transcript
or selecting across a long tool trace is a different interaction.

**Adaptation and acceptance.** Select the last assistant prose from a bounded,
branch-aware saved prefix in core. Provide browser copy, TUI/REPL `/copy`, and a
CLI plain/JSON retrieval path. Clipboard transport belongs to the frontend; use
explicit user action, bounded text and a readable fallback when unavailable.
Never include reasoning, tool payloads or private-input values by default. Make
summary copying a separate explicit choice because Rook's `share` profile omits
compaction internals. Test rewind/fork, tool-only replies, empty text, incomplete
stream output, UTF-8, no clipboard utility, redirected output and terminal framing.

### R17-7 — Measure a summarizer-specific output budget

**Evidence.** Pi [6fcb48670fc0](https://github.com/earendil-works/pi/commit/6fcb48670fc0)
removes fixed output allocations from coding-agent summary requests, leaving
provider/model sizing in charge. This is from the coding-agent package, separate
from the durable harness examined in R16. See pinned
[compaction](../../references/pi/packages/coding-agent/src/core/compaction/compaction.ts)
and [reasoning tests](../../references/pi/packages/coding-agent/test/compaction-summary-reasoning.test.ts).

**Existing Rook / gap.** [summary_request](../../crates/rook-core/src/agent/compaction.rs)
caps output at `min(output_room, 2048)` and requests low effort. Per-model R16
compaction overrides control trigger/reserve/retained tail, not this separate
summary output cap. Low effort does not guarantee that every model leaves enough
visible summary after spending output on reasoning. No quality regression was
measured in this review.

**Adaptation and acceptance.** Experiment with an explicit summary output budget
bounded by the summarizer's actual window, provider output ceiling, remaining
cost/time and existing byte admission. Keep the current value when omitted.
Compare fixed versus model-aware budgets on long real task transcripts, including
reasoning-only/truncated replies and a small summarizer behind a large main model.
Score preservation of goal, refusals, unresolved work and source references, plus
latency/tokens. Only retain a default change if measured benefit justifies its
cost. If the model is unavailable or there is no improvement, report the result
and preserve the current default. Do not copy an unbounded-output interpretation
or infer an improvement from upstream's deletion alone.

### R17-8 — Explicit reasoning capabilities for a configured model

**Evidence.** OpenCode [d889cb20447f](https://github.com/anomalyco/opencode/commit/d889cb20447f)
reads model reasoning support, toggle, effort levels and budget ranges from
configuration. The pinned [provider schema](../../references/opencode/packages/schema/src/config/provider.ts)
and [provider tests](../../references/opencode/packages/core/test/config/provider.test.ts)
cover these overrides; variant construction consumes the resulting capabilities.

**Existing Rook / gap.** [ModelCapabilities](../../crates/rook-llm/src/catalog.rs)
and [Observed](../../crates/rook-core/src/model_catalog/runtime.rs) already handle
endpoint/cache reasoning facts and effort-level intersections. They deliberately
cannot teach a dialect an unknown wire parameter. [ModelSource](../../crates/rook-core/src/config.rs)
has no manual reasoning-capability override, and the
[OpenAI adapter](../../crates/rook-llm/src/openai.rs) still requires a known effort
mapping. Changing a requested effort alone cannot describe an unfamiliar local
model's support. This is a configuration gap, not a claim that every configured
model currently ignores effort.

**Adaptation and acceptance.** Add bounded, validated per-source reasoning
declarations with explicit provenance and inheritance. Resolve them alongside
observed metadata and show requested versus effective behavior in model status,
doctor and all settings frontends. Use only wire controls explicitly supported by
each adapter; do not treat a capability flag as permission to invent an arbitrary
request-body field. Support disabling reasoning controls as well as known effort
levels and validated budgets, and explain unsupported declarations. Test an
unrecognized model alias with a supported wire mapping, omitted/stale metadata,
explicit disable, impossible budgets, fallback to another model and endpoint
rejection. Preserve producer-scoped reasoning replay. A changed capability
declaration must invalidate the relevant cached/effective route facts.

## Decisions by reference and deferred directions

| Reference | Decision |
|---|---|
| ACP | Eight incoming commits include schema/release work, foreground lifecycle wording, top-level `_meta` patches and v2 mismatch errors. This refresh is not registry-only. Inspecting the v2 draft supports a future explicitly negotiated adapter; preserve current v1 and do not silently reinterpret its lifecycle or merge rules. No v2 implementation proposed as a quick port. |
| Cline | No incoming commits; keep its existing reviewed snapshot and prior adoption decisions. |
| Codex | Four commits include content-bound plugin-hook trust, structured evidence, sender context and compatibility-test release lookup. Content fingerprints are a useful future extension-trust design, not evidence that Rook has Codex's approval-reuse bug: Rook plugins currently package skills/MCP, while hooks are separately configured. Before adding a trust registry, define which Rook grant is pinned and how runtime/dependency changes and execution races are handled; retain the ban on automatic workspace-plugin MCP startup. No Guardian or app-thread subsystem port. |
| Goose | One dependency/advisory-policy commit. Check dependency advisories in Rook's own graph when relevant; copying lock versions or waiver dates does not add a feature. |
| Hermes | Select combined settings, reachable drain and typed delegation; compression cancellation is supporting evidence. Route/credential fixes reinforce existing R16 route binding. Messaging adapter moves do not justify adding every platform or a plugin runtime. Queue exhaustion fixes inform acceptance checks; no Rook message-loss incident was reproduced. |
| OpenClaw | Select compaction phase control and purpose-specific fallback. Stream repetition protection already exists in `agent/repetition.rs`, including bounded per-channel tails and distinct-row exclusions. Do not gate it on guessed parameter counts or duplicate it. UI migration, V8 heap attribution and PostgreSQL work are architecture-specific. Session/reindex/import performance changes need a Rook profile before a port. |
| OpenCode | Select last-answer copy and explicit reasoning-capability configuration; retain existing observed metadata and effort negotiation. New experimental dialog designs can inform targeted accessibility work, not a wholesale TUI redesign. Browser-opener errors are useful failure-path evidence, not a new authentication subsystem. |
| OpenHands | One onboarding SAML destination fix; no matching Rook organization/SAML onboarding capability to port. |
| OpenResearch | No incoming commits. Cross-session attention was already adopted in R16. |
| Pi | Select only the summarizer-budget experiment. Whitespace tool-result normalization is a small transport edge case to check against Rook's own wire shapes; provider default-model substitutions are not general features. |

## Boundaries and verification

Implement accepted capabilities in shared core with CLI/API/TUI/browser parity,
bounded settings and front-end-owned services. Existing cancellation, permission,
secret and writer-ownership rules stay authoritative. A declined or unanswered
question/credential is an ordinary outcome; never count silence as permission,
retry a refused credential in a loop, or block unrelated available work on it.
Update/version JSON companions deliberately; do not add fields to persisted
postcard records without migration. Remote backend and external ACP paths must
report unsupported contracts rather than claim enforcement they cannot provide.

This pass verifies source/pin identities, ancestry, clean reference worktrees,
review links and the documentation/gitlink diff. It does not claim that proposed
features are implemented or that upstream tests were run. The full Rook runtime
gate is not rerun for this documentation-and-reference-only change. No model,
daemon, user installation or global cache setting was changed by the review.
