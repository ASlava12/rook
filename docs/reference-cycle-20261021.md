# October 21 reference and reliability cycle

Baseline: `53d91a8`, following release `v0.19.0`. Release candidate: `0.20.0`.

## Evidence and accepted work

The local session audit found that `completion_unchecked` discarded a finished
answer and restarted working turns. A user prerequisite described in prose was
therefore followed by more GUI experiments. The previous fix retained answers,
separated classification retries and bounded unresolved failures. This cycle
keeps that work and adds a deterministic human-blocker checkpoint.

Reference inspected: Pi pinned at
`482c1381a00d8d00bc81697e4a980c593db6803e`, restored at its existing gitlink
without advancing it. Relevant sources are durable `types.ts` (task/report
states and parked tasks), `harness/scheduler.ts` (`blockedReason`, explicit
retry), and `test/harness-ownership.test.ts` (committed waiting checkpoints).
The transferred idea is a committed scheduler boundary distinct from a running
executor. Pi's generic state machine is not described as an equivalent
model-callable human-blocker tool.

Delivered in the candidate:

- `block_work` atomically parks an admitted root stage until explicit resume.
- Delay and human checkpoints share control/generation/pending-guidance checks.
- Output-contract handling and completion classification are skipped at a human
  checkpoint, and unexecuted batch tails remain protocol-correct on replay.
- The original classifier-only recovery fix retains replies across process/store
  reopen and parks repeated unresolved failures instead of replaying tools.

See [the contract](managed-blockers.md) and [draft release notes](releases/0.20.0.md).

## Verification and publication ledger

Earlier targeted checks established the classifier fix across managed work,
ordinary agent loops and execution recovery. New engine fixtures establish
human-blocker persistence, explicit same-session resume, skipped tail effects,
invalid argument rejection, output contracts and loss immediately after commit.
Context fixtures cover forged calls from ordinary/checker/child/restricted/
read-only contexts. Changed-control checks cover both checkpoint tools.

The first local `cargo xtask ci` passed formatting, workspace/all-target clippy
and frontend builds, but the full test link exhausted the Z drive (LNK1180).
A new full gate used a separate C-drive target with incremental compilation
disabled and three build jobs. Formatting, clippy, builds and all test targets
except the CLI target passed. The inbound HTTP CLI scenario depended on POSIX
`printf` through Windows `cmd`, leaving an empty redirected file when `printf`
was absent. Its scorecard also lived at the workspace root instead of
`.rook/evaluation.toml`, so the declared check had not run. The fixture now uses
ordinary `write_file`, puts the scorecard in its actual location, selects a
platform-native content check and asserts that its passing report is present.
The focused rerun is pending; this full gate is not recorded as a pass.

The owned HTTP/CLI/daemon probe passed on candidate binaries, including a human
blocker, physical daemon restart, explicit same-session continuation and a
protocol-correct skipped batch tail. Its first Windows attempt exposed `EPERM`
when the Python observer reader overlapped Node's atomic snapshot replacement.
The probe now coalesces snapshots and bounds retries of that specific Windows
sharing contention; a stopped observer reports its saved bounded event trace
and stderr immediately. The complete probe passed after that fixture correction.

On source `a57afe3013ef59b48fa070f548187c99a3654eea`, CI run `39569636543`
passed Linux, macOS, Windows and FreeBSD tests, MSRV and three cross-checks.
The deliberately nonblocking Ollama/Qwen 3B smoke passed five of eight scenarios;
command obedience, unchanged file content and cross-session memory scenarios
failed. Those failures are retained as model-quality evidence, not converted
to passing checks or a quality-improvement claim.

All five release dry-run packages from `d1f7ae0` built successfully. Archive
paths, checksums, binary architectures, versions and all 50 bundled skills were
verified. Both host binaries report `0.20.0`, and the owned HTTP/CLI/daemon probe
passed again using the packaged Windows binaries. The following `9e0eba5`
revision changes only release verification notes.

Exact-revision CI for the final source, tag/publication and verification of
published assets remain pending. No release has been claimed complete.
