# Reference adoption, September 2026

Implementation follow-up to the September 29 reference review. The goal is to
bring useful capabilities into Rook and fix the defects found while comparing
them. Reference commits identify ideas, not copied implementations. Existing
durable goals, scheduled tasks, recovery receipts and worktrees do not need a
second implementation.

## Work and evidence

The table and final audit describe the current state. The sections between them
record implementation history, including failures that subsequent gates resolved.

| Item | Required behavior | State |
|---|---|---|
| Shared offline configuration validation | Form and runtime agree on model structure; compaction thresholds are not silently changed after save; offline check runs no credential helpers or network requests | Implemented; focused regressions and full cargo xtask ci passed |
| ACP context usage | Emit standard `usage_update` with current context and effective window, distinct from cumulative billed tokens; preserve decreases after compaction | Implemented; core compaction regression, ACP wire test and full cargo xtask ci passed |
| MCP image results | Carry bounded images through tool execution, durable replay and provider encoders; preserve batched tool-result ordering | Implemented; transport/replay/provider tests, storage compaction benchmark and full cargo xtask ci passed |
| Diagnostic export and timings | Local bounded, redacted support artifact with session state and actual measured request/tool durations; no automatic upload | Implemented; core privacy/limits/timing/cancellation, API, CLI and live TUI/browser export checks and full cargo xtask ci passed |
| Model capabilities | Distinguish configured effort from supported/sent effort; discover endpoint capabilities with bounded caching and an offline fallback | Effort reporting, bounded metadata and cache/offline CLI passed full CI; pagination passed full CI; model-scoped context learning/reports passed full CI; native local metadata passed full CI; updated effort mappings passed full CI; runtime capability constraints passed full CI (1574.3s) |
| Current provider request compatibility | Model-specific effort controls and request fields; Responses transport for reasoning with tools where Chat Completions cannot support it | Effort/request mappings and Responses transport foundation passed full CI; durable replay passed full CI (831.2s) |
| MCP connection management | Status, reconnect and authenticated HTTP onboarding without losing pending work or leaking credentials | Managed status/reconnect and OAuth implemented in CLI/core/API/TUI/browser; native and web consent including HTTPS proxy verified; local/daemon terminal consent checks and combined full gate passed (944.5s); final combined gate including reflected-credential protection passed (1012.5s, exit 0) |
| Transcript navigation | Search, jump and quote within a session using paged history; retain bounded memory on long sessions | Implemented in core/CLI/API/TUI/browser; focused navigation, local/daemon CLI, HTTP and browser checks passed; live TUI quote checks and full CI passed (962.7s) |
| MCP catalog budget | Bound per-server tool advertisement without corrupting schemas; keep deferred tools discoverable and ordering stable | Implemented; HTTP catalog/schema/budget and agent/config/API checks passed; full CI passed (872.7s) |

Validation is proportionate to each change: regression tests for the observed
failure, affected protocol/provider integrations, then `cargo xtask ci`. A green
focused test does not imply a green full gate or a multi-day live run. No release
or reference submodule advancement is implied by this work.

The first three items passed the full gate on September 30 (807.8 seconds;
exit status 0). Browser script syntax also passed `node --check`. Subsequently,
MCP CLI probes were fixed to avoid acquiring the store lock, after reproducing
the failure with an isolated running daemon. The regression uses a real stdio
server while the store is held: list, tools, call, empty JSON output, disabled
servers and the user/workspace plugin trust boundary all pass. CLI Clippy also
passes; this later correction awaits the next full gate.

Diagnostic export now selects state fields explicitly and includes measured
request, checking-agent, compaction and tool-dispatch durations. Raw log tails
require an explicit option and are bounded and redacted. Request timing includes
queue/retry waiting; dispatch timing includes approval waiting. Timing notes are
recorded after tool result binding, preserving execution receipts and image
delivery. Core tests exercise actual elapsed time, cancellation, compaction,
reopen/fork, privacy, log/event limits and refusal to overwrite an export. HTTP
and CLI tests cover a held store and missing workspace. PTY tests export during
a live tool in both standalone and daemon-backed TUI and ensure the command is
not forwarded to the model. A real Chrome 154 browser downloaded both default
and opt-in log reports against an isolated daemon: valid JSON, logs excluded by
default, a seeded token redacted, and no browser errors. The full gate (858.1s)
passed fmt, Clippy and every test target except the no-panics source check, which
found three bare unwraps in constant regex initialization. Those now have explicit
invariant messages; the next gate will include this correction and effort reporting.

