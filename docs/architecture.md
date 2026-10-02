# Architecture

[English](architecture.md) · [Русский](ru/architecture.md)

## The shape

```
                    ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
                    │  rook (CLI)  │   │  rook (TUI)  │   │   web UI     │
                    └──────┬───────┘   └──────┬───────┘   └──────┬───────┘
                           │                  │                  │ HTTP/JSON
                           │                  │           ┌──────┴───────┐
                           │                  │           │    rookd     │
                           └──────────┬───────┘           └──────┬───────┘
                                      │                          │
                                 ┌────┴──────────────────────────┴────┐
                                 │            rook-core               │
                                 │  Rook: config · env · agent loop   │
                                 │  context budget · file captures    │
                                 └──┬────────┬─────────┬──────────┬───┘
                                    │        │         │          │
                            ┌───────┴──┐ ┌───┴────┐ ┌──┴─────┐ ┌──┴─────┐
                            │rook-store│ │  -skills│ │ -tools │ │  -llm  │
                            └──────────┘ └────────┘ └────────┘ └────────┘
```

Three front ends, one engine. The CLI, the TUI and the web UI are views over the
same [`Rook`](../crates/rook-core/src/service.rs) façade, which is what keeps them
from becoming three products that disagree about what the agent did. Anything the
web UI can show, `rook … --json` can print.

The CLI entry point parses and routes commands. Its grammar lives in
`rook-cli/src/args.rs`, handlers in `commands/`, and turn configuration shared
with the REPL/TUI in `turn_options.rs`. In particular, command-line and interactive
JSON Schema loading use the same bounded reader.

## Why the pieces are separate

**`rook-store` knows nothing about agents.** It stores bytes by content hash,
appends to session logs, and reclaims space. It does not know what a skill or a
checkpoint is — when garbage collection needs to know that a manifest keeps its
files alive, the caller supplies an
[expander](../crates/rook-store/src/maintenance.rs). That boundary is what keeps
the store small enough to reason about and testable on its own.

**`rook-skills` knows nothing about the store.** It reads directories and resolves
versions against an [`Environment`](../crates/rook-skills/src/env.rs). Persisting a
skill's history is `rook-core`'s job, using the store. A skill system that could
only work against one storage backend would be much harder to reuse.

**`rook-llm` has no branch on vendor.** One trait, one HTTP implementation of the
chat-completions dialect that Ollama, LM Studio, llama.cpp, vLLM and OpenAI all
accept. Providers with their own wire format — Anthropic's Messages API, Google's
`generateContent`, and OpenAI Responses — get their own implementation of the
same trait, and the agent loop never learns which is answering. Responses is
selected explicitly; it shares endpoint/authentication/catalog handling with
the compatible adapter while encoding typed input/output items separately.

Provider-owned assistant state is stored as a bounded companion note paired
atomically with the visible reply or usage record. Tool bindings preserve original
call IDs through interrupted turns and forks. Transcript views omit opaque
payloads; request replay and context reporting share the same reconstruction.
Compaction retains or summarizes a complete batch, accounting for its hidden
state size. No postcard field is added for provider-specific data.

An explicit named-model phase policy is carried beside the physical provider;
`AgentLoop` owns its transitions. A successful top-level file tool records bounded
branch state before the execution journal closes the operation. The next complete
tool-batch boundary can select the configured implementation source, rebuild its
request prefix and apply its context window. Forks inherit only transitions in
their saved prefix; compaction does not reset the companion state. Images and
opaque reasoning currently retain the analysis provider, and native tools cannot
silently downgrade. Recipes replace the selected policy; independent errands
keep their existing selection. See [phase routing](phase-routing.md) for the
opt-in configuration, persistence limits and remaining adoption work.

Durable transcript navigation lives in `rook-core::transcript`: bounded event
pages, snapshot search cursors, body parts and attributed source-data quotes.
CLI, TUI and browser use the same operations. The TUI owns one bounded reader
worker; HTTP routes move decompression off the asynchronous socket executor.
The store verifies complete hashes while retaining only requested byte ranges;
a head/tail preview uses one decode. Navigation decodes attachment envelopes
under their format limits and shows only visible text, never base64 images or
opaque provider state. Quoting reads history and edits the frontend draft; it
neither appends an event nor starts or interrupts a turn.

