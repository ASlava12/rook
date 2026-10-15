# Reference functionality adoption

## Second adoption scope — 2026-10-15

The user requested every candidate from the October 15 review. The whole scope
remains active until each row is implemented and verified, except cache changes
which require measured benefit before adoption. ACP v2 remains a draft watch
item, as recommended in the review, not a replacement of the working v1 adapter.
Starting commit: `9cab33f`; refreshed reference gitlinks and the resource/input
baseline were saved in local commit `65b4f40`. Their snapshot table is in
`references/PORTED.md`; no push, release or daemon restart is part of this batch.

| Requirement | Acceptance scope | State |
|---|---|---|
| Correlated diagnostics | Provider request IDs and root/child/ACP/MCP trace identity; local bounded reports; no credential/transcript export by default; real transport/reopen checks | Verified |
| MCP resources/templates | Bounded paged discovery and source-addressed reads, resource-only servers, common policy/engine/frontends, cancellation and transport/reconnect checks | Verified |
| Private secret requests | Private conversation-bound collection, only name/status returned; decline, dismissal, no answer, timeout, disconnect and replay are ordinary states; no value in model/history/compaction | Verified |
| MCP elicitation | Server-initiated form/URL requests, exact connection/call identity and common user interfaces; explicit refusal, timeout, cancellation and concurrent ID collisions | Verified |
| PDF documents | Bounded local extraction with selected pages and page provenance; ordinary text-only local model; optional scan rendering/OCR with explicit partial notices; actual document/frontend checks | Verified |
| Remote workspace files | Explicit SSH-native read/edit/search/list and execution, remote path identity, bounded transfers, checkpoint/undo and disconnect ownership; preserve existing replica mode | In progress |
| Prompt cache experiment | Supported dialect identity and effort changes; compare cache hits, first-token latency and cost on equal tasks; adopt only measured improvement | Verified; no change adopted |
| Domain network policy | Opt-in domain rules and attributable decisions, enforced with OS containment on supported platforms; direct-bypass and allowed LAN/registry checks; explicit unsupported behavior | Pending |

User clarification: a person may choose not to answer a question or provide a
secret. Absence of an answer is never consent. A declined or expired request must
not repeat itself, silently select a default, leak private values, or certify
task completion. The task either proceeds without the optional input or reports
the concrete missing prerequisite. Tests must exercise these real control paths.

This section is the current work tracker. The older evidence below describes
the previous fourteen-item scope; it cannot prove these additions complete.

Current verification snapshot: the complete resources/private-input/correlation/
elicitation/PDF tree passes `cargo xtask ci` in the main checkout
(`/tmp/rook-adoption-mcp-pdf-ci-final.log`, exit 0, `RUST_TEST_THREADS=4`). The
first integrated gate found the explicit checker admission list missing PDF;
that list now admits the bounded read-only decoder, and the full rerun is green.
Native CLI/local TUI/daemon TUI form acceptance and refusal pass; stdio/HTTP
ownership, colliding IDs, duplicate/flood refusal and cancellation pass. PDF
source/page hashing, continuation, scan/render/local OCR and native CLI/daemon
text-only model checks pass (`/tmp/rook-pdf-native-final.log`,
`/tmp/rook-pdf-cli-native-final.log`). The optional Poppler/tesseract tests were
run explicitly with installed local tools. The deployed Qwen cache experiment
found no useful improvement, so no caching or effort parameter is adopted; see
[the observations and limits](research/prompt-cache-20261015.md). SSH-native and
domain containment work remain unfinished. No installed daemon was restarted.

Elicitation implementation constraints: route server requests before responses
so a server request ID cannot consume a client response waiter; support integer
and string IDs with separate namespaces and bounded per-connection deduplication.
Bind handlers to the originating invocation and connection generation. HTTP
callbacks belong to the POST stream that carried them; shared stdio needs an
exclusive invocation lease while interaction is possible, with retirement after
ambiguous cancellation. Late human answers must never bind to a new invocation.
Use bounded flat forms with validation and explicit review/submit, decline and
cancel. URL mode displays the canonical host and URL for a user-initiated visit;
opening it is distinct from completion of third-party authorization. A skipped
or absent answer cannot accept defaults or cause automatic re-prompting.

### October 15 implementation evidence

Correlated diagnostics now records physical generation spans, bounded response
IDs and common root/child trace identity in existing JSON notes. Local support
exports select those metadata fields without prompts, model URLs or credentials.
`telemetry.propagate_trace` is opt-in for native LLM headers and MCP/ACP metadata;
the default is local correlation only. Native OpenAI, Responses, Anthropic and
Google HTTP checks, real retries, failover and stream cancellation pass in
`/tmp/rook-correlation-wire-tests-final.log`. Native generation report/reopen and
MCP resource propagation checks pass in `/tmp/rook-correlation-engine-tests.log`;
actual delegated ACP writes retain a common trace and carry the measured ACP
span in `/tmp/rook-correlation-acp-test.log`. The merged all-target check passes
(`/tmp/rook-correlation-merged-check.log`). ACP session metadata/load checks pass
in `/tmp/rook-correlation-acp-session-final.log`; reached ancestry bounds and
corrupt links pass in `/tmp/rook-correlation-unit-tests.log`. All-target Clippy
for the engine, native providers, MCP/ACP, CLI and daemon passes in
`/tmp/rook-correlation-clippy-final.log`. Final CLI export and combined regression
checks remain pending.

The first complete private-input gate ran all workspace suites. The only failed
test was the no-panic source audit, identifying four newly added panic sites in
the MCP resource fixture. Those now return I/O/protocol errors; the audit rerun
passes in `/tmp/rook-private-input-no-panics-rerun.log`. The other suites,
including the corrected resource CLI scenario, passed in
`/tmp/rook-private-input-full-ci.log`; its exit remains 101, not a green gate.

MCP resource/template pages and resource reads now use the existing stdio/HTTP
transports. Resource-only connections are retained without tools/list. Shared
equipment registers bounded resource helpers, ordinary external-call policy,
source hashes, UTF-8 text continuation and normal image handling. No store or
wire enum layout changed. Unsupported binary content is described rather than
injected as base64; native PDF handling remains a separate pending requirement.

The focused native MCP resource tests passed (process cancellation,
ambiguous disconnect without replay, reached count/byte limits and capability
validation): `/tmp/rook-resource-focused-mcp.log`. Core resource tests passed
(HTTP/SSE, local server discovery without I/O, bounded complete paging, changed
sources, Unicode, binary/image admission, shared equipment and durable results):
`/tmp/rook-resource-core-tests.log`. A subsequently added explicit external-policy
assertion first failed compilation because Policy::compile returns a tuple; the
test was corrected to assert its diagnostics before using the policy. Its rerun
and the dead-API checks passed in `/tmp/rook-resource-focused-core.log`.
Clippy for all targets in rook-mcp, rook-tools, rook-core and rook-cli passed in
`/tmp/rook-resource-clippy.log`. Existing browser tool-card and chat rendering
checks passed in `/tmp/rook-resource-web-tests.log`. The CLI/daemon native
scenario and full regression gate remain pending; these focused passes do not
close the entire feature or goal.

The user's refusal clarification exposed a separate terminal bug: EOF was fed
through empty-line defaults, including an affirmative approval. Terminal EOF
now stays unanswered, blank approvals require an explicit affirmative answer,
and `/skip` declines any question shape. Unanswered-question guidance explicitly
forbids inventing credentials, facts or permissions and says to report essential
missing input. The focused ask tests pass in `/tmp/rook-input-ask-tests.log`;
terminal helper assertions also pass in `/tmp/rook-input-cli-tests.log` (the
command additionally visits filtered integration targets). This fixes the existing input
baseline. Private requests are now implemented in the separate private-input
worktree, with final integration verification still in progress.

Private-input focused checks pass: broker ownership, duplicate/late replies,
reached capacity, refusal, timeout, cancellation and missing listener
(`/tmp/rook-private-input-tools-tests.log`); the real engine's same-turn command
use/redaction and repeated refusal suppression, plus preservation of concurrent
Vault edits (`/tmp/rook-private-input-core-tests.log`); browser private HTTP
submission and clearing on disconnect (`/tmp/rook-private-input-web-tests.log`);
and the daemon HTTP route's delegated owner binding, status-only errors and
replay rejection (`/tmp/rook-private-input-api-test.log`). Native local/daemon
PTY hidden-entry/save/skip checks pass in `/tmp/rook-private-input-pty-final.log`;
the initial fixture repeated calls forever and was corrected to finish after
one request. Durable receipt cancellation, replacement, no-overwrite and reopen
checks pass in `/tmp/rook-private-input-receipts-test.log`. Final regression
checks remain pending. ACP deliberately advertises
no private input because ordinary permission choices are not a private channel.

Input integration constraints from current source: the ordinary Vault snapshots
entries per turn, so a newly saved secret needs an explicit same-turn refresh
before a command can resolve it. Secret values must use a private collection
path, not ordinary Question/Answer serialization. MCP equipment may be shared
by sessions; an elicitation response must bind to its actual connection
generation and invoking owner, never the most recently attached asker. Late
callbacks after cancellation must not bind to a newer call or conversation.

Verification follow-up: the first native CLI scenario reached its daemon fixture
but failed the existing 30s readiness deadline after rebuilding rookd; stderr was
empty and the fixture killed/reaped its owned child. A sample of the freshly
linked CLI test process separately showed it waiting in `_dyld_start` before
Rust execution. Fixture setup now runs the built sibling's `--version` before
starting the unchanged daemon-initialization deadline; the corrected native
scenario still needs a passing rerun. The earlier complete rook-mcp/rook-tools
regression command exited 0 in `/tmp/rook-resource-tests.log`.

The first workspace CI passed Clippy, then was explicitly interrupted during
build when the shared target volume had about 1 GiB free. It was not a passing
gate. `cargo xtask clean` subsequently reclaimed 20.55 GiB of incremental and
cross-target build caches (`/tmp/rook-reference-clean.log`); a preceding release
cache cleanup reclaimed 1.3 GiB. The current full rerun is
`/tmp/rook-reference-20261015-ci-final.log` and remains in progress. No agent
installation, real daemon restart or LAN host change was performed.

The user requested all fourteen capabilities from the reference review on
2026-10-11. This document preserves the full implementation and verification
scope across changes. A library helper, configuration field, or passing mock
alone does not establish delivery: each capability needs a usable product path.

Implementation and acceptance verification are complete for the full scope:
twelve capabilities are delivered; two conditional optimizations were evaluated
and removed. Runtime checks used scratch state; the installed daemon and operator
configuration were unchanged. Optimization defaults require measured
benefit on the user's local LLM; preserve baseline quality, report latency and
context/cost, and remove unsuccessful optimization changes. This includes Code
Mode and semantic retrieval; the earlier OPUS-MT experiment is not reopened.

## Acceptance criteria

