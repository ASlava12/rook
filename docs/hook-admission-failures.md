# Failed prompt hooks refuse admission

A configured `prompt` hook can reject a turn before its prompt is admitted to
the conversation or sent to a model. If that hook cannot run, exits unsuccessfully,
times out, or returns an invalid enabled UI protocol reply, admission is refused
as well. The error identifies the failed hook operation. A failed check cannot
stand in for an approval.

The submission remains pending and no user-message event is committed. After
fixing the hook, the caller can retry the submission through the normal admission
path. Existing `pre_tool` hooks also refuse the operation on hook failure.
Observer and context callbacks retain their best-effort behavior.

For ordinary command hooks, timeout errors report whether stdin delivery, stdout
draining, stderr draining and process waiting completed. These flags describe
the observed wait state; they do not establish why a child or pipe failed to
finish. The command deadline and bounded output limits remain in force.

When an ordinary hook process has exited, Rook retires its remaining stdin
delivery instead of waiting for a pipe writer that can no longer be consumed.
Both output streams are still drained and the actual process exit determines
the hook result. A live child still has the configured watchdog. Interactive
hook frame writes also stop if their child exits; after the final flushed reply,
closing stdin delivers EOF without an additional asynchronous shutdown wait.

This addresses an observed FreeBSD failure where process waiting and both output
streams had finished but stdin delivery remained pending until the watchdog.
Failed exits still refuse prompt and tool gates; cancelling an unusable input
writer does not turn a failed hook into an approval.

