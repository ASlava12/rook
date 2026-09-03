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
- **Windows: a low integrity level.** Windows has no fork, so nothing runs
  between the parent and the command; what it has is integrity levels, and a
  process may lower its own and never raise it. So the command is started
  through a launcher — the same `rook` binary, told by an environment variable
  to lower itself and run `cmd /C` — and inherits the level and the pipes. A
  process at low integrity reads what any process reads and writes only what
  is labelled low, so the parent labels the workspace low first, and a scratch
  directory of rook's own under the temporary directory — not the temporary
  directory itself, which the label would walk and mark for every program that
  uses it. A persistent mark on the directory, and only that. Not
  an AppContainer: one reads nothing of the user's profile by default, so the
  toolchain under `~/.cargo` would not run without rewriting the profile's
  ACLs. Not Codex's design of dedicated sandbox users, DPAPI secrets and
  firewall rules, which needs an administrator to set up. An integrity level
  says nothing about the network, and the result says so.
- **FreeBSD: nothing yet.** Capsicum's capability mode breaks a shell and
  jails need root.

`[sandbox] isolate` is `auto` by default — contain where possible, run as-is
and say so where not — and `required` refuses to run a command without
containment. `[sandbox] network` is **on** by default.

## Consequences

- A runaway command cannot write outside the workspace and the temporary
  directory, whatever it is called and whatever it starts. That is the whole
  of the protection, and it is real on three of the four platforms. A build's
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
- Reading is everywhere a build might need, which is nearly everywhere. The
  exception is the agent's own state directory: it holds every project's
  transcripts, every checkpoint's contents and everything it was told to
  remember, and a command run for one project has no business reading
  another's — with the network open, reading is the whole of what an
  exfiltration needs. Seatbelt denies it outright, which is a rule after the
  blanket read because the last match wins. Landlock grants and never denies,
  so "everything except this" is spelled as everything else: walk down the
  excluded path and name every sibling at each level. A path that cannot be
  read is left ungranted, which errs towards refusing a read rather than
  allowing one. A Windows integrity level is about writing and cannot do this
  at all, and the result of every command says which of the two it got.
- Reporting is the point. `auto` on a platform with nothing to contain a
  command is the same as `off` in effect, and the difference is that the
  result says so. A sandbox that quietly did nothing is worse than none,
  because it is believed.
- The language-server installer and hooks run uncontained: the first writes
  outside the workspace and reaches the network by definition, after the
  person said to; the second is the person's own command, as are the MCP
  servers `[[mcp]]` names and the language servers that answer `rook lsp`. A
  command run in an editor's terminal is the editor's. What is contained is
  what the model asked to run.
- The file tools' path check and the pattern rules stay. They are cheaper,
  they cover the platforms this does not, and a deny rule is still final.
