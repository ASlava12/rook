# Web workspace browser probe

Run from the checkout root on Windows with Node 24, Git and Microsoft Edge (or
Chrome) installed. Build current native executables first; a Cargo test harness
alone does not replace the daemon that this probe starts.

```powershell
cargo build -p rook-cli -p rookd
node xtask/probes/web-workspace/run.mjs
```

For a downloaded release, point both components at the extracted package:

```powershell
node xtask/probes/web-workspace/run.mjs --bin-dir Z:/verification/package/bin --web-dir Z:/verification/package/share/rook/web
```

The result retains the supplied binary SHA256s and verifies that they stay
unchanged. The UI host and assets are loaded from the supplied web directory.
Both `ROOK_HOME` and `ROOK_CONFIG_DIR` are isolated from inherited user settings.

The probe launches the real daemon, the separate web server and a headless
browser. It uses the browser's DevTools protocol with real keyboard and mouse
events. Every run creates a scratch project, Git repository and `ROOK_HOME`
under `target/web-workspace-*`. A bounded loopback model fixture provides the
tool calls and streaming answers, so no model credentials are required. Its
private API credential is generated for that run and passed only to the child
environment. Existing user stores and projects are not used.

The normal run checks draft recovery and isolation, approval and reattachment,
scroll position during streaming, Markdown rendering, source-addressed diffs
and line feedback, context and agent panels, the command palette, themes,
mobile navigation, Stop, and file mentions after changing projects. It also
submits work through the real HTTP MCP endpoint, retries it through the agent
API, verifies route isolation, waits for the managed worker and independently
checks its output file. Closing the UI must leave the daemon API available.

The printed artifact directory retains screenshots, browser text, provider
requests, daemon logs and a JSON result. On failure it also retains a failure
screenshot and the last worker status when available. Child processes are
closed on exit. `--baseline` captures only the current checkout's initial
desktop layout; it does not restore an earlier revision.

This is a controlled integration check, not a real-model benchmark, remote
computer deployment check or accessibility audit. Use `node web/check.mjs`,
`node --test web/tests/*.mjs` and `cargo xtask ci` for the complementary syntax,
bounded behavior and native checks. The browser probe currently targets the
Windows executable and installed browser paths; the Node unit suite runs on
both Windows and Linux in CI.