While a turn is running, the TUI keeps a bounded preview of the next queued
message at the bottom of the chat, directly above the status line. The composer
and approval controls sit above it, so streaming output and a growing draft do
not move the preview. The queue page supplies ordering and count in both local
and daemon modes.
The running/quiet-model status sits on the composer's border. Its length never
changes draft wrapping or caret movement; a scrolled draft keeps its hidden-row
hint on the opposite border.
The TUI executes slash commands as interface actions while a turn runs.
Their command text does not become a queued user correction; plain messages,
`/followup` and `/goal` retain their explicit delivery paths.
The shared `prompt.followup` action is configurable and available through the
palette without a default shortcut. It admits the current text through the same
bounded queue worker as `/followup`, before copying it into prompt history.
Recalled prompt history includes `/followup` to preserve the delivery mode.
Questions, attachments, recipes and output options cannot be silently converted
into a text-only follow-up; immediate refusal preserves draft and options.
If a daemon connection fails after an ordinary Stop, the open TUI retains the
session, turn and caller ID. `/retry-stop` resends the same scoped request;
`/discard-stop` clears it. An acknowledgement or a successor turn clears the
attempt. A bounded private retry journal under `ROOK_HOME` keeps one pending
attempt per workspace and session across window restarts. Opening that session
restores the ID for explicit retry and pins a retry hint in the TUI footer even
after a live snapshot replaces the chat. The daemon remains the authority on
whether the Stop was applied.
Explicit retry can stop the same still-running ordinary turn or goal generation
after rejoining it. A busy view with an unknown or different owner cannot send
the saved request. An idle view can ask the daemon whether its scoped Stop had
already committed, including after a daemon restart.

An explicitly reviewed branch summary is a bounded Note in the target session.
Its record names the source session and last source event. Replay treats the
summary as attributed source data, while transcript reading renders that
attribution for people. It does not imply that files or tests still match the
departed branch. Ordinary branch navigation does not create this event.
The TUI history worker prepares and saves an opted-in navigation draft through
the same local/daemon Source methods. A separate bounded editor pins its source
and boundary and keeps the chat prompt intact. A save reply from a replaced
viewer reports the committed event without switching the new conversation.
Every history entry point carries the actual open conversation as the departed
source; the session being inspected may be a different one.

Managed MCP equipment lives in `rook-core::mcp_connections`. Initial admission
bounds declarations and concurrent handshakes; reports contain no endpoint,
command, headers, environment or server-authored error text. A reconnect reloads
trusted config and installs a candidate only after complete discovery. Agent
turns keep fixed connection/catalog snapshots, so replacement cannot interrupt or
replay their calls. CLI, TUI and browser reuse the daemon's per-workspace equipment;
standalone frontends own their manager. TUI requests use one bounded worker queue.

MCP authorization lives in `rook-core::mcp_auth`. A shared `Login` owns PKCE,
issuer/resource binding, consent expiry, grant verification and private storage.
Native frontends attach a loopback listener; the daemon keeps a bounded set of
attempts and consumes browser callbacks from its trusted origin. Frontends see
only authorization URLs and sanitized outcomes. The TUI uses a separate bounded
consent worker so catalog status and ordinary input remain responsive. Account
replacement affects new MCP connections; token refresh retains the identity of
an existing connection and serializes refresh-token rotation across processes.

Core decorates validated generation routes with a bounded, fresh snapshot of
cached model capabilities before adding retries and failover. The snapshot is
fixed for the provider lifetime; an ordinary request does not probe metadata.
It constrains known effort mappings, selects prompt-encoded tools where native
tools are unavailable, and rejects unsupported image inputs without stripping
them. Catalog reads for context discovery can seed later instances.

The wrappers around that trait are the same trait again, which is what keeps the
loop from having to know about any of them: `Retrying` waits out what means
*later*, `Limited` counts how many requests one endpoint may have at once,
`Failover` holds the list from `[models]` and moves on from an endpoint that is
not there, and `Proxy` decides whether a request leaves through a proxy at all.
The order matters and is the shape of what was meant — each candidate carries its
own retries and the failover sits on top, so "later" is answered where it was
said and only an endpoint that has run out of "later" hands over. Which endpoint
a caller gets depends on what it is: a turn wants the one it was pointed at,
because moving it part-way throws away its cached prefix, and an errand wants
whichever has room.

**`rook-contain` is the floor.** Platform glue and capability filesystem operations,
with no internal dependencies, and the one place Win32 lives. Its external
dependencies are cap-std and platform bindings, without native C builds, so
`cargo check --target x86_64-pc-windows-msvc -p rook-contain` works from a Mac
while the rest of the workspace does not. Anything may reach for it: starting a
process without a console window is its answer as much as containing one is.

