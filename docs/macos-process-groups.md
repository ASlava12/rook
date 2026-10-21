# macOS process groups after cancellation

Darwin sends a group signal to a snapshot of member PIDs. A shell forking while
that snapshot is used can leave a new descendant outside that one signal's
recipients. Cancellation must keep ownership long enough to stop that descendant
as well.

On macOS, `rook-contain::Group::end` repeats the stop while live members remain,
with a five-second cleanup bound. The synchronous ownership boundary also covers
dropping a cancelled async call. Its boolean still reports that a signal was
sent, not proof of termination; callers require a verified terminal observation
before certifying execution effects.

A terminated process can also remain as a zombie until its parent reaps it.
Darwin's group signal-zero probe can return EPERM for an all-zombie group, so
that error alone cannot distinguish terminal members from refused visibility.

On macOS, `rook-contain::Group::alive` combines the existence check with a bounded
snapshot of group members and their native BSD process states. A live descendant
still counts when its leader has exited. A group containing only confirmed
zombies or departed members is terminal. Refused, incomplete or oversized
inspections remain unknown; the snapshot admits at most 1023 members before
requiring conservative recovery. This does not acknowledge unknown execution
effects or restore files automatically.

The native regressions retain an actual unreaped child and check terminal native
state despite the signal-probe ambiguity. A second case keeps a real descendant
alive after its leader becomes a zombie, then ends the group and checks terminal
state. A 32-round cancellation case deliberately keeps its ready cue before the
shell forks `sleep`, then requires no running descendant or late file effect.
Owned subprocesses are serialized and cleaned up.
The existing cancellation/shell-undo scenario continues to require the whole
group to be terminal before inspected recovery and selected restoration.

The macOS CI job runs these lifecycle regressions before the full workspace gate.
Cross-compiling both Apple architectures checks API types and target branches;
it does not establish native behavior on a Windows host.

The liveness distinction was also reviewed against OpenClaw's pinned
`e1e4a0dffa2bdb930f8fa2ea65e425ba0fd9c354`, `src/shared/pid-alive.ts`.
Its individual-owner identity checks are independently adapted to Rook's owned
process groups; no upstream implementation or dependency is copied. Native query
semantics were checked against Apple's
[BSD process inspection implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c)
and [libproc wrappers](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.c).
Group-signal snapshot behavior is visible in
[signal delivery](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c)
and [process-group iteration](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_proc.c).
