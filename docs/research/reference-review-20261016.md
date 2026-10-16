# Reference review — 2026-10-16, after v0.15.0

This is the historical source review and backlog proposal. Subsequent implementation, checks and verification limits are recorded in the [October 16 adoption record](../reference-r16-adoption.md).
Rook baseline: `ed5e4442c0388ddf0b5508cf9b8398f3f647547d` (`v0.15.0`).
All eight candidates from the [previous review](reference-review-20261015.md)
are implemented; see the [adoption record](../reference-followup-adoption.md).
The proposals below describe additional work, with existing behavior called out.

## Snapshots and coverage

All ten clean reference worktrees were fetched from their configured branches
with `--no-tags --depth=200`, checked out detached at the fetched tips, and their
gitlinks staged. No reference source files were edited. These are the snapshots
observed during this review, not a promise about later upstream branch tips.

| Reference / branch | Previous pin | Reviewed pin | Incoming commits / coverage |
|---|---|---|---|
| acp (`main`) | `7855babf8bee9db299ce668dfa911289f4d1cf4e` | `9bec5b0975238dabafaa84d53a6c0aebbd5fac75` | 2 |
| cline (`main`) | `c8f808b51e2ae80ac7fb7b41c6a780a0bdbdec89` | `01b8a9724da905423e636396d7fb4acdf47796bb` | 10 |
| codex (`main`) | `97ebf2d94d62e2ff2045f3ea17b6e11eae9a21fb` | `6ae8b8aa7c608730d4315d59c5c081897ae02780` | 13 |
| goose (`main`) | `0df818175bc854229746c81f01ea154b1c6c5e9e` | `ea4ad800d12c7c6cbb44b738921ce3e888aa4448` | 5 |
| hermes (`main`) | `60f56a6b13c8eb9a9e088c6fc1165257c92f7e11` | `34e8635cb9cfa6e55ee31b337f005697e2db3d37` | 255 fetched; bounded history |
| openclaw (`main`) | `0ee67b8da3820cf33035d7612bbfb98516280e59` | `51042e8c1a27c1361ad5f270c89fecc86acd432b` | 200 fetched; bounded history |
| opencode (`dev`) | `412b2b1d8563b665b5e7fed56cb46f7c333f34d1` | `50d4280dbdb2a8e5d9c89eb6c893d12c7a37f69f` | 33 |
| openhands (`main`) | `cb835dd3283b1974b8dc88efe98793eb4c4fd2d0` | `ccdb08783cf83ffe4d72d8c41298c700fe7fda94` | 15 |
| openresearch (`main`) | `816ce27a6b25bb1291c9df98e0e6782e9640fb91` | `8fc90cf355cda1e89a6aabc02d49a098f159f463` | 6 |
| pi (`main`) | `4954d760504c11df9b2cbdc040346107b6a8153d` | `0fbdbd3f8ab1876ac260c0f08226a400759323b8` | 4 |

For eight references the previous pin is a proven ancestor and the counts cover
the incoming range. For **Hermes and OpenClaw the previous pin was not reached
within the shallow graph**. Their counts describe fetched reachable commits,
not the complete incoming range; this is not evidence of an upstream history
rewrite. All fetched commit subjects were screened, then selected implementation
and test paths were inspected. This was not an exhaustive audit of every diff.
For the two bounded histories, conclusions rely on the selected source at the
new snapshot. No third-party builds, model runs or performance experiments were
performed. A bounded CodeGraph query timed out; focused source inspection was
used instead.

## Recommended backlog

P1 means first in this proposed adoption order, not a confirmed vulnerability.
Effort is relative: M is a focused shared-core feature; L crosses persistence,
protocol or execution boundaries. Neither is a time estimate.