| # | Capability | Required behavior and evidence | Status |
|---|---|---|---|
| 1 | Stalled-session notifications | Daemon-owned activity tracking distinguishes queue, model, approval and tool waits. Notify once per inactivity episode, reset on real progress, remain usable after a client disconnects, and expose last activity and session identity. Test progress resets, false alarms and detached clients. | Complete |
| 2 | Conversation recall | Model-visible bounded discovery and anchored reading of previous conversations with session/event provenance, workspace scope, lineage deduplication and explicit scan/output omissions. Historical content stays source data. CLI/API/TUI/browser retain access to the same history. Test real agent dispatch and retrieval of a lost task. | Complete |
| 3 | Code Mode | Execute model-written scripts through the ordinary tool dispatch, approval, checkpoint, journal and cancellation paths. Bound execution time, memory, calls and output; support batches, dependent calls and selected results without exposing host credentials or unrestricted interpreter capabilities. Compare multi-tool tasks against ordinary calling on the local LLM before adopting an optimization. | Not adopted: native comparison did not establish benefit; product prototype removed |
| 4 | Experiment tree | Persist experiment lineage, hypothesis, immutable source snapshot, parameters, run receipts, artifacts and measured metrics. Compare siblings with a baseline, retain failures and explicitly promote a winner. Expose creation, execution, comparison and inspection through the common engine and frontends. Verify reproducibility and restart recovery. | Complete: fixed source/parameter contracts, measured sibling comparisons, explicit promotion and uncertain-run recovery; real commands, CLI/HTTP, both TUI modes, browser checks and full CI pass |
| 5 | Change review | Review a selected diff/commit/base in a separate reviewer context; report findings with severity and exact file/line provenance. Let the user attach line comments and send them back as scoped corrections. Verify comments remain bound to the reviewed source and that the reviewer does not silently modify the workspace. | Complete: frozen read-only reviewer, immutable source/comments and common frontends; bounded unambiguous JSON admission with one narrative repair; actual final Qwen finding at ratio.py:2 with no writes; final full CI passed |
| 6 | Shell-change undo | Capture preimages for workspace changes made by foreground/background shell commands; retain new/deleted files and support selected restore with conflict detection. Budget captures without staging the whole repository or discarding existing user edits. Verify edits, creation, deletion, cancellation and delegation. | Complete: bounded foreground/background preimages, selected restore, conflicts, cancellation and recovery; common frontends and full gate pass; actual LAN model edits/create/delete were restored with a late-edit conflict check; its unrelated completion-classifier failure is retained |
| 7 | Semantic memory | Optional local embeddings plus lexical retrieval, explainable source attribution, bounded cache/index and keyword fallback. Demonstrate improved recall on paraphrases and Russian/English queries without regressing exact IDs; measure local cost and keep the existing path when no benefit is established. | Not adopted: native comparison regressed; product prototype removed |
| 8 | Learning after tasks | Produce evidence-bound, reviewable skill/memory proposals after completed tasks. Display diffs, deduplicate facts and preserve corrections/version history. Apply explicitly or under configured policy; failed tasks must not become unsupported rules. Verify proposal provenance, application and rollback. | Complete: task-bound review/revision, atomic memory application, selected user-skill versions, conflict-checked inverses and observed recovery; focused engine/frontends and full CI pass |
| 9 | Browser control | MCP-backed or native browser navigation, snapshot, click, type, wait and screenshot tools with ordinary policy/recovery accounting. Expose browser observations and results in the shared frontends. Verify a local web application end to end and record failures without replaying uncertain effects. | Complete: Chromium actions, cancellation, shared frontends and full gate pass; native LAN browser task makes one POST, captures PNG, closes the browser and returns the correct receipt; independent checker returns VERDICT: holds |
| 10 | Remote execution | Select local, SSH or container execution environments using trusted config. Reuse environment resources across turns; carry workspace paths, cancellation, bounded output, approvals and recovery. Verify local/container execution and SSH on the available LAN machine when credentials permit. | Complete: local/container/SSH execution, ownership and recovery; native Docker/OpenSSH and all frontends/full CI pass; authorized LAN root@192.168.1.72 verifies foreground/background, reuse, cancellation, recovery and owned release |
| 11 | Event-triggered tasks | Authenticated inbound event adapters launch ordinary durable tasks in a configured workspace/session. Bound payloads, deduplicate retries and show source/event identity; separate external data from permissions. Verify invalid auth, duplicate delivery, restart and task inspection. | Complete: signed bounded ingress, atomic source/deduplication and ordinary task dispatch; engine, actual CLI/HTTP/restart, both TUI, browser and full CI pass |
| 12 | Messaging client | Telegram adapter maps chats/topics to sessions and supports follow-up, stop, approvals and results. Persist routing, enforce configured sender access and expose outbound delivery state/retries. Verify protocol fixtures and live transport when user credentials are available; do not invent credentials or send unsolicited messages. | Complete: ordinary chat bridge and durable Bot API routing/delivery; protocol, actual CLI/HTTP/restart, both TUI modes, browser and full regression gate pass; live bot check explicitly skipped by the user on 2026-10-14 |
| 13 | Composite recipes | Declare named parameterized subrecipes, execute them in isolated contexts with sequential/parallel composition, pass bounded outputs and surface individual sessions. Shared budgets/policy only tighten; retain stage receipts and recover without replaying uncertain effects. Verify failure, cancellation and nesting limits. | Complete: frozen named definitions, ordinary sequential/parallel child execution, shared limits and bounded source/output receipts; failure, cancellation, nesting, CLI/HTTP/restart, both TUI modes, browser and full regression gate pass |
| 14 | ACP interoperability | Advertise implemented list/resume capabilities, provide filtered/paged discovery and correct lifecycle semantics, and permit configured external ACP agents as executors with their tools/progress/approvals bridged. Validate against the pinned ACP schema and socket tests; editor/provider behavior must be exercised, not just helpers. | Complete: discovery/resume/close, complete bounded root replay, session settings and selected workspace/MCP readiness; trusted external execution and native tools/progress/permission/cancellation/source paths; genuine Rook-to-Rook, CLI/API/restart, both live TUI and browser checks; final combined full gate passed |

## Implementation constraints

Use one engine behind CLI, API, TUI and browser. Keep persisted postcard types
unchanged; new state uses versioned, size-limited JSON companions. Every result
retains source authority, recovery uncertainty and secret redaction. New tools
must remain within measured catalog/context budgets. Do not copy reference
subsystems; implement the ideas within Rook's architecture.

## Delivery evidence

Record product entrypoints, focused checks, applicable runtime measurements and
the full `cargo xtask ci` result as implementation proceeds. A status becomes
complete only after the row's full behavior has been inspected and verified.

The 2026-10-11 gate completed with exit status 0 using
`CARGO_TARGET_DIR=/Users/vart/.cache/rook-reference-target cargo xtask ci`.
Formatting, all-target Clippy, frontend builds and the entire workspace test
suite passed. The log is `/tmp/rook-reference-ci-ssd.log`. Browser checks also
passed with `node --test web/tests/*.mjs` (`/tmp/rook-web-reference-final.log`).
The earlier gate exposed an idempotent-prompt attachment race and was stopped
while old binaries were delayed in the external disk's macOS loader; the final
isolated gate exercised every workspace target after the fix. Passing this gate
establishes regression coverage for the current changes, not delivery of the
remaining acceptance rows.

### Current implementation

- Conversation recall: `session_recall` tool, `rook session recall`, chat/TUI
  `/recall`, POST `/api/history/recall` and browser Search. Focused core checks
  verify workspace boundaries, inherited-prefix deduplication, deleted parents,
  anchored reading, scan limits, continuation and oversized bodies. Browser
  checks verify source links and discarded stale responses. A LAN check against
  `huihui-ornith-1.5-35b-a3b-abliterated-i1` on 192.168.1.46 recovered a
  synthetic lost task and its continuation code, then read the original session
  event with separate search/read calls. The successful recovery turn returned
  `end_turn`, changed no files or facts, and retained the correct session/event
  anchors. Its full durable tool-result body was inspected in byte pages, since
  the ordinary transcript display elides large JSON. Receipts are in
  `/Users/vart/.cache/rook-recall-live-check/20261011-retry`.
  The preparation turn's unnecessary command was denied and its completion
  remained unknown; that outcome is retained separately. The earlier check
  (`20261011`) discovered the code but rejected mixed query/session read calls;
  the tool schema now excludes that shape. The full gate passed.
- Inactivity observations: daemon-owned watchdog, one notice per episode,
  Activity transport, diagnostic JSON and browser Check activity. Focused checks
  verify real-progress resets, waiting-line exclusion, live user-input waits,
  bounded metadata, persisted owner uncertainty and joining after a period with
  no attached client. Generation-scoped transactional writes prevent late
  cleanup from overwriting the next owner. Publishing the ending marks the
  owner complete before cleanup returns, preserving idempotent prompt retries.
  Browser rendering and episode deduplication checks pass. Provider-capacity
  waiting now reaches the engine observer separately from physical generation
  admission: cancellation in a queue sends and bills no request. Foreground
  stdout/stderr and background-job pipes count actual bytes including omitted
  output; active dispatch samples fresh counts at most twice a second. Waiting
  prose and old job snapshots cannot reset inactivity. Standalone managed workers
  own a watchdog with cancellation cleanup and live diagnostic overlays; an old
  completed chat cannot shadow their current owner. Their notices are retained
  in the companion and daemon log; conversations additionally publish Activity.
  Focused queue, pipe and daemon tests exited 0. The actual HTTP two-workspace
  worker fixture reaches one held generation and one queued request without a
  client, observes both stalls and verifies persisted stopped owners after
  cancellation. The final full regression gate exited 0; see the activity gate
  record below.
- ACP list/resume/close: advertised capabilities, bounded filtered discovery with
  cursors, resume without historical extension-preview replay, and session-owned
  equipment cleanup. Native editor/process checks exercise cancellation and
  resource ownership; the lifecycle gate record below retains its current status.
  Configured external execution and common frontend behavior were subsequently
  completed and checked; see the final ACP combined gate and completion audit.
- Semantic memory: `xtask/probes/semantic-memory/bench.py` compares the actual
  production lexical scorer with local BGE-M3 and Nomic embeddings on a fixed
  synthetic corpus. The [first measurement](research/semantic-memory-20261011.md) found improved
  BGE-M3 retrieval with preserved exact identifiers. A subsequent paired LAN LLM
  probe improved exact supporting-source selection from 10/30 to 24/30, with
  fourteen wins and no losses, at increased latency and token usage. Incorrect
  prose and incomplete/malformed outputs are retained separately; source scores
  do not certify answer quality. Product integration and actual agent-task
  validation were still pending at that point; the final native comparison below
  subsequently regressed and the product prototype was removed.

### Activity integration gate

The final `CARGO_TARGET_DIR=/Users/vart/.cache/rook-reference-target cargo xtask ci`
exited 0 in 1,030.1 seconds: formatting 0.9, all-target Clippy 3.8, frontend
builds 20.3 and the complete workspace tests 1,005.2 seconds. The final log is
`/tmp/rook-reference-activity-ci-verified.log`. It includes the queue admission,
actual foreground/background pipe observation, detached-client and standalone
HTTP worker/diagnostic/cancellation fixtures, plus existing native frontends,
approvals, recovery, storage and ACP checks. This closes row 1 only; row 2 was
already complete, and the other twelve remain in the full scope above.

The first new focused run exited 101 because its background-job fixture matched
`ready` in the echoed command before any pipe byte arrived. It now synchronizes
on the actual received-byte count and inspects pipe output. The first daemon
fixture also exited 101 when it attempted two standalone runs in one workspace;
it now uses two owned workspaces sharing one real configured capacity queue.
An intermediate core fixture compilation failed on a missing Tool name and was
corrected. Subsequent focused runs exited 0. The first full gate exited 1 on a
constant-only output-cap assertion; the final assertion checks the observed
runtime count, and the entire gate was repeated successfully. Failed logs are
retained rather than presented as passes.

The paired semantic answer probe also exited 0 with all sixty physical requests
retained. That is source-grounding research, not row 7 completion or product
default adoption. No installed daemon, operator store or configuration was used.

## Sources

The review used the locally pinned projects listed in
[references/README.md](../references/README.md). The relevant areas are Hermes'
stall watcher, conversation search, terminal environments and background review;
Pi's Code Mode; OpenResearch's experiments; OpenCode's diffs and snapshots;
OpenClaw's memory, browser, event hooks and messaging; Goose's composite recipes
and ACP providers; and the ACP protocol specifications.


### Direct reference evidence

These paths were inspected in the pinned local trees; they identify the source
of the idea, without asserting that the same behavior has shipped in Rook.

