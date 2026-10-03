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
