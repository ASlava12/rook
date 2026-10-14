# Shell changes and selected undo

Shell undo retains bounded preimages before `run_command` and observes the
workspace after its owned processes stop. It covers foreground commands and
jobs started with `background: true`. Commands executed by a delegated worker
have receipts in that child's session; select the child in the session tree
or use its session ID.

Enable it explicitly in `config.toml`:

```toml
[shell_undo]
enabled = true
# Exact entry names excluded at any directory depth:
exclude = [".git", "target", "node_modules", ".venv"]
```

Keep `ROOK_HOME` outside the workspace. The defaults admit eight command
receipts per session, 1,024 regular files, 4,096 directory entries, 8 MiB per
file, 32 MiB per observation and 4 MiB of receipt state. Limits are enforced
while walking and reading. Unsupported files, exhausted limits or unavailable
capture capacity refuse command admission instead of claiming an undo point.
The configuration guide lists the bounded ranges for each setting.

This scope is explicit: excluded entries, paths outside the workspace,
symlinks, special files, directory permissions and external side effects are
not restored. The capture records observed files; it is not an atomic snapshot
of every concurrent writer. Editor-owned terminal execution is refused while
shell undo is enabled because Rook cannot certify that terminal's file window.

## Inspect and select files

CLI, chat/TUI, HTTP and browser use the same core receipt contract:

```sh
rook session shell-undo SESSION_ID
rook session shell-undo SESSION_ID '{"action":"inspect","receipt":"RECEIPT_ID"}'
rook session shell-undo SESSION_ID '{"action":"prepare","receipt":"RECEIPT_ID","paths":["src/main.rs"]}'
```

The prepare response contains the exact selected paths and a token tied to
the saved source, current file contents and permissions, session ownership
and restore generation. Prepare does not change files. Inspect the selection,
then pass that token with the same paths:

```sh
rook session shell-undo SESSION_ID '{"action":"restore","receipt":"RECEIPT_ID","paths":["src/main.rs"],"token":"PREPARED_TOKEN"}'
```

Existing files return to their pre-command contents and file permissions;
deleted files are recreated, and files created during the command window are
removed. User edits present before the command are part of the preimage. All
selected files are checked before the first write. A later edit or mode change
causes a conflict. Completed paths cannot replay; select other changed paths
and prepare a new token. Each receipt permits at most eight restore attempts,
with at most 128 paths per attempt.

In chat or TUI use `/shell-undo` with the same JSON actions. In the browser,
open a session, expand **Shell changes and undo**, list receipts, select exact
paths and preview before clicking **Restore selected files**. HTTP clients
POST the same bounded JSON to `/api/sessions/SESSION_ID/shell-undo`.

Restores exclude other owned shell windows and other restores in the same
process. Every selected preimage is validated before writing, and each path
is checked again at publication. This is a conflict-checked sequence rather
than a multi-file transaction or a filesystem compare-and-swap against
unrelated processes. Current selected preimages, file modes and absent paths
are retained in the restore attempt before changes. If a later write fails,
inspect `restore_attempts`: `completed` records acknowledged writes and
`pending` names a write whose result may require inspection. Do not replay
an interrupted or partial restore blindly.

## Interrupted commands and restores

Cancelling a foreground call ends its originally owned process group. A
background receipt stays active until its job finishes and the original group
cleanup is certified. A missing job or failed cleanup is uncertainty, never a
successful completion. Process loss can also leave a started receipt without
an after-source.

Inspect `rook session recovery SESSION_ID` and the actual files/output. Stop
any remaining processes. Acknowledge unknown execution operations through
the existing recovery command with an inspection note. Then record the shell
recovery observation:

```sh
rook session shell-undo SESSION_ID '{"action":"recover","receipt":"RECEIPT_ID","note":"Inspected changed files and confirmed remaining processes stopped"}'
```

Recovery records what exists now as `recovered`; it does not claim the
interrupted command succeeded. For a partial restore it retains completed and
pending paths and records the inspection. Prepare a new selection for the
remaining files. A pending path already restored but not acknowledged may
conflict; inspect its actual bytes rather than overwriting it again.

File blobs remain reachable through non-model-visible source notes and survive
restart and garbage collection. Receipt state is versioned bounded JSON;
existing session/event postcard formats are unchanged. Deleting the owning
session removes its receipt companion. The installed daemon is not updated
or restarted by this feature's isolated verification.