| Items | Reference evidence |
|---|---|
| 1 | [Hermes stall watcher](../references/hermes/gateway/run_watchers.py) tracks actual agent activity independently of channel delivery. |
| 2 | [Hermes conversation search](../references/hermes/tools/session_search_tool.py) separates discovery and anchored reading and deduplicates lineage. |
| 3 | [Pi Code Mode](../references/pi/packages/codemode/README.md) exposes injected tool calls inside a bounded QuickJS VM. |
| 4 | [OpenResearch experiment tree](../references/openresearch/agent-skills/orx-experiment-tree/SKILL.md) branches hypotheses from measured parents and freezes answered nodes. |
| 5 | [Hermes review engine](../references/hermes/agent/review_engine.py) uses an independent reviewer; [OpenCode review panel](../references/opencode/packages/app/src/pages/session/review-tab.tsx) attaches exact file/line comments. |
| 6 | [OpenCode snapshots](../references/opencode/packages/opencode/src/snapshot/index.ts) retain workspace changes for restoration. |
| 7 | [OpenClaw memory search](../references/openclaw/docs/concepts/memory-search.md) combines embeddings and lexical matches with single-path fallback. |
| 8 | [OpenClaw skill proposals](../references/openclaw/docs/tools/skill-workshop/proposals.md) keeps drafts separate from live skills and versions their application. |
| 9 | [OpenClaw browser tools](../references/openclaw/docs/tools/browser/agent-tools.md) supplies navigation, snapshots and page actions. |
| 10 | [Hermes SSH environment](../references/hermes/tools/environments/ssh.py) reuses remote connections and carries working-directory state. |
| 11 | [OpenClaw inbound webhooks](../references/openclaw/docs/automation/cron-jobs/webhooks.md) authenticates external event submissions separately from internal hooks. |
| 12 | [OpenClaw Telegram session routing](../references/openclaw/docs/channels/telegram/threads-and-sessions.md) binds chats/topics to persistent sessions. |
| 13 | [Goose subrecipes](../references/goose/documentation/docs/guides/recipes/subrecipes.md) composes isolated, parameterized contexts. |
| 14 | [ACP specifications](../references/acp) and [Goose ACP provider](../references/goose/crates/goose/src/acp/provider.rs) supply editor interoperability and an external-agent execution path. |


Codex, Cline and OpenHands were also inspected. Several relevant ideas are
already recorded in [PORTED.md](../references/PORTED.md), including bounded
transcript navigation and diagnostic exports. Streaming frontend updates are
also already exercised by the browser checks. These do not add new
implementation rows here. The local gitlinks used by this review are the
ones listed in the 2026-10-03 review, with Pi at `ee602414`; this review reads
those trees without advancing or fetching references.


### Semantic memory decision, 2026-10-11

The optional prototype was tested through the real CLI and then removed from
production after the fixed seven-task holdout completed. Verified conservative
scores were lexical 3/7 and hybrid 1/7, zero hybrid wins and two losses. Hybrid
had five native timeouts; medians were 108.663 and 240.007 seconds. These are
ordinary agent-task results, unlike the earlier positive retrieval/source-only
probes. Automatic checker calls that violated the task's no-command contract
also count as failures, even when the parent wrote the correct artifact. Failed
turns have incomplete token receipts, so completed-outcome token totals cannot
establish total cost savings.

The original scoring parser used the wrong native tool-kind spelling. The
terminal report was repaired against saved `tool-call` events and retained
checker sessions without model retries. Both reports and all fourteen physical
attempts remain in `/Users/vart/.cache/rook-semantic-memory-agent-eval/20261011-product`.
The unsuccessful prototype source and a restoration patch are archived there
under `prototype/`; configuration, engine/frontend bindings and prototype-only
tests were removed without reverting the earlier activity/recall/ACP changes.
The initial prototype's full gate passed; the later browser integration gate
also passed after the product prototype was removed. The installed daemon and
configuration were never changed.

Item 7's measurement condition has been evaluated. There is no adopted semantic
memory capability or changed retrieval default. A future attempt must produce
better actual agent-task results before product integration is kept. The
remaining capabilities retain their original acceptance scope.

### Browser gate and Code Mode preparation

The browser implementation now has actual local Chromium form-submission,
retained PNG and lifecycle/cancellation checks, with explicit pixel delivery for
text-only versus vision providers. Native LAN attempts performed the page
actions but did not complete the whole independently checked turn within the
fixed budget; those failures remain in
[the browser research record](research/browser-control-20261011.md). Item 9 is
therefore still partial.

The full `/tmp/rook-browser-final-ci.log` gate exited 1 after 943.6 seconds. The
only failing target was `rook-core --test layering`: test-only `ps` commands in
`browser.rs` did not use the shared process-launch helper. Those three calls now
use `rook_contain::quietly`; all three layering checks passed in
`/tmp/rook-browser-layering-fixed.log`. The replacement full gate in
`/tmp/rook-browser-ci-process-fixed.log` exited 0: formatting 1.1 seconds,
all-target Clippy 11.6, frontend builds 9.0, complete workspace tests 819.7,
whole gate 841.4 seconds. Previously interrupted runs remain non-passing
evidence. Native LAN browser budget failures keep item 9 partial despite the
passing regression gate.

Code Mode has an independent QuickJS-WASM capability/bounds probe and fixed
paired local-model preflight in `xtask/probes/code-mode`. Twenty-one focused
runtime checks passed. The
[research record](research/code-mode-20261011.md) preserves ownership/drain
failures and the ordinary-engine integration constraints. The original probe
was preparation and did not expose a product path. Item 3 remains incomplete until its full acceptance
criteria and native measurement are satisfied. The revised preflight completed
all eight pairs: unambiguous correct target values were direct 5/8 and Code Mode
6/8, with two wins and one loss. Median wall time was 51.195 to 33.349 seconds;
physical model requests increased from 26 to 47. Direct usage coverage is
incomplete, so reported token subtotals cannot prove total cost savings. Strict
JSON-format scores are separate from value correctness. This read-only,
Code-Mode-only fixture comparison is not native acceptance or permission to
change the ordinary path.

### Code Mode engine integration

The archived optional `code_mode` prototype ran provider-free program frames through
the common engine. Every admitted
script owns a linked child journal and uses the ordinary per-call policy,
hooks, checkpoints, file claims, diagnostics, output redaction and recovery.
Its source/result notes and actual calls are retained without fabricated user
or assistant messages, model requests or usage. Script tools execute in order;
unawaited admitted calls drain before completion. File authority is shared with
the caller, while another session's touch still makes overwrites stale. Parent
rewind restores child checkpoints. Script contexts and calls have shared turn
budgets, and equipment has separate wall and guest execution limits.

Offline engine fixtures verify denial, file authority, shared budgets,
checkpoint/rewind, oversized frames and unknown-effect recovery after an actual
shell command is interrupted. The shell fixture's initial one-second deadline
could expire during preparation on a loaded host; its five-second test deadline
still asserts that the partial write actually occurred before checking recovery.
The product default remains sixty seconds. Genuine Node/QuickJS engine checks
exercise dependent calls, unawaited effects and CPU exhaustion; the final log is
`/tmp/rook-code-mode-native-fixtures-final.log`. The guest runtime checks are in
`/tmp/rook-code-mode-product-runtime-extra.log`. Combined browser/script catalog
checks retain the existing 2,800 eager/1,250 lazy token ceilings without changing
them. Those fixture catalogs do not register an interactive question tool.

The full gate `/tmp/rook-code-mode-ci.log` exited 0: formatting 0.9 seconds,
all-target Clippy 12.4, frontend builds 23.4, complete workspace tests 1,034.4,
whole gate 1,071.1 seconds. Earlier failed focused checks remain evidence;
they identified incorrect test assumptions about checker request counts,
unsupported completion-status vocabulary and excessive catalog descriptions.

The native eight-pair run finished in
`/Users/vart/.cache/rook-code-mode-native-eval/20261011`; the terminal report and
derived program analysis retain all sixteen arms. Fully correct native completion
was 1/8 in both arms, with no completion wins or losses. Correct answer artifacts
were direct 3/8 and candidate 5/8, but medians including failures increased from
199.125 to 203.959 seconds. Six candidate scripts made zero injected tool calls:
these calculator-only uses do not establish orchestration benefits. The native
outcome token subtotals do not cover every physical request and prove no savings.

The original harness hashed a mutable build path, and the later executable has
a different hash after full CI. The exact executable identity of every original
arm is therefore unproven; this weakens the comparison further and cannot support
adoption. Future harness runs freeze a retained executable, but this original run
was not replaced or retried. All source, stores and failures remain intact.

Under the user's conditional-adoption instruction, the product tool, settings,
dispatch bindings and prototype-only fixtures were removed selectively. Source
and a restoration patch are retained under the run's `prototype/`; its archived
post-CI binary is explicitly distinguished from the original measured hash.
Research probes remain. Activity, recall, browser and ACP changes are preserved.
Item 3 has no adopted product capability; the remaining scope is unchanged.

### Change-review integration constraints

The original item 5 investigation found that the ordinary checker in
`agent/checks.rs::run_checker_in` uses a linked, independently accounted context
and removes named file-edit tools, but retains `run_command`. Its own comment
explicitly acknowledges that commands can still write. Reusing that path
unchanged would not satisfy the reviewer's workspace immutability requirement.
The implemented review path uses a restricted toolbox, ordinary child journals,
cancellation and frozen cost receipts. The remaining constraints below describe
the required review contract.

The selected working diff, commit or base must resolve into a retained source
snapshot before review. File/side/line anchors and the snapshot identity must
bind both findings and user comments; later workspace changes cannot silently
retarget them. Store the original selection and resolved Git identities, expose
bounded source pages and retain failed/incomplete reviewer runs. Source text is
untrusted data, not additional reviewer permission. The current worktree Git
helper already bounds stdout/stderr while draining and disables external diff
and text conversion at its callers; review must also preserve these properties
and restrict path/revision arguments.

Versioned review companions can use keys ending in the owning session's
32-character hexadecimal identity so the store's actual `delete_session`
cleanup removes them. Comment updates need `kv_update_session` transactions,
not a read/change/write sequence that loses simultaneous comments. Each snapshot,
source page, finding, comment and accumulated companion needs byte/count admission
before copying or decoding. The CLI, TUI, daemon/browser and model tool must
expose the same core source/comment contract. The implementation and current evidence are recorded below; a passing helper
alone does not establish the complete product contract.


### Change review implementation and focused evidence

[The guide](change-review.md) documents `review_changes`, CLI `session review`,
chat/TUI `/review`, browser Change reviews and POST session reviews. Sources are
frozen before the independent reviewer starts. Its toolbox permits only paged
retained reads; forged shell/edit calls are refused before dispatch. Mode-only
changes stay visible, working sources use a directory-relative no-follow regular
file boundary, and stored source checksums are verified on read and before a
transaction commits. Known multiline credential redaction preserves line anchors.

Focused core integration checks passed, covering retained edits/additions/deletions,
commit/root/base selection, reached byte bounds and literal paths, independent
review with refused shell/edits, cancellation of an actually started reviewer,
concurrent comments, corruption, regular-file admission and redaction expansion.
Config catalog and layering checks also passed; the ordinary browser
checks passed (its separately opt-in live Chromium test was not run in this set).
The log is `/tmp/rook-change-review-final-focused.log`. New frontend checks passed
in `/tmp/rook-change-review-web-tests-fixed.log`: retained owner/source binding,
paging, draft isolation and stale/detached responses. The actual CLI/HTTP fixture
passed in `/tmp/rook-change-review-cli-http.log`, after proving daemon routing and
comparing the same retained source across local and HTTP paths.

An intermediate all-target Clippy run failed on one unnecessary closure in the
new API handler; it was fixed. The later admission change requires the final
full regression gate. The earlier Code Mode removal gate failed on tool catalog
budgets; the focused catalog/browser rerun passed after shortening descriptions.
These earlier results are not represented as a green gate for review integration.

The configured LAN model endpoint `http://192.168.1.46:1234/v1/models` timed out
on two bounded discovery requests. No native model review has been claimed.
Real model validation remains outstanding for item 5; all fourteen
rows above remain in scope.


