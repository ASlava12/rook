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
cost and latency comparison, complete comparison accounting and live frontend
interaction checks remain pending. No savings have been measured or claimed.

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
aggregation and branch-summary generation are still required. The existing note
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

The known subtotal still comes only from saved priced response receipts.
Physical attempt counts and response counts are independent: subtracting old
response receipts from new attempts could conceal a later failure. Neither
completed nor failed establishes a verified bill. Full accounting remains
explicitly unknown until missing usage/pricing and other scopes are covered.