**`rookd` is a separate binary from `rook`.** A container, a headless box or an
editor integration should be able to run the backend without linking a terminal UI
into it. Its `/api/chat` websocket runs a turn and streams it back, including the
approval round-trip, so the browser is a way to use the agent and not only to
read what it did.

Each chat socket bounds queued and in-flight JSON by both frame count and encoded
bytes (`server.chat_queue_events`, `server.chat_queue_bytes`). Byte admission
counts JSON escaping before allocating the encoded frame. Permits remain held
until socket delivery finishes. Terminal clients use the same core admission
queue and retain its leases through TUI forwarding until the event is processed.
Their socket reader has a separate write half, so waiting for display capacity
does not prevent sending Cancel or an answer. Only the view's relay waits for
capacity; it holds no engine or live-registry lock.

An ordinary socket prompt may carry a caller ID. For a new conversation,
`rookd` reserves that ID and creates the session in one store transaction.
For a named idle session it reserves a bounded session companion. The execution
journal binds the ID to its turn, then marks it admitted in the same transaction
as the UserMessage. A retry of the same text and options joins the matching live
turn or receives an `already_admitted` acknowledgement without starting another;
a conflicting retry is rejected. A turn that ended before admission requires
recovery inspection before a new request. Removing a session removes its claim. Frames
without an ID keep their legacy behavior.
Named-session claims share the configured `work.max_messages` receipt limit;
existing IDs remain readable when the limit is reached.
After ordinary prompt admission commits, core reports its caller ID with the
execution turn. The daemon adds optional `prompt_id` to the existing `turn`
frame and retains it through bounded replay eviction. CLI, TUI and browser
settle only the exact in-flight caller and observed session; `Started` alone
does not settle an uncertain prompt. Leaving an admitted ordinary turn therefore
allows a new prompt in another branch before the departed turn finishes.
Legacy turn frames still omit the optional field and use completion/retry.
TUI branch switches detach the previous daemon observer and advance its socket
epoch, excluding already queued source errors and snapshots. The daemon's turn
keeps running. Local busy turns refuse navigation before preparing a summary.
The browser retains at most four admitted File references across view replacement
and shows their names with an explicit clear action. Count and metadata byte
limits precede copying references or reading contents. A conversation or view
change during asynchronous file reading refuses submission and retains the draft.
The same caller ID can guard `/goal` creation. The managed run generation, goal
note, current goal value and admitted claim commit together. Retrying that ID
joins its live generation or gets `already_admitted`; it cannot create another
generation or silently attach to a later goal. A pending claim with a different
active goal requires inspection. Frames without IDs retain their old behavior.
Managed runs and their index, session queues, execution receipts, saved outcomes
and evaluation caches admit stored JSON through the same 8 MiB limit used by
their writers. `persistence::read_json` uses `Store::kv_get_limited` before
copying or decoding the companion. Follow-up driver settings use their existing
16 KiB maximum at the same boundary. Oversized values remain stored and produce
an explicit refusal; they are never shortened into executable recovery state.
Evaluation validates its stored cache before starting a journal or admitting a
task. JSON defaults and postcard formats are unchanged.
Identified continuation also admits its claim in the transaction that saves
the resume control and run state. The same fixed-size owner slot retains the
original goal generation. A retry confirms or rejoins that owner; it cannot
resume a replacement generation, including after restart. Failed admission
leaves the claim pending and commits no resume receipt. Named claims retain
the existing per-session message cap, and run serialization remains bounded.
After the creation transaction commits, an existing `agent` event carries an
optional `admission` with the caller ID and saved session. This is a request
acknowledgement, independent of stage execution IDs and queue receipts. CLI,
TUI and browser release only the exact in-flight request and its intended or
already observed session, even before `Started` arrives. The acknowledgement
does not reset the live goal, turn, metrics or draft. The daemon starts or joins
the saved goal before awaiting delivery, with the admission lock released.
An explicit retry of an admitted goal returns the same scoped acknowledgement;
refused creation leaves the original frame retryable. No receipt is inferred
from status text, and legacy `agent` events omit the optional field.
Successful identified goal continuation uses that same early acknowledgement.
An admitted continuation for an earlier generation reports `already_admitted`
without joining or changing its replacement. Legacy generations and prompts
without caller IDs keep the existing bare resume path.
The browser keeps one exact prompt frame in its tab while delivery is uncertain,
and attempts to retain it in session storage across reloads. Its explicit retry
resends the same ID, destination, text and options. The browser leaves the
candidate available after reconnect until the original turn is identified by
its durable admission or start and completion, a matching `already_admitted` reply, or explicit
discard. A new prompt waits for the saved candidate to be resolved.
Large frames can exceed browser storage quota and then remain retryable only
while that tab stays loaded.
The TUI likewise retains one size-checked prompt frame while its process runs.
After a daemon socket failure, `/retry` resends that frame and `/discard`
explicitly releases it; a new prompt cannot silently replace the ID. An
exact durable admission, observed start and completion or `already_admitted` acknowledgement resolves
the retained frame. Closing the TUI process does not persist this outbox.
The daemon-backed plain REPL uses the same bounded frame and explicit
`/retry`/`/discard` contract. It holds the original connection settings while
the frame is unresolved and re-discovers the daemon's live address before a
manual retry, so a restart on another port cannot send to the old process.
The REPL retains a new session ID from `Started` even if that turn later fails,
so a prompt after explicit discard stays in the same conversation. The local
REPL has no socket outbox.

