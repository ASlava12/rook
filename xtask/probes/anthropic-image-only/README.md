# Owned Anthropic legacy image replay

Requires Python 3.11+, Cargo/Rust, Node with global WebSocket, the source checkout
and an actual CLI/daemon pair:

```sh
python3 xtask/probes/anthropic-image-only/run.py --bin-dir /path/to/bin \
  --output /path/to/fresh-report.json --tmp-root /path/to/owned-temp \
  --build-target /path/to/owned-cargo-target
```

The temporary Rust helper only seeds/verifies a fresh private store. Its offline
build copies the source lockfile and checks every resolved dependency identity
against it. It saves a legacy bare-image user message with two valid PNGs,
followed by an assistant response so ordinary adjacent-user coalescing does not
merge the image turn with the new prompt. The original event record and body
must remain byte-for-byte unchanged after replay.

The requested CLI continues one session with the five-minute cache setting;
the requested daemon continues another through `/api/chat` with one-hour caching
and a whitespace caption. An owned loopback Anthropic endpoint records real
wire JSON and returns successful SSE plus a valid separate completion verdict.
Exactly one main request and one classifier request per frontend are required.
No live model, external credential, user daemon or user store is used.

All eight flags are required: CLI image-only shape, daemon image-only shape,
exact image payloads, cache boundaries/TTLs, normal completed turns and exact
owned requests, original saved input preservation, unchanged workspace, and
unchanged binary pair. The published 0.21.11 pair fails the two shape conditions
and cache placement while preserving the other five controls. The development
correction passes all eight. This is not a live provider refusal or a benchmark.

The model observer caps eight requests at 1 MiB each. Command captures are
bounded to 64 KiB per stream; chat frames are capped at 512 and 1 MiB per frame.
Build, startup and chat operations have independent bounded deadlines. Inventory
tools are shadowed with owned no-op binaries. Only the owned daemon process is
stopped; fixture stores, wire reports, command logs and helper inputs remain
available on success or failure. No installation, cache or user data is changed.
See the [wire contract](../../../docs/anthropic-image-only.md).
