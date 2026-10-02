# Phase routing

A named source can opt into analysis on its own model, then implementation on
another configured source. Nothing routes by default. These names and endpoints
are illustrative; choose models and limits for your own server.

```toml
[agent]
model = "analysis"

[models.analysis]
api = "openai"
model = "my-analysis-model"
url = "http://127.0.0.1:1234/v1"
context_window = 65536
implementation_model = "implementation"

[models.implementation]
api = "openai"
model = "my-implementation-model"
url = "http://127.0.0.1:1234/v1"
context_window = 32768
```

The target must be a different configured physical source with no further phase
policy. Policy/source model names fit 256 UTF-8 bytes without control characters.
Configuration checking and model selection reject missing targets and chains
before starting a request. An empty `implementation_model` preserves ordinary
selection, including existing configuration files.

## Boundary and continuity

The first successful top-level `write_file`, `edit_file`, `delete_file` or
`move_file` records implementation intent. Reading, a failed/refused tool and
writing-looking model text do not. The next model request after the complete
tool batch can use the target. A retry of that request retains its provider;
there is no classifier request. Independent errands and delegated/checking
agents retain their existing provider selection. Children and checkers keep
their ordinary/inherited provider; they do not independently apply the parent's
phase target. An already routed parent can pass its current physical provider
to a child under existing selection rules. Errand builders carry no phase policy.

Before sending to the target, the loop rebuilds the conversation and source
manifest with its tool mode/cache prefix, resets the old usage anchor and applies
the target's context window. Existing context compaction and overflow checks then
run against that window. Native tool schemas cannot silently become text tools.

Images can move only when every target/fallback candidate has a fresh,
credential-matched catalog observation confirming image input. Unsupported or
unknown support retains the analysis provider. Catalog refreshes affect the next
turn; the source and first constructed phase target freeze their observations
for the current turn. There is no generation-time metadata probe or classifier.

Provider-owned reasoning moves only when every target/fallback candidate can
replay every block unchanged. Responses validates its scoped envelope and visible
text/tool bindings. Anthropic tags newly received thinking/redacted blocks with
an internal 32-byte endpoint/credential/model/window scope, removes only that
tag before the wire request, and preserves signed/opaque bytes. Compatible
aliases of the same transport can move; changed endpoint, key, model or window
cannot establish that compatibility. Legacy unscoped Anthropic blocks remain
usable on their original provider and retain it for a phase handoff.

Automatic generation fallback applies the same scoped replay check before
copying/sending a request to another candidate. A recovered preferred endpoint
cannot take over a conversation whose opaque state belongs to its fallback.
Compatible aliases may continue unchanged. If no compatible candidate answers,
the request fails with the candidate names and recovery guidance; state is not
silently stripped. Compatibility is filtered before endpoint cooldowns, so a
healthy incompatible endpoint cannot prevent retrying the only compatible one.
Legacy unscoped Anthropic blocks can resume on the configured primary only;
they cannot establish compatibility with a fallback. Signed and redacted
Anthropic blocks retain their relative wire order as well as their bytes.

An incompatible native-tool target also retains the analysis provider instead
of converting native schemas to text. These holds explain their reason once per
turn, leave implementation intent durable and preserve images, schemas and state.
No placeholder is substituted. Context inspection shares the same decision and
reports the next effective window. A later turn re-evaluates fresh observations.
An explicitly selected recipe model replaces the preceding phase policy.

## Branch state and recovery

Intent is recorded as a bounded JSON companion value and an attributable note,
atomically together. It describes the selected policy and phase, not today's
file contents or a verified result. It survives reopening and compaction. A fork
inherits only transitions before its exclusive saved-history boundary; a fork
before the first transition starts analysis. A changed target is a different
policy, so it starts analysis until a successful file change activates it.

At most 16 policies are retained per branch, under a 16 KiB read/encoding limit.
The limit is checked before copying stored bytes. Reaching either limit reports
an error and preserves prior state; old phases are not silently evicted. Deleting
the session removes its companion value. Existing postcard records are unchanged.

Phase intent becomes durable before closing the successful tool's execution
receipt. A crash between the external change and that completion remains an
unknown side effect under ordinary execution recovery. Inspect and acknowledge
the operation; the agent does not automatically repeat it. The phase note and
cache are atomic with each other, not with the external filesystem operation.

The selected session model remains the analysis source. A routing progress line
and bounded dispatch note expose the target provider identity. Phase notes and
response receipts are not additional model instructions. Same-task quality,
cost and latency comparison remain pending. Live frontend interaction checks and
saved cost coverage are implemented and verified below; omitted usage and prices
remain explicit unknown facts. No real-model savings have been measured or claimed.

## Route and cost receipt