The live replay has independent event and encoded-byte limits
(`server.chat_replay_events`, `server.chat_replay_bytes`). Broadcast notifications
carry sequence numbers rather than retaining payloads. Joining snapshots and
subscribes under one lock. Missing sequences make a slow observer replace its
view from a fresh snapshot; current metrics and the terminal outcome take
priority over old transcript fragments. Oversized terminal details are shortened
and marked partial before delivery. Durable session history remains separate.

Current approvals/questions come from bounded pending maps, not old replay
events. Answer, timeout and cancellation remove them, and revision notifications
clear resolved controls in other windows without waiting for model output.
`user_input.max_requests` and `user_input.max_bytes` apply separately to question
and approval channels. Snapshots preserve the unsent draft and answers still being
edited for active questions. New frontends request `live_snapshots=true`; legacy
clients receive ordinary events and a text marker for partial replay. An input
or settings frame exceeding the socket budget closes that view without silently
truncating a control or cancelling the daemon-owned turn.
The `ask` tool accepts up to four questions and four choices per question,
checking text lengths before copying model arguments into a pending request.
Canonical choices are strings; `{id, text}` choices from older callers are
displayed by their `text`, and answers echo that visible text. Malformed
questions return a bounded error with a valid call example instead of echoing
the model's whole input.

