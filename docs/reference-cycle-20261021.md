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

The complete local `cargo xtask ci` is running for the candidate. The owned
HTTP/CLI/daemon probe now includes a human blocker, physical daemon restart and
explicit same-session continuation; its candidate-binary run is pending.
Exact-revision platform CI, tag/publication and archive/host verification are
pending. No release has been claimed complete, and no live-model quality claim
is made from scripted provider checks.