Effort reporting now comes from the accepted request inside each retry candidate,
so it names the route that answered and distinguishes preference from wire
parameters. A refusal updates capability reporting, and simultaneous requests
retain their own observations. Metadata does not become conversation content or
stop first-token wait notices. Dialect HTTP-body assertions, rejection/reuse,
concurrency, fallback, core wait behavior, CLI session selection and protocol/TUI
checks pass. Chrome exercised both `max -> high` and retry without effort, while
preserving the selected level. Visual inspection also found literal `null` text
from absent browser controls; these are now filtered before DOM replacement.
This does not yet implement endpoint discovery or its bounded cache, nor replace
all current family-based wire mappings. Full-workspace Clippy and the TUI footer
PTY check pass. Chrome was checked again after the DOM correction; the controls
contain no literal null text. A combined full gate has been started.


The combined effort/diagnostics gate completed in 829.3s with exit status 1.
Formatting, Clippy and all test targets except `rook-cli --test cli` passed.
The sole failure was an old REPL assertion expecting bare effort names instead
of the new requested-level/mapping response. That assertion now checks both the
saved preference and the mapping; its targeted rerun passed. The next combined
gate includes that correction.

Model metadata now has configurable byte, entry-count and whole-response time
limits, forwarded through provider wrappers and used by CLI listing/doctor,
endpoint rechecks and context discovery. Anthropic capability fields, Google's
`thinking` flag, and compatible endpoint `supported_parameters`/input modalities
are preserved as optional facts; absent fields stay unknown. LM Studio's native
metadata request now also sends the configured bearer key and obeys the limits.
The CLI JSON listing exposes the facts without changing runtime mappings.
HTTP regressions cover three dialects, announced/chunked/error-body limits,
trickling responses, count limits, malformed/duplicate envelopes, authentication,
wrapper forwarding and invalid UTF-8. All nine HTTP catalog regressions and the complete `rook-llm` test suite pass;
21 core configuration/wiring checks also pass. The CLI regression confirms
configured limits, JSON capability facts and operation while the session store
is locked; it and the corrected REPL test pass. A new combined gate is running. An existing fixture revealed that
`{"object":"list"}` intentionally means an empty compatible catalog; that behavior
is preserved while an unrelated JSON error object is rejected. No persistent
cache, offline fallback, pagination, Ollama native discovery or revised effort
mapping is claimed by this foundation.

The combined gate including diagnostics, effort reporting, the bounded metadata
reader and the corrected REPL assertion passed (897.8s, exit status 0). This also
covers MCP probes with the store locked. That green gate predates the following
cache work and does not validate it.

A bounded private cache and `models --offline` / `--refresh` are now implemented.
Ordinary JSON stays an array; `--json --metadata` adds explicit origin, observation
time, age, credential-resolution status and notices. Online cache reuse checks
the resolved credential fingerprint, including changed output from a secret
helper. Offline lookup neither builds a provider nor resolves external secrets;
a missing/invalid cache yields the configured model with unknown capabilities.
Connection failures may show a labelled old observation; authorization failures
and explicit refresh errors remain errors. Writes use a nonblocking file lock
and atomic private replacement; read/encode/entry/model bounds apply before
retaining oversized data. Seven cache integration tests, four cache/permission/locking unit tests, six
configuration checks and three real CLI regressions pass. The filesystem helper
suite also passes. Full-workspace Clippy passed before the last two test-only
additions. Cloud offline lookup was checked with its key removed in the child
process: it succeeds, while the online command still rejects the missing key.
A new combined gate is being started. Pagination, native local-server discovery,
updated effort mappings and model-scoped context learning remain outstanding.

