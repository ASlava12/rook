# Execution environments

`run_command` can use a trusted local, container or SSH profile. Selecting a
remote profile is optional; the default is local. Both remote transports require
a Unix client. Containers require a reachable Docker engine and an already
installed Linux image with
`sh`, `setsid`, `ps` and `awk`. It never pulls images automatically.

```toml
[execution_environment]
selected = "box"
max_resources = 4
setup_timeout_secs = 30

[execution_environment.profiles.box]
kind = "container"
image = "alpine:3"
memory_mb = 512
pids = 256
```

Profiles come from configuration, not tool arguments. `local` is reserved for
local execution. There may be at most 16 profiles/resources; setup is bounded
by 1–300 seconds. The ordinary policy and execution journal see the original
command before the transport is prepared. Selecting an environment does not
approve that command. Editor-owned files or terminals refuse this transport
rather than changing their authority.

The Docker server must see the actual client workspace. Setup writes a unique
short-lived probe in that workspace and verifies it through the bind before
running user code. The workspace is mounted at `/workspace`; commands use
relative paths or that container path. File tools and the `cwd` argument keep
local workspace paths. Cwd must resolve inside the bound workspace. Commas or
control characters in Docker bind paths are refused. Agent state must remain
outside the bind, and a workspace overlapping an unreadable state path is refused.
There is no automatic copy or synchronization to an unrelated Docker server.

A new container uses the image's resolved immutable ID, a read-only root,
a bounded 64 MiB temporary filesystem, one CPU, the configured memory/PID
limits, dropped capabilities and no-new-privileges. It runs with the client's
effective UID/GID. `sandbox.network=false` selects `--network none`. Only the
workspace bind is shared; other configured host scratch paths are not mounted.
This is command execution, not a promise of fully reproducible dependencies,
hardware, or protection against a deliberately escaping privileged workload.

An engine-shared pool retains one immutable container ID per configured
profile/workspace/network/state identity across turns and delegated workspace
engines. A resource admits one command at a time; a background job retains its
lease until completion or stop. Capacity refuses additional resources rather
than evicting a live command. Idle released entries may be discarded. The cap
is per process; orphaned resources from lost processes require inspection.

The proxy owns a separate remote process group. On normal exit it terminates
remaining descendants and checks for live group members, excluding zombies,
before publishing a per-call terminal record. The driver checks that record
against the client's exit status before returning the resource to the pool.
Timeout/cancellation stops the entire immutable owned container. Failure to
confirm remote cleanup is retained as uncertainty; killing the local Docker
client alone cannot certify remote job quiescence or finish shell preimages.
A command that deliberately creates another process session is outside the
normal proxy-group scope; cancelling the resource still stops its container.

Named secrets remain foreground-only. The Docker client inherits resolved
values and passes only their environment-variable names in its argument list.
Values are masked from returned text and ordinary output spill; they do not
become profile state, proxy arguments or the next command's environment.
Container commands otherwise inherit the image environment, not arbitrary
host variables. Tool-result display companions retain the profile, transport
kind and immutable resource ID alongside ordinary command status.

## Inspection and recovery

Use `rook session environment <session>` or `/environment` in chat/TUI. The
browser session sidebar has **Execution environment**. HTTP uses the same
engine through `POST /api/sessions/<session>/environment`, limited to 8 KiB.
The session must exist and belong to the current workspace.

```json
{"action":"inspect"}
{"action":"prepare_release"}
{"action":"release","token":"<reviewed token>","note":"What was checked"}
```

Inspection reports bounded retained state, actual immutable ID/ownership label,
Docker running state and whether another process still owns the resource.
A disconnected Docker daemon is not interpreted as a missing container.
An active command/job must be stopped through its frontend first. A different
live owner cannot be released by a new pool. A stale process ID that has been
reused is conservatively treated as live; no automatic takeover is attempted.

Prepare release returns a token bound to those exact observations and saved
bytes. Release checks them again, then removes only the observed immutable ID
with the expected ownership label. It never runs the original command. A
changed observation or replayed release token is refused. Core entrypoints
retain the release in a linked child session and ordinary execution journal.
Errors identify that child so its unknown effects can be inspected.

State files are bounded 16 KiB JSON under `output/execution-environments`,
written through the no-follow/conflict-checking filesystem layer with mode 0600.
Interruption before an immutable ID was returned retains the initial owned name;
explicit inspection resolves its ID and checks its label before release.
Reopening the process refuses retained unreleased state. After confirmed release,
a later explicit command may create a fresh resource; nothing is replayed.
Changing profile parameters or workspace/state roots changes the identity; retain
the previous trusted configuration to inspect an older resource.

Normal frontend/pool shutdown removes its known immutable resources. Process
loss bypasses destructors, so actual orphan inspection/release is tested in a
separate subprocess. Filesystem replacement checks are not a distributed lock
against independently changing files after observation. Neither release nor
resource state replaces unknown-effect acknowledgement in the command's original
execution session.

## SSH workspace execution

SSH uses a configured alias and its existing authentication. Server keys must
already be verified: the driver forces `BatchMode=yes` and
`StrictHostKeyChecking=yes`. It never asks to accept a key or falls back to
local execution. The remote host needs Linux, `/bin/sh` and Python 3.9 or newer
with `pidfd_open` and `pidfd_send_signal` support. No remote packages are installed.
The client uses a private control socket, independent of the user's SSH masters.

```toml
[execution_environment]
selected = "lan"
file_bytes = 1048576
total_bytes = 16777216
max_files = 1024
max_entries = 4096
max_calls = 8
exclude = [".git", "target", "node_modules", ".venv"]

[execution_environment.profiles.lan]
kind = "ssh"
host = "my-verified-ssh-alias"
remote_workspace = "/home/me/rook-workspaces"
```