The changes were copied from the isolated checkout only after comparing each
primary destination with its saved baseline SHA256. All changed destinations
matched; unrelated WIP was retained. The copy receipt is
`/Users/vart/.cache/rook-change-review-worktree-20261011.merge.json`.
The full `cargo xtask ci` against the merged primary source exited 0 in
849.9 seconds: formatting 1.0, all-target Clippy 20.6, frontend builds 25.3 and
workspace tests 803.0 seconds. The terminal log is
`/tmp/rook-change-review-ci.log`. This proves regression coverage, not native
model review success. The complete browser test command `node --test web/tests/*.mjs` exited
0; its log is `/tmp/rook-change-review-web-all.log`.

### Shell undo investigation

Item 6 is still pending. `agent/effects.rs::checkpoint_before` asks each tool
for `touched_paths`; the default implementation returns no paths, and
`rook-tools/src/exec.rs` does not override it for `run_command`. Thus ordinary
shell commands do not enter the named-file checkpoint path. Existing
`fileset.rs` snapshots retain file contents and absent paths, while
`service.rs::rewind` follows shared delegated checkpoints and excludes isolated
worktrees. Those mechanisms are useful integration points, not shell undo.

A shell receipt must retain bounded preimages before execution, then detect
new and deleted files for foreground and background completion. Captures must
use the bounded no-follow regular-file reader rather than copying
`capture_paths`' unbounded `std::fs::read`. Restore must compare a selected
reviewed source with current file hashes before replacing or deleting anything;
`FileSet::restore` alone does not provide that conflict contract. Background
jobs, cancellation and process loss need explicit terminal/uncertain receipts,
so source capture or cleanup failure cannot be described as recoverable shell
work. Existing ignored/generated-file exclusions must be exposed as scope,
rather than silently promising to undo files the snapshot did not retain.


### Shell undo implementation, 2026-10-11

The isolated shell-undo checkout implements pre-command source capture in the
ordinary policy/journal dispatch, post-command observations after owned group
cleanup, background job ownership, bounded versioned receipts and selected
restore with byte/mode conflicts. A process-wide read/write admission guard
keeps restores out of owned shell windows. Retained source notes pin file blobs
through GC without entering model context. Restore attempts preserve observed
preimages, completed paths and pending writes before mutating files. An explicit
recovery observation follows existing execution acknowledgement; it never turns
an interrupted command into a successful receipt. CLI `session shell-undo`,
chat/TUI `/shell-undo`, the session HTTP route and browser panel share the same
contract. The [guide](shell-undo.md) states the exact scope and race limitations.

Focused checks found a real cancellation gap: dropping the foreground tool
future killed the shell but left descendants alive. An owned group guard now
covers cancellation, while ordinary completed calls retain their previous
background-child behavior when file capture is disabled. Nine core checks
passed in `/tmp/rook-shell-undo-focused7.log`, including actual group termination,
post-cancellation acknowledgement/recovery and a real partial restore failure.
The one-file snapshot/union check covers simultaneous creation and deletion.
A privileged runner cannot exercise the directory-permission failure fixture;
the precondition was reached on this local non-privileged run.

All rook-tools tests passed in `/tmp/rook-shell-undo-tools.log`. Browser owner,
selection/token binding and explicit recovery checks passed in
`/tmp/rook-shell-undo-web.log`; all browser tests passed in
`/tmp/rook-shell-undo-web-all.log`. Real delegated shell ownership and frontend
closure are included in the core fixtures. The CLI/HTTP test passed in
`/tmp/rook-shell-undo-cli-http.log`, exercising actual local and routed restore,
source agreement and token replay refusal. All-target workspace Clippy passed
in `/tmp/rook-shell-undo-clippy2.log`. Full regression CI and live native agent checks remain
open; item 6 is not complete. All other numbered requirements retain their full
scope. No installed daemon or operator configuration was changed.


### Shell undo full regression gate

The primary workspace shell-undo gate completed with exit status 0 in
`/tmp/rook-shell-undo-ci.log`: formatting 1.1 seconds, all-target Clippy 21.2,
frontend builds 24.2, complete workspace tests 918.1, whole gate 964.5 seconds.
This includes the actual delegated shell and foreground/background cancellation
fixtures, local/HTTP selected restoration, generic slash-command and TUI tests,
and the prior review/activity/recall changes. Native model behavior remains a
separate open observation; no improvement claim or operator update is implied.


### Experiment tree integration, 2026-10-13

The isolated checkout adds the [experiment contract](experiments.md): bounded
versioned projects, linked nodes and run sessions, fixed source/parameter/command
identities, separate execution copies, certified process cleanup, finite metrics,
retained output/artifacts, direct-child comparisons and explicit winner selection.
CLI, chat/TUI, HTTP and browser access the same engine. Nodes cannot be mutated
through ordinary agent dispatch after measurement or while an effect is uncertain.
Unknown owners require execution acknowledgement and explicit inspection; no
command replays automatically. Publication intent and terminal metadata capacity
are recorded/admitted before filesystem or command effects.

Eight actual-command core checks passed in
`/tmp/rook-experiments-focused7-20261013.log`, including a fresh-copy repeat with identical source/parameters/contract/environment and
metrics/artifact identities, plus reopening the store before unknown-effect
acknowledgement. The real CLI/HTTP fixture passed in
`/tmp/rook-experiments-cli-http-20261013.log`. Both local and daemon-backed TUI
paths passed in `/tmp/rook-experiments-pty-20261013.log`, preserving the draft
and avoiding model requests. All 79 browser tests and JavaScript syntax checks
passed in `/tmp/rook-experiments-web-all2-20261013.log`. Workspace all-target
Clippy passed in `/tmp/rook-experiments-clippy2-20261013.log`; the final gate
covers the subsequent comparison and fixture changes. Earlier focused failures
are retained: missing fixture accessors, a source-size-change error label, and
one Clippy tuple-complexity diagnostic. They were corrected without treating
the failed runs as passes. The final primary regression gate is recorded below. The implementation observes selected environment variables,
not a complete reproducible container/dependency/hardware identity. Local model
validation for review, shell undo and browser remains deferred while the user’s
LLM is temporarily offline. All remaining numbered acceptance criteria stay
in scope; no installed daemon, operator configuration or release was changed.


### Experiment tree full regression gate

The 23 experiment-specific files were copied into the primary workspace only
after every existing destination matched its saved baseline SHA256. The receipt
is `/Users/vart/.cache/rook-experiments-worktree-20261011.merge.json`; prior WIP
was preserved, including the current reference-adoption rows.

`CARGO_TARGET_DIR=/Users/vart/.cache/rook-reference-target cargo xtask ci` against
the merged primary source exited 0 in `/tmp/rook-experiments-ci-20261013.log`.
Formatting took 1.1 seconds, all-target Clippy 22.9, frontend builds 27.6,
complete workspace tests 807.9, and the whole gate 859.5 seconds. It includes
all eight experiment core fixtures, real CLI/HTTP admission and promotion,
local/daemon TUI reads, and the prior review, shell-undo, activity, recall,
browser and ACP regressions. This closes row 4; native checks in rows 5, 6 and
9 remain deferred while the local LLM is offline, and all other rows retain
their stated acceptance scope. No release or operator daemon restart occurred.

The next isolated checkout is
`/Users/vart/.cache/rook-learning-worktree-20261013` with 579 saved primary
source baselines. Initial source inspection found existing immutable turn
results, memory CAS/history, and skill versions. These are foundations for
reviewable evidence-bound proposals, not row 8 delivery. A finished turn can
retain an unproven checker result; proposal admission must preserve that
distinction instead of equating `end_turn` with proved success. The bounded
CodeGraph investigation was stopped after its optional Rust indexer did not
finish; the source findings above were confirmed directly, not inferred from
a nonexistent graph result.


### Task learning prototype

The isolated checkout `/Users/vart/.cache/rook-learning-worktree-20261013` adds
[reviewable proposals](learning.md), revision lineage and exact source/target
tokens. Its producer has no tools, retains linked usage/attempts and runs only
when explicitly requested or under the disabled-by-default automatic setting.
Memory and its receipt publish in one session-owned CAS transaction; corrections
preserve scope, tags and pinning. Skill changes retain inverse snapshots before
publication, durable per-file progress and ordinary unknown-effect recovery.
The selected user version directory and declared version are preserved.

Eight actual-engine fixtures with scripted providers passed in
`/tmp/rook-learning-focused5.log`, including concurrent ref/receipt publication
and rejection after deletion of the actual owning session. Browser draft/source/token/detachment checks passed in
`/tmp/rook-learning-web.log`. The real local/HTTP skill fixture, including reached partial inverse recovery,
passed in `/tmp/rook-learning-cli-http2.log`. Reached partial application and
unknown-effect acknowledgement passed in `/tmp/rook-learning-partial-cli2.log`
on this unprivileged host; privileged runners cannot reach that filesystem
precondition. Local and daemon-backed TUI reads passed in
`/tmp/rook-learning-pty.log`. All 82 web tests passed in
`/tmp/rook-learning-web-all.log`. All-target workspace Clippy passed in
`/tmp/rook-learning-clippy3.log`; the final gate also covers the later inverse
fixture. The complete regression gate remains open. No LAN model quality is claimed; it
remains offline. Earlier focused failures are retained: correction metadata was
initially lost and has been preserved; the initial partial HTTP fixture lacked
TLS-provider setup; initial Clippy runs exposed a disabled diff-deadline feature
and two ordinary code diagnostics. The 31 learning-specific files were subsequently copied into the primary
workspace only after all saved baseline SHA256 guards matched. The receipt is
`/Users/vart/.cache/rook-learning-worktree-20261013.merge.json`. The primary
`cargo xtask ci` is running with log `/tmp/rook-learning-ci.log`; no passing
full gate is claimed until its authoritative process is terminal. Row 8 is
not complete. All fourteen acceptance rows retain their full scope.


### Task learning full regression gate

The primary `/tmp/rook-learning-ci.log` process completed with exit status 0.
Formatting took 1.0 seconds, all-target Clippy 23.7, frontend builds 29.2, all
workspace tests 843.5, and the whole gate 897.4 seconds. The gate includes all
eight learning engine fixtures, concurrent ref/receipt publication, actual
local/HTTP user-version selection and reached partial inverse recovery, reached
partial application/unknown-effect acknowledgement, both TUI modes and previous
reference changes. The 82 web checks passed separately. Native model proposal
quality is not claimed; automatic reviews remain disabled by default. This
closes row 8's implemented provenance/review/application/rollback contract and
leaves every other row's scope/status intact. No operator daemon restart or
release occurred.

The next isolated source is
`/Users/vart/.cache/rook-execution-worktree-20261014`, with 587 saved primary
source baselines. Syntax-only CodeGraph located the existing editor-terminal
bridge, confirmed directly in `rook-tools/src/exec.rs`. Its callback lacks an
execution-resource ownership contract, while background jobs still launch local
shells. Reusing that bridge alone cannot deliver row 10.

Docker is available through the local Colima Unix socket; server inspection
reported Docker 29.5.2 on Ubuntu 24.04.4 LTS, with installed `alpine:3` and
`ubuntu:22.04` images. These are availability observations, not a container
execution acceptance test. LAN SSH port 22 accepts a TCP connection, but the
read-only strict-key probe to 192.168.1.46 was refused because no trusted host
key is known for that IP. A configured trusted SSH alias has been requested;
no key was accepted automatically and no remote command executed. Row 10
remains pending until the actual execution/path/cancellation/recovery contract
is implemented and verified.


### Execution environment draft

`/Users/vart/.cache/rook-execution-worktree-20261014` contains a separate draft
for trusted `execution_environment` profiles and an engine-shared bounded pool.
The selected container maps the ordinary local workspace to `/workspace`; the
original command still enters ordinary policy, journals, bounded output and
file-effects capture. The owned immutable container ID is retained separately
from its display name. Resource permits survive background admission and stop
with job cancellation/frontend closure. Interrupted resources remain refused
rather than silently restarting a command. Source and operator configuration
have not been copied or enabled in the primary workspace.

