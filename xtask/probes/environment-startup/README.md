# Environment version probes

```sh
python3 xtask/probes/environment-startup/run.py --bin-dir target/debug --output /tmp/environment-probes.json
```

Requires the actual CLI and a Rust compiler. The fixture compiles a small owned
version program, copies it under all sixteen inventory names and invokes the
real `doctor --json` with an isolated home/config/tool PATH. Every fake tool
waits two seconds and reports99.0.1. A bounded loopback receiver acknowledges
start/end events and counts active probes in one serialized order; concurrency
does not rely on comparing clocks from different processes.

Eight flags require exact language/tool maps, one execution per source probe,
complete cleanup, actual parallel execution, at most four active probes, no
model HTTP requests and unchanged CLI/workspace bytes. Time is recorded without
an arbitrary speed threshold. The fixture expects at least two probe slots to
be available; thread-creation fallback and serial/missing results have separate
unit coverage. The old sequential implementation fails the parallel-execution
flag, with all other controls retained.

Compiler/CLI output is drained with64KiB limits per stream and bounded child
deadlines. Event frames, inventory, retained events and errors are bounded. A
fresh report is required; --tmp-root selects the owned temporary parent. On
success only the resolved owned fixture directory is removed. Failed state and
its diagnostic report are retained. No installed daemon or user store is used.

This controls independent sleeping tools, not actual installed tool startup
costs or production throughput. The per-detection cap is not a global bound
across separate agent instances; existing subprocess descendant/output contracts
are unchanged.