`rook session context ID`, TUI `/context` and the browser Context view show the
last historical response separately from the last request attempt. The receipt
distinguishes the selected policy, its phase, the physical candidate that opened
the response and the model name returned by the adapter. The latter uses the
configured model when the server omits its model field; it does not prove an
unreported model version behind a proxy. A successful fallback
reports its own identity. Legacy/custom providers with no dispatch metadata stay
unknown. Names are admitted under 256 UTF-8 bytes before copying; known secret
values are withheld, and no endpoint URL or credential is included.

The receipt contains provider input/output/cache-read/cache-write counters,
elapsed request/reception time including waits and retries, and whether the
stream supplied a completion marker and both primary input/output counters.
Native adapters distinguish a terminal marker from transport EOF; a synthesized
end-of-stream delta alone cannot verify completion or produce a cost estimate.
Explicitly reported zero is distinguished from an omitted counter, including
zero fresh input for a fully cached Anthropic prompt. It describes one response, not total
session spend or an invoice. Zero counters can mean the server omitted usage.
It commits atomically with the visible response and any opaque assistant state,
survives reopening, and follows only the saved prefix of a fork. Reads/encoding
are limited to 4 KiB; oversized or unsupported stored data is refused.

Optional rates are set per named physical source, in USD per million tokens:

```toml
# Under [models.implementation]; example arithmetic, not market prices.
input_usd_per_million = 2.0
output_usd_per_million = 6.0
cache_read_usd_per_million = 0.2
cache_write_usd_per_million = 3.0
```

Rates must be finite and in 0..1000000. Input/output rates are required for an
estimate; a cache rate is required when its counter is nonzero. Cache counters
are subtracted from inclusive input totals, and added separately for a dialect
that reports fresh input only. Inconsistent counters, no confirmed completion,
either primary counter omitted,
all-zero counters, a missing/mismatched physical identity or missing rates leave
the monetary estimate unknown. Rates and the computed estimate are saved in
the receipt; changing configuration cannot reprice history. They are operator
estimates from provider counters, not verified billed charges.

## Auxiliary receipts and cost coverage

Completion checks and structured-output repair retain native nonstreaming
metadata. Compaction, aside questions and final-answer calls retain streaming
metadata. Each successful call saves a `rook:model-aux:v1` note, limited to
4 KiB before copying/encoding and committed with its usage-bearing transcript
event. The note contains its purpose and the same bounded identity, wire facts
and saved rates as the main receipt. It carries no second token charge. Empty
final answers still retain usage. Aside and compaction calls now contribute to
stored token counters; custom providers remain unpriced without verified facts.

`session context`, TUI `/context`, the HTTP context response and browser Context
show `cost_coverage`: main/auxiliary receipt counts, priced/unpriced counts,
usage events without a receipt and an optional known subtotal. Event metadata
is read in bounded pages against a fixed saved-history boundary; accumulation
uses constant space. The subtotal adds saved estimates without consulting
today's prices. Inherited fork receipts remain historical, not additional
charges. An auxiliary call cannot replace `last_response` or enter model context.
Restored TUI/browser chat hides auxiliary receipts and the compaction usage
carrier; human aside notes remain visible. The saved journal and inspector keep
the accounting data.

`complete_accounting` is currently false. The subtotal is a known subset of
saved branch history, never total session spend. Missing rates or facts leave
receipts unpriced; no priced receipts means an unknown subtotal, not USD zero.
Failed/interrupted attempts may lack usable counters and pricing. Delegated-session
aggregation uses frozen child snapshots, described below. Branch-summary generation
is accounted below. The existing note
formats and postcard records remain readable; `cost_coverage` is an optional
JSON field and added counter fields default to zero when reading older JSON.

## Physical generation attempts

Main/checking, completion-check, repair, compaction, aside and final-answer paths
observe each actual provider leaf through retry, fallback, capacity, catalog and
selected-policy wrappers. A `rook:model-attempt:v1` JSON admission must persist
before opening generation. Admission and ending commit with immediate durability
using the existing event transaction; no companion KV value or new store layout
is introduced. Its matching ending has the same ID and one of
completed, failed, incomplete or interrupted. No terminal marker means
incomplete even when a native adapter synthesizes Done; legacy/custom providers
retain their existing contract without gaining verified native counters.
Opening-future and stream cancellation retain interruption and available usage
facts. A process crash cannot run Drop, so its admission remains pending rather
than implying success or zero cost. Inspection and forks never resume it.

Each note is at most 4 KiB, checked before encoding/reading. Identity is bounded
and known secrets are withheld. No reply, reasoning, endpoint, credential or
provider error body is copied into the ledger. Notes add no store token charge
and do not replace the last response. Restored chat and model history omit them;
the journal retains them. The inspector reports started/completed/failed/
incomplete/interrupted/pending counts over a fixed saved prefix with bounded
pages and constant memory. A fork before an ending retains a pending historical
attempt even if its parent later finishes it. Counts cover recorded attempts;
older sessions without these notes cannot establish complete call coverage.

