# Real-model phase comparison: 2026-10-03

Status: in progress. This records native inference, not the scripted frontend
or harness tests. Use the existing
[benchmark protocol](../../xtask/probes/phase-routing/BENCH.md) for limits and
interpretation. The earlier 9B load failure remains in the
[adoption tracker](pi-adoption-20260930.md); it was not repeated.

## Availability

A bounded read-only inspection found one configured source, `home-lmstudio`,
with model `huihui-ornith-1.5-35b-a3b-abliterated-i1`. The catalog listed
`google/gemma-4-e4b`; all models were reported not loaded at inspection time.
The catalog log is `target/pi-phase-availability-20261003.log`.
A single 16-token-limit availability request to Gemma returned HTTP 200 and
`READY`, 23 input / 2 output tokens and 11.147 seconds wall time. Evidence:
`target/phase-candidate-0d83b867-1623-4f9d-91cb-a169d506c0ca/report.json`.
That request was not a tool/quality benchmark or part of pair token totals.
No model-management load/unload request or user configuration edit was made;
the configured endpoint handled inference loading.

## First rename pair: retained protocol failure

Root: `target/phase-bench-021617c8-0d3a-4ca1-9d81-32ef2addad8d`.
Runner log: `target/pi-phase-gemma-pair-20261003.log`.
The runner terminated with exit 1. Both arms ran their two native stages; the
fixed arm violated planning protocol, so this is not a successful paired
workflow comparison and cannot substantiate percentage savings/speedups.

| Arm | Analysis exit / stop | Implementation exit / stop | Plan valid | Independent quality | Wall seconds | Reported input / output tokens |
|---|---|---|---|---|---|---|
| Fixed 35B | 2 / completion_unchecked | 2 / looping | No | Fail | 388.527 | 22,781 / 5,251 |
| 35B → Gemma | 0 / end_turn | 0 / end_turn | Yes | Pass | 102.815 | 33,707 / 3,808 |

The fixed saved journal shows two valid file reads, then a `write_file` call
with null arguments rejected as invalid JSON. Later replies claim that no task
was given and repeat `list_dir`; neither the plan nor implementation files were
written. This identifies the observed failure, not its provider/internal cause
or the cause of an unrelated operator session's UI behavior.

The routed arm wrote DESIGN.txt without altering the seed files in its planning
stage and passed the external rename oracle. Its final saved receipt has
`phase=implementation`, dispatch `model=google/gemma-4-e4b`, and matching native
`reported_model`; implementation dispatch is verified. No unverified savings
claim follows from that successful arm alone.

Both bounded journals include all counted main/auxiliary receipts: fixed 12,
routed 9. Each physical attempt completed; there are no failed/pending admissions
in those final coverage snapshots. Configured prices are absent, so every
receipt/attempt is unpriced and USD totals remain unknown. These totals include
auxiliary requests; cumulative coverage snapshots, receipts and attempt totals
must not be added together. Reported cache token counts do not establish the
server's physical cache state. Wall time includes loading, server and completion
checks; the fixed arm ran first. The failed protocol also confounds timing.

## Three-task suite: 1536-token profile

The finite suite used the same sources, prompts, bounds and independent
oracles for `rename`, `ports` and `money`, one repetition each. No benchmark
settings or oracle were relaxed to erase the first failure.

Authoritative report:
`target/phase-bench-8bd23576-7847-481b-8220-061ecc85b7ab/report.json`.
Log: `target/pi-phase-gemma-suite-20261003.log`.
The suite is terminal with runner exit 1: routed rename edited the seed during
planning, and fixed money did not create a valid plan. All six arms finished
their two native stages; there was no provider/process abort. The failed planning
pairs remain visible and are excluded from controlled workflow comparisons.
Per-stage PID, stdout/stderr, saved context and journals reside in each arm's
fresh root. No native benchmark process remains live from this suite.

| Task / arm | Native exits (analysis / implementation) | Plan valid | Oracle | Wall seconds | Reported input / output tokens |
|---|---|---|---|---|---|
| rename / fixed | 0 / 0 | Yes | Pass | 286.823 | 56,459 / 5,315 |
| rename / routed | 0 / 0 | No | Pass | 108.241 | 46,980 / 4,392 |
| ports / fixed | 0 / 0 | Yes | Pass | 327.543 | 45,919 / 4,687 |
| ports / routed | 0 / 2 | Yes | Fail | 201.340 | 77,382 / 9,752 |
| money / fixed | 2 / 2 | No | Fail | 322.233 | 16,910 / 5,088 |
| money / routed | 0 / 0 | Yes | Pass | 174.536 | 76,638 / 8,304 |

`ports` is the only eligible paired workflow. Its routed implementation reached
the eight-step limit, and the external oracle found a missing rejection for an
invalid port. This candidate therefore traded lower observed wall time for
failed correctness on this case. Do not treat faster incorrect output as an
equivalent completed implementation or make a general routing recommendation.
The routed rename arm changed code in its planning stage; routed money passed
both planning and quality, but its fixed counterpart did not satisfy planning.

[The committed summary](pi-phase-comparison-20261003.json) retains source commit,
script hash, stage stops, receipt coverage and per-dispatch-model input/output
subtotals, including auxiliary requests. Every arm's journal matches all counted
receipts (11/11, 11/11, 11/11, 14/14, 7/7, 14/14 respectively). Every counted
physical attempt completed, with zero failed/pending admissions. All receipts
are unpriced: USD remains unknown, not zero, and no dollar savings were measured.
Do not add cumulative snapshots or attempt totals to these receipt subtotals.
Small ordered samples cannot support general model recommendations. Token
counts mix different model tokenizers; no unit-price equivalence is assumed.

The first mandatory CI attempt exited 1 after 63.6 seconds: Windows denied Cargo
removing `target/debug/rook.exe` while the live benchmark used that executable.
Fmt/Clippy passed; this failed build is not a green gate. Evidence:
`target/pi-phase-gemma-comparison-ci.log`. Repeat the full CI after the owned
benchmark finishes; do not interrupt or duplicate a live measured stage merely
to free the executable. After the suite became terminal, the full gate rerun
exited 0 in 479.1 seconds (`target/pi-phase-gemma-comparison-final-ci.log`),
including fmt, Clippy, frontend builds, workspace tests and doctests. No production storage
changes require a new compaction.

## Measurement improvements and next profile

The initial rename journal's malformed write immediately follows a native
receipt reporting exactly the 1536-token generation limit. This does not prove
the provider's internal cause, but warrants testing a larger output budget
before drawing broad conclusions from planning failures. The runner now accepts
`--output-tokens 512..8192`, with the same cap in both arms; its original default
is preserved. The next real profile should use 4096 without changing the task,
eight-step bound, planning oracle or independent quality checks. Keep profiles
separate and retain all failed/ineligible cases.

The CI lock failure also prompted pinning a CLI snapshot in each future run's
owned root. Copy admission is 128 MiB, buffers are 64 KiB, and a growing file
cannot cross that budget while copying. Byte count, SHA-256 and the executable's
version are recorded; all measured and inspection commands use the snapshot.
This protects stage consistency and frees `target/debug/rook.exe` for builds.

The focused Node suite exited 0, 13/13 tests
(`target/pi-phase-gemma-runner-final-tests.log`): actual native fixture requests
verify the chosen 4096-token cap in both models, copied binary metadata/hash,
failed-session coverage and abort behavior. Invalid caps are refused before
configuration/evidence creation. These controlled tests are harness evidence,
not real-model quality or savings measurements. The real higher-budget profile
remains outstanding; phase adoption is still in progress.
