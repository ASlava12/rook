# Confirmed model responses before effects

The agent checks the stream assembler's completion evidence before admitting
tool calls or treating an answer as the end of a task. A clean HTTP EOF is not
terminal evidence. Complete-looking JSON arguments can arrive before a lost
finish frame; they do not authorize a file write or another effect.

When a stream is unconfirmed, native tool calls are discarded and textual or
reasoning calls are not promoted. Provider-owned signed assistant state is
withheld from canonical replay. Partial visible text and reported usage are
retained with an incomplete route receipt; physical model-attempt accounting
keeps its existing incomplete status. No calls from that response execute.
Earlier confirmed steps remain in the session history.

Managed pause/cancel controls are checked before reporting the incomplete
generation. Otherwise the turn returns an error describing the missing
confirmation, rather than silently reporting a successful `end_turn` or asking
the completion classifier to authorize an unconfirmed answer. Existing managed
retry admission and session controls continue to apply.

The limit-triggered final-answer path also refuses unconfirmed output before
the explicit output artifact can be written. Its partial text and reported
usage remain saved; effects from earlier confirmed steps are retained.
Compaction records its reported usage before refusing an unconfirmed summary
and uses the existing explicit source-log fallback instead of marking partial
model text as a successful summary. The underlying history is not deleted.

This uses existing stream evidence. A recognized finish reason without a later
`[DONE]` sentinel remains sufficient where the adapter already accepts it.
Legacy/custom streams retain their explicit `Done` contract; a stream with no
`Done` is unconfirmed. Nonstream native replies without a finish reason already
use `StopReason::Other`, so completion classification and output repair continue
to refuse those decisions. No new configuration or stored format is introduced.

The terminal-before-done pattern was reviewed in pinned Pi
`482c1381a00d8d00bc81697e4a980c593db6803e`,
`packages/ai/src/api/openai-completions.ts`: its default compatibility path errors
when the stream ends without a finish reason, before publishing a done event.
Rook independently gates effects using its existing cross-provider completion
metadata, preserving partial text and usage. No upstream code or reference pin
is changed.

Native HTTP regressions cover complete tool JSON without terminal evidence,
partial text, a non-SSE success body, textual/reasoning call promotion, and a
confirmed finish without `[DONE]`. They also retain the existing refusal of
unconfirmed classifier verdicts and output repairs. The failing baseline wrote
an owned fixture file and returned `end_turn`; the corrected turn preserves its
reported usage while leaving that file absent. These checks establish protocol
admission, not live-model task quality.

The [owned daemon probe](../xtask/probes/model-terminal/README.md) repeats the
tool, partial-text and forced-final-answer failures through ordinary chat sockets
using an extracted package. Its twelve checks also verify stored incomplete
receipts and bind the result to the daemon's SHA256. Native HTTP tests separately
cover compaction fallback; run these when the admission boundary changes.
