# Owned daemon model-terminal probe

Requires Python 3.11 or newer, Node.js with built-in `WebSocket`, and the
extracted daemon. Run without Python optimization; assertions are part of
verification. Choose a fresh report path in an existing directory.

```sh
python3 xtask/probes/model-terminal/run.py --bin-dir /path/to/package/bin \
  --output /path/to/model-terminal-report.json
```

Two ordinary chat sockets use isolated state and a local scripted model. Each
stream ends cleanly after a complete SSE frame, without a finish reason or
`[DONE]`. One contains full `write_file` arguments; the other contains partial
text. Neither may finish successfully, execute a tool, start classification or
make another model request. Partial text and reported 11/7 token usage must
survive with incomplete model-route and physical-attempt records.

The report binds eight checks to the tested daemon's SHA256. Successful process
exit and all eight flags are required. The probe cleans up only its owned
processes and temporary state. It never changes an installed daemon or uses a
live model. The published 0.21.1 baseline writes the owned fixture file and
returns `end_turn`, failing seven flags. Protocol admission is separate from
live-model quality, archive provenance and version verification.