The response subtotal comes from saved priced main/auxiliary receipts.
Attempt endings also retain an optional configured-rate estimate and its rates.
The observer prepares at most 512 priced source entries, keyed by bounded
identity hashes and containing numeric rates only. Excess sources, omitted
counters, unconfirmed completion, invalid cache arithmetic and missing rates
remain unpriced. Config changes/reopen never reprice a saved ending. An ending
can retain an estimate even when cancellation or process loss prevents the
successful-response receipt from being saved. Attempt notes still charge no
additional store tokens.

Context inspection exposes priced/unpriced ending counts and a separate
`attempt_known_subtotal_usd`. Response and attempt subtotals overlap; do not add
them. Counts remain independent: subtracting old response receipts from new
attempts could conceal a later failure. Neither completed nor failed establishes
a verified bill. Full accounting remains unknown until missing facts/pricing,
legacy history are covered. Delegated-session aggregation is described below.

## Branch-summary generation costs

Product CLI/REPL/TUI/browser/API paths prepare one owned summary generator,
release the daemon engine guard before model I/O and charge the source branch.
Every retry/fallback leaf retains a `branch_summary` physical-attempt purpose.
A received response records a durable usage/`rook:model-aux:v1` pair with that
purpose, even if its empty text or output limit makes the draft unusable.
Completed physical generation does not mean the summary was accepted. Native
EOF without confirmed completion is rejected and stays unpriced; available
usage is retained. Early consumer rejection can leave unknown usage, explicitly
represented by the attempt rather than USD zero.

The source boundary in the draft is captured before generation. Source
bookkeeping does not modify the target, transfer text, replace the main response
or enter model/restored chat context. Prices survive restart and a fork's saved
prefix. Existing summary/receipt records and postcard layouts remain readable;
attempt costs and coverage fields have JSON defaults. The older public
config-only `suggest_summary` function remains for embedded callers without a
store; session-owned integrations use `prepare_summary_suggestion`.

## Frozen delegated costs

Both ordinary delegation and verification checker execution save durable
`rook:model-delegation:v1` notes on their parent. Admission precedes child
generation. An ending captures the child's cost coverage at an exclusive saved
event boundary before returning or collecting its result. A checker nudge stays
in the same child session and is included once; collecting a finished child
again adds neither costs nor tokens.

Nested child counters and optional response/attempt estimates are flattened into
the snapshot, using constant memory and bounded event pages. JSON is admitted
under 4 KiB before copying/encoding. No child text, file or test result, secret
or provider error body enters this ledger. Existing transcript layouts and
token carriers are unchanged. Older coverage JSON defaults to no recorded child
costs, which cannot prove that older delegated work was free.

Context inspection in CLI/TUI/API/browser shows recorded direct child execution
states, captured session counts, missing snapshots, unfinished descendants,
pending attempts and separate parent-plus-captured response/attempt estimates.
These two estimates overlap; do not add them. Completed execution is not a
claim that a goal was met or that all native usage was reported. Missing facts
remain unknown rather than USD zero.

Parent history never consults the child's current transcript. A fork before the
ending keeps historical admission pending; a fork after it inherits the frozen
estimate. Later child activity and rate changes cannot modify either prefix.
Failure and graceful cancellation retain their states. Cancellation can freeze
a still-pending physical attempt before that attempt's own Drop saves its
interruption. Process loss keeps durable parent and child admissions pending;
neither scenario invents a zero-cost completion. Missing or unreadable child
accounting retains an explicit missing snapshot.

## Reproducible live frontend checks

[Windows probes](../xtask/probes/phase-routing/README.md) drive the actual local
and daemon TUI executables and Edge with the shipped browser modules. A bounded
loopback HTTP fixture emits an analysis tool call followed by an implementation
stream. Its usage and pause are prescribed; this is protocol/UI evidence, not a
comparison of real model quality, latency or savings.

While the implementation stream is open, Context keeps the preceding historical
analysis receipt, shows the implementation model's next-request window and
reports one pending physical attempt. After terminal usage and the completion
classifier, the saved subtotal includes all three generations. Selected policy,
dispatched source and server-reported model are shown separately. Receipt and
attempt subtotals overlap and must not be added.

Actual browser reload retains the saved receipt and subtotal. Actual local/shared
TUI reopen and selection of forks before/after the successful write result retain
their respective analysis/implementation receipt, subtotal and next window.
Inspection creates no additional model POST and does not rewind workspace files.
Scratch roots, raw requests, native screen text, browser PNGs and a verification
index stay under `target/`; the checked-in scripts recreate them on another
Windows machine. Interactive probes remain separate from `cargo xtask ci`.
