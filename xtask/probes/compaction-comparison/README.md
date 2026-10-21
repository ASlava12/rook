# Compaction comparison admission

Build the real experiment example and run without Python optimization:

```sh
cargo build --locked -p rook-core --example compaction-ab
python3 xtask/probes/compaction-comparison/run.py \
  --binary target/debug/examples/compaction-ab --output /fresh/report.json
```

Use `.exe` on Windows. The probe owns temporary configs/stores and a local HTTP
provider. It creates synthetic protocol fixtures, never reads a user transcript,
and does not call a live model, installed daemon or agent tool. Its eight flags
require stopping after one incomplete, empty or usage-unreported response,
retaining failed-arm usage/diagnostics without quality scores, and preserving
normal A/B then B/A flow for confirmed text and reasoning summaries. The report
binds results to the example binary SHA256. Reports are created exclusively.

The baseline marks four source-fallback records available after incomplete SSE
while still recording token usage. The corrected example admits only a committed
summary with a confirmed model receipt and reported usage. Metadata is read with
explicit byte limits rather than inferred from prose or a truncated display.

These are harness regressions, not summarizer quality measurements. R17-7 still
requires paired real-transcript results and semantic review; the default output
budget remains unchanged. See [the experiment record](../../../docs/reference-r17-adoption.md#summarizer-experiment).
