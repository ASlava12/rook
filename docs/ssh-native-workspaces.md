# SSH-native workspaces

Opt in to direct remote file authority with a trusted SSH profile:

```toml
[execution_environment]
selected = "lab"
native_workspace = true

[execution_environment.profiles.lab]
kind = "ssh"
host = "user@trusted-host"
program = "ssh"
remote_workspace = "/srv/project"
```

`native_workspace` defaults to false. Existing SSH replica execution retains its
copy/synchronize behavior. In native mode the local workspace holds Rook session
bookkeeping. No replica is created: `ssh_workspace` reads, lists, searches,
edits, writes and runs foreground commands at the configured remote root.
Ordinary local file/command tools and local language servers are withheld.
Project instructions on the remote host must be read explicitly. The context
identifies local probe results separately from the remote environment.

SSH uses existing trusted host keys and batch authentication, with no automatic
host acceptance, agent forwarding or credential prompt. A Unix client and remote
Python 3 are required; foreground execution additionally requires Linux, Python
3.9+ and pidfd support. Editor-owned filesystem/terminal bridges and standalone
`rook mcp serve` cannot provide this session-bound authority and refuse native
mode. CLI, daemon/API, TUI and browser turns share the same engine/tool.

Paths are relative to the configured root. Reads refuse symlink traversal and
nonregular files. Results include the configured host, root, profile, workspace
identity and content hash. Search is literal. Reads support hash-bound byte
continuations; directory listing supports name continuations. Transfers and
scans use the existing execution environment file, total, entry and output
limits, applied before retaining data. Text operations require UTF-8.

Every edit/write/run has a session-owned receipt. Preimages stay in a private
checkpoint directory on the host; local receipts contain only identity, paths,
hashes and phase. Preparation is observed and saved before admission. Unknown
outcomes block new mutations; no original command is automatically replayed.
Inspect or undo through the tool, or without a model:

```sh
rook session environment SESSION '{"action":"native_inspect"}'
rook session environment SESSION '{"action":"native_inspect","id":"RECEIPT"}'
rook session environment SESSION '{"action":"native_undo","id":"RECEIPT"}'
```

The TUI accepts the same JSON after `/environment`. In the browser session's
Execution environment panel, list owned checkpoints, inspect one and choose
Undo inspected checkpoint. Merely opening or inspecting the panel changes no
files and makes no model call.

Undo checks the recorded after source before restoring. It refuses another
session's receipt, a changed profile/root or external changes. Recovery itself
has a durable child operation session. Ordinary session rewind refuses while
remote changes remain applied or uncertain; undo those owned receipts first.

A foreground command runs with the SSH account's permissions, not inside an OS
sandbox. Descendant cleanup is restricted to its owned process tree; loss of the
SSH channel stops it and retains an inspectable outcome. Command snapshots are
bounded and omit configured `exclude` names (such as `.git`, `target` and
`node_modules`). They cover the selected remote root only, not writes elsewhere
on the host or excluded data. Unsupported objects or a capture limit prevent a
claim of complete capture. Inspection reports quiescence separately from exit
status and captures. A nonzero exit retains its file effects and undo receipt.
Multi-file restoration is sequential: an interruption or filesystem failure can
leave a partial undo requiring inspection/manual recovery. It is not a
transactional filesystem rollback.

Named secrets must already exist in Rook's private secret store; missing values
refuse execution before admission. Values pass on stdin into an ephemeral remote
environment, never SSH arguments. Output is redacted before remote receipt
persistence and again before local reporting. No secret is requested repeatedly
when the user declines or leaves it unanswered.

Verification uses deterministic CLI/daemon provider fixtures and a trusted Linux
host with uniquely owned temporary workspaces. It covers same-named local file
isolation, source limits/symlink escape, receipt ownership, changed-source undo,
read-only checks, exit failures, secret output and actual SSH disconnect cleanup.
