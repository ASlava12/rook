# Inspecting session-family background work

`rook session background SESSION` reads a lazy family page. Use `--json` for the
structured response and `--after CURSOR` to scan more direct children. Ancestors,
the selected session and direct children retain their source IDs; inspect a
child's ID to explore deeper descendants. Empty child pages can still carry a
continuation because unrelated sessions spend the configured scan allowance.
The reader reuses `[branches]` page entries, bytes, scan and ancestry bounds.
If an archived workspace/config cannot be opened, the daemon reads shared saved
metadata with its main configuration bounds.
It never changes complete billing or recovery traversal.

REPL and TUI support `/background [SESSION] [--after CURSOR]`. The TUI performs
the read in its existing bounded history worker and leaves the draft, selected
chat and active turn intact. Browser chat and session views have an independent
“Background work · session family” panel with Refresh, Scan more children,
Earlier ancestors and Explore family here controls. `GET
/api/sessions/SESSION/background?after=CURSOR` exposes the same core page.

Each row correlates lineage, saved conversation-goal status and last-progress activity. Standalone legacy work
iterations expose observed activity but have no conversation-goal status on their
session row.
A current daemon executor can attest its own activity and that of admitted
children. Child observations retain at most 128 entries per executor, pruning
finished entries first; a missing observation is not evidence that an external
process has stopped. Child idle time measures time since observed work and can
include human-input waits; child stall alarms are disabled because their input
ownership is not independently tracked. Root stall detection keeps its existing
approval/question handling. Child observations are process-local; persisted root
observations remain available after restart.

`owner_live = null` means an offline reader cannot verify the executor.
`owner_live = false` means no current executor was observed by this daemon; it
makes no claim about a foreign daemon or external process. Saved markers never
become liveness evidence. A saved goal's scheduling state is independent of its
executor and is labelled accordingly. Lost sessions can therefore be inspected
without pretending that a saved “running” marker proves execution.

Ordinary background `run_command` admission records `ToolContext.session`.
Private delegated registries retain their scope and cannot be reassigned by the
caller. Direct/legacy starters without an owner remain explicitly unassigned.
The overview reads metadata without cloning command text, output, spill paths or
execution-environment payloads. Job counts refer only to existing registries:
inspection never creates equipment. The daemon joins existing project registries
so children in registered worktrees are covered. Private ACP registries are not
exposed through this shared view. A page retains at most 256 matching job rows;
`omitted_jobs`, `unassigned_jobs` and `jobs_observed` state its coverage. Driver
exit is distinct from remote process cleanup; `processes_stopped = false` keeps
uncertified cleanup visible without copying environment payloads.

Verification uses ordinary `RunCommand` admission in the tools jobs suite,
`reference_cycle` lazy-page/saved-state checks, daemon API and child-owner tests,
and a TUI regression preserving a busy draft. The native R17 probe additionally
checks API/CLI busy-owner parity and actual browser controls against an isolated
daemon. These observations do not kill, retry or recover work automatically.

## Background command text

Background commands decode stdout and stderr separately. A UTF-8 character split
between pipe reads waits for its remaining bytes instead of becoming replacement
characters or being interpreted through the Windows code page. At most three
unfinished bytes are retained per pipe. While they are pending, a job read can
end just before that character.

EOF, a read error or cancellation flushes any unfinished bytes through the
existing best-effort text conversion. Invalid bytes keep the existing platform
fallback; this is not a declaration of an arbitrary command's encoding. Display
head/tail bounds and raw capture/redaction rules continue to apply. Received-byte
counts and quiet time track actual pipe activity, including unfinished characters;
flushing retained text does not count as new activity.

The jobs tests exercise every split of multilingual UTF-8 text, one-byte reads,
independent simultaneous pipes, display bounds, invalid-byte fallback, EOF,
read errors and cancellation.

`xtask/probes/background-utf8/run.py` also drives an actual local CLI turn and a
CLI turn through a private daemon. The child holds unfinished stdout/stderr
characters until the first job read, then finishes stdout and waits for its
observed output before finishing stderr. Its 24 checks include the intermediate
and final text, exit status, confirmed
execution route and unchanged workspace sentinel. The same probe has an explicit
`--expect-corruption` mode to reproduce the failure on an older binary pair;
ordinary CI requires every check to pass on its newly built pair.
