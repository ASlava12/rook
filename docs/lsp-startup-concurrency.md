# Independent language-server startup

Each configured language has its own initialization gate within the workspace's
shared server pool. Concurrent requests for that language reuse one initialized
client or one cached startup failure. The collection mutex covers lookup and
publication, not the process launch or LSP handshake.

An unrelated pending initialization therefore does not block access to an
initialized client, write diagnostics, or startup of another language. A generic
workspace query first chooses an initialized client with an observed live process
in configuration order. A cached client whose process has exited returns a closed
error and cannot hide another startable language. Process liveness is checked
without waiting for its ownership lock; it does not establish RPC health. If
none is initialized, the existing ordered startup/fallback behavior applies.
This is process startup, not package installation or automatic failure recovery.

Background warming coalesces requests per configured language. Repeated cold
diagnostic requests retain at most one warm task for that language. Its ownership
guard resets even when the future is dropped before its first poll. Registry keys
remain bounded by the configured languages.

Shutdown closes new-start admission as soon as its writer is queued. New requests
are refused without waiting for an unrelated handshake to finish. Starts already
admitted retain shared lifecycle ownership until initialization succeeds, fails,
or is cancelled; shutdown then closes their clients. A delayed start cannot
publish a new client after the pool has closed. Shutdown can be retried after
cancellation without skipping clients that remain.

No configuration keys, dependencies, stored formats or tool names change.
Startup failures remain cached until a new pool is constructed. Configured mock
or real servers still use the same existing protocol, deadlines and permissions.

## Reference and verification

The per-server in-flight pattern was reviewed in OpenCode's historical
`55ecb0674846ad9f914fd852c53ad8e8deab7da4`,
`packages/opencode/src/lsp/lsp.ts`. It shares a startup promise per root/server
and removes only the matching in-flight entry. Rook independently uses Rust
per-language gates and lifecycle admission; no code is copied. The current
reference pointer remains unchanged. This is a historical source review, not a
claim about OpenCode's current implementation.

The native regressions use controlled LSP mock processes. A Rust process logs its
actual initialization request and remains held while Python reuse, a generic
workspace query, diagnostics and a new Go startup complete. Two Rust callers
receive the same client and one physical initialization. Sixty-four cold
diagnostic requests add at most one pending warm task, measured through the
test runtime while initialization is still held.

Separate cases deliberately fail a held initialization and require one physical
attempt across concurrent and later callers, then successfully start another
language. A shutdown race refuses a new Go start before releasing the held Rust
handshake and verifies that Go never physically starts. Control files are
published atomically and subprocess tests are serialized and cleaned up.
Another case stops an initialized Python client, then verifies that its cache
returns a closed error and a generic query starts Rust instead, without restarting
Python.

These checks exercise protocol and lifecycle behavior with owned mock servers;
they do not establish real-server installation, indexing performance or model
task quality. Platform source gates remain required before publishing the change.
