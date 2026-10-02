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
agents retain their existing provider selection in this initial block.

Before sending to the target, the loop rebuilds the conversation and source
manifest with its tool mode/cache prefix, resets the old usage anchor and applies
the target's context window. Existing context compaction and overflow checks then
run against that window. Native tool schemas cannot silently become text tools.

Images and provider-owned reasoning currently hold the analysis provider, with
one visible notice per turn. They are neither dropped nor replaced by text
placeholders. Routing can proceed when the retained request contains neither.
Supporting proven compatible image/reasoning handoffs is still adoption work.
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
and bounded dispatch note expose the target provider identity; request provenance
and ordinary response usage records continue through their existing paths. Phase
notes are not additional model instructions. A full selected-versus-dispatched
route/cost report and same-task quality/cost/latency comparison remain pending.
No savings have been measured or claimed.