The cache/offline combined gate passed (956.5s, exit status 0), including
formatting, Clippy and all workspace tests. Subsequent pagination work follows
Anthropic `after_id` and Google `pageToken` with stable page sizes and a shared
request-count, byte and time budget. Record limits count duplicates; overlapping
IDs retain their first observation. Cursor cycles, missing required cursors and
unsupported continuation markers fail rather than returning a successful prefix.
Google empty repeated fields may be omitted. A cache completeness marker excludes
older potentially truncated observations. Nine pagination HTTP regressions and
the existing catalog/model tests pass. Nine cache integration tests and six
configuration checks also pass, including later-page failure preserving the
previous complete cache and migration from older observations. The combined full
gate for this subsequent work is running.

The pagination combined gate passed (942.1s, exit status 0). This includes every
workspace test, Clippy and formatting. The next correction scopes in-memory
learned context windows by actual connection, credential, model, explicit limit
and fallback group. Discovery now asks for the selected model rather than the
global default or an alias treated as a wire model name. Every fallback route
contributes its discovered or assumed minimum; explicit limits bypass discovery.
Learned observations have a configurable LRU bound and smaller observations win
when a late catalog response races a refusal. These changes are implemented and
under verification; the pagination gate predates them.

The complete provider suite and agent-loop target passed, as did configuration,
model resolution and cache integration checks. The context report now uses the
session-selected model and the same learned window, with an explicit comparison
window still taking precedence. Its focused regressions passed after that
correction. A combined full gate now covers these context changes. Native local
metadata discovery, current effort mappings, runtime capability integration and
the remaining MCP/transcript work are still outstanding.

The model-scoped context gate passed (926.0s, exit status 0). Native local-server
metadata is now implemented separately: explicit `metadata_api` on named models
or shared endpoints, legacy shorthand recognition, bounded authenticated
LM Studio v1 with 404/405-only v0 fallback, and Ollama selected-model-first show
requests plus running-instance metadata. Active/configured windows and
architectural maxima are separate facts. Optional metadata failure leaves
capabilities unknown while retaining the complete compatible model listing.
Cache namespaces include metadata mode and selected model, and offline snapshots
preserve native observations. This work also corrects a follow-up defect found
in context scope: shorthand factory defaults had been stored as explicit
`context_window` values, suppressing discovery. Built-in assumptions and user
overrides now have distinct endpoint fields, with a real HTTP regression.

The complete provider suite passed, along with core model resolution, native
cache integration and configuration validation. After the final thinking-control
mapping, the native HTTP tests passed again: boolean controls indicate reasoning
without inventing named effort levels. The native metadata combined gate passed (942.9s, exit status 0).
This gate predates the following effort mapping changes. Runtime capability/cache
integration and remaining MCP/transcript work are still outstanding.

