# Owned LSP discovery pool fixture

```sh
python3 xtask/probes/lsp-discovery-parallel/run.py --bin-dir /path/to/bin \
  --output /fresh/report.json --tmp-root /owned/temp
```

Requires unoptimized Python 3.11+, a Rust compiler and the actual CLI. One mixed
workspace has files for all five known languages. The fixture shadows all five
server commands and sixteen environment inventory names with owned helpers.
Servers acknowledge a version-only invocation with closed stdin, then retain a
TCP connection until Rook's existing ten-second probe patience and owned cleanup
retire them. An independent 65-second helper timeout bounds failed runs.

Eight conditions require observed overlap, at most four acknowledged live probe
connections, exactly one valid invocation for each server, retirement of all
connections before fixture cleanup, the original language output order, no model
HTTP and unchanged workspace/CLI bytes. The published serial implementation
fails only overlap while retaining the seven other controls. Timings are retained
without a speed threshold; these are controlled lifecycle observations rather
than production latency, global daemon concurrency or overall startup guarantees.

Compiler/CLI output is drained with 64 KiB per-stream capture ceilings and bounded
child deadlines. Frames, events, errors and connections are bounded. Reports bind
the actual binary and fixture source hashes. All new private homes, stores,
workspaces and helper files are retained; only this fixture's receiver connections
are closed after measurements. No user daemon, installation, cache or live model
is used. The separate workspace/detection fixtures retain applicability, refusals,
installed-payload bypass and descendant controls; this fixture adds overlap proof.
