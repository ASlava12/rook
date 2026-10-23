# Recorded model calls

Recording saves canonical Rook requests and visible replies for local debugging
and review of possible regression or training examples. It is disabled by
default. It adds no model request and performs no upload or training.

Enable it for a turn:

```text
rook run --record-model-calls "Inspect the example"
```

Chat accepts `/record-model-calls on`, `off`, or `config`. An explicit false
overrides configuration. Optional configuration is:

```toml
[telemetry.model_calls]
enabled = false
max_call_bytes = 262144
max_turn_bytes = 8388608
max_calls = 64
```

Limits are clamped to 4 KiB–1 MiB per call, 4 KiB–16 MiB per turn and
1–256 calls. Concurrent child calls share the turn budget. Request and response
share each call's reservation. A quota or size failure saves an omission reason,
never a fabricated empty response. Recording failures that prevent durable
request admission prevent that physical request from being sent.

The explicit turn choice follows delegated/checking/review turns and frozen
recipes. Managed conversation work stores its turn options and uses them again
when resumed, including an explicit false. `config` continues to use the current
configuration. A new ordinary turn or background queued follow-up uses its own
options/configuration; enabling one turn does not silently enable the session.
No persistent user configuration is changed by the turn flag.

Inspect metadata first:

```text
rook session model-calls SESSION
rook session model-calls SESSION --from 0 --through 140 --purpose main
rook session model-calls SESSION --format records --output calls.json
rook session model-calls SESSION --format dataset --output pairs.json
```

The reader freezes the saved prefix once, or uses an explicit inclusive range.
Use the returned `through` to repeat a selection while the session continues.
Calls crossing a selected boundary are incomplete in that selection. Nothing
outside it is fetched to complete them. Narrow the range if the reader refuses
more than 2,048 events, 128 attempts, 16 KiB event metadata, 8 MiB selected
source bodies, or 4 MiB output. Purpose filtering selects results after bounded
source admission. CLI and daemon use the same reader; the daemon accepts a
bounded POST to `/api/sessions/SESSION/model-calls` with the same request fields.
Publication happens on the CLI machine and never replaces an existing file.
An empty selection or dataset with no eligible pairs creates no file.

`preview` exposes identities, range, coverage and exclusion reasons without
prompt/reply text. `records` includes the admitted visible payloads, including
failed or partial attempts. Known-secret redaction is applied when recording,
including JSON keys. It is not complete anonymization: review exported content
before sharing it. Ordinary transcript and HTML exports omit these payloads.

`dataset` emits **one physical call per example** in
`rook-canonical-visible-pair-v1` format. Each example retains the exact captured
system/messages/tool schemas, request settings, physical dispatch, event/object
provenance, and its own response. Only `response` is a target; historical
assistant messages are prompt context. No histories are joined or deduplicated.
This is a Rook review format, not a ready-made trainer or raw HTTP wire format.
Provider credentials and headers are outside the representation. ACP calls are
outside this provider capture.

Eligible pairs need a confirmed completed physical attempt, both snapshots,
supported content and complete visible context. Redacted content, omitted
images, signed provider state, hidden reasoning, incomplete/max-token replies,
invalid tool arguments, missing tool results and unadvertised response tools
are excluded with reasons. Retries and failover retain distinct attempts.
`review_required: true` and `quality: "unlabelled"` remain on every example:
completion is evidence of completion, not evidence that an answer is correct.
Compaction and later tool/model changes do not rewrite the saved snapshots.
