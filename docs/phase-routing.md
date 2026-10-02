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
