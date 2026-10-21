# Owned daemon language-server startup probe

Requires Python 3.11 or newer, Node.js with built-in `WebSocket`, and a directory
containing the daemon to test. Run without Python optimization; assertions are
part of verification. The report path must be fresh and its parent must exist.

```sh
python3 xtask/probes/lsp-startup/run.py --bin-dir /path/to/extracted/package/bin \
  --output /path/to/lsp-startup-report.json
```

The probe starts an isolated daemon, a local scripted model and three real mock
LSP processes. Six ordinary WebSocket chats share one project pool. Python is
initialized first; two Rust callers then wait behind a physical initialization
gate. While Rust remains held, ready Python diagnostics, a fresh Go startup and
a generic workspace-symbol query must complete. The generic query must reach
Python before the Rust gate is released. Every language initializes once.

The model fixture honors streaming chat requests and separately answers one
completion classification per chat. All six chats must finish with exactly two
main model requests and one classification each. Eight check flags, physical
initialization counts and the tested daemon's SHA256 are written to the report.
Successful process exit and all eight flags are required; the gate is released
and owned processes are cleaned up even when the baseline fails.

The published 0.21.1 daemon fails the three independence checks: its global pool
lock delays ready and unrelated clients until Rust is released. A locally built
corrected daemon passes while both Rust callers remain held. This is controlled
protocol and pool-lifecycle evidence. It does not establish real-server indexing
performance, live-model task quality or global installation deduplication.
Version, archive provenance and bundled skills are checked separately by the
[release verifier](../release-packages/README.md).
