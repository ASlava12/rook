# Known connection credentials in actual CLI and daemon turns

The owned local HTTP fixture supplies an artificial credential containing quotes
and a backslash through a model connection. It exercises direct CLI and private
daemon turns in both OpenAI-compatible and Anthropic dialects. The real request
headers must keep the original credential; saved tool calls/results and the next
model request must contain masked values. It also checks request counts, routes,
model identity, unchanged workspace/binaries, server errors and daemon retirement.

The 43 conditions run on Linux, macOS and Windows in CI. `--source`, binary
versions/hashes and the executing script digest are retained for external receipt
validation. Use `--stage downloaded-release` for a verified downloaded pair and
the source/version identified by that package; development results do not prove
release-package behavior. `--stage baseline` labels an old binary comparison and
does not turn expected privacy failures into successful functional conditions.

No account key or cloud request is used. Every child receives a private home and
workspace, the model URL is loopback, and only the fixture's daemon is stopped.