| ID | Priority / effort | Capability | Reference | Additional value in Rook |
|---|---|---|---|---|
| R16-1 | P1 / M | Durable model selection with explicit queued/effective status | Hermes | Acknowledged selection survives restart; retries cannot overwrite a newer choice |
| R16-2 | P1 / M | Attention inbox across sessions and branches | OpenResearch | Find live questions, approvals and private-input requests without opening each chat |
| R16-3 | P1 / L | Daemon scheduling and blocking diagnostics | OpenClaw | Distinguish lack of agent progress from delays inside the daemon |
| R16-4 | P1 / M | Export profiles for sharing a conversation | OpenCode | Explicitly omit internal payloads and show what will be shared |
| R16-5 | P2 / M | Compaction policy per model | Pi durable harness | Tune reserves and retained history for different model windows and workloads |
| R16-6 | P2 / L | Recipe-declared tool and extension requirements | Goose | Reproducible recipe environments with no authority increase |
| R16-7 | P2 / M | Terminal program status via OSC 7501 | Goose | A supported terminal can distinguish working, finished and waiting for input |
| R16-8 | P2 / L | Stream SSH file payloads instead of assembling transfer buffers | Codex | Reduce peak memory for large permitted file transfers while preserving recovery |

### R16-1 — Durable model selection, not a second model switch

**Evidence.** Hermes `51357f428393596f9691df810d36e3ef2d519643`
queues a busy-session model choice until the next claim boundary;
`4dcad0e701a9faa0b29486920fe742c1dfd2a74c` stores the mutation receipt
and deferred choice together. See
[session_deferred_pick.py](../../references/hermes/gateway/session_deferred_pick.py)
and [busy-model tests](../../references/hermes/tests/gateway/test_session_mutation_model_busy.py).
`c473d338dc330255eaca85ca5f4d13a580f846af` also binds credentials to
the resolved route, in
[session_mutation_model.py](../../references/hermes/gateway/session_mutation_model.py).

**Existing Rook / gap.** [Settings and ClientMessage::Setting](../../crates/rookd/src/chat.rs)
already leave the active request's provider fixed and apply a model change to a
later turn. They update the in-memory setting before saving follow-up / managed
state, can report a persistence failure after the change, and do not carry a
request identity or separate queued/effective model statuses. The missing part
is an acknowledged durable transition, not basic switching.

**Adaptation and acceptance.** Use a shared session mutation with bounded,
versioned receipts and a generation/revision check. Distinguish the running
model from the accepted next model. A failed durable write must not acknowledge
a durable change. Verify busy switch, idle switch, restart, two clients and
A→B→retry-A: retry-A returns its original receipt without replacing B. Apply
once at the next turn boundary. Preserve Rook's existing resolved model/API/URL/
credential tuple from [ModelSource](../../crates/rook-core/src/config.rs);
revalidate if its route changed. This review did not establish a credential leak
in Rook. Expose the same receipt and status through CLI/API/TUI/browser.

### R16-2 — A cross-session attention inbox

**Evidence.** OpenResearch `0165a286d81a56af5e0f471de04c6608a87d1c22`
adds asynchronous question cards and waiting indicators. The pinned
[hasPendingInput](../../references/openresearch/ui/src/chatRendering.ts) follows
the active branch, and
[ChatPanel SessionRow](../../references/openresearch/ui/src/components/ChatPanel.tsx)
uses loaded state or a saved `awaitingInput` summary.

**Existing Rook / gap.** Rook already has questions, approvals, private input,
reconnect snapshots, session trees and activity stages.
[Live::inputs / needs_input](../../crates/rookd/src/chat.rs) can recover the
current requests of a live owner. However,
[SessionSummary](../../crates/rook-core/src/service.rs) and the
[session list](../../web/dist/sessions.js) do not project an attention inbox;
the browser currently offers a per-session activity check instead.

**Adaptation and acceptance.** Add a bounded shared projection of *live* waiting
requests, grouped by parent/child and branch, with count/type and a navigation
action. Saved activity is a hint, not proof that a request remains answerable
after restart. Resolve current requests on attachment; bind every response to
the session, owner generation and request identity. Verify two simultaneous
sessions, inactive branches, completed/replaced requests, reconnect and restart.
Do not put secret values or sensitive question bodies into list summaries.
Provide skip/decline and continue independent work when possible. Ignoring a
request is not consent; a declined secret is not repeatedly requested. Retain
explicit permission checks even if ordinary question defaults are configured.

