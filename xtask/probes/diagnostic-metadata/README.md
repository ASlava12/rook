# Owned diagnostic metadata runtime fixture

Requires Python 3.11+, Cargo, the source checkout and an actual CLI/daemon pair:

```sh
python3 xtask/probes/diagnostic-metadata/run.py --bin-dir /path/to/bin \
  --output /path/to/fresh-report.json --tmp-root /path/to/owned-temp \
  --build-target /path/to/owned-cargo-target
```

The optional build target reuses compiler artifacts; without it, a fresh fixture
build directory is used. Cargo runs offline on a temporary helper manifest using
path dependencies and a copied source lockfile. Every resolved dependency
identity must match the source lockfile. The helper only seeds and verifies a
fresh private store; report behavior always runs the requested CLI/daemon pair.
No diagnostic method from the helper substitutes for those binaries.

The seeded store contains an oversized event label between ordinary neighbors,
a valid inactive activity receipt, an oversized primary session and an oversized
ancestor of a readable child. Local CLI reads, direct HTTP and CLI-to-daemon
reads must preserve useful report data while refusing/omitting oversized input.
A loopback proxy observes the remote CLI; complete identifiers must not request
`/api/sessions`. Missing sessions remain refused and model HTTP is forbidden.

All eighteen flags are required. They cover local/HTTP neighbors, omissions,
activity generation/progress, primary refusal, ancestor correlation omission,
missing-session refusal, remote CLI parity and no listing, report privacy,
no model requests, unchanged workspace, binary/source SHA256s and preserved saved
data. A retained content digest covers all seeded session/event metadata,
event bodies and activity before/after the report reads. No private content is
exported from these deliberately synthetic fixtures.

The published 0.21.10 pair fails ten reader/listing conditions while retaining
eight status/privacy/isolation/data controls. A development correction passes
all eighteen. These are functional resource-admission checks across build
profiles, not a latency, peak-RAM or live-model quality measurement.

All helper/store/config/report state is retained on success and failure. Only
the fixture-owned daemon is stopped. No user daemon, session, installation or
cache is changed, removed or moved. See the
[reader contract](../../../docs/diagnostic-metadata-admission.md).
