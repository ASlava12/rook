# macOS process groups after cancellation

A terminated process can remain as a zombie until its parent reaps it. Signal
zero can still find that process group even though no member can perform more
writes. Treating that observation as running can delay command cleanup and
inspected shell-undo recovery.

On macOS, `rook-contain::Group::alive` combines the existence check with a bounded
snapshot of group members and their native BSD process states. A live descendant
still counts when its leader has exited. A group containing only confirmed
zombies or departed members is terminal. Refused, incomplete or oversized
inspections remain unknown; the snapshot admits at most 1023 members before
requiring conservative recovery. This does not acknowledge unknown execution
effects or restore files automatically.

The native regressions retain an actual unreaped child, assert that signal zero
still sees its group, and check that it is no longer running. A second case keeps
a real descendant alive after its leader becomes a zombie, then ends the group
and checks terminal state. Owned subprocesses are serialized and cleaned up.
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
