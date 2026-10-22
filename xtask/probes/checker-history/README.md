# Bounded checker history

Run against an owned source build or extracted package:

```sh
python3 xtask/probes/checker-history/run.py --bin-dir target/debug --output /tmp/rook-checker-history.json
```

The actual CLI completes 80 distinct, read-only catalog queries against a local
scripted provider. This creates more visible events than the 128-entry retention
limit. Its checker receives acquisition coverage and uses the parent session's
actual harness-suggested sequence anchor to read an old proof back with
`session_recall`. The hint must start at the stage floor because returned cursors
move forwards; starting at the newest omitted event strands older proof. Require all ten
flags. The fixture bounds requests to 2 MiB, output capture to 1 MiB, physical
requests to 92 and the owned CLI process to 180 seconds. It uses a fresh isolated
home and workspace, verifies the workspace sentinel and hashes the binary before
and after. It does not contact a real model or control an installed daemon.

This checks the actual acquisition/recall path, not latency or whole-checker peak
memory. Unit fixtures separately reach the 1024-event scan limit, refuse oversized
metadata, enforce a stage floor and frozen upper boundary, omit large attachment
frames before decoding, and retain browser attribution without decoding pixels.
Verified range reads check the whole object. The checker therefore admits at most
2 MiB both raw and stored before reading any payload, including browser caption
lookup. A compressed oversized-result fixture reaches the raw limit while its
encoded size still fits; neither text nor caption verification may decode it.
An actual corrupt external-payload fixture must produce an explicit issue and
anchored placeholder. Event metadata is admitted at 16 KiB, session metadata at
64 KiB. These are acquisition bounds, not a whole-checker peak-memory measurement.
The original journal is preserved. Missing or shortened evidence cannot establish
the absence of an action or narrow the user's goal and constraints.

The committed-range idea was read in Pi at
`482c1381a00d8d00bc81697e4a980c593db6803e`,
`packages/durable/src/harness/context.ts`. Rook independently implements the
admission limits and coverage record. The owned provider/capture fixture follows
Rook's existing credit-output probe.