### R16-3 — Diagnose daemon delays separately from agent inactivity

**Evidence.** OpenClaw `8e234e5a43b382e0e9c50c19c05cf21ff845df8f`
adds bounded attributed synchronous-stall observations:
[main-thread-stall.ts](../../references/openclaw/src/infra/main-thread-stall.ts),
[event-loop-health.ts](../../references/openclaw/src/gateway/server/event-loop-health.ts),
and [request diagnostics](../../references/openclaw/src/gateway/server/ws-connection/request-diagnostics.ts).
Its bounded recorder counts omitted observations instead of growing indefinitely.

**Existing Rook / gap.** [Activity observation](../../crates/rookd/src/chat/activity.rs)
already distinguishes queue/model/tool/approval/question inactivity and saves
last progress; [phase timers](../../crates/rook-core/src/diagnostics.rs) record
request and dispatch durations. The existing inactivity threshold does not
measure executor scheduling lag or attribute time spent waiting for a blocking
worker / lock. A timer running on the same delayed executor may itself be late.

**Adaptation and acceptance.** Measure a small set of queue, scheduling and
blocking-work boundaries, with bounded aggregation and explicit unavailable
states. Adapt to Tokio's multithreaded runtime; do not transplant Node's
single-main-thread assumptions or its 250 ms threshold. Keep correlation IDs
out of metric labels and do not log prompts, command bodies or secret values.
Controlled checks must distinguish slow model I/O, a busy admission queue,
blocking work, an unanswered question and a disconnected owner. Correlation is
an observation, not proof of which task caused a scheduler delay. Measure probe
overhead before enabling it by default; do not automatically kill/retry a turn
because a threshold was crossed.

### R16-4 — Explicit share-export profiles

**Evidence.** OpenCode `706a688994c2343243615b0bd38083fc8fe69671`
removes compaction `providerContext` from sanitized exports.
[sanitizeMessage](../../references/opencode/packages/core/src/session/transfer.ts)
also substitutes/redacts text and opaque provider state. This upstream export
is evidence for explicit content handling, not a ready-made Rook export format.

**Existing Rook / gap.** [HTML export](../../crates/rook-cli/src/commands/html_export.rs)
is a bounded, escaped local transcript artifact, with no automatic publication.
It offers no content profile. HTML escaping protects rendering, not disclosure.
Rook's [support diagnostics](../../crates/rook-core/src/diagnostics.rs) already
exclude conversation payloads by default; that is a separate capability.
No secret-storage bypass was found by this review.

**Adaptation and acceptance.** Define a core export policy shared by CLI/API/UI:
a shareable conversation profile omits reasoning, compaction internals, opaque
provider state and tool arguments/results unless selected; a fuller local
archive remains explicit. Show a bounded preview and an omission manifest.
State that retained user/assistant prose may itself contain private information;
never claim a content profile proves an artifact anonymous. Keep existing
redaction, CSP, byte/page limits and atomic no-overwrite publication. Verify
all event kinds, old sessions, private-input markers, hostile HTML and omitted
payloads; selected event ranges must not pull hidden data back into the export.

### R16-5 — Per-model compaction policy

**Evidence.** At Pi's new pin,
[compactionPolicy](../../references/pi/packages/durable/src/harness/compaction.ts)
merges `modelOverrides[provider/modelId]` with the global policy field by field;
[CompactionSettings](../../references/pi/packages/durable/src/harness/types.ts)
defines the contract. Reviewed the aggregate old→new diff and
[harness tests](../../references/pi/packages/durable/test/harness-compaction.test.ts).
The individual `c4cf7957b4e6` subject is insufficient evidence: its diff alone
contains reversions, so the recommendation is based on the pinned implementation
and aggregate range, not that title.