The CLI's local HTML review export uses the same paged transcript source in
direct and daemon modes. It writes an inclusive selected range into a new
local file, limited to 512 events and 8192 displayed body bytes each. It
escapes event content, marks shortened bodies, and never treats saved history
as evidence of current files or tests. Output is streamed into a temporary
file and published at the chosen path only after completion.
The browser history panel assembles the same selected range from paged API
reads into a local HTML download. It enforces the event and body limits plus
a 16 MiB output limit before creating the download, and uses the same source
boundary and historical file/test disclaimer.
The shared `/export-html [FROM..THROUGH] NEW_FILE` slash command reaches this
writer from both REPL modes and the TUI; the TUI uses its history reader worker
so an export does not block redraws or live turn output.
Native local/daemon TUI exports and direct CLI output share the same writer
bytes and no-overwrite behavior, including paths with spaces. Browser defaults
export the displayed history page when its range fields are blank; explicit
bounds select the requested saved range. Downloaded files declare a restrictive
CSP; script and remote-image markup in historical bodies remains escaped text.
The browser history panel also renders saved tool calls and results as native
expandable cards. Their summaries come from bounded page metadata; opening a
card fetches one bounded body part by event number, and further parts require
an explicit next/previous action. Event text remains DOM text, not HTML.
Saved tool results also expose optional dispatch measurements from existing
`rook:timing:v1` notes, matched by exact result event number and phase.
Readers inspect at most sixteen following event records and reject timing
bodies over 1 KiB before loading them. Missing, invalid, cancelled and
out-of-window measurements remain unknown; result prose never supplies a
verdict. The TUI calls/history panes, CLI history, browser cards and HTML
exports show the saved status, duration and timing event source. Dispatch
duration includes approval waits and hooks, and historical completion does
not verify current workspace files or tests. Forks see only copied notes;
stored event structs and the timing format are unchanged. Older daemon JSON
responses without this optional field remain readable.
Command, search and MCP outcomes also have a display-only
`rook:tool-details:v1` companion, committed atomically before the result and
before its image companion when present. The optional `tool_details` in
transcript JSON names that exact source event. Readers inspect at most two
preceding records, check the 4 KiB object bound before decoding and require
the recorded tool name and exact result body hash to match. Binding uses the
final redacted/hook-expanded text and image caption, so a fork cut at a companion
cannot assign its facts to another result with a different body. The writer admits bounded names/image metadata
before copying and stops during JSON encoding at 4 KiB. Stored postcard structs
and the storage version are unchanged; old JSON and unmeasured results remain
readable. These notes do not enter model replay or compaction summaries.
Command exit codes come from structured outcomes, with separate timeout,
background-running and unavailable states; a dispatch completing is not a
background process completing. Search counts are matching lines, with scanned
file count and explicit partial-scan status for file limits, unreadable paths
or oversized text lines. A display limit alone does not make the scan partial.
MCP identity comes from the configured tool/catalog route, including deferred
calls, rather than server-returned metadata. Typed text/resource/unsupported
counts and validated image MIME/dimensions describe the original outcome;
transport or image-validation failures leave content types unavailable.
Image metadata requires an adjacent retained image companion and never embeds
base64 in transcript views. CLI, TUI readers/calls, browser cards and both HTML
exporters render these saved facts with their source and preserve text escaping.
`write_file` and `edit_file` also report bounded saved change previews. Rook
stores a redacted plain-text `rook:tool-changes:v1` note atomically before the
result (before the image companion when present). The result's optional
`change_note` points to that event in the same session. Readers check at most
two preceding records and reject preview objects over 32 KiB before loading
the heading. Existing history body parts read the preview on demand; no
current files are compared during history browsing. The browser renders added,
removed and hunk lines as text spans inside an expandable saved-change card;
TUI calls/history use `c` to open its bounded reader. CLI history/entry and
HTML exports also identify the source event. Exporting only the result does
not pull the change note into a selected range automatically.
Preview production admits at most three files, labels up to 512 bytes and
8 KiB per diff. The formatter keeps a bounded head and tail, evicting before
copying new output chunks and reporting middle elision. It never builds the
whole unified diff in memory. Diff computation has a 200 ms approximation
budget on inputs at most 64 KiB
each. Approval and model-result diff previews use the same bounded generator.
An optional write preview reads old disk text through the capability root with
a 64 KiB limit; editor-owned buffers have no bounded preview-read contract and
leave old text unavailable. A missing or limited preview never prevents a
write. These notes are display data, excluded from model replay/compaction,
and forks retain only their own copied event history. Stored structs and the
format version are unchanged; older daemon JSON can omit the new field.
Live browser tool notices are expandable cards as well. The tab matches
same-name completions in arrival order and reports failure plus elapsed time
observed in that tab. This is live transport state, not a persisted verdict or
tool duration. Completion also carries an optional exact result event number
from that session. Core emits it after binding result receipts/images and
attempting to save dispatch timing, so an observer can immediately browse the
committed result without guessing from the latest event or tool name. Older
JSON can omit the reference; calls without one keep their history notice.
Successful `load_skill` completions link the execution journal's existing
`ToolResult`, alongside the separately retained `SkillLoaded` event. Finding
the live link also binds saved dispatch timing; it does not wrap the scoped
skill instructions again or add replay events. Failed built-ins use their
existing journal results as well, even when their own event is an `Error`.
Live completion, resumed chat and history use the same expandable card reader.
Results can also carry an optional `image_note` reference. This is computed from
the adjacent existing image companion and its object hash in the saved result's
caption, using only a 128-byte result tail; an orphaned companion at a fork
boundary cannot lend pixels to a different answer. The same binding is used by
model replay and explicit image retrieval. Valid historical records and stored
formats are unchanged. Default history/live frames do not include encoded pixels.
`GET /api/sessions/{id}/history/{seq}/images/{index}` explicitly returns one
validated raster image and its source/index/count. The reader checks raw object
size before loading, borrows stored strings while admitting at most four images,
and validates each file's 2 MiB limit, MIME and dimensions before copying. HTTP
admits at most two decoding workers; their slots outlive an abandoned connection
until the blocking work actually finishes. CLI routed responses and browser
image reads are bounded while bytes arrive, including base64 and the small JSON
wrapper. Missing pixels return 404; indices outside 0–3 return 400.
Browser **Show saved image**, paging and **Hide image** controls are explicit.
Only one picture is retained across cards; a new request cancels the previous
one, closing releases the picture, and detached cards ignore late data. Each
picture identifies its original session/result and image companion. CLI
`session image ... --output NEW_FILE` exports the checked raster bytes without
replacing existing files. TUI Calls/history keep an attributed text fallback
and a pinned CLI export command; terminal pixel rendering remains an experiment.
Each reader admits one request at a time and replaces its bounded body part;
result and diff parts load only on explicit expansion or paging. Disconnected
cards ignore late replies. The saved timing and change-note source remain
historical, separate from the tab's observed elapsed time. Pending card metadata
is capped at 128 entries in addition to bounded chat scrollback.
TUI chat completion retains the same result reference through local and daemon
events. Each compact call row holds an exact session/event link for the lifetime
of its byte-bounded scrollback row; trimming a prefix moves links and selection
together, and replacement snapshots/session switches clear them. Identical
call descriptions pair in announcement order. F5/F6 select previous/next saved
results and reveal the selected row; F7 opens its existing bounded history
reader (latest saved result by default). These are named, configurable actions
in the common key registry and palette; the chat border shows active bindings.
The draft and running turn stay intact while reading result parts or following
`c` to a saved diff. Resumed chat also adds links and uses saved measurements;
older results without a measurement keep their status/duration unknown.
`rook session context` also shows the last attempted model request's bounded
tool catalog: configured provider ID, native or prompt-encoded delivery,
stub or full schemas, estimated schema tokens and omitted-name count. Rook
records this as a service note before sending the request. It is excluded
from model replay, contains no prompt or schema body, and remains readable
through the daemon's existing context API. A recorded attempt does not prove
the provider accepted it or that current tools match that old request.
The same note lists bounded sources assembled for that request: project
instruction paths and whether each was complete, environment and hook
context, skills shown as catalog cards or inlined bodies, and sources added
beside the prompt such as recalled memory, plan, workspace sketch and output
schema. Skill counts distinguish discovered, applicable and advertised entries;
the loaded-event count includes only skill bodies still in live context after
compaction. A card does not imply that its body was loaded. Origins and token
counts describe the recorded request attempt; the inspector does not reread
those files or claim they still match the workspace.
The same note snapshots the MCP catalog installed for that attempt. It counts
discovered tools, those offered directly in the request, and tools reached
through `mcp_tools` and `mcp_call`; at most 16 deferred names are copied, with
an omitted count. These are catalog entries available at setup, not a claim
that a remote server remained reachable when the model later called it.
`agent.lazy_tools` uses shorter advertised schemas and does not itself defer
tools. Older request notes default the MCP summary to empty.
The TUI's context pane uses the same local or daemon context source as the
CLI/API. It separates the live estimate from the saved request attempt and
labels the latter historical; `r` reads the latest note again. The pane never
reads today's project files to explain an older request.
The browser's Context tab reads the same daemon endpoint for the selected
session and passes that session's workspace for the effective model window.
It renders the live estimate and historical request in separate sections,
using DOM text nodes for saved names and origins. Selecting another session
or refreshing invalidates older in-flight reads.

