# Experiment trees

Create a measured baseline, branch alternative hypotheses, and explicitly select
an improved result. CLI `rook session experiment <session> '<JSON action>'`,
chat/TUI `/experiment <JSON action>`, the browser session's **Experiments** panel,
and POST `/api/sessions/<session>/experiments` use the same core contract.
An empty slash/CLI argument lists the tree. Each node and run is a linked session.

For example, the evaluator `evaluate.sh` can read string parameters from
`.rook-experiment/parameters.json`, write `{ "score": 0.85 }` to
`.rook-experiment/metrics.json`, and save declared artifacts under
`.rook-experiment/artifacts/`:

```json
{"action":"create","title":"baseline","hypothesis":"Establish current score","contract":{"command":"sh evaluate.sh","metrics":{"score":"maximize","time":"minimize"},"artifacts":[".rook-experiment/artifacts/report.txt"]},"parameters":{"batch":"16"}}
```

That example requires both `score` and `time` in the metrics file. Commands need
ordinary execute permission under the configured sandbox policy. A denied or
approval-required command does not launch; configure an appropriate allow rule
or stance when setting up this explicit evaluator.

Use `prepare` with the returned node ID to inspect the saved command, source and
parameters. Pass its exact token to `run`. Local CLI execution waits for the
receipt; the daemon also accepts `wait:false`, returning admission while keeping
an owned worker alive after the caller disconnects. `inspect` reports durable
state and actual in-process ownership separately. The browser uses detached
runs and provides refresh and stop controls.

A successful command must have certified descendant cleanup, unchanged saved
source, exactly the declared finite numeric metrics, and all declared artifacts.
An unsuccessful command cannot establish an answer by leaving metric files.
Measured nodes are frozen, including writes and shell commands through the
ordinary agent. Use `branch` with an answered parent, title, hypothesis and
optional parameters for another question. Its candidate directory is separate;
edit that directory and use `capture` before preparing a run. An active or
uncertain run also blocks candidate edits through the agent and recapture.

`compare` returns the selected baseline and its direct children, their metric
deltas and a retained comparison token. An improved child with no regression
in any declared metric can be `promote`d using the current selection baseline's
token. Promotion records a winner; it does not merge or overwrite user files.
Branch the next round from that winner. Ties, failed runs and arbitrary
out-of-round comparisons cannot silently select a winner.

Each run materializes its own saved source and string parameter file. Original
workspace changes made after capture stay in the original workspace. The fixed
contract includes the command, declared metric directions and artifact paths,
plus an observation hash of OS, architecture, shell, PATH, LANG, LC_ALL,
PYTHONHASHSEED, CUDA_VISIBLE_DEVICES and OMP_NUM_THREADS. A changed observation
requires a new baseline. This fingerprint does not identify dependencies,
hardware, remote services or every environment variable; capture relevant
requirements and lockfiles with your source. It is not a promise of numerical
determinism or a container image.

`read` selects a node/run and optional artifact path, with a bounded byte cursor;
omit the artifact for command output. Raw metrics use
`.rook-experiment/metrics.json`. Content checksums are verified before reads.
Source, metrics, artifacts and output are pinned by retained notes through GC.
Failed run directories and their source/output receipts remain inspectable.

`stop` requests cancellation of the live worker. Cancellation kills its owned
command group and leaves uncertain execution effects. Inspect the run session
with `rook session recovery <run-session>`, acknowledge each unknown operation
with the existing recovery command, and then `recover` the experiment with an
inspection note. A lost process owner never replays a command automatically.
Recovery records inspection, not a successful measurement. A candidate whose
publication failed or lost its owner remains recorded; create a fresh project
after inspecting the saved source and any partial directory.

`[experiments]` bounds nodes (32), runs per node (8), encoded state (4 MiB), source
files (1024), walked entries (4096), one source/artifact (8 MiB), aggregate source
or declared artifacts (32 MiB), declared artifacts (32), output (64 KiB), and
command time (120 seconds). Excluded entry names default to `.git`, `target`,
`node_modules`, `.venv`; `.rook-experiment` is always excluded from source.
Sources admit only regular files through the no-follow capability filesystem;
symlinks and excluded/generated directories are outside the snapshot. There
are at most two concurrent experiment workers per process and eight source
recaptures per node. State capacity for active runs' terminal metadata is
reserved before publishing an execution workspace or launching a command.
The store must be outside the captured original workspace.