**Existing Rook / gap.** [ModelSource / Agent](../../crates/rook-core/src/config.rs)
already support model-specific context windows and a separate summarizer, and
v0.15 budgets the actual summarizer request. There is no model-specific policy
for retained recent history / reserves. These are different concerns from the
already completed summarizer admission fix.

**Adaptation and acceptance.** Begin with optional overrides for measured
reserve and retained-tail budgets, keyed by the resolved configured source.
Have one effective policy feed proactive compaction and overflow recovery.
Preserve hard admission limits and required tool-call/result relationships.
Verify switching between small and large windows, sparse overrides, invalid
budgets, overflow recovery and a separate small summarizer. Keep current
defaults until local quality/latency/context-retention measurements justify
changing them; additional knobs alone do not establish an improvement.

### R16-6 — Recipe requirements and restricted equipment

**Evidence.** Goose `21d873eec92ad2f0cd7f39d00680b4e3dbc41f9a`
makes slash/delegated recipes honor declared extensions.
[resolve_subagent_config](../../references/goose/crates/goose/src/agents/platform_extensions/summon.rs)
resolves recipe extensions first and then restricts them with the caller filter.

**Existing Rook / gap.** [Recipe](../../crates/rook-core/src/recipes.rs) and
[composite recipes](../../crates/rook-core/src/composite_recipes/mod.rs) already
freeze definitions, validate parameters, pick a configured model/skill and
restrict budgets. They do not declare required tools or MCP extension sets.
A prompt that mentions an extension is not a reproducible environment contract.

**Adaptation and acceptance.** Declare requirements by configured names, with a
preflight that explains missing capabilities before spending a model request.
Treat required/allowed equipment as a restriction and availability check; a
recipe cannot install/start arbitrary server commands, supply credentials or
expand the parent policy. Define omitted versus explicit-empty sets, and
intersect child capabilities with parent authority. Reuse front-end-owned MCP/
LSP lifetimes. Verify nested recipes, unavailable equipment, denied startup,
empty lists, stale frozen definitions and unchanged legacy recipe behavior.
A user declining a credential leaves a clear unavailable/skip result, not a
repeating prompt or a permanently blocked unrelated task.

### R16-7 — Terminal lifecycle status

**Evidence.** Goose `5eca486214c86945ac6de2603ee27f51083822fb`
implements [OSC 7501 program status](../../references/goose/crates/goose-cli/src/session/program_status.rs)
for working/done/error/blocked states, gated on text TTY output and bounded
sanitized messages.

**Existing Rook / gap.** [notify.rs](../../crates/rook-cli/src/notify.rs)
provides a terminal bell with opt-out. It does not convey the lifecycle or
whether a terminal tab needs an answer rather than a completion notification.

**Adaptation and acceptance.** Map existing shared lifecycle/input events into
an optional terminal reporter. Use literal status codes, no task/secret text by
default, and clear state on exit/cancel. Emit nothing into JSON, redirected
output or unsupported/disabled terminal modes; keep terminal configuration and
help explicit. Verify normal completion, error, question, permission, private
input, cancellation and overlapping child work. Terminal support is not
universal, so retain the existing visible statuses and bell behavior.

### R16-8 — Streaming SSH transfer with retained atomic publication

**Evidence.** Codex `a45b8ccb016fb74f63a6bc29e701568e387cbe38`
adds `write_file_stream` in
[environment_accessor.rs](../../references/codex/codex-rs/file-system/src/environment_accessor.rs),
[remote_file_system.rs](../../references/codex/codex-rs/exec-server/src/remote_file_system.rs)
and [stream tests](../../references/codex/codex-rs/exec-server/tests/file_stream.rs).
The upstream contract permits partial replacement after failure and buffers on
older executors; neither behavior should be adopted blindly.

**Existing Rook / gap.** Rook's remote/file operations already have limits and
recovery. In the SSH environment,
[Snapshot::capture / frames](../../crates/rook-tools/src/environments/sync.rs)
holds base64 file contents and builds a complete transfer `Vec<u8>`;
[ssh.rs](../../crates/rook-tools/src/environments/ssh.rs) passes this buffer to
the transport. This is bounded by configuration, but peak memory scales with
the permitted snapshot size and additional encoded copies. It is not evidence
that existing transfers are unbounded or that every remote backend needs work.

