# Owned Cargo output capture

Requires Python 3.11+, Rust, Node with a built-in WebSocket client and a real
CLI/daemon pair:

```sh
python3 xtask/probes/cargo-output/run.py --bin-dir /path/to/bin \
  --output /path/to/fresh.json --tmp-root /path/to/owned-temp
```

The standard-library Rust helper emits 4,096 compiler progress lines, an error
with its source span and warning, then 4,096 passing-test lines and a failed
summary. Its 239,726 raw bytes place the diagnostic between both ends of a 2 KiB
capture. It is compiler-shaped controlled data, not an actual Cargo build.

An owned OpenAI-compatible stub makes two ordinary `run_command` calls through
the actual CLI and daemon WebSocket frontends: automatic and explicit raw
presentation. A harmless distinct helper argument keeps the second execution
independent of command-repetition suppression. An invocation ledger proves
both physical executions. The provider only requests these fixed commands and
then a normal final response; it is not a model-quality evaluation.
Each frontend must make exactly three main requests, one independent verifier
request and one completion-classifier request. The fixture answers those two
different auxiliary protocols separately.

Require all twelve flags: both frontend diagnostics, raw override, four distinct
original captures with identical bytes, actual failed exit codes, the offered
tool argument, exactly two owned commands per frontend, preserved warning/span/
test summary, request/capture bounds, only expected workspace effects and
unchanged binaries/helper/source. The sentinel is unchanged; the only added
workspace file is the helper's two-line invocation ledger.

Each frontend gets a private home/config/workspace, isolated skills, no memory,
an owned provider/key and an owned PATH tool inventory. No user daemon or
installation is touched. All subprocess streams are capped at 64 KiB, WebSocket
frames at 512 and 1 MiB total, HTTP request bodies at 2 MiB, request paths at
512 bytes, error records at eight and requests at twenty. Original capture reads
are capped at 1 MiB; foreground commands have the normal tool caps. CLI/compiler
children have a 120-second fixture deadline, WebSocket clients have 110 seconds
and daemon startup has 35 seconds. The owned daemon is retired in cleanup. Reports,
logs, cases, original captures and helper bytes are retained.

See the [contract and limits](../../../docs/cargo-output-condensation.md).