The genuine Docker gate
`ROOK_ENV_TEST_ROOT=/Users/vart/.cache CARGO_TARGET_DIR=/Users/vart/.cache/rook-reference-target cargo test -p rook-tools --test environments -- --ignored --nocapture`
exited 0 in `/tmp/rook-execution-container-tests4.log`. Three actual fixtures
verify shared local file edits, original user-file preservation, one resource
reused across separate calls, owned resource deletion, cancellation after a
real started command, stopped container state, refusal to reuse uncertainty,
and background resource ownership/stop. They use the already installed
`alpine:3` image on Colima, with no network and bounded PID/memory resources.
The first background fixture compilation failed because `is_running` requires
an optional job selector; corrected terminal evidence is retained.

This is not row 10 completion. SSH remains an explicit non-launching draft;
secret forwarding, remote workspace/mount verification, public inspection and
recovery, editor authority, all frontend/native engine checks and the full
regression gate remain open. The optional Docker tests do not certify SSH or
the complete command path merely because the library fixture passes. All
fourteen rows retain their original scope; the active goal is not complete.


### Container ownership and recovery checks

The isolated [execution environment guide](execution-environments.md) describes
the implemented container contract and its limits. The transport now verifies
the Docker bind against a unique client-side probe, pins the resolved image ID,
uses the client's effective UID/GID, and checks a per-call terminal certificate
only after the originally owned remote process group has no live members.
Failed proxy verification cannot masquerade as a completed remote exit.
Background job quiescence also requires the remote cleanup observation, not
merely death of its local Docker client. Core tool-result companions retain the
configured profile/kind and immutable resource ID for frontend history.

The native Docker run `/tmp/rook-execution-container-tests11.log` exited 0: ten
actual runtime fixtures plus one subprocess helper. It includes ordinary
foreground/background completion, redirected descendants, actual cancellation,
per-call secret forwarding/redaction, operator file ownership, timeout/release
and no token replay, real process exit without destructors and inspection after
reopening, and refusal to release another live pool's resource. A reached stop
fault deliberately left an actual container running; job quiescence was refused
and the state remained `interrupted_unknown`. The fixture removed only its own
immutable ID afterwards. These checks do not claim SSH, deliberate process-session
escape prevention, or native model quality.

The actual AgentLoop fixture passed in `/tmp/rook-execution-engine-tests2.log`,
retaining the original command, checking the real filesystem effects and shell
undo, displaying bound container identity, and journaling explicit release in a
linked child session. Foreign/deleted owner admission passed alongside it. The
real orphan CLI/HTTP fixture passed in `/tmp/rook-execution-cli-container2.log`:
an actual child process exited without cleanup, local/HTTP inspection agreed on
the exact token, release removed the owned container and preserved user files.
The default local profile CLI/HTTP fixture passed in
`/tmp/rook-execution-cli-local.log` without writing a model prompt.

All 85 web checks passed in `/tmp/rook-execution-web-all.log`, including the
new owner/token release, busy resource and detached-view fixtures. All-target
workspace Clippy passed in `/tmp/rook-execution-clippy2.log` before later
fixtures/admission changes; the final gate remains required. Earlier native
failures are retained: raw secret text needed masking, initial result identity
was not persisted in frontend companions, a CLI fixture used a nonexistent
temporary-directory constructor, and initial Clippy rejected two needless
returns. Each was corrected; the failed logs are not passing evidence.

Current source still resides in the isolated checkout. SSH is unimplemented and
the LAN strict-key probe remains refused pending a verified configured alias.
Native model checks in rows 5, 6 and 9 remain deferred while the LLM is offline.
All fourteen rows retain their acceptance scope; no release, operator setting
change or installed daemon restart occurred.


### Remote uncertainty integration

Four native engine fixtures passed in `/tmp/rook-execution-engine-tests5.log`.
Reached foreground and background stop failures leave an actual container
running, preserve an unknown ordinary execution operation and prevent the next
agent turn from starting another write. Explicit release retains its bounded
inspected source in a linked journal before the effect; it does not silently
acknowledge the original command. The source and removed immutable ID remain
inspectable after the resource is gone. The successful fixture also verifies
real shell undo and the original user edit. The initial parallel fixture run
properly refused a restore during another fixture's active global shell window;
the native engine fixtures now serialize that shared admission.

The latest tool runtime gate `/tmp/rook-execution-container-tests12.log` exited
0 for ten actual runtime fixtures and their subprocess helper. The standalone
MCP tool context now carries trusted execution configuration; agent turns replace
its default pool with the Rook engine's shared pool. `doctor` names a configured
nonlocal profile without claiming the host containment probe describes it.
Inspection itself does not consume the live-resource capacity. The first TUI
fixture reached the correct empty history and failed only on a copied one-event
expectation; its corrected local/daemon run and the final regression gate remain
to be recorded. No installed daemon was restarted.


### Container integration candidate

Corrected local/daemon TUI checks passed in `/tmp/rook-execution-pty2.log` in
12.37 seconds; the draft survived and the empty history gained no prompt.
The container implementation is being copied from isolation only after every
primary destination matches its saved baseline SHA256. The full primary
`cargo xtask ci` will be recorded in `/tmp/rook-execution-ci.log`; it is not a
passing gate until the authoritative process is terminal. The guide and native
fixtures remain explicit about the unimplemented SSH transport, per-process
capacity and ownership/quiescence scope. This is row 10 progress, not completion
of row 10 or the fourteen-item goal.


The first full CI process was explicitly stopped during test compilation after
its formatting/Clippy/build stages passed; it is not a completed gate. Its
retained log is `/tmp/rook-execution-ci-pre-bind-fix.log`. A further bind audit
found that a not-yet-created state directory inside the workspace escaped the
existing-path check. Admission now resolves the existing ancestor plus missing
components before any setup-directory effect. The reached preflight fixture
passed in `/tmp/rook-execution-bind-preflight.log`, with no state directory or
command file created. Docker also receives explicit `--pull never`, including
the image-inspect/run race. The corrected complete runtime tool gate is
`/tmp/rook-execution-container-final.log`; its terminal status is recorded after
completion. These three files were synchronized back to the isolation archive.


The first complete primary container CI exited 0 in
`/tmp/rook-execution-ci-before-secret-cache.log`: fmt 1.2, Clippy 9.7, builds
9.0, tests 839.9, whole 859.8 seconds. A subsequent dynamic-secret audit reached
a duplicate helper lookup on genuine Docker (`/tmp/rook-execution-dynamic-secret-before.log`,
failed count 2 instead of 1). The transport now retains the already admitted
value for its nested execution and masking, without rerunning a helper. The
static-secret fixture did not establish this property. The corrected full runtime
gate is `/tmp/rook-execution-container-final2.log`; a final full CI is required
for this last change, so the earlier complete gate is not represented as the
final-source regression result.


### Container final regression gate

The final primary `/tmp/rook-execution-ci.log` process completed with exit status
0 after the dynamic-secret correction. Formatting took 1.2 seconds, all-target
Clippy 19.1, frontend builds 20.9, complete workspace tests 838.5 and whole gate
879.7 seconds. All 85 browser tests passed separately in
`/tmp/rook-execution-web-final.log`; browser source did not change in the final
credential correction. The full optional native tool gate passed in
`/tmp/rook-execution-container-final2.log`: eleven actual Docker runtime fixtures,
the no-effect bind preflight and one subprocess helper. Four native engine
checks passed in `/tmp/rook-execution-engine-tests5.log`, including reached
foreground/background stop failures and preservation of ordinary execution
uncertainty across the next turn. Real orphan CLI/HTTP and both TUI modes were
verified in the recorded focused logs and all ordinary frontend regressions ran
in the final CI. The optional native fixtures are not implied to run automatically
in normal CI.

The 38 execution-specific files were guard-merged into primary; the receipt is
`/Users/vart/.cache/rook-execution-worktree-20261014.merge.json`. The subsequent
bind and dynamic-secret corrections were mirrored to that isolation archive and
recorded in the receipt. Prior mixed WIP was preserved. This completes the
container portion of row 10, not its SSH requirement. No native LAN/model
quality or SSH success is claimed. All fourteen rows retain their scope; no
release, operator configuration change or installed daemon restart occurred.


### SSH execution transport — 2026-10-14

The SSH implementation now joins the trusted execution pool and ordinary command
policy, foreground/background output, cancellation and journals. It retains
bounded before/after replicas, merges file changes with conflict checks and
records partial publication progress. Strict host verification is mandatory.
Observed stop/recovery/release require a fresh token and a note; recovery does not
replay commands or replace the original unknown-effect acknowledgement. Source
reads remain available offline and after release. The SSH driver does not enforce
remote OS containment or network denial; those policies refuse this transport.
Default mirror exclusions and dependencies are documented in
[the execution guide](execution-environments.md).

The owned OpenSSH fixture passed 14 tool tests in 7.06 seconds
(`/tmp/rook-ssh-native8.log`), including actual descendants in a separate process
session, reached transfer/retention/output limits, deadline stop, conflicts,
partial writeback, file/directory replacements and process loss. Three filesystem
recovery checks passed in `/tmp/rook-ssh-sync.log`; twelve container regression
checks passed in `/tmp/rook-ssh-container-regression3.log`. CLI/HTTP process-loss
recovery passed in `/tmp/rook-ssh-cli-http3.log`, and actual local/daemon TUI source
reads passed in `/tmp/rook-ssh-tui.log`. Browser checks passed 87/87 in
`/tmp/rook-ssh-web-final2.log`. Deadline validation exposed missing timeout output
counters and unpublished SSH effects incorrectly treated as known after process
stop; both were corrected and the actual deadline fixture passed. The final three engine checks passed in 7.97 seconds in
`/tmp/rook-ssh-engine4.log`, including the reached deadline's original unknown
journal and refusal to recover before acknowledgement. Five filesystem/control
boundary checks passed in `/tmp/rook-ssh-boundaries.log`, including actual
oversized control output, deadline termination and process reaping. The full
CI result will be recorded after completion.

No installed daemon was restarted, no release was issued and no LAN SSH or
model-quality success is claimed. The LAN check remains contingent on a verified
SSH alias or host key and authentication; the 127.0.0.1:9999 tunnel is not needed.
All fourteen adoption requirements remain in scope.

The first primary full CI completed with two failed convention targets in
916.5 seconds (`/tmp/rook-ssh-ci-failed-conventions.log`): the production panic
scan found an unchecked UTF-8 parent unwrap, and the process-launch scan required
an explicit quiet-launch marker in the SSH command-building module. Both were
corrected in the feature source and mirrored to the isolation archive. All six
focused convention tests then passed in `/tmp/rook-ssh-ci-corrections.log`.
A fresh complete primary CI is required before recording final delivery.

The final primary full CI exited 0 in `/tmp/rook-ssh-ci.log`: formatting 1.0
seconds, all-target Clippy 14.7, frontend builds 11.1, complete workspace tests
832.6 and whole gate 859.4 seconds. Both previously failing convention targets
passed. This is the final production source after the two corrections; the
first failed gate is retained separately. The 26 feature files were guard-merged
with their baseline hashes checked in
`/Users/vart/.cache/rook-ssh-worktree-20261014.merge.json`. The exact native test
image recipe and rerun commands are now retained with the fixture and execution
guide. Container and SSH product paths are implemented and verified on owned
native servers; row 10 remains partial only for the contingent LAN SSH check.
The next phase is row 11, authenticated event-triggered tasks. Its separate
checkout has a 602-file source baseline and an unwired settings draft; no event
handler or live adapter delivery is claimed yet.


### Event-triggered tasks — 2026-10-14

