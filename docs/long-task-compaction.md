# Long-task compaction regression

Compaction replaces an older transcript span with a saved summary while retaining
the recent tail. The summary is quoted session data. The original events and
tool results remain in the journal; compaction does not certify current files,
answer a pending question, or grant approval for an approach the user rejected.

The working-summary instructions preserve actual user requests, unchecked work,
failed analysis and decisions against an approach. Continuing a task depends on
those distinctions: an accepted correction supersedes the earlier plan, an
unanswered question remains open, and a rejected approach keeps its reason.

## Four observed stages

The `provider_history` integration fixture
`long_task_compaction_preserves_accepted_correction_unanswered_question_and_rejected_approach`
uses an owned HTTP Responses endpoint and records:

1. The actual request before compaction, including the accepted correction,
   pending deployment question, rejected deletion approach and original signed
   provider batch with completed tool results.
2. The actual summary request, including those facts and read results, with
   neither opaque provider state nor executable tools.
3. Replacement replay before another turn, together with the durable summary
   and source boundary. The question must fall inside the compacted span. Replay
   is compared after closing and reopening the store.
4. The first subsequent request, carrying the saved distinctions once, without
   the discarded signed batch or orphaned tool outputs.

The fixture reaches the configured compaction threshold and retains one original
question in the journal. It then saves a new signed provider response, reopens
again and verifies that the new state is replayed while the discarded old state
stays absent. Human-readable transcripts expose neither opaque value.

The endpoint supplies a controlled summary after its actual input is checked.
This proves the engine's transport, replacement, source-boundary and persistence
contracts. It does not establish how accurately a particular model summarizes
real multi-day work; that requires inspecting a separate real-model run.

## A retained tail is not a new summary span

If the complete remaining tail is retained, compaction has no new source span
to replace. This remains true when a previous summary exists. Rook refuses that
attempt before asking the summarizer and leaves both the previous summary and
its source boundary unchanged. No additional compaction record is saved.

Previously, an existing summary bypassed the insufficient-history guard even
with an empty new span. A second request spent tokens without advancing the
boundary; a provider failure could also replace the carried summary with a
fallback describing the failed request. The actual AgentLoop regression first
creates a valid summary, then checks repeated attempts with the retained tail
and with one additional short message. It verifies one physical summary request,
one saved compaction, and the unchanged previous summary/boundary. Existing
coverage checks that later history can still be compacted and carries the
previous summary into the new request.

Pi's `prepareCompaction` at pinned revision
`482c1381a00d8d00bc81697e4a980c593db6803e` provided the audit prompt: it declines
preparations without messages to summarize. The Rook guard is independent and
retains its existing failed-attempt observations; it adds no new public event
type or context-window policy.
