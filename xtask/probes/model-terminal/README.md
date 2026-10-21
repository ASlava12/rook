# Owned daemon model-terminal probe

Requires Python 3.11 or newer, Node.js with built-in `WebSocket`, and the
extracted daemon. Run without Python optimization; assertions are part of
verification. Choose a fresh report path in an existing directory.

```sh
python3 xtask/probes/model-terminal/run.py --bin-dir /path/to/package/bin \
  --output /path/to/model-terminal-report.json
```

Three ordinary chat sockets use isolated state and a local scripted model. The
first two streams end cleanly after a complete SSE frame, without a finish reason or
`[DONE]`. One contains full `write_file` arguments; the other contains partial
text. Neither may finish successfully, execute a tool, start classification or
make another model request. Partial text and reported 11/7 token usage must
survive with incomplete model-route and physical-attempt records. The third
executes one confirmed file write, exhausts its step limit and receives an
unconfirmed forced final answer. The earlier write must survive while an
existing explicit output artifact remains untouched; combined 12/8 token usage
and the incomplete final-answer receipt must remain saved.

The report binds twelve checks to the tested daemon's SHA256. Successful process
exit and all twelve flags are required. The probe cleans up only its owned
processes and temporary state. It never changes an installed daemon or uses a
live model. The published 0.21.1 baseline executes the unconfirmed tool, returns
`end_turn` for incomplete text and replaces the final artifact, failing seven
flags. A daemon with only the main-response guard still fails the two checks
for refusing an unconfirmed forced final answer. Protocol admission is separate from
live-model quality, archive provenance and version verification.