Row 11 now has an independently implemented HMAC-SHA256 ingress protocol,
strict route/delivery/timestamp/body binding, bounded body arrival and admission,
and a durable source/receipt transaction. An existing-work conflict leaves the
delivery pending. Managed run/index and receipt state are committed together;
startup dispatch uses the ordinary supervisor. Literal operator instructions,
workspace and finite ceilings are frozen independently of external data. Task
iterations link to the immutable source parent. Source previews are bounded and
large bodies use a current-session read-only tool; no event content becomes a
human UserMessage or permission grant. All frontends use the common core.
The product contract is in [the event guide](inbound-events.md).

The focused engine/regression suite passed twelve inbound checks, thirty-four
managed-work checks and both three-test convention suites in
`/tmp/rook-inbound-core-final.log`. The independent HMAC vector passed in
`/tmp/rook-inbound-core3.log`; that command filtered out the integration suite,
whose actual twelve checks are recorded in the separate final log. The actual
CLI/HTTP fixture passed in `/tmp/rook-inbound-http4.log` (15.95 seconds), using
an owned local protocol model: invalid authentication and reached body bounds
admit no source, duplicate delivery retains one root, pending delivery survives
a daemon restart, one ordinary shell command writes the requested code and its
independent checker actually reads the result before completion. Local/daemon
source reads retain identical bytes. Earlier fixture failures are retained:
its initial decoder read only the environment envelope, and its checker first
claimed success without reading; neither outcome was reported as delivered.
Both local/daemon TUI views passed in `/tmp/rook-inbound-tui.log` (12.95 seconds),
retaining the draft and creating no task. Browser checks passed 90/90 in
`/tmp/rook-inbound-web-all.log`. All-target Clippy passed in
`/tmp/rook-inbound-clippy4.log` (9.01 seconds).

The implementation still resides in the isolated checkout until the feature
hashes are guard-merged and the full primary CI completes. No installed daemon,
operator configuration or external messaging account has been changed. All
fourteen adoption requirements remain in scope; Telegram, composite recipes
and remaining ACP execution/lifecycle still require their product paths.


The final primary event-task CI exited 0 in `/tmp/rook-inbound-ci.log`:
formatting 1.0 seconds, all-target Clippy 24.8, frontend builds 28.2,
complete workspace tests 858.5 and whole gate 912.4 seconds. The gate ran the
actual HTTP/restart task fixture and the native TUI event fixture alongside
all existing frontend/runtime regressions. Source stayed unchanged throughout
that run. The 36 feature files were guard-merged against 603 saved baselines;
the receipt is `/Users/vart/.cache/rook-events-worktree-20261014.merge.json`.
The separate configuration/catalog checks passed 16 and 18 tests in
`/tmp/rook-inbound-config2.log`; the single independent HMAC-vector check was
recorded separately. Source/model paging keeps the default catalog unchanged
outside event sessions. This completes row 11's generic authenticated JSON
handlers and durable task path. Telegram remains a separate pending transport,
with a 612-file isolated baseline and an unwired settings draft only; a live
bot check needs a configured vault alias and authorized user/chat IDs.
All fourteen reference-adoption rows remain in scope. No native model-quality
improvement or external messaging delivery is claimed by these fixtures.


### Telegram transport — 2026-10-14

The default-off adapter uses the existing daemon chat admission, steering queue,
live approvals/questions and ordinary agent loop. Authorized chat/topic routes,
input capture/dispatch decisions, send attempts and output publication survive
restart. Source input is durable before polling confirms the offset. Callback
capabilities bind the first route owner, chat/topic, current request and live
epoch; stale callbacks cannot authorize replacement turns. Forwarded/media/bot
updates do not become human instructions. Bot turn policy and finite budgets
only tighten the global configuration. Full input inspection retains text and
captured target before prompt admission. Status retains adapter errors without
credential URLs or upstream response bodies.

All output parts and their event cursor commit together. Sends enter durable
`sending` before HTTP; lost replies/process loss become `uncertain` and require
an explicit reviewed decision. Definite flood waits respect their deadline and
finite attempt budget. Resolved receipts can be forgotten without replaying
published session output. The CLI, chat/TUI `/telegram`, POST `/api/telegram`
and Tasks browser panel share bounded core inspection and observed-attempt
controls. The guide is [docs/telegram.md](telegram.md).

The protocol/core checks passed in `/tmp/rook-telegram-core-final.log`.
The actual daemon/Bot API/tool fixture passed in
`/tmp/rook-telegram-http-final.log` (27.78 seconds including daemon build): it
holds a real command for approval, retains a follow-up once, refuses another
sender's button, writes the file after the owner's live decision, retains a
lost send across restart without replay, allows one explicit retry, separates
topics and stops an actual model wait. Both TUI modes passed in
`/tmp/rook-telegram-tui.log` (10.81 seconds), preserving the draft and creating
no model turn. The complete browser suite passed in
`/tmp/rook-telegram-web-final.log`; all-target Clippy previously passed in
`/tmp/rook-telegram-clippy5.log` (12.00 seconds). Configuration, layering,
no-unused-API and no-panic checks passed in
`/tmp/rook-telegram-conventions.log`. The earlier HTTP fixture failure is
retained in `/tmp/rook-telegram-http.log`; a nonblocking accepted fixture
socket lost the model response and that run was not represented as success.
The initial core transport check likewise exposed missing TLS initialization;
that was corrected before the final passing protocol checks.

The 30 feature files were guard-merged against 612 saved primary baselines.
The final primary `cargo xtask ci` exited 0 in `/tmp/rook-telegram-ci.log`: fmt
1.1 seconds, all-target Clippy 25.8, builds 28.4, complete workspace tests 883.6
and whole gate 938.9 seconds. Source hashes remained unchanged throughout
the gate. The merge receipt is
`/Users/vart/.cache/rook-telegram-worktree-20261014.merge.json`. No live bot credentials have been supplied,
no external bot messages have been sent and no installed daemon/operator
configuration has changed. On 2026-10-14 the user confirmed that no bot is
configured and explicitly requested skipping the live bot check. That check is
therefore waived for this delivery; the passing owned Bot API fixtures remain
the transport evidence. Remaining ACP execution/lifecycle stays in scope.


### Composite recipes — 2026-10-14

Named version-1 subrecipes now use the existing child executor and fresh session
contexts. Definitions freeze before the first request, retain workspace-bound
file sources and reject cycles/depth/count/source-size violations. Fixed parent
values cannot be replaced. Sequential stages bind bounded output from earlier
completed stages; independent parallel stages share the current parent token
allowance and concurrency limit. Child recipe limits only tighten the inherited
steps/tokens/absolute deadline and ordinary policy. All stages use ordinary tool
journals, checkpoints and cancellation. Source/output event identities commit
with their receipts using sequences assigned inside the store transaction;
child session/input/stage admission is atomic. See
[the composite recipe guide](composite-recipes.md).

Missing child cost coverage stops subsequent family model requests, including
final-answer/output repair and goal checking, rather than assuming unused quota.
Original execution uncertainty still requires its ordinary recovery controls.
A reviewed recipe interruption records no replay and no original-operation
acknowledgement. Explicit later calls can use inspected prior output; an
interrupted call is never automatically rerun. The parent goal checker remains
independent of a stage's completed execution and can retain an unproven verdict.

The common inspection paths are `rook session recipes`, chat/TUI `/recipes`,
POST `/api/sessions/{id}/recipes`, and the Sessions browser panel. The model's
selected recipe advertises `run_subrecipes` and bounded `subrecipe_read`; the
default catalog remains unchanged. Source and parameter pages are bounded;
large parameter sets are omitted from summaries. Definition/output hashes refuse
replacement events under stale restored receipts.

The final ordinary engine fixtures passed in
`/tmp/rook-composite-engine-final.log`, including real shell cancellation,
observed process death, reopen/review without replay, nested lineage, actual
shared-token exhaustion, lost cost coverage, independent parallel requests,
frozen definitions and dependent output handoff. The native CLI/HTTP fixture
passed in `/tmp/rook-composite-cli-http.log` (36.11 seconds including daemon
build): one child writes an actual file, the next reads it and consumes the prior
output, and CLI/HTTP/restart source reads match without another stage execution.
Both TUI modes passed in `/tmp/rook-composite-tui2.log` (12.18 seconds), preserving
the draft and producing no execution receipt. The earlier TUI fixture assumed
that opening the window creates no empty session; the corrected fixture verifies
that no conversation turn/execution is created. Browser and convention checks
passed in `/tmp/rook-composite-web-all.log` and
`/tmp/rook-composite-core-conventions.log`. Existing plain-recipe checks passed
in `/tmp/rook-composite-legacy-recipes.log`. The lost-cost fixture first exposed
a supplementary final-answer request after the stop; the corrected final engine
fixture also exercises a structured output contract and proves that neither
repair nor final generation occurs after coverage is lost.

The 32 feature files were guard-merged against the saved primary baselines.
The final primary `cargo xtask ci` exited 0 in `/tmp/rook-composite-ci.log`: fmt
1.1 seconds, all-target Clippy 27.3, builds 23.7, complete workspace tests 904.5
and whole gate 956.7 seconds. Feature source hashes remained unchanged through
the gate. The receipt is
`/Users/vart/.cache/rook-recipes-worktree-20261014.merge.json`. This completes
row 13. All fourteen rows retain their scope; remaining ACP execution/lifecycle
and contingent live model/LAN checks are still required. The live Telegram bot
check was explicitly waived by the user. No installed daemon, operator
configuration, release or external messaging account changed.


### ACP session lifecycle — 2026-10-14

The pinned ACP v1 close contract at `c849ac2f0c3e4a9f69fdd76a1bdbc659e3f5979d`
requires cancelling ongoing work and releasing session resources. The previous
server returned `{}` for `session/close` without doing either. The selected
session now owns retained jobs, run grants and MCP/LSP pools; close awaits turn
future destruction, releases editor waiters, requests known-terminal kill/release
and shuts down its equipment. Other sessions retain their resources, history
remains listed/resumable, and resume does not replay commands. The configured
`server.max_acp_sessions` admits a finite number of active runtimes and refuses
excess before stopping another session. Run grants cannot cross sessions and
are removed on close; connection-wide mode/effort behavior remains compatible.

The actual ACP checks passed in
`/Users/vart/.cache/rook-acp-lifecycle-startup.log`: an owned editor terminal executes
a real long-running command, closing another session leaves it running, closing
its owner requests kill/release and answers the prompt once, and inspection and
resume still find its history. Separate physical background processes verify
session ownership and observed death after release. Nested closure retains
unknown child outcomes and refuses independent child close. A socket fixture
reaches the configured equipment cap, verifies refusal/invalid close, then
observes admission after valid close. Editor cleanup requests themselves do not
prove a remote process died; original execution uncertainty remains separate.

The first convention check refused a test-only subprocess in the source module
without the platform launcher wrapper. The fixture now uses
`rook_contain::quietly` through an explicit development dependency, rather than
exempting the check. Its initial failed log is retained separately. The first
manifest edit incorrectly inherited a dependency absent from the workspace table;
the explicit local development path corrected that before the successful run.
Configuration/help wiring, layering, no-unused-API and no-panic checks passed in
`/Users/vart/.cache/rook-acp-lifecycle-conventions3.log`.
The 14 feature files were guard-merged against saved primary hashes. The first
primary gate exited 0 in `/Users/vart/.cache/rook-acp-lifecycle-ci.log`: formatting
1.1 seconds, Clippy 23.5, builds 17.1, complete workspace tests 877.2 and whole
918.9 seconds, with feature hashes unchanged. A subsequent source audit found
MCP initialization inside the incoming reader, preventing close/cancel during
an unanswered handshake. Initialization now belongs to the cancellable turn and
its once-cell retains successful equipment between prompts. The final focused
ACP fixture reaches an actual owned HTTP MCP initialization request, holds its
reply and verifies close/cancellation before the handshake's timeout. The
additional cancellation fix passed the final primary full gate in
`/Users/vart/.cache/rook-acp-lifecycle-ci-final.log`: formatting 1.1 seconds,
all-target Clippy 17.2, builds 13.1, complete workspace tests 877.3 and whole
908.6 seconds, exit 0. Feature source hashes remained unchanged throughout
this final gate; its receipt is
`/Users/vart/.cache/rook-acp-lifecycle-worktree-20261014.merge.json`.