Primary field references:
[Anthropic models](https://platform.claude.com/docs/en/api/models),
[Google models](https://ai.google.dev/api/models),
[OpenRouter models](https://openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties),
[LM Studio models](https://lmstudio.ai/docs/developer/rest/list),
[Ollama show](https://docs.ollama.com/api-reference/show-model-details),
[Ollama running models](https://docs.ollama.com/api/ps).

## Provider compatibility follow-up

The current mapping correction selects Gemini GenerateContent `thinkingLevel`
for 3.x and numeric budgets for 2.5. It distinguishes Claude effort support
from adaptive thinking, clamps unsupported xhigh on 4.6/Preview, and preserves
newer OpenAI upper levels. Model family matching respects version boundaries.
OpenAI reasoning requests use `max_completion_tokens` without temperature.
HTTP regressions compare independently specified expected values with accepted
streaming request reports and bodies. The full provider suite passed after these
changes, including 205 model/effort HTTP cases, direct completion limits and
output-refusal recovery. The final Google streaming output-refusal regression
also passed, preserving the thinking level and remembering the accepted limit.
The combined full gate passed (921.3s, exit status 0).

The primary documentation audit also found a transport gap: GPT-6 reasoning
with tools needs Responses (Astra/6.1 Sol require it for tool calls, while
Sol/Luna Chat Completions allow tool calls only at `none`). A correct effort
string is insufficient. Implement explicit Responses routing and encoding,
streaming assembly, replay of reasoning/tool state, bounded reads, cancellation,
retry/failover effort reports and regression coverage before claiming these
models work for agentic tasks. Do not silently turn reasoning off to retain the
older transport. Check Responses-only pro/Codex routes as part of this work.
Runtime capability/cache follow-up is described below; MCP/transcript work remains open.

The explicit Responses transport is now implemented: named-source and shared
endpoint routing, shorthand aliases, typed items, incremental streaming with
terminal validation, bounded reads, cancellation, retry/failover reports,
original call IDs and opaque ordered-output replay within a live turn. Pro
model minimum effort levels are retained. Local HTTP regressions cover these
paths, including incomplete/failed responses never releasing tool calls. The
complete provider suite and the final focused cache-write/alias/Anthropic
checks passed. The combined transport foundation gate passed (891.5s, exit 0).
A shared SSE defect found by Responses fixtures is fixed: CRLF and lone CR
frames now work, including line endings and UTF-8 split across network packets.
Anthropic filters foreign replay envelopes while retaining native signed blocks.

Durable integration now records assistant state atomically beside the visible
answer or usage note, without changing postcard schemas. Explicit call bindings
preserve original IDs and batched order across reopen, full forks and interrupted
prefixes. Missing results stay visibly missing. Opaque state is bounded during
serialization, checked by stored size before loading, hidden in transcript views,
and scoped to matching Responses connection/model settings. Known-secret
redaction withholds signed blocks rather than forging changed signatures.
`agent.max_provider_state_bytes` is configurable (4 MiB default, 1 KiB–32 MiB).

Replay and context reporting now share one implementation; budgeting includes
call arguments and opaque state. Compaction accounts for full state size and
keeps batches intact. A reproduced case where a large encrypted batch could
never leave the retained tail now compacts it whole after a later response has
consumed its results. HTTP integration covers reopen, full and partial forks,
compaction, transcript privacy, and oversize rejection before tool effects.
Unit checks cover pruning, redaction, binding validation and serialization/read
bounds. The existing agent loop and MCP image integration suites pass after
these changes. A file-diff fixture now supplies enough context for its scripted
300 KB tool arguments; the old budget had silently omitted them.

During parallel checks a cache writer reported contention after its previous
write returned. A duplicated descriptor reproduced the lock lifetime problem:
closing the writer's descriptor alone leaves the lock held by the duplicate.
The cache now explicitly unlocks on every return path, with a regression holding
a duplicate across release. The complete provider suite and final core HTTP,
configuration and API-usage checks passed. Oversize state retains billed usage
while stopping before tools. The combined gate for durable replay and this fix
passed (831.2s, exit status 0). These results do not claim a live cloud-model or
multi-day run. MCP/transcript/catalog work remains open.

Primary protocol sources:
[Responses migration](https://developers.openai.com/api/docs/guides/migrate-to-responses),
[reasoning](https://developers.openai.com/api/docs/guides/reasoning),
[streaming](https://developers.openai.com/api/docs/guides/streaming-responses),
[function calling](https://developers.openai.com/api/docs/guides/function-calling).
The migration guide places the Chat Completions reasoning/tool restriction
starting at GPT-5.4, so this is not only a GPT-6 concern.
Codex's `codex-rs/codex-api/src/sse/responses.rs` at
`c4017a87aacc7558002b7cb510025e967c1d765e` was also read for response event
and cache usage shapes.


Primary mapping/request references (checked September 30):
[Claude effort](https://platform.claude.com/docs/en/build-with-claude/effort),
[Gemini GenerateContent thinking](https://ai.google.dev/gemini-api/docs/generate-content/thinking),
[GPT-5.2 guidance](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-5.2),
[GPT-5.5](https://developers.openai.com/api/docs/models/gpt-5.5),
[GPT-5.6 Sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol),
[GPT-6 migration](https://developers.openai.com/api/docs/guides/latest-model),
[Chat Completion token limits](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create).

## Runtime capability snapshots

Provider construction now reads the bounded catalog cache once and decorates
each validated route before retry/failover. Only a fresh, complete observation
matching source configuration, model, current credentials and proxy routing
constrains generation. Unknown, expired, future-dated, corrupt or incomplete
metadata leaves the ordinary dialect behavior intact. Construction adds no
network probe; context-window discovery writes observations to the same cache
for subsequent provider instances. Snapshots remain fixed for their lifetime.

A model without native tools uses the existing prompt-call path. That exposed
an old gap: native function messages remained in later requests even when tool
schemas were omitted. History now encodes calls and explicitly untrusted results
as text, retains tool images with their provenance, and drops stale signed
native state when its representation changes. An image-incompatible model
refuses image input before HTTP rather than silently stripping it. Named effort
levels are intersected with known wire mappings; metadata does not fabricate
mappings for unknown model families. Anthropic adaptive-thinking support is
handled separately from effort support. Retry/fallback reports keep the original
requested level and the control sent by the answering route.

Real HTTP tests cover cold construction without probes, fresh/expired/disabled/
future-dated/mismatched observations, context-cache reuse, metadata learned by
runtime discovery, image/tool rejection before sending, prompted execution through
the actual agent loop, effort minima/absence, adaptive-thinking constraints and
distinct controls on preferred/fallback routes. Core agent-loop, catalog and
model-resolution suites passed. The full provider suite and final runtime/API-usage
checks passed, including exact cloud-model matching for a source named `ollama`.
The combined gate passed with exit status 0 (1574.3s).
This does not claim live cloud acceptance or enable unknown API controls.

## MCP advertisement budgets

Implementation source re-read: Codex `codex-rs/core/src/mcp_tool_exposure.rs`
at the local reference HEAD `c4017a87aacc7558002b7cb510025e967c1d765e`.

The shared equipment path now builds one stable bounded advertisement for MCP
servers. Limits apply to serialized JSON bytes while counted, per server and
across the catalog, as well as directly advertised tool counts. Complete remote
schemas retain nested properties, required fields, enumerations and constraints,
including when built-in tools use lean advertisements. A schema that cannot fit
is deferred whole; invalid non-object input schemas are not advertised natively.

`mcp_tools` searches every retained tool and exposes complete serialized schemas
through bounded byte pages. `mcp_call` invokes the selected retained tool through
the same approval policy and pre/post hooks, with the actual target name and
arguments. The durable call still identifies the wrapper and its selected target;
results use existing image, redaction and receipt handling. Discovery does not
mutate the request's tool prefix. Configured zero per-server budgets provide a
discovery-only mode. Ordinary aliases remain unchanged; names that need escaping
or shortening carry a digest of the original pair, and duplicate aliases are
resolved deterministically within the catalog.

Validation fixtures include real HTTP MCP calls, an oversized Unicode schema
read back whole through pages, reversed server/tool enumeration, nested-schema
preservation under lazy loading, image results and refused target calls. Shared
agent-loop coverage includes deferred discovery/call, target deny rules and a
pre-tool hook matched against the target. The tools-level HTTP suite passed (including schema roundtrip, actual byte/count
limits, stable ordering, target risk and images). The agent integration, config
and API-usage suite passed as well, including target deny rules and target-matched
pre-tool hooks. The first combined gate stopped in Clippy on two test-only
mutex guard lifetimes; those assertions now use explicit lexical scopes.
The next combined gate passed with exit status 0 (872.7s).

## Sources

- Configuration: Hermes `361dc5cf69fb9b2677e8b2c1e1ac95a92e40cfd1`.
- Context usage: ACP v1 pinned at `c849ac2f0c3e4a9f69fdd76a1bdbc659e3f5979d`,
  `schema/v1/schema.json` and `docs/protocol/v1/prompt-turn.mdx`; Goose
  `2c02ed6fcf6a91b435aacfd5d02d713db8862153`.
- MCP images: Goose `6d5eba9b14f5fade13e29d43eab2ea9329f1a3a6`.
- Diagnostics: Cline `104f3dc8a4461682a5a123ece3c6d45725914cbf`.
- Model metadata and thinking: Goose `4dea9b483efbd2541d43500b8ed3c044c65e6d2f`
  and `07396897cda7ec916ce793b6f1c829750d363ab0`.
- MCP login: pinned Codex `c4017a87aacc7558002b7cb510025e967c1d765e`,
  `codex-rs/rmcp-client/src/perform_oauth_login.rs`, `oauth.rs` and
  `oauth/refresh_transaction.rs`. The initially listed review commit was not
  available in the local reference; these checked sources anchor the attribution.
- Transcript navigation: Codex `df7f717c856e0634b04a12f7d4fc9e8ecb3e65be`.
- Catalog budgets: Codex `339e981ba71b131eae3e2086a22b143f455a8cb8`.

These are selected improvements, not a claim that every upstream change has
been reviewed or should be imported. The review covered all commit titles for
seven smaller references and sampled recent history in Hermes and OpenClaw;
the large backlog in those two remains outside the exhaustive review.


## Transcript navigation follow-up

History now defaults to the most recent bounded page, with exclusive backwards
and inclusive forwards event cursors. Literal case-insensitive search carries an
upper event snapshot and a byte offset inside an unfinished event; an empty hit
page can still have more to scan. Entry bodies and attributed quotes are paged
separately. Quotes are source data with session/event/object provenance, and only
enter a draft. Attachments expose visible text, while image bytes and signed
provider companions remain hidden. Limits live under `[transcript]` with editor
help and shared offline validation. Store head/tail reads use one verified
streaming decode, preserving full hash checks without whole-object allocation.

Core regressions traverse a session beyond the former 500/2000-event view limits,
reach the serialized byte cap with escaped content, reconstruct Unicode bodies,
find matches across scan boundaries, retain a fixed search snapshot under append,
and preserve navigation after reopen/fork. CLI regressions compare local and
held-store daemon results; HTTP checks cover navigation and error statuses. The
browser's unbounded append loop is replaced by one page at a time, and chat resume
now reads the tail. Chrome 154 exercised empty search pages with continuation,
body parts, jump, quote, draft preservation and reading during a live response.
It also caught the need to keep streaming safe with no chat viewport and restore
running controls when coming back from the Sessions tab. No browser errors in
the final check; syntax checks passed. Live TUI quote checks additionally exposed
invisible insertion feedback and an out-of-bounds cursor for long JSON drafts;
feedback and a grapheme-aware viewport now address those defects. The corrected live local/daemon TUI quote checks and full-workspace Clippy
passed. The combined `cargo xtask ci` gate passed (962.7s, exit 0), including every
workspace test target. This result predates the MCP groundwork below.


## MCP connection groundwork

Before reconnect/authentication UI work, the client audit found an incomplete
catalog: tools/list ignored nextCursor. Discovery now reads all pages, with
per-server total descriptor bytes, tool count, page count and a whole-operation
deadline. Empty intermediate pages work, cursor cycles and ambiguous duplicate
names fail, and no successful partial catalog is installed. The interactive
configuration and offline validator expose the same limits. HTTP response bytes
are checked before accumulation, error excerpts retain only their bounded
prefix. Incremental SSE framing applies its cap before copying and combines
multiline data fields across split UTF-8 and CRLF chunks. Initialized notification refusals/timeouts no longer masquerade as
successful startup. The complete MCP transport suite, catalog boundary regressions and core
configuration checks passed in an isolated working copy. The changes were transferred to the main working copy after matching every
changed file against the isolated baseline. The combined full gate passed
(857.6s, exit 0). This is groundwork, not completion of managed reconnect or OAuth onboarding.

Protocol references: [MCP tools and pagination](https://modelcontextprotocol.io/specification/2025-11-25/server/tools),
[MCP authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).
The remaining OAuth flow needs protected-resource and authorization-server
metadata discovery, verified S256 PKCE, resource-bound tokens and private token
storage. Reconnection must preserve active calls and must never replay a tool
whose outcome is unknown.


## Managed MCP connections

The next isolated implementation replaces immutable frontend MCP lists with a
bounded manager. A successful handshake and complete discovery precede a swap;
failed candidates preserve the installed catalog. Existing turns retain their
Arc snapshots and no committed tool call is replayed. Cancellation clears the
reconnect marker. Settings cap retained declarations and parallel startup, with
offline validation and inline config-editor help. Status exposes installed state,
generation, active requests on the installed connection, and static summaries of
request/reconnect failures; raw server errors, commands and credentials are omitted.

The daemon API shares equipment with turns and scopes requests by workspace or
session. CLI status/reconnect, a background TUI panel and browser controls use it;
standalone chats retain their own manager. Focused core/configuration and API checks passed. Real CLI probes and managed
reconnect checks passed. PTY checks exercised successful/failed reconnects in
standalone and daemon TUI, plus inspection during a running tool without model
submission. They caught and corrected a missing TLS-provider initialization in
the new standalone background worker; the corrected checks pass. The full MCP
transport suite and Clippy for all affected crates/targets pass. Chrome 154
exercised a deliberately held catalog response, continued draft editing, closing
and reopening the panel while reconnecting, a successful swap and a refused
candidate preserving the old generation. No browser errors or server-echoed
secret appeared. Browser artifacts: `/Volumes/cfb/tmp/rook-browser-mcp-cHSZGH`.
A new combined gate will validate the transferred implementation. This
work does not implement the still-required authenticated HTTP OAuth onboarding.


## MCP management and OAuth follow-up

Managed connection status/reconnect is implemented in core, CLI, API, TUI and
browser. Replacement completes the full catalog before installation; failure
keeps the previous connection and active turns keep their original generation.
The combined gate completed in 917.7s with exit status 1: all targets passed
except an obsolete empty `/mcp` text assertion and missing config-editor help
for `mcp_connections`. Both are corrected in the OAuth follow-up, with targeted
checks passing.

Native OAuth login/logout is now implemented in the core and CLI. Discovery,
public-client registration, PKCE and issuer-bound callbacks precede a real MCP
handshake/full-catalog check; only then are credentials saved in a bounded
private file. Refresh rotates once across concurrent readers/processes and
persists an uncertain-exchange marker before sending a refresh token. Replaced
identities never silently enter an existing connection. Header credentials and
stdio behavior remain available. Short-lived grants use their actual expiry,
including token-response network time, instead of an unconditional 30-second
margin. OAuth discovery uses the same protocol revision as the MCP handshake;
HTTP notifications and requests carry the version negotiated by the server.

Local OAuth regressions cover discovery fallbacks, PKCE, callback issuer/state,
metadata and credential limits, loss of refresh responses, failed new catalogs,
identity changes, expiry and private permissions. A real Chrome browser against
an isolated CLI/daemon and local authorization server completed consent, login,
reconnect, an authenticated tool call and logout, without exposing tokens in
CLI output. This is fixture evidence, not validation against every hosted OAuth
provider. Direct TUI/browser initiation, remote browser callbacks and their
lifecycle tests remain required before this roadmap row is complete.

Protocol reference for the negotiated HTTP header:
[MCP Streamable HTTP, 2025-06-18](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-06-18/basic/transports.mdx#protocol-version-header).


The native OAuth combined gate completed in 1000.1s (exit 1): formatting,
Clippy and all functional targets passed. The only failure was the source panic
audit treating the dedicated, test-only OAuth fixture file as shipping code.
That file now declares `#![cfg(test)]`, and the audit recognizes file-level test
attributes with a regression preserving checks of `cfg(not(test))` code.

The frontend follow-up uses the same consumable `Login` in native and browser
flows. The daemon admits a configurable bounded set of attempts, preserves them
across panel closure, refuses duplicate concurrent logins, expires abandoned
consent and consumes callbacks once. Origin/path checks precede exchange; config
changes refuse completion. Tokens stay in the daemon; authorization responses
are not cached and callback codes are cleared from page history. Web sign-in,
wrong-state refusal, full admission, cancellation, panel reopening, an
authenticated tool call and logout passed in Chrome. A separate run completed
through a local HTTPS reverse proxy with the callback on its public origin.
These remain local fixtures, not live third-party provider certifications.
TUI sign-in/logout, cancellation and system-browser launching are implemented
with a separate consent worker. Focused terminal and final combined-gate results
will determine whether this row is complete.


Both live PTY consent checks passed: standalone and daemon-backed TUI start
login, preserve a draft while the panel is closed, cancel without storing a
grant, sign in again, install authenticated tools and sign out without exposing
tokens in terminal output. The panic audit correction and affected-crate Clippy
also passed. A gate integrity issue was found while checking these results:
PTY helpers launch `target/debug/rookd` whenever it exists, while Cargo can build
only its test harness. `cargo xtask ci` now explicitly builds both frontend
executables before tests so the gate cannot silently exercise an older daemon.
The final combined gate includes that correction; earlier green test output
alone does not prove daemon subprocess freshness.


The frontend combined gate completed in 941.3s (exit 1), with freshly built
CLI/daemon executables. Formatting, Clippy, all functional tests and the panic
audit passed. The only failure was the process-spawn portability guard: the TUI
browser opener lacked Windows `NO_WINDOW`. The opener now sets this flag; the
layering/portability target must pass before the next full gate.


## Final requirement audit

The review below follows the nine required behaviors through production callers
and the regression assertions, not merely through exported function names.
The final combined `cargo xtask ci` gate passed (1012.5s, exit 0).

| Requirement | Production path | Evidence and boundary |
|---|---|---|
| Offline validation | `config/validate.rs`, `config/edit.rs`, CLI `config check` | `config_validation.rs` checks structural agreement and unchanged accepted thresholds; offline model-cache tests prove credential helpers are not executed. |
| ACP context usage | `agent/budget.rs` → `Progress` → `rook-acp` `usage_update` | ACP wire fixture distinguishes live context/window from one-token billing; agent-loop compaction regression checks that context can decrease. |
| MCP images | `rook-tools/mcp.rs` → `tool_images.rs` / history → provider image encoders | `tool_images.rs` traverses a real MCP batch, compaction, reopen, fork and explicit retrieval; provider tests check results precede images. Limits are reached in the fixtures. |
| Diagnostics | shared `Rook::diagnostics` → CLI/API/TUI/browser | `diagnostics.rs` asserts measured elapsed durations, cancellation, bounded exports, no prompt/tool/credential-helper leakage, and refusal to overwrite. Live browser and terminal exports were checked earlier. |
| Model capabilities | `model_catalog` cache → `models.rs` provider construction → runtime metadata/effort decorators | Cache/runtime HTTP tests check account and route identity, age, completeness, offline behavior, explicit limits, fallback reports and prompted tools. An absent fact remains unknown. |
| Provider requests | `rook-llm/openai/responses.rs` and effort mappings → `provider_history.rs` | HTTP assertions cover request bodies, streaming terminal validation, ordered call IDs, reopen/fork/compaction and rejection before tool effects. Live commercial-provider acceptance has not been claimed. |
| Managed MCP and OAuth | `mcp_connections.rs`, `mcp_auth`, daemon API, CLI, TUI and browser | Manager fixtures retain running calls on replacement; OAuth fixtures exercise PKCE, issuer/state binding, atomic storage and uncertain refresh. Browser/proxy and local/daemon PTY consent checks passed. The additional reflected-token regression below addresses an audit finding. |
| Transcript navigation | `transcript.rs` and bounded store readers → CLI/API/TUI/browser | `transcript.rs` traverses beyond the old event cutoff, reaches byte limits, resumes Unicode search inside an event and preserves a fixed search snapshot. Quotes enter a draft with provenance. |
| MCP catalog | bounded `rook-mcp/catalog.rs` → `rook-tools/mcp/catalog.rs` → shared equipment | MCP/tools fixtures reject incomplete catalogs, preserve nested schemas and deterministic order, page a schema beyond the budget, and call deferred tools through target approval/hooks. |

The final audit reproduced an OAuth privacy defect with a local server echoing
its Authorization header in a successful tool result. The HTTP transport now
retains the credential only for the lifetime of its own request and refuses
responses containing that credential before exposing decoded content. This also
checks structured resource keys and RPC error data. Authenticated HTTP error
bodies, malformed response excerpts and challenge descriptions are withheld,
including cases where truncation could otherwise expose only part of a token.
Concurrent requests retain their own credential after rotation; no history of
retired tokens accumulates. This is protection against accidental reflection,
not a claim that arbitrary allowed shell commands cannot read local credential
files or that an intentionally encoded value can always be recognized.


The portability correction and all accumulated frontend changes passed the full
`cargo xtask ci` gate (944.5s, exit 0) and were committed as `038d3bd`. The
reflection regression failed before the transport correction, then passed for
JSON, SSE, RPC messages/data, structured resource keys, HTTP/decode errors and
initialized notifications, with independently rotating concurrent credentials.
The complete MCP suite, focused core OAuth suite and affected-crate Clippy
passed in an isolated copy. The source files were transferred only after matching
baseline and candidate hashes. The final combined gate passed (1012.5s, exit 0),
including freshly built frontend executables. All nine requirements in the table
above now have implementation and verification evidence. Validation is local; it
does not claim a multi-day soak, live commercial-provider acceptance or a green
remote platform matrix.
