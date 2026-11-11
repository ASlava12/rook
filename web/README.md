# Rook web workspace

The web interface is a separate, buildless application. `rookd` serves the HTTP
API, chat WebSocket and agent MCP endpoint; no UI files are linked into its binary.
The CLI and daemon build and run without Node.

From a source checkout, build the native components, start or reuse the daemon,
then start the UI host:

```sh
cargo build -p rook-cli -p rookd
target/debug/rook daemon start
node web/serve.mjs --daemon http://127.0.0.1:7717 --port 3000
```

On Windows use `.\xtask\cargo.cmd build -p rook-cli -p rookd` and
`.\target\debug\rook.exe daemon start`. The daemon starts in the background;
repeating `daemon start` reports the existing instance. Check it with
`daemon status`, and use `daemon stop` when you intend to stop it. One daemon
owns a store, so do not start a second `rookd` against the same `ROOK_HOME`.
If your configured daemon port differs, pass its printed URL to `--daemon`.
The default project is the directory from which the daemon starts; other folders
can be selected in the UI. Example paths such as `/absolute/project` must be
replaced with a real existing folder when explicitly using `--workspace`.

Open **http://127.0.0.1:3000**. Keep the UI host running; closing its terminal
disconnects the browser. The UI server needs Node 24 or later and no
packages or install step. It serves the adjacent `dist/` directory directly, so
editing HTML, CSS or JavaScript needs only a browser reload. Keep `serve.mjs` and
`dist/` together when distributing or updating this component.

`cargo xtask dist` copies this component under `web/` beside the release binaries.
Release archives carry it under `share/rook/web`; run that directory's `serve.mjs`
to use it. The native binaries can be installed without that directory.

The local server binds to loopback and proxies `/api/`, including WebSocket
upgrades, to a loopback daemon. It validates Host and Origin before forwarding
requests and preserves the validated public authority for OAuth callbacks.
Missing scripts return 404; app routes fall back to `index.html`. Private OAuth
callbacks are never cached. Assets are constrained to their real directory and a
per-file byte limit; proxy responses stream with backpressure.

For a daemon on another computer, forward its port with SSH and point `--daemon`
at the local forwarded port. For a hosted UI, serve `dist/` with your own
authenticating HTTPS reverse proxy and proxy `/api/` to the daemon on the same
origin. The general daemon API includes conversations and controls and belongs
behind that authentication. Scoped automation access is described in
[agent-api.md](../docs/agent-api.md).

The workspace offers recent sessions, project selection, a command palette
(`Ctrl/Cmd K`), `Ctrl/Cmd Shift O` for a new session, `Alt I` for details,
light/dark/system themes, responsive navigation and a composer with attachments,
file mentions, settings and optional task output contracts. Session details keep
recorded file changes, source-addressed diffs and line feedback beside chat;
feedback enters a draft and does not submit itself. The Agents and Context tabs
read the existing bounded APIs. Additional history, branches, queues, MCP
connections and maintenance controls remain under Tools.

Text drafts are stored in sessionStorage by session or unsent project. Recovery
retains at most eight drafts and 256 KiB total, with 64 KiB per serialized draft;
older entries retire at the bound. An oversized draft or unavailable storage
leaves text in the composer and displays a recovery notice. File objects and
private credential inputs are never persisted as drafts. A pending file selection
must be sent or removed before switching sessions.

Connection loss appears beside the composer, with an explicit offline state and
Reconnect action. The visible chat retries read-only probes with backoff up to
30 seconds and reattaches its observer when available. It never automatically
resends a saved prompt or Stop. An unavailable project pauses retries and offers
folder selection; drafts remain available while the connection is down.

```sh
node web/check.mjs
node --test web/tests/*.mjs
```

The real Windows browser/daemon probe is documented in
[xtask/probes/web-workspace](../xtask/probes/web-workspace/README.md). It uses an
isolated store and a scripted provider; it does not need real model credentials.
