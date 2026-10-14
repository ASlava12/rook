# Composite recipes

An explicitly selected version-1 TOML recipe can declare named subrecipes. Rook
loads the complete bounded definition tree before the first model request. A
running composition uses those definitions even if files change later. Each
stage starts a fresh conversation linked to its parent; it does not inherit the
parent's conversation. Stages share the workspace and ordinary tool policy,
checkpoints, cancellation and execution recovery. File tools retain their normal
conflict checks. This is context isolation, with the existing workspace tools.

```toml
# .rook/recipes/main.toml
version = 1
prompt = "Run analyse first, then summarize its output."

[subrecipes.analyse]
path = "analyse.toml"
[subrecipes.analyse.values]
focus = "{{focus}}"

[subrecipes.summarize]
path = "summarize.toml"

[parameters.focus]
default = "correctness"
[limits]
tokens = 65536
seconds = 600
```

Paths resolve relative to the declaring recipe and must stay inside the selected
workspace. Fixed `values` can use declared parent parameters in one expansion;
calling arguments cannot overwrite a fixed value. Subrecipe parameters/defaults,
skill, configured model, output contract and tighter limits use the existing
recipe format. Cycles, excessive nesting/counts, source bytes and invalid
parameter/model declarations are refused. Source files cannot grant permissions.

Select the root with `rook run --recipe main 'additional request'`, chat/TUI
`/recipe`, or the browser's existing recipe controls. A recipe with named children
advertises `run_subrecipes` to its model. The root procedure and declared child
parameter cards remain separately quoted source data. A call can request:

```json
{
  "mode": "sequential",
  "stages": [
    {"id": "scan", "recipe": "analyse"},
    {"id": "summary", "recipe": "summarize", "inputs": [
      {"stage": "scan", "parameter": "findings"}
    ]}
  ]
}
```

A dependent parameter must name an earlier completed stage in the same call.
Only its bounded saved output is passed, as quoted data; partial/failed outputs
cannot start a dependent stage. Sequential failure skips the remaining stages.
`parallel` runs independent stages within `[agent] max_parallel_subagents`; it
refuses dependencies within the same parallel call. Nested subrecipes use the
same executor and are limited by `composite_recipes.max_depth`. The shared
per-turn subagent allowance is not refreshed by nesting.

Each call divides the parent's remaining token allowance among its requested
stages. Children share the parent's absolute deadline; their recipe limits only
tighten it. Known usage is folded into the parent, and sequential dispatch checks
the remaining parent allowance before starting another child. Token spending
uses the engine's existing reported-usage boundary. If a failed child lacks a
complete cost report, the family stops further model requests rather than
assuming an unused allowance. The ordinary billing receipts retain coverage and
uncertainty separately. A completed stage means its execution produced a result;
it does not turn the parent's independent goal check into a proven success.

## Inspect sources and recover

`rook session recipes SESSION '<JSON>'`, chat/TUI `/recipes <JSON>` and POST
`/api/sessions/SESSION/recipes` share the same core. The Sessions browser page
has a Composite recipes panel. Empty requests list the retained calls; lists are
paged. Individual stages link to their original sessions and retained outputs.

```json
{"action":"list","after":null}
{"action":"inspect","id":"CALL_ID"}
{"action":"read","id":"CALL_ID","seq":42,"offset":0}
{"action":"parameters","id":"CALL_ID","stage":"scan","offset":0}
```

Source and output pages contain at most 4 KiB as base64, an exact byte offset,
size and continuation. Definition/output events are pinned by object identity;
a replacement event cannot masquerade as the original source. Large parameter
sets are omitted from summaries and remain readable in byte pages. Original
source events survive forgetting the bounded call index.

A child session, its parameter source and its stage receipt are admitted
atomically before execution. Source/output anchors commit with their receipt
using the event sequence assigned in the store transaction. Cancelled or lost
owners leave interrupted stages; no stage is automatically rerun. Review the
original child sessions and their ordinary execution recovery before deciding:

```json
{"action":"resolve","id":"CALL_ID","note":"Reviewed the original child executions"}
{"action":"forget","id":"CALL_ID"}
```

Resolve requires a note and refuses a live child. It records the reviewed
interruption, replays nothing and does not acknowledge original unknown tool
operations. Those still require their ordinary execution controls. Forget accepts
completed, failed or reviewed calls. Continuing unfinished work requires a new
explicit call with the selected stages and inspected prior output; it does not
replay the interrupted call. Original stage sessions/history remain inspectable.

`[composite_recipes]` configures definition depth/count, stages/calls, raw source
bytes, retained output bytes and encoded state bytes. Bounds are applied while
reading and encoding. Source snapshots are ordinary retained session events;
there is no new unbounded cache or separate agent engine.
