# Owned PATH language-server detection

Requires Python 3.11+, Rust and an actual CLI binary. Run without Python
optimization; assertions are part of verification. Use a fresh report path:

```sh
python3 xtask/probes/lsp-detection/run.py --bin-dir /path/to/package/bin \
  --output /path/to/lsp-detection.json --tmp-root /path/to/owned-temp
```

Five isolated `rook lsp servers` invocations exercise a zero exit, exit seven,
a still-running version probe, and successful/refused parents that each leave
a connected descendant. Real helpers are compiled before reducing PATH. All
sixteen environment inventory commands are shadowed by unavailable stubs;
only the Rust language-server probe is available. No real LSP is initialized.

The eleven flags require the expected three admission verdicts, retirement of
the running probe and both descendants, exactly five physical discovery calls,
closed stdin, no model HTTP, and unchanged workspace/binary bytes. A TCP
handshake proves that each descendant exists before its parent exits; EOF or
reset measures retirement independently of the direct parent's exit status.
The report retains case timings, process IDs and connection endings observed
before fixture cleanup. Timing is observational, not a throughput score.

Each held helper also has its own thirty-second bound. After recording a failed
negative control, the fixture can cooperatively release only its own held
connections. That cleanup cannot turn a measured failure into a pass. Successful
temporary state is removed after validating the exact owned directory; failures
are retained. This does not touch installed servers, user caches or a running
user daemon. Archive/source provenance is checked separately.