**Adaptation and acceptance.** Stream source-addressed binary chunks through a
negotiated SSH transfer version, retaining per-file/total byte limits, source
hashes, preimage checks and existing recovery receipts. Stage bytes before
atomic publication and clean up cancelled temporary files. Older workers must
use an explicitly bounded compatibility path or refuse; no hidden unlimited
buffering and no partial successful file. Verify binary/empty files, mid-stream
source changes, cancellation, disconnects, disk-full, hash mismatch and old
workers. Measure peak RSS and transfer latency at a configured bound before
claiming a performance win. Keep large payloads out of model tool arguments.

## Other incoming work: disposition

| Reference | Reviewed changes / decision |
|---|---|
| ACP | Both incoming commits change registry documentation. No new negotiated runtime protocol to port; keep existing ACP compatibility. |
| Cline | Ten incoming commits mostly cover release/editor/packaging/dependency and provider telemetry/retry details. No distinct core feature selected. Do not transplant retry wording or packaging-specific CA handling without a matching Rook transport need. |
| Codex | Select streaming-transfer concept above. Root suspension with live descendants (`341de32c33fb`) is valuable lifecycle evidence, but Rook already has stop/pause/recovery. Defer a separate ownership-transfer feature until required: root journal flush alone must never imply descendant writers have stopped, and interrupted descendants must not restart automatically. SQLite/NFS reclamation does not apply to Rook's redb store. |
| Goose | Select recipe equipment and terminal status. OpenRouter context discovery/caching is the same class of capability as Rook's existing bounded runtime model catalog; no duplicate generic discovery feature proposed. |
| Hermes | Select durable model mutation receipts. Large portions of the fetched graph are plugin-catalog updates; catalog entries do not establish that plugin implementations were reviewed. Existing delivery/recovery and credential-route rules remain constraints, not claims of newly found Rook defects. |
| OpenClaw | Select attributed daemon diagnostics. Context breakdown, bounded recall, browser diagnostics and local endpoints without mandatory API keys already have Rook equivalents. Session-list optimization needs a measured bottleneck. UI-framework migration and PostgreSQL support are not useful ports for this architecture. Descendant wakeup/ownership fixes inform future lifecycle checks rather than a duplicate feature. |
| OpenCode | Select share-export profiles. Typed MCP timeouts, request admission and recent diff work already have equivalents. Ignoring unreadable config must not replace Rook's explicit errors for policy-relevant configuration. |
| OpenHands | Semantic colors, contrast, dialog sizing and streaming word fades are useful UI references, but no new core capability selected. Prefer targeted accessibility fixes after finding a concrete issue; do not add animation timers or adopt its frontend framework for parity. Multi-organization Azure DevOps discovery is deferred until Rook needs a first-class integration. |
| OpenResearch | Select cross-session waiting indicators. Per-project GitHub owner selection is useful for publishing workflows, but existing external CLI/skills can carry it; do not introduce an account/publisher subsystem solely for this review. Contrast and feedback UI changes are presentation references. |
| Pi | Select per-model compaction from the durable harness, not an assumed coding-agent CLI feature. Guarded transport retries and producer-scoped reasoning already exist in Rook; retain partial-output/completion evidence when classifying retries. |

## Adoption boundaries

Implement ideas independently in the appropriate shared Rook layer, preserving
CLI/API/TUI parity and front-end-owned service lifetimes. Use bounded versioned
KV/labels where appropriate; do not extend stored postcard records without a
format migration. Every new setting needs validation, help and runtime wiring.
Question/secret refusal must be a supported outcome. Measure experimental
quality/performance claims with the local model/environment before adopting a
default; no translation or model-routing quality improvement was established by
this source review. No runtime changes or new release are part of this pass.
