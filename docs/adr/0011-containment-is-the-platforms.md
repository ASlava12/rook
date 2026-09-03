# ADR-0011: Containment is the platform's, best-effort, and reported

## Status

Accepted.

## Context

Until now the boundary around a command was text: the file tools checked
paths against the workspace, and pattern rules over the command line
([ADR-0009](0009-ask-before-acting.md)) decided what to ask about and what to
refuse. Neither reaches a command's children. `cargo test` runs whatever the
crate's build script says; `npm install` runs whatever the package's
`postinstall` says; and a `rm -rf` spelled in a way no pattern anticipated is
a `rm -rf`. The roadmap called real containment a serious, platform-specific
piece of work, and it is.

The reference agents divide three ways. Codex contains commands with Seatbelt
on macOS and with a helper binary applying Landlock and seccomp on Linux, with
bubblewrap as an alternative, and defaults the network to off. OpenHands runs
in a container and calls that the sandbox. Everything else — cline, goose,
opencode — asks for approval and calls that enough.

## Decision

A command runs under the operating system's own containment where there is
some, in one shape on every platform that has it: the workspace and the
temporary directory writable, everything else readable and nothing else
writable, and the network a switch. What was applied is recorded on the result
of every command.

- **macOS: Seatbelt**, through `sandbox-exec` with a generated profile.
  Deprecated by Apple for a decade, present in every release, and what Codex
  and Claude Code use.
- **Linux: Landlock**, unprivileged, applied in the child between fork and
  exec. The ruleset is built in the parent, where allocating is fine; what
  runs after the fork is the syscall that applies it. No helper binary and no
  bubblewrap: a second executable to ship, find and trust is a dependency the
  kernel's own mechanism does not need.
- **Windows and FreeBSD: nothing yet.** Capsicum's capability mode breaks a
  shell and jails need root; on Windows a restricted token or an
  AppContainer is the shape, and neither is a day's work.

`[sandbox] isolate` is `auto` by default — contain where possible, run as-is
and say so where not — and `required` refuses to run a command without
containment. `[sandbox] network` is **on** by default.

## Consequences

- A runaway command cannot write outside the workspace and the temporary
  directory, whatever it is called and whatever it starts. That is the whole
  of the protection, and it is real on two of the four platforms. A build's
  caches — `~/.cargo`, `~/.npm` — are outside it, so the first build under
  containment fails until `[sandbox] writable` names them: each is a hole,
  and the person whose caches they are is the one to cut it. A failed
  contained command says it was contained and names the setting.
- The network being open by default is a deliberate weakening. A build that
  fetches its dependencies is most builds; Codex's off-by-default costs a
  round of approval prompts per build, and a sandbox that breaks every build
  is the one that gets turned off. The switch is one line of config, and a
  contained command with the network off is refused at the socket.
- Landlock's limits are the kernel's: before 6.7 it cannot restrain the
  network, and it never restrains UDP, so a command can always resolve a
  name. The result says which of these held. A container would close that
  and is a different product.
- Reporting is the point. `auto` on a platform with nothing to contain a
  command is the same as `off` in effect, and the difference is that the
  result says so. A sandbox that quietly did nothing is worse than none,
  because it is believed.
- The language-server installer and hooks run uncontained: the first writes
  outside the workspace and reaches the network by definition, after the
  person said to; the second is the person's own command. A command run in an
  editor's terminal is the editor's.
- The file tools' path check and the pattern rules stay. They are cheaper,
  they cover the platforms this does not, and a deny rule is still final.
