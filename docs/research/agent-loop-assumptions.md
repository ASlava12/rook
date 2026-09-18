# Agent-loop assumptions checked against the implementation

Reviewed on 2026-09-18. This replaces an uncommitted review draft whose line
numbers and several conclusions no longer matched the code. Fix status and
validation are in [audit remediation](audit-remediation.md) and
[report 05 validation](report05-validation.md).

## Tool calls must remain identifiable

The loop suppresses duplicate call ids within an assistant message rather than
executing both and building an ambiguous reply. Its result tells the model that
another call with the same id was not made. Replay reconstructs call/result pairs
from event order and supplies a missing-result message for an interrupted call.
An unreadable stored body now produces a visible gap in the request.

The OpenAI streaming ToolCallBuffer supplies process-unique ids for missing ids,
refuses genuinely nameless calls, and ignores empty name/id updates that would
erase earlier fragments. It limits both the index and retained bytes. These
are guarantees of this buffer, not of every custom Provider implementation.

Evidence: rook-llm/tests/stream_limits.rs, and the duplicate-id and unreadable
history regressions in rook-core/tests/agent_loop.rs.

## Invalid input must reach an explicit refusal

Malformed JSON arguments become null and are rejected before approval. A native
call for an unknown tool receives the toolbox's unknown-tool response. The
prompt-encoded path only adopts names from advertised tools: JSON embedded in
ordinary answer text must not become an arbitrary call. These paths differ
intentionally. Reasoning blocks and tool results preserve provider-specific
message shapes. Request construction folds consecutive user messages but does
not fold separate tool results into one user message.

Evidence: argument and message-order tests in rook-core/tests/agent_loop.rs,
rook-llm/src/types.rs, and the dialect tests in rook-llm/tests.

## End of a model message is not proof of task completion

The loop examines actual tool calls before accepting the provider's stop reason.
A nonempty proposed final answer with EndTurn is classified by a separate
request with no tools unless this is already a checker. A promise to continue
gets at most two nudges without intervening tool work, then stops as incomplete.
An unavailable or invalid verdict produces completion_unchecked, or the
applicable time/spend limit. The classifier has a 60-second bound, shortened by
the remaining turn deadline, and its reported usage is charged to the turn.

This checks whether the reply is a final answer, not whether its claims are true.
A requested plan, explicit blocker or question can legitimately end a turn.
Goal verification remains separate. Continuation preserves the original policy;
new user instructions arriving during classification are delivered to the loop.

ACP's max_tokens and refusal mapping uses the engine's snake_case spelling.
CLI reports unfinished outcomes separately from a successful end_turn.

Evidence: completion tests in rook-core/src/completion.rs,
rook-core/tests/agent_loop.rs, rook-cli/tests/run_json.rs, and rook-acp/src/lib.rs.

## Context estimates and reported spending are different

The loop accepts reported input usage as a context anchor only when it is at
least the measured message text. Context estimates, compaction and output-room
limits help fit requests into the provider's window; they are not a replacement
invoice for missing usage. Turn input/output counters add the provider's usage.

Assembler::finish can infer a stop reason when no Done delta arrived and then
returns default usage. The earlier draft incorrectly claimed this necessarily
made turn-spend accounting use estimates. A provider that omits usage can still
understate spending; that limitation is not repaired by the completion classifier.

Byte and block limits bound streamed replies, including tool arguments and
reasoning. They do not prove that a provider's termination marker or token usage
is accurate. Provider wire-stream limits and assembled-content limits protect
different stages of accumulation.

Evidence: rook-llm/src/stream.rs, the three provider streaming implementations,
and usage accumulation and context-anchor logic in rook-core/src/agent.rs.
