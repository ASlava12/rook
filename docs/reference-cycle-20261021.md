# October 21 reference and reliability cycle

Baseline: `53d91a8`, following release `v0.19.0`. Release source:
`664972c8c4d15c2aad9a7b58fb16e5e8350936eb`, annotated tag `v0.20.0`.
Published [v0.20.0](https://github.com/ASlava12/rook/releases/tag/v0.20.0)
at `2026-10-21 11:48:46 UTC`; final public-asset verification passed.

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

Delivered in the release source:

- `block_work` atomically parks an admitted root stage until explicit resume.
- Delay and human checkpoints share control/generation/pending-guidance checks.
- Output-contract handling and completion classification are skipped at a human
  checkpoint, and unexecuted batch tails remain protocol-correct on replay.
- The original classifier-only recovery fix retains replies across process/store
  reopen and parks repeated unresolved failures instead of replaying tools.

See [the contract](managed-blockers.md) and [release notes](releases/0.20.0.md).

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
The focused Windows rerun passed, including the assertion that the content
scorecard actually ran. The earlier full local gate remains a failed historical
run; the final platform CI below supplies the complete passing gates.

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
verified. This did not inspect Windows CRT imports. Both host binaries report
`0.20.0`, and the owned HTTP/CLI/daemon probe
passed again using the packaged Windows binaries. The following `9e0eba5`
revision changes only release verification notes.

On the exact tagged source, [CI run 39582861014](https://github.com/ASlava12/rook/actions/runs/39582861014)
passed all eight required jobs: Linux, macOS, Windows and FreeBSD, MSRV and three
cross-checks. The Windows full gate took 2267.3 seconds and passed both the
corrected inbound HTTP scenario and the one-slot/six-delegate scenario. An
earlier `9e0eba5` Windows run exceeded the delegate test's 10-second ceiling and
then the overall gate's 45-minute ceiling; those failures remain recorded.
Neither ceiling was increased to obtain the final pass.

The tagged source's deliberately nonblocking Ollama/Qwen 3B smoke passed four
of eight scenarios. Failures were command obedience (used `read_file` instead
of the requested command), claim verification (edited the subject and reported
the original claim unproven), file movement (streamed repetition was stopped),
and cross-session memory (returned an invented `staging` tool as text). The
earlier five-of-eight result above belongs to its own source/run. These are
observed model-quality limitations, not a claim of improvement or passing tests.

The annotated tag was pushed after the required checks passed. In
[release workflow 39587774300](https://github.com/ASlava12/rook/actions/runs/39587774300),
both Linux archives passed additional ELF checks for absence of an interpreter
and shared-library dependencies. The Windows archive passed its structure and
host-version checks, but PE import inspection found `VCRUNTIME140.dll` and
dynamic CRT API imports in both executables. That contradicts the standalone
binary contract. The workflow was cancelled before publication; no release was
created from those artifacts.

The revised build recipe uses Windows static CRT linkage, following the
[Rust reference](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes),
and rejects dynamic CRT imports before packaging. Linux loader/dependency
checks are also part of the recipe. Optional `source_tag` dispatch checks out
an existing stable tag so its source can be rebuilt without moving the tag.
The archive version must match both binary package versions. Manual dispatch
always uploads artifacts without publishing a release, including when its
workflow ref is a tag. Build jobs have read-only repository permissions.
Each corrected archive records source commit, recipe commit, target, flags,
compiler and workflow URL in `BUILD-INFO.txt`; a recipe rebuild can therefore
prove that its binary source remains the immutable tag.

Actionlint, extracted Bash/PowerShell syntax and the tag/package-version guards
passed locally. The exact Windows linkage guard rejected the known dependent
archive. The corrected [rebuild 39591199255](https://github.com/ASlava12/rook/actions/runs/39591199255)
passed all five build jobs and their linkage guards. Its manual publish job was
skipped. Every artifact identifies source `664972c` and recipe `012456b`; the
tag remains unchanged. All archive checksums, names, architectures, source/
recipe/run metadata, versions and 50 bundled skills passed verification against
a detached worktree of the tag. Linux ELF files have neither an interpreter
nor shared-library dependencies; Windows PE imports have no dynamic CRT.

A draft release received exactly five corrected archives and `SHA256SUMS`.
Those GitHub-hosted files were downloaded and verified again. The final Windows
artifact passed all 16 owned HTTP/CLI/WebSocket checks: due scheduling and
capacity, same-session continuation, physical daemon restart, no repeated file
effects or effects from skipped calls, paired wire history, pause/cancel, CLI-visible
waits, conversation ownership handoff and explicit human-blocker resume.

After publication, all six assets were downloaded from public URLs without
authentication. The five archive SHA256s match the tested draft assets; their
structure, provenance, linkage, skills and host binary versions passed again.
The release is public, stable and marked latest. Native execution evidence is
for Windows; the other packages have archive/linkage and source-platform CI
evidence rather than a claim of native execution on every published target.

Earlier shape/host checks are not substituted for the corrected CRT-linkage
and final downloaded-asset checks. The initial dependent Windows archive was
never published as a release asset.
