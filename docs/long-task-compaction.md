# Long-task compaction regression

## Registered-secret masking during compaction

Admitted summary-source bodies and labels are scrubbed using the turn's known
secret registry before summary excerpt preparation. Previously committed
prose and file-request metadata are scrubbed again, so registration after an old
record was saved does not copy its known values into a new compaction record.
The same bounded masking applies to newly generated summary prose and summary
failure reasons before their excerpts are saved. Standard JSON/debug-escaped
forms use the existing vault matcher.

Masking expansion must fit admission: source bodies allow 16,000 bytes after
the existing 8,000-byte transcript-body read, labels 1,024 bytes, prior prose
1 MiB, file paths 512 bytes with the existing ledger entry-byte cap. A source
that cannot fit is explicitly omitted. A generated summary whose redacted form
exceeds `agent.max_compaction_summary_bytes` uses the existing source fallback;
no oversized or unmasked replacement is committed.

This covers registered complete values present in admitted text. It neither
discovers every credential shape nor reconstructs a value already split by an
earlier storage/read truncation. Original saved records and ordinary non-summary
history replay are not rewritten by this feature. The retained recent tail is
also outside the span being compacted. Actual-loop checks admit a legacy
secret-bearing source, inspect the physical summarizer input, fail one summary,
echo a known value in the next, and check saved prose/file metadata plus the
unchanged original record. A separate control crosses only the redaction-
expansion ceiling.

Compaction replaces an older transcript span with a saved summary while retaining
the recent tail. The summary is quoted session data. The original events and
tool results remain in the journal; compaction does not certify current files,
answer a pending question, or grant approval for an approach the user rejected.

The working-summary instructions preserve actual user requests, unchecked work,
failed analysis and decisions against an approach. Continuing a task depends on
those distinctions: an accepted correction supersedes the earlier plan, an
unanswered question remains open, and a rejected approach keeps its reason.

## Bounded file requests and recovery after a summary failure

Compaction now saves a separate `file_activity` JSON companion in the existing
compaction event. It keeps the latest request for each native file tool/path
pair, with the original event number. It tracks `read_file`, `write_file`,
`edit_file` (including canonical `files` batches), `delete_file`, and both paths
of `move_file`. This projection reads the canonical `path`, `files`, `from`, and
`to` argument fields; permissive input aliases are not a complete file inventory.
Replay appends this small ledger to the quoted summary; subsequent compactions
carry the structured companion independently of the summarizer's prose. A model
that omits every filename therefore does not erase the admitted file references.
Existing compaction records without this field remain readable.

The ledger describes **requests**, including failed or refused calls. It is not
a list of verified modifications, an inventory of shell/MCP/delegated work, or
current file contents. File paths are JSON-quoted source data. It never reads a
file, executes a recorded call, or changes approval policy. Use saved tool
results and session diffs to inspect outcomes.

Admission keeps at most 32 entries, paths of at most 512 UTF-8 bytes and encoded
entry bytes of at most one eighth of `agent.max_compaction_summary_bytes`, capped
at 4 KiB. Newer requests evict older entries before insertion; `omitted` reports
evictions, oversized paths and truncated/malformed argument bodies. Such a body is
omitted, rather than guessing paths from partial JSON. Compaction's existing
8,000-byte transcript-body admission still applies. Redaction has already run
before tool arguments enter the journal. Smaller configured summary bounds also
apply when reloading a previously saved ledger.

Summary/checker input shortens individual tool results to at most 512 estimated
tokens (less for a small input budget), retaining the beginning and end with an
explicit omission marker. JSON escaping and separators count toward the whole
rendered span's limit, including a single oversized entry. The original result
and event number remain in the journal. This prevents one compiler dump from
displacing all the surrounding decisions; it does not prune the saved result.

If a new summary fails, the fallback now retains a bounded beginning and end of
the previously committed prose summary, labels it as earlier history only, and
explains that the new span remains unsummarized in `rook session show`. Retained
prose uses an estimated-token budget of `min(usable_tokens / 4,
max_compaction_summary_bytes / 8)`, with the byte setting clamped to its existing
valid range. Under the existing four-byte token estimate, this retains at most
half of the configured byte admission before fixed wrappers. Repeated failures
cannot grow this context without bound. This is
best-effort continuity, not a claim that omitted history has been summarized.
The file ledger remains independently available during these failures.

The November 11 regression executes native file tools, including a failed read,
compacts with a summary that omits their paths, then fails the next summary and
checks ordinary next-turn replay. Additional tests exercise encoded-byte/count
limits, Unicode, pathological JSON quoting and repeated fallback size.

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
