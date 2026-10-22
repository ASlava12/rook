# Version capture and inherited pipes

```sh
python3 xtask/probes/version-capture/run.py --bin-dir target/debug --output /tmp/version-capture.json
```

Requires the actual CLI and a Rust compiler. The compiler proxy's executable
name is preserved before isolating PATH; resolving rustc to rustup would change
the selected operation. The fixture compiles one owned version program and
installs all sixteen inventory aliases in a fresh temporary tool directory.
Six exercise specific behavior; the others return status1 without output so
Windows cannot discover an unrelated system program through native lookup.
Real JSON doctor sees:

- rustc: a version parent exits while its owned descendant retains both pipes
  and a liveness connection for thirty seconds unless terminated;
- python3: a valid stdout banner with 60 KiB of additional data;
- java: a valid stderr version with exit status 1;
- git: stdout and stderr individually below 64 KiB but together above the ceiling;
- cargo/npm: oversized stdout/stderr respectively.

Eight flags require exact language/tool maps, exactly one call per each of the
sixteen owned sources,
completion within twenty seconds of the holder handshake, closure of that owned
holder's connection, no model HTTP calls and unchanged CLI/workspace bytes. The
deadline check is a functional control against a thirty-second holder, not a
throughput benchmark. Unit subprocess cases separately verify lossless large
captures and prove that individually smaller streams share one cap.

A bounded loopback receiver serializes events and acknowledges helper starts.
The holder keeps its socket through the controlled sleep, so EOF/reset provides
retirement evidence for this owned case. This does not prove universal process
containment or close the Windows post-spawn job-attachment race. Reports retain
actual timings, source/helper/binary hashes, physical calls and event order.

Compiler/doctor output is drained with 64 KiB limits per stream and bounded child
deadlines. Frames, event/error collections and helper sizes are bounded. A fresh
report is required; --tmp-root chooses the owned temporary parent. Only a
successful, resolved owned directory is removed. Failed diagnostics are retained;
an old implementation's holder still has its own thirty-second lifetime. No
installed daemon, user store or live provider is used.
