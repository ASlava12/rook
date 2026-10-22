# Provider backoff observations

Run against actual CLI/daemon binaries, with Node's built-in WebSocket support:

```sh
python3 xtask/probes/provider-wait/run.py --bin-dir target/debug --output /tmp/provider-wait.json
python3 xtask/probes/provider-wait/run.py --bin-dir target/debug --refuse auxiliary --output /tmp/provider-wait-auxiliary.json
```

The fixture creates an isolated home, config, workspace, empty chat and daemon.
A local model refuses the first physical streaming request with HTTP429 and
Retry-After45. The real retry must retain that minimum and finish, followed by
one completion classification. No installed daemon, real endpoint or user store
is accessed. Only owned processes are stopped during cleanup. The auxiliary mode
instead refuses the first completion-classification call: one main request and
two classification requests must occur. Both modes run in native source CI.
The fixture resolves Node before giving the owned agent an empty tool PATH and
skill directory. It therefore avoids probing the CI runner's unrelated tool
shims while creating its empty conversation; the model fixture needs no tools.

Eight required flags per mode cover physical call counts and minimum delay, visible chat
status, scoped provider deadlines, unchanged physical progress through at least
40 seconds of idle time, absence of false stalls, and unchanged binaries and
workspace. The 30-second notice threshold is deliberately exceeded. Request
arrival lateness is observational; a loaded runner is not required to meet an
arbitrary upper latency target. Diagnostics and WebSocket captures, requests,
response bodies and overall observation time are bounded.

An optional --tmp-root places owned temporary state on a chosen volume. A fresh
output path is required. The report is retained on a failed condition as well.
Published0.21.6 and0.21.7 were independently observed announcing inactivity during
this allowed wait; the fixed source must satisfy every flag. Those historical
package observations are separate from a source-built run.

Cancellation, exhausted/terminal retries, all three retry paths, overdue notices
and parent/child isolation have deterministic Rust coverage. This scripted API
does not establish real-model task quality or production throughput.