## The agent loop

[`AgentLoop::run`](../crates/rook-core/src/agent.rs) owns turn orchestration and
step ordering. Its private modules in `agent/` separate the responsibilities:

| Modules | Responsibility |
|---|---|
| `lifecycle`, `setup` | Admission, recipes, hooks, execution receipts, installation and closing |
| `prompt`, `history`, `budget`, `compaction` | Stable instructions, source context, replay and context limits |
| `stream` | Provider progress, partial answers and releasing the endpoint before follow-up requests |
| `tool_catalog`, `tools`, `effects` | Tool schemas, dispatch, approvals, checkpoints and change tracking |
| `delegation` | Child execution, isolation, steering and collection of results |
| `checks`, `output` | Verification, structured answers and final artifact writes |

Subagent futures and collected results belong to `Nursery`; the provider stream
pump advances it through methods. Collecting a ready result records it before
returning, so cancellation of a wait cannot lose an already received result.
These modules share one `AgentLoop` and preserve the public API used by every
front end. Per step:

1. **Budget check first.** [`ContextBudget`](../crates/rook-core/src/context.rs)
   compacts *before* the request when the estimate crosses the threshold. An agent
   that discovers the limit by being rejected has already lost the turn.
2. **Build the request.** System prompt with the detected environment and the skill
   *catalog* — cards, not bodies. Tool *stubs*, not full schemas.