All fourteen capabilities stay in scope. Row 14 still requires complete root
conversation replay for `session/load`, correct setup/workspace/readiness
semantics, the standard `session/set_mode` method alongside the legacy alias,
and configured external ACP agent execution. The pinned setup contract requires
all root history before a successful load response; current preview association/
compaction recovery does not establish that. Native model/LAN evidence remains
contingent as recorded above. The Telegram live check was explicitly waived.
No installed daemon, operator config, release or external account changed.


### ACP root history and standard session settings — 2026-10-14

The pinned v1 setup contract requires the complete conversation before a successful
`session/load` response. Load now streams the fixed root prefix with source/event
IDs and byte offsets: user/assistant text, narrative thoughts, tool arguments,
results and retained user/tool pixels. Metadata pages and per-object raw/encoded
admission precede copying; one object is verified once before bounded UTF-8 frames
await the shared queue. The incoming reader stays available while history waits
on a physically blocked output writer. Opaque provider companions are excluded;
historical admissions retain no claim that a command is live now, and terminal
tool status comes from a saved measurement. Loading creates no journal event,
model request, permission request or repeated command. Resume returns configuration
without historical updates. An oversized object/image, metadata or missing source
fails load explicitly rather than reporting a successful truncated conversation.

The standard `session/set_mode` and `session/set_config_option` persist bounded
per-session mode/effort selections, and selectively update the current runtime's
policy. Close releases temporary grants while retaining those selections for
resume. The old `session/set_stance` keeps its connection-default behavior for
sessions without explicit overrides. Corrupt preferences cannot become a more
permissive default. Existing postcard layouts and format version are unchanged.

Native ACP/store fixtures passed in
`/Users/vart/.cache/rook-acp-contract-native-final2.log`: the actual ACP server
suite finished in 7.17 seconds and storage suite in 5.12 seconds. They
exercise a long Russian/control-character source beyond normal body/frame limits,
more events than either a metadata page or preview tail, exact reconstructed
content, user/tool image attribution, saved outcomes and no command replay.
Reached image-wire, metadata and object-body bounds fail without partial success.
A 128-byte output pipe physically holds a larger history frame and still admits
incoming close; unavailable control delivery closes the view explicitly, retaining
source history. A separate owned HTTP model dispatches actual writes: selected
readonly refusal is saved in one session while another session really writes its
file. Selections survive close, leave other sessions/defaults intact and reject
oversized saved state. Whole/paged storage reads preserve dictionary generations,
and encoded payload growth is refused at its recorded input bound.

The first image-attribution fixture failure is retained in
`/Users/vart/.cache/rook-acp-contract-history1.log`: its handmade image companion
lacked the existing source-hash caption. The corrected fixture retains the source
binding rather than weakening it. The first encoded-growth fixture used a body
that still compressed inline and failed its external-source precondition; its
failed run is retained in `/Users/vart/.cache/rook-acp-contract-native-final.log`.
The corrected fixture reaches the actual external payload before growing it.
The subsequent source audit found that tiny
range calls would verify/decompress a whole object per fragment; the configured
single-object read replaced that before final verification. All-target Clippy
passed in `/Users/vart/.cache/rook-acp-contract-clippy.log` (27.20 seconds).
Configuration/help wiring, layering, unused-API and no-panic checks passed in
`/Users/vart/.cache/rook-acp-contract-conventions.log`.

The 17 feature files were guard-merged against saved primary hashes. The final
primary full gate exited 0 in `/Users/vart/.cache/rook-acp-contract-ci.log`: fmt
1.1 seconds, all-target Clippy 27.3, builds 20.3, complete workspace tests 879.3
and whole 928.1 seconds. Product source remained unchanged throughout; a single
documentary focused-duration correction is recorded in the merge receipt. The
receipt is `/Users/vart/.cache/rook-acp-contract-worktree-20261014.merge.json`. Correct
setup/workspace/MCP readiness and configured external ACP executors remain
required by row 14; native model/LAN evidence remains contingent. All fourteen
capabilities stay in scope; only the live Telegram bot check was explicitly
waived. No installed daemon, operator configuration, release or external account
changed.


### ACP workspace and requested MCP readiness — 2026-10-14

New selects an existing canonical project; load/resume validate the saved identity
and cannot silently move execution to another directory. Explicit stdio and
streamable HTTP MCP declarations are admitted before discovery/process startup,
combined with configured/plugin servers without name replacement, and kept only
with the session's equipment. Setup waits for requested initialization/catalogs,
reports definite failure without credential text, reuses unchanged resources,
replaces an idle intended list and releases an empty list. Active execution must
finish/cancel before different setup is accepted. Startup and history are owned
cancellable tasks while the incoming reader remains live; an unacknowledged new
can be cancelled without knowing its assigned session ID. Settings and retained
history remain separate from temporary process ownership. Client declarations
never change the operator configuration or persist their credentials.

Native socket fixtures exercise an actual write in another selected project,
wrong/unavailable cwd rejection, held HTTP initialization with wildcard cancel,
HTTP catalog readiness before new acknowledgement, real MCP tool output across
reused prompts/resume, reconnection after close, explicit list replacement and
failed initialization without false readiness or token echo. The stdio fixture
runs the existing MCP mock in the selected project, observes its real PID across
ordinary calls and observes death after close; loading with an empty intended
list does not resurrect it. Transient header/environment credentials are hidden
in ordinary tool history. A separate real delegated read retains redacted child
history and model context while leaving the source file intact.

Focused ACP checks passed in `/Users/vart/.cache/rook-acp-setup-native-last.log`
(9.67 seconds for the server suite). The delegated redaction fixture passed in
`/Users/vart/.cache/rook-acp-setup-family-secret.log` (0.23 seconds). Core agent,
configuration/help wiring, layering, unused-API and no-panic checks passed in
`/Users/vart/.cache/rook-acp-setup-core.log`. The existing MCP fixture was rebuilt
explicitly in `/Users/vart/.cache/rook-acp-setup-mock-build.log`; targeted stdio
checks must build it before launching the binary. The first all-target Clippy
failure on an initializer is retained in `/Users/vart/.cache/rook-acp-setup-clippy1.log`.
The first actor regression used awaited control replies against an intentionally
blocked writer; the original fail-closed synchronous control admission was
restored, preserving its existing lease/connection test. Initial failed logs are
`/Users/vart/.cache/rook-acp-setup-compile2.log` and `compile3.log`.

The 17-file change was merged after primary baseline hash checks. The full
primary gate passed in `/Users/vart/.cache/rook-acp-setup-ci.log`: formatting
1.1 seconds, Clippy 26.9 seconds, build 18.5 seconds, tests 916.0 seconds,
whole 962.6 seconds (`ci: ok`). Product hashes match the merge receipt. All
fourteen capabilities remain in scope. Configured external ACP agents are the
remaining implementation in row 14; contingent native model/LAN checks remain.
Only the live Telegram bot check was explicitly waived. No installed daemon,
operator configuration, release or external account changed.


### ACP configured external executor foundation — 2026-10-14

An explicitly trusted operator profile is now selectable with the ordinary
`delegate.executor` argument; commands/arguments come from configuration only.
It creates an ordinary linked child session and uses its selected workspace,
policy/approver, workspace lease, prompt admission and execution journal. The
bounded v1 stdio client initializes a fresh external session and sends the task
plus explicitly selected reference context. Writes use native dispatch and
checkpoints; external tool updates are retained observations and never rerun a
foreign operation. Duplicate callback IDs and wrong-session messages fail.

The owned Rust subprocess fixture actually requests permission, writes a file,
retains its child source and rolls that file back through parent rewind. Other
native cases cover read-only/untrusted launch refusal, real process cancellation,
transport loss after a completed write, partial output retention and reached
frame/text/message ceilings. Lost external completion retains unknown execution
rather than retrying. Diagnostic stderr is drained within its bound and the
loss fixture confirms inherited transient-secret redaction. External model usage
is represented by an incomplete, unpriced receipt; context updates never become
billing and no further family model allowance is inferred.

The final native cases passed in
`/Users/vart/.cache/rook-acp-executor-native-last.log` (1.08 seconds). The owned
fixture must first be built with `cargo build -p rook-mcp --bin mcp-mock`;
its preparation logs are retained under `/Users/vart/.cache/rook-acp-executor-mock-*.log`.
Earlier compile failures and corrected fixtures are retained in the check/native
logs. The wider core/convention suite, including composite family accounting,
passed in `/Users/vart/.cache/rook-acp-executor-core-final.log`; it preceded the
final external context/stderr refinements, which passed the final native cases.
All-target core/MCP Clippy passed in
`/Users/vart/.cache/rook-acp-executor-clippy-last.log` (9.25 seconds).

Primary integration requires baseline hash checks and its full regression gate.
This is a usable first executor path, not completion of row 14: client reads,
terminals, broader native interoperability and frontend fixtures still need
implementation/verification. All fourteen capabilities remain in scope; native
model/LAN evidence remains contingent. Only the live Telegram bot check was
explicitly waived. No installed daemon, operator configuration, release or
external messaging account was changed.


### ACP raw reads and native terminal lifecycle — 2026-10-14

External client reads now use ordinary dispatch, source receipts, inherited file
ownership and read observations. Raw content preserves CRLF/UTF-8, one-based line
selection and explicit reference provenance; post-tool reports remain outside the
actual file body. Source size/stream admission and encoded-window bounds precede
owned copies. Secret redaction precedes JSON escaping. A genuine Rook ACP editor
fixture drives a configured external subprocess, verifies the child association
before its editor request, returns an unsaved buffer and proves the disk is not
used as a fallback. Its oversized response is rejected before a text copy.

Native terminal callbacks use ordinary background `run_command` and `job`
dispatch. Arguments/environment stay literal, selected output limits are reached,
and invalid limits refuse before launch. A child-scoped registry and opaque IDs
prevent parent-job confusion. Finished terminal output remains pinned until
release even when newer commands finish. Cancellation ends the actual owned
child while an existing parent job remains alive. Foreign/released terminal IDs
fail. Editor-owned terminal authority is respected by not advertising a native
terminal capability for that frontend.

The process fixture found an actual plaintext credential in background completion
metadata; scoped job snapshots now redact command/output before serialization,
with inherited/foreign redactions available to spill capture. Ordinary permission
text also hides credential values while native policy still decides on the real
invocation. The runtime fixture verifies the real argument/environment values in
the written file, secret-free event bodies and permission text, and distinct
capture-truncation semantics.

The wider core/convention suite passed in
`/Users/vart/.cache/rook-acp-bridges-core-last.log`; file/job checks including
output-lease retention passed in `/Users/vart/.cache/rook-acp-bridges-tools-last.log`.
The final all-target checks passed in
`/Users/vart/.cache/rook-acp-bridges-clippy-final.log` (12.21 seconds). ACP checks
passed in `/Users/vart/.cache/rook-acp-bridges-acp2.log` (server suite 10.00
seconds), including the actual editor-buffer fixture. The initial ACP preview
regression overflowed the debug thread stack (`rook-acp-bridges-acp.log`);
boxing the external branch and recorded dispatch restored the existing native
child lifecycle fixture. The final core checks cover that dispatch change.

The first primary executor gate failed solely in the config-editor PTY fixture:
adding `acp_executor` changed its alphabetically first section away from `agent`.
The initial agent selection was made stable rather than changing a timeout or
weakening the test. The exact original PTY check passed in
`/Users/vart/.cache/rook-acp-bridges-config-ui.log` (8.45 seconds). The failed
full log is `/Users/vart/.cache/rook-acp-executor-ci.log` (1023.8 seconds).
The combined primary full gate passed in `/Users/vart/.cache/rook-acp-bridges-ci.log`: formatting 1.2 seconds, Clippy 29.1 seconds, build 33.8 seconds, tests 940.5 seconds, whole 1004.6 seconds. The union of the executor/bridge receipts matches the authoritative primary source hashes. The earlier failure remains recorded separately.

