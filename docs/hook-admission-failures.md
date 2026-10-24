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

This closes an observed failure path in which a prompt hook's timeout was logged
but the prompt was subsequently sent to the model. It does not establish or fix
the cause of that timeout.