3. **Call the provider.**
4. **Dispatch tool calls**, including the `load_skill` pseudo-tool that pulls a
   skill body into context on demand.
5. **Append everything to the session log**, bodies stored by content hash. The
   flush to disk is not per event — see
   [storage.md](storage.md#what-a-power-cut-can-take) for where it happens
   instead, and what that cost before it moved.

Two behaviours are structural rather than optional.

**Progressive disclosure.** A hundred skills cost a few hundred tokens per turn
instead of tens of thousands, and on local models a tool-heavy prompt is roughly an
order of magnitude slower to process than plain text. Cards and stubs are the
default; `lazy_skills` / `lazy_tools` in config turn them off, not on.

The two are not the same trade. A skill card defers the *whole body*, and
`load_skill` fetches it — the model asks by name. A tool stub defers only the
prose: the first sentence of the description, and every argument's name and type
without the guidance around them. There is nothing to fetch, because a tool
advertised without its shape could not be called at all.
`the_whole_advertised_tool_list_stays_within_a_budget` holds both numbers — the
full schemas under 2,500 tokens a request, the stubs under 1,100, which is what
is actually paid because lazy loading is the default — and asserts that stubs
cost less than half of full, or the deferral buys nothing. It lives in
`rook-core`, because the loop adds tools of its own on top of `rook-tools`, and
the two largest advertised are among them: the test lived in `rook-tools` first
and guarded 729 tokens of a list that cost 1,476. `cargo run -p rook-core
--example schema-cost` prints where they stand today. Each time the cap was
raised the commit says which tool did it and what was cut first.

The skill catalog is capped by `agent.max_skill_cards`, and what does not fit is
named as a count rather than dropped silently: `load_skill` answers an unknown
name with the skills that match it, so a skill off the end of the list is still
reachable by description. It is sorted by source, nearest first, with the name
breaking ties — for two reasons that happen to agree. What the cap cut used to be
whatever came last alphabetically, so a project's own skill lost to one Rook ships
with, for no reason anybody chose; and the list is the front of the request, so an
ordering that shuffles between turns invalidates the cached prefix of everything
behind it.

**The environment in the system prompt.** The model is told the OS, arch and
userland it is operating in, and which toolchains exist. This is cheap and it stops
the most common cross-platform failure in agent transcripts — reaching for GNU
`sed -i` semantics on a BSD box.

## Where data lives

```
~/.rook/                 (or $ROOK_HOME)
  config.toml            everything tunable, with bounded defaults
  secrets.toml           0600, and never in the store: a fork would copy it
  format.json            store format version; a newer one is refused, not corrupted
  rookd.addr             where a running daemon is listening; removed on shutdown
  store/
    index.redb           metadata, session logs, refs, and inlined small objects
    objects/aa/bb/<hex>  payloads too large to inline
    dicts/<kind>.zdict   trained zstd dictionaries
    tmp/                 staging; anything left here is crash residue
  skills/<name>/SKILL.md user skills
  plugins/<name>/        Agent Plugins: skills and MCP servers together
  servers/<name>/        language servers `rook lsp install` fetched
  running/               one file per turn in flight; one left behind is a turn that died
  output/                the whole of a runaway command's output
  cache/sources/         skill sources between searches; deleting it costs a download
  logs/
```

`running/` and `output/` are outside the store deliberately. The store takes one
writer and what is kept there has to outlive that writer's death, and a command's
output is the agent's record rather than the project's — in the workspace it
would be in every checkpoint and every `git status`.

One root directory rather than the platform-idiomatic split across config, data
and cache locations. An agent's state is one thing people back up, sync and
inspect together, and scattering it across three OS-specific paths turns "where did
my agent's memory go" into a support question.

Project-local skills live in `<workspace>/.rook/skills` and shadow user skills of
the same name — a skill vendored into a repository is there on purpose.

## Failure handling, on purpose

- **One broken skill does not empty the catalog.** Discovery collects errors and
  keeps going; `rook doctor` lists what failed to load.
- **A capture that exceeds its budget is an error naming the budget**, not a slow
  path that stages 45 GB.
- **The payload file is written before its index entry.** A crash between the two
  leaves an orphan file that `gc` reclaims; the reverse order would leave an index
  entry pointing at nothing.
- **GC is mark-and-sweep, not refcounting.** Refcounts drift after a crash or a
  manual edit, and a store that miscounts deletes live data silently.
- **Every object is verified on read.** The hash is recomputed after decoding.

## Reading further

- [storage.md](storage.md) — the on-disk format and where the compaction comes from
- [skills.md](skills.md) — authoring, versioning, variants
- [platforms.md](platforms.md) — the four targets and what actually constrains them
- [adr/](adr/) — the decisions, with their alternatives
- [research/agent-landscape.md](research/agent-landscape.md) — what this is built against

### Source authority

`rook-core::sources` labels retrieved material as JSON data with harness-owned
provenance. AGENTS files, skill catalogs/bodies and hook context travel outside
the system role; live tool results, replayed history, memory and compaction
inputs preserve the same boundary. Content-pinned sources can carry scoped
instructions, but never tool permissions. Replayed skills recheck the current
pins; old records without provenance remain data. `ask` separates actual chosen
answers from questions and appended hook data. The completion classifier records
refusals/blockers as incomplete (`blocked`), without forcing a refused task to
continue. Neither this classifier nor prompt framing is a deterministic security
boundary; tool policy and containment remain independent enforcement layers.

## Durable task supervision

`rook-proto::work` defines task states and steering receipts.
`rook-core::work::managed` owns durable transitions, budgets, retry deadlines,
stage admission and completion verification. Session goals reuse their existing
conversation; standalone runs use iteration sessions. `rookd::work` schedules these
turns independently of HTTP/WebSocket lifetimes. TUI `/goal` uses the existing
chat registry for streaming and approvals and continues in the same session,
including after restart.

Scoped correction receipts retain their caller ID and goal generation across
daemon restarts; local `session queue` reads and socket retries resolve the same
saved receipt. A changed text with that ID is refused.
The real-daemon recovery scenario checks that a correction submitted before
shutdown enters the resumed model request exactly once, with its acceptance
receipt persisted before that request. The same caller ID retrieves the
original receipt on retry after restart.

Conversation goals write the current goal and its history note when their
generation is created. Accepted corrections update both in one transaction.
Starting another stage reads that saved goal without writing it again; this
also prevents a stage that read an older run from replacing a newer correction.
Standalone work still records its effective goal in each new iteration session.

Identified `/api/work/{id}/control` mutations save a bounded control ID receipt
beside the run state under the managed-work write lock. A repeat with the same
action and generation reads the current run without applying the control again;
an old generation cannot affect a replacement conversation goal. The bare
action request and response remain compatible with older clients.
Cancellation retires retained paused context only after both the managed stage
and its execution have stopped, and when recovery has no unknown effects.
A process-local guard owns the entire managed `advance` future, including the
gaps before execution and during verification. Claims are serialized with
controls and capped by `work.max_parallel_runs` per store before copying paths;
duplicate stage claims are refused, and dropping the future releases its claim.
Identified cancellation saves its receipt and state together, then removes the
retired family's managed-work tags. Starting a new goal also retires stopped
context left by older cancelled records. Execution/recovery receipts and stored
run formats remain unchanged.
The socket's `/continue` prompt reuses its caller ID for a paused goal's resume
control. Repeating that prompt after another pause finds the saved control
receipt and leaves the later pause intact. Older socket prompts without IDs
still use the legacy transition.

Socket Stop controls for ordinary turns carry the execution receipt's turn ID.
The daemon compares it with the current active receipt under the same writer
lock that reserves follow-ups, then saves the pause and aborts that live turn.
An old Stop cannot stop the next ordinary turn in the same session. The daemon
announces the turn ID in live events and retains only the latest ID in bounded
replay. Goal Stop continues to use the managed goal generation and its durable
identified control receipt; older ordinary Stop frames without a turn ID keep
their legacy behavior.

An identified ordinary Stop stores a caller ID and turn pair in the existing
execution receipt, retaining the 64 newest pairs. The daemon commits that pair and the follow-up
recovery pause in one session transaction before aborting the live task. Exact
retries return an already-applied acknowledgement, including after another
turn or daemon restart. A different turn with a retained ID is rejected. New
socket clients include the session ID so a retry can reach an idle session
without a live observer.

Scheduled tasks are exposed through `/api/tasks`; the TUI and web views share
this API. Each occurrence reserves a session ID durably, then creates an
ordinary goal session. `/api/work` remains the execution and legacy
compatibility API. Calendar computation and persistence live in
`rook-core::schedules`.
See [durable work](durable-work.md) and [ADR-0014](adr/0014-durable-work.md).