Each profile/workspace/state/transfer-policy identity owns a separate random
replica under `<remote_workspace>/.rook-exec/<identity>/<owner>/workspace`.
Commands use relative POSIX paths; file tools and `cwd` keep local paths. Setup
transfers a bounded before image and refuses changed remote replicas. Completion
retains the after image before merging changed files, modes and directories back
to the client. Later local edits to affected paths cause a conflict; unrelated
local edits survive. File/directory replacements are supported. Directory modes
are not mirrored; new remote directories are private. Excluded names apply at
any depth. Symlinks, special files, non-UTF-8 paths and reached bounds refuse
transfer. The default exclusions omit Git metadata and dependency/build trees;
this replica does not promise the local host's dependencies or Git environment.

Each file is limited to 1 KiB–8 MiB and all file contents to 1 KiB–64 MiB.
The configured file/entry caps are 1–8192 and 1–32768, with entries covering
files. `max_calls` retains 1–32 before/after image pairs per resource identity;
only older resolved calls are pruned before another call. Changing the policy
changes the identity. Retained sources occupy bounded JSON files under
`output/execution-environments/ssh`, outside the workspace, with mode 0600.
Commands and resolved secret values travel only in the bounded stdin frame,
not SSH arguments. Secrets remain foreground-only and are redacted from normal
command output; a command that writes a secret into a workspace file still
writes that file. Remote commands otherwise inherit the remote SSH environment.

SSH cannot enforce mandatory remote OS containment or disabled network access;
those configurations refuse execution. The authenticated account controls the
remote host, and commands can act outside the replica with that account's rights.
Resource cleanup and writeback cover the owned replica and command descendants,
not arbitrary external effects. Ordinary approvals and unknown-effect accounting
still apply. SSH output capture and background jobs use the ordinary bounded
command paths.

The worker becomes a Linux child subreaper and uses birth-checked PID handles to
stop adopted descendants, including processes that create a new session. A normal
exit needs a matching terminal/stop certificate before writeback and reuse.
Cancellation stops the remote worker and retains partial remote changes for
review; a lost connection cannot certify cleanup. No uncertain command is replayed.

SSH management adds these requests to the shared CLI/chat/TUI/HTTP interface:

```json
{"action":"prepare_stop"}
{"action":"stop","token":"<reviewed token>","note":"What was checked"}
{"action":"prepare_recover"}
{"action":"changes","call":"<call>","after":null}
{"action":"read","call":"<call>","which":"after","path":"file.txt","offset":0}
{"action":"recover","token":"<reviewed token>","note":"Selected observed changes"}
```

The browser provides the same preview, source and confirmation controls. Changed
paths are paged in groups of 16; retained file reads return at most 4096 bytes in
Base64 with the next offset. These source reads work while SSH is unavailable.
Inspection reports connection errors separately from authenticated missing roots.
Stop, recover and release require a token bound to fresh observations and a note;
core entrypoints record each effect in a linked child and ordinary journal.
A different attached live owner must be stopped through its own frontend.

After process loss, inspect and explicitly stop any remaining worker, acknowledge
unknown effects in the original execution session, then prepare recovery and
review both file images before confirming. Recovery only publishes the reviewed
observed bytes; already-published paths are accepted after a partial writeback.
A changed remote source invalidates the preview. An interrupted upload that never
started a command cannot be recovered as file deletions: release its partial
replica instead. Release removes only the owned remote root and preserves local
source history. Idle normal shutdown releases its replica; interrupted state is
retained. Reopening refuses an unreleased resource until explicit management.

## Verification scope

Native checks use an owned Docker OpenSSH server with fixture-generated trusted
keys. They exercise actual transfer, commands, background jobs, secrets, trusted
key rejection, count/byte limits, conflicts, cancellation, partial writeback and
process-loss recovery. Engine checks verify original commands, shell undo and
unknown-effect acknowledgement. CLI/HTTP and both TUI modes inspect retained
sources without command replay. The user-authorized LAN check on `root@192.168.1.72` additionally passed
foreground/background execution, replica reuse, owned-process cancellation,
explicit source recovery and release with strict existing host-key trust. It
used an isolated temporary replica, preserved the running model processes and
removed its owned scratch directories. The retained receipt is
`/Users/vart/.cache/rook-lan-final-20261014/ssh-probe/run/report.json`.
These checks do not claim model-quality improvement.

To reproduce the optional native checks with an installed `alpine:3`, Docker
and an OpenSSH client, build the owned test image explicitly. The fixture
creates temporary client/server keys and a private known-hosts file; the server
listens on an ephemeral loopback port and uses only those keys.

```sh
docker build --pull=false -t rook-ssh-fixture:20261014 -f crates/rook-tools/tests/support/ssh.Dockerfile crates/rook-tools/tests/support
ROOK_ENV_TEST_ROOT="$HOME/.cache" cargo test -p rook-tools --test ssh -- --ignored
ROOK_ENV_TEST_ROOT="$HOME/.cache" cargo test -p rook-core --test environments native_ssh_ -- --ignored
ROOK_ENV_TEST_ROOT="$HOME/.cache" cargo test -p rook-cli --test cli lost_ssh_execution_uses_matching_local_http_sources_and_reviewed_recovery_without_replaying_the_command -- --ignored --exact
ROOK_ENV_TEST_ROOT="$HOME/.cache" cargo test -p rook-cli --test tui_pty actual_ssh_retained_sources_are_readable_in_both_tui_modes_without_running_a_model_or_command -- --ignored --exact
```

The fixture root must exist and be shared with Docker; `$HOME/.cache` was used
for the native macOS/Colima checks. The test image recipe matches the image used
for the recorded runtime checks. Ordinary CI does not run these ignored fixtures.