All fourteen capabilities remain in scope. Broader external provider/editor
interoperability and CLI/API/both-TUI/browser fixtures remain required; native
model/LAN evidence is still contingent. Only the live Telegram bot check was
waived. No installed daemon, operator configuration, release or external
messaging account was changed.


### ACP external provider and shared frontends — 2026-10-14

Trusted profiles now have bounded named environment overrides. Secret values use
existing `rook secrets` aliases, resolved only after launch admission and bounded
before process creation. This lets an external Rook own a separate home/store
without re-entering the parent's store lock. Project configuration still cannot
supply an executable profile. Environment shape/aggregate bounds and alias-preserving
configuration round trips are covered without spawning an unconfigured agent.

The genuine provider fixture starts the actual `rook acp` executable, with its
own scratch home, configured model and an environment-backed secret alias for
that model's authenticated request. The parent delegates through the real CLI
and daemon WebSocket, the foreign Rook writes an actual workspace file through
ACP callbacks, and the saved child remains linked to its parent. Local/HTTP
history matches across daemon restart with no foreign prompt replay. Unverified
external pricing remains explicit: the CLI reports an unfinished outcome rather
than claiming verified completion, and WebSocket retains the original stop reason.
The first CLI fixture incorrectly expected success (`rook-acp-frontends-cli1.log`);
it now asserts the intended incomplete result and the actual file/history effects.
The corrected native path passed in `/Users/vart/.cache/rook-acp-frontends-cli2.log`
(20.37 seconds, including its explicit fixture daemon build).

Both live TUI modes execute a configured owned ACP subprocess and prove real
file writing, retained child history, one parent model request and draft preservation.
The first run found a product gap: local TUI forwarded only model deltas, while
remote TUI ignored `Done.reply`. Thus an engine-generated final report was lost.
Both paths now display the final reply; ordinary streamed answers are not repeated.
The original failed fixture is `/Users/vart/.cache/rook-acp-frontends-tui1.log`;
the corrected local/daemon path passed in `rook-acp-frontends-tui2.log` (11.17
seconds). Meaningful final-reply/draft regressions passed in
`rook-acp-frontends-final-reply.log`. Browser outcomes explain unverified delegated
usage/cost in plain text, retain the external report and inspection question, and
never retry the external operation. A raw child tool card retains its exact saved
source when the current session changes; markup is rendered as text. All browser
checks passed in `/Users/vart/.cache/rook-acp-frontends-web-all.log` (1.15 seconds).

Core agent/configuration checks passed in `rook-acp-frontends-core.log`; final
all-target core/ACP/CLI Clippy passed in `rook-acp-frontends-clippy-last.log`
(2.49 seconds). These are owned protocol/model fixtures, not paid-provider or LAN
model-quality measurements. Final primary hash admission and the combined full
regression gate remain required before row 14 is closed. All fourteen capabilities
retain their original scope; native model/LAN evidence remains contingent. Only
the live Telegram bot check was waived. No installed daemon, operator configuration,
release or external messaging account changed.


### ACP combined gate and completion audit — 2026-10-14

The final primary gate exited 0 in
`/Users/vart/.cache/rook-acp-frontends-ci.log`: formatting 2.1 seconds,
Clippy 27.2 seconds, build 22.1 seconds, tests 946.3 seconds and whole 997.7
seconds (`ci: ok`). The union of executor, bridge and frontend merge receipts
matches the current primary source hashes. The inspected ACP audit matrix is
`/Users/vart/.cache/rook-acp-completion-audit-20261014.json`; discovery/paging,
resume without replay, full bounded load, session-owned close/settings, workspace
and requested MCP readiness, trusted named process/env admission, native callback
policy/checkpoints/editor buffers, terminal/output ownership, uncertainty and
shared frontend paths have corresponding actual runtime fixtures and final gate
coverage. Row 14 is complete within its advertised capability contract. Text-only
delegated tasks and unadvertised native terminals for editor-owned terminal
frontends remain explicit capability boundaries, not silently substituted owners.
No model-quality improvement or complete external price accounting is inferred.

The full fourteen-item goal remains incomplete. The outstanding real-LAN checks
are rows 5 (review), 6 (shell undo), 9 (browser model/checker) and 10 (LAN SSH).
Current read-only probes return connection refused on `192.168.1.46:1234` and
`192.168.1.46:22`. Strict SSH used existing trust only, disabled host-key updates,
and executed no remote command. These checks were not waived; only the live
Telegram bot check was explicitly skipped. No installed daemon, operator
configuration, release or external messaging account changed.

### Native LAN acceptance and review-format admission — 2026-10-14

The LAN model became available again. Original failures remain under
`/Users/vart/.cache/rook-lan-final-20261014`: `review` records the raw-JSON
parser rejecting a valid finding inside a prefixed JSON fence; `review-fixed`
records an ordinary narrative answer, and `review-schema-native` records an
incorrect after-side line 3 plus an incomplete 2048-token representation repair.
`review-qwen-final` records a stricter candidate rejecting valid JSON preceded
by prose without a fence. None is counted as a completed independent review;
parent fallback explanations are retained separately.

The final review path bounds the complete reply before decoding, canonicalizes
one validated JSON object or explicit JSON fence, and accepts a final JSON object
following prose. It rejects competing objects/fences, malformed structured
answers, unknown fields and schema-invalid structured findings without asking
another model to choose or hide them. Only ordinary narrative may use one
budgeted tool-free representation repair through the existing output contract.
The final source still validates complete diff-page reads, retained path/side/line
anchors, source checksums, finding counts/bytes and reviewer workspace immutability.
Ten integration tests cover denied edits, cancellation and source contracts,
including six format cases (fenced, narrative, prose plus JSON, invalid line,
ambiguous objects and overlong title); the parser test and all-target core Clippy
also pass. Logs are `rook-review-bare-tests-20261014.log`,
`rook-review-bare-parser-20261014.log` and `rook-review-bare-clippy-20261014.log`
under `/Users/vart/.cache`.

The exact final candidate ran on the user's `qwen3.8-flash-next` at
192.168.1.72:8080 through an owned ephemeral SSH forward with existing trusted
host authentication. `review-qwen-bare/report.json` retains the completed
independent finding at `ratio.py:2`, error null, all 248 diff bytes read,
unchanged file, no shell call and parent `end_turn`. Elapsed time was 81.530
seconds, input tokens 26,079 (20,571 cached) and output tokens 1,939. The actual
reviewer body includes an over-broad generator example: generators lack `len()`
in the old version too. The established acceptance case is the empty-list
regression and exact source anchor; successful execution does not certify every
claim in model prose. Earlier successful Qwen runs and every failed attempt
remain intact. These are functional checks on a different model from the Ornith
failures, not evidence of an optimization or general model-quality improvement.
The temporary SSH forward ended and the selected scratch credential file was
removed; the operator configuration was unchanged.

`shell-undo/report.json` records a real Ornith `run_command` modifying
`before.txt`, deleting `deleted.txt` and creating `new.txt`. The command's receipt
is finished. A user edit after prepare rejected the complete three-file restore
without altering any selected path. After restoring the probe's expected command
postimage, a new prepare and selected restore recovered both original files and
removed the new file. No command was replayed. The model turn itself stopped
`completion_unchecked` (CLI exit 2): feature effects and source/restore correctness
were independently verified, while that classifier failure remains explicit.
Elapsed time was 93.272 seconds, input tokens 16,728 and output tokens 2,612.

`browser/report.json`, `transcript.json` and `checker-transcript.json` retain the
real browser task: exactly one matching POST, page confirmation, PNG capture,
browser close, empty workspace, parent `end_turn` and independent `VERDICT: holds`.
Elapsed time was 189.883 seconds, input tokens 78,346 and output tokens 4,278.
The finite scratch limit was raised from 64,000 to 150,000 tokens for this check;
the original checker-budget failure remains recorded. No product budget default
was raised and no performance improvement is claimed.

The user authorized SSH to `root@192.168.1.72`. Existing strict host-key trust
and authentication worked. `ssh-probe/run/report.json` records actual Rook
transport commands in a unique scratch replica, foreground output/writeback,
background completion, resource reuse, detached owned-child cancellation,
source inspection, explicit recovery without replay and release. The owned
replica and empty probe parents were removed. `postcheck.log` and `post-qwen.log`
retain unchanged original llama-server PIDs 2012, 2701 and 2702. No remote
packages, model configuration or model processes were changed.

The intermediate primary schema gate exited 0 in
`/Users/vart/.cache/rook-review-schema-ci-final-20261014.log`: fmt 1.1 seconds,
Clippy 23.8, frontend build 17.2, workspace tests 913.7, whole 955.8 seconds.
All 635 frozen files matched before the final three-source guarded copy;
`review-final-merge.json` records it. The last full gate for canonicalization
and the native acceptance evidence passed; its final receipt is recorded below.
All fourteen acceptance rows were inspected; only the live Telegram bot check
was waived by the user.

### Final combined acceptance gate — 2026-10-14

The final `cargo xtask ci` against the authoritative primary source exited 0 in
`/Users/vart/.cache/rook-reference-final-ci-20261014.log`: formatting 1.1 seconds,
all-target Clippy 20.3, frontend builds 12.9, workspace tests 917.9 and whole gate
952.2 seconds. All 635 frozen files matched after the terminal gate. The complete
web suite separately passed all 98 tests in 1.161 seconds; its log is
`/Users/vart/.cache/rook-reference-final-web-20261014.log`.

During the completion audit the older `/tmp` native transport logs were found
unavailable. The ignored owned fixtures were therefore repeated against the
same frozen primary sources and stored permanently in `/Users/vart/.cache`:

- `rook-final-native-containers-20261014.log`: 12 tool tests passed, including
  the process-loss helper, 9.41 seconds.
- `rook-final-native-ssh-20261014.log`: 14 SSH tool tests passed, including the
  process-loss helper, 17.68 seconds.
- `rook-final-native-environment-engine-20261014.log`: all six engine checks
  passed for original command authority, shell undo, cancellation/unknown effects
  and acknowledged recovery, 12.91 seconds.
- `rook-final-native-container-cli-20261014.log`: actual CLI/HTTP process-loss,
  observation and reviewed release passed, 15.91 seconds.
- `rook-final-native-ssh-cli-20261014.log`: actual CLI/HTTP source inspection,
  restart and explicit recovery without command replay passed, 16.99 seconds.
- `rook-final-native-ssh-tui-20261014.log`: actual retained SSH source reading
  passed in both local and daemon TUI modes, 10.33 seconds.

These fixtures used local owned Docker/OpenSSH resources; the user's Qwen host
was not involved. Owned SSH fixture containers were removed after completion.
The final remote-model check also preserved llama-server PIDs 2012, 2701 and
2702 in `ssh-probe/post-final-review.log`.

`/Users/vart/.cache/rook-lan-final-20261014/all14-completion-audit.json` records
all fourteen original behavior requirements, concrete acceptance criteria,
implementation sources, 51 exact passing test witnesses and the actual native
receipts. The separate detailed ACP audit's source hashes still matched.
Twelve capabilities are delivered; Code Mode and semantic memory were removed
under the user's measured-benefit condition, with original experiments and
failures retained. Successful review execution does not certify every model
claim, the shell model's completion-classifier failure remains recorded, and the
browser check used the explicit larger scratch token allowance; these limits
were not replaced with stronger claims. The live Telegram bot was explicitly
waived by the user. No daemon restart, operator configuration update, install,
commit or release was performed as part of this implementation goal.

Only documentary status/evidence text was updated after the final gate; product
sources and build inputs remain the verified version.
