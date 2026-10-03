# Browser streamed Markdown batching, October 2026

Part of [reference adoption](reference-adoption-20261003.md), following the
OpenHands display mechanisms described in the [review](reference-review-20261003.md).

## Behavior

`streamed-markdown.js` retains one admitted source prefix and one frame plus one
background fallback timer, rather than an array per token or a full source copy
in `dataset.text`. Whole-prefix parsing preserves Markdown fences split across
arrivals. Normal updates run in the next animation frame; a 100 ms watchdog
handles a background tab whose frames are suspended. Completion, errors, Stop,
questions/tools and disconnect flush immediately. Snapshot and session/turn/view
changes discard pending work. Callbacks capture connection, session, turn and
viewport identities, and a generation invalidates already queued callbacks.

An authoritative final reply replaces the current partial block; an equal final
reply does not trigger another parse. History renders each saved answer before
advancing. Successful or failed late history loads, including two loads of the
same session, cannot overwrite the newer view. Draft/question recovery and queue
receipts retain their existing independent ownership.

One answer admits at most 1,048,576 UTF-16 units before copying/concatenating.
Shortening preserves surrogate pairs and names the source session and saved
history. Scrollback admits 4,194,304 model-source units in addition to the existing
2,000-block limit; old nodes retire before parsing/building the next DOM. Model
source attributes have been removed; finished buffers release their source.
These limits affect the displayed prefix, not the durable record.

## Controlled actual browser measurements

The probe serves the real UI through an owned scratch `rookd`, drives actual Edge
headlessly with a fresh profile and injects deterministic arrivals into the
production chat handler. It neither calls a model nor measures server/provider
throughput. Instrumentation times from Markdown fragment creation to its DOM
replacement and counts these replacements. The baseline uses `d4e98e0`'s
`chat.js` through a CDP response override, without changing the checkout.

The identical corpus has 75,957 UTF-16 units, 1,134 arrivals of up to 67 units,
36 cohorts of up to 32 arrivals, and a fenced SQL body including Cyrillic and
emoji. Each cohort waits two frames. Both runs verify every final code character
and the final paragraph, with exactly one answer block. Raw run artifacts are
`target/reference-browser-rendering/baseline.json` and `batched.json`.

| Measurement | Before | After |
|---|---:|---:|
| Markdown parses | 1,134 | 36 |
| Markdown parse time, ms | 57.4 | 4.4 |
| Synchronous arrival handlers, ms | 7,298.3 | 4.1 |
| Entire controlled delivery, ms | 7,732.7 | 1,298.9 |
| Full source units retained in a DOM attribute | 75,957 | 0 |

Handler time includes forced layout/scrolling; it is not all Markdown parser
time. Batched parsing/DOM work occurs in the subsequent frame, outside that
handler measurement. The cohort frame waits contribute to elapsed time, so
these figures describe this controlled workload, not a general speed ratio.

Repeat on Windows with Node and Edge installed (set `ROOK_PROBE_BROWSER` for a
different executable); on other platforms point that variable to Chromium:

```text
cargo build -p rookd
node xtask/probes/browser-rendering.mjs baseline
node xtask/probes/browser-rendering.mjs batched
```

The runner copies the daemon binary into its fresh artifact root so later Cargo
builds cannot collide with an executing Windows binary. Its `ROOK_HOME`, workspace
and Edge profile are owned scratch paths. It closes only its own processes.

## Verification

Real-source Node tests cover frame bursts, pending Done/Error/Stop/disconnect,
final replacement, split fences and Unicode bounds, stopped/stale callbacks,
snapshot/private answer retention, old socket/branch/turn/view ownership,
synchronous history and overlapping successful/failed history loads, and actual
admission plus eviction at the default per-answer/scrollback bounds. Queue
acceptance flushes pending text while stale/foreign receipts leave the current
burst and the receipt's newest revision intact.

The complete Node suite exited 0 (`target/reference-browser-node.log`). Its first
focused run exited 1 because a fixture invoked a saved callback while trying to
capture it and another asserted the snapshot notice instead of the answer node;
both fixture errors were corrected. No storage format or replay change.
The embedded-module tests and JavaScript syntax checks exited 0. The final Edge
probe also exited 0 with actual DOM checks for error, pending Stop, same-question
private answers, composer draft, snapshot, new branch/old socket and disconnect.
It also verifies queue receipt revisions and acceptance in the actual DOM.
Its first boundary run sent new-branch events before the asynchronous viewport
and replacement socket were ready. The harness now waits for that actual
condition before delivery. The runner waits for its owned processes to exit;
startup errors also go through scratch cleanup. The final probe exited 0
(`target/reference-browser-edge-final.log`). `cargo xtask ci` exited 0 in 540.7
seconds (`target/reference-browser-ci.log`). Storage and wire formats were
unchanged, so no new compaction measurement was required.
