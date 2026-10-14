# Browser control

Rook offers an optional `browser` tool through the same agent engine in CLI,
TUI, API, browser chat and ACP. It launches the pinned Playwright MCP package
on the first approved navigation, with a separate headless Chromium context
for each conversation. Enable it in trusted user configuration:

```toml
[browser]
enabled = true
```

Install Node.js and the browser required by the pinned dependency first:

```sh
npx --yes @playwright/mcp@0.0.83 install-browser chrome-for-testing
```

The default launcher is `npx --yes @playwright/mcp@0.0.83`. For a preinstalled
package, set `command = "node"` and `args = ["/absolute/path/to/cli.js"]` in
the same table. These are trusted equipment settings. Project instructions
and page content cannot select the executable or grant permissions.

Ask the agent to open a page, inspect its snapshot, use a textbox/button ref,
wait for a visible result, take a screenshot and close. All calls go through
ordinary approvals, execution receipts and cancellation. An unattended caller
without permission receives a refusal before any process starts. `status`
only inspects the local runtime and does not contact a page.

| Action | Arguments | Result |
|---|---|---|
| `navigate` | `url` | Open an HTTP/HTTPS address without embedded credentials. |
| `snapshot` | — | Read page text and accessibility refs. |
| `click` | `ref` | Click the referenced element. |
| `type` | `ref`, `text` | Replace textbox contents; an empty string clears it. |
| `wait` | `text` or `seconds` | Wait for visible text, or 1–30 seconds. |
| `screenshot` | — | Capture the 1280×720 viewport as PNG. |
| `status` | — | Inspect context, activity, call count and uncertainty. |
| `close` | — | End the context and invalidate its refs. |

Call `snapshot` after navigation to read the page. Playwright may return an
artifact link instead of inline page text from navigation or other actions;
those temporary paths are not workspace files. Screenshot images are retained
in Rook's ordinary result store and use the existing image/tool-card views.
Terminal clients provide the existing image metadata and export fallback.
`browser.include_images` defaults to `false`: PNGs remain available to you,
while the model receives an explicit retained-image notice and can read page
state with `snapshot`. Set it to `true` for a model that accepts image input.
This choice also governs replay and compaction; user attachments and ordinary
MCP image delivery keep their existing behavior.

Refs contain both conversation identity and a unique context generation.
A ref from another conversation, an expired context or an earlier engine
lifetime is refused before dispatch. A delegated child gets its own context.
Only one call per context is admitted at a time; busy contexts are never
reclaimed to make room for another conversation.

Contexts are runtime resources. They are not resumed across daemon restart.
Timeout, cancellation or an error makes the previous outcome uncertain and
closes its connection. Inspect the recorded result, explicitly `close`, then
navigate again. Rook does not repeat an action whose answer was lost. Cancelling
`close` also preserves uncertainty. Browser equipment receives a graceful
termination before its owned process group is ended, including during engine
drop; this lets Playwright close Chromium's detached process.

| Setting | Default | Accepted range |
|---|---:|---|
| `max_sessions` | 4 | 1–16 |
| `max_calls` | 128 | 1–1024 |
| `idle_secs` | 600 | 30–3600 |
| `startup_timeout_secs` | 30 | 1–120 |
| `call_timeout_secs` | 45 | 1–120 |

Idle reclamation runs on the next browser access, not on a background timer.
`max_calls` counts attempted page calls, including failures; local `status`
and `close` do not consume it. A full pool refuses new contexts. Arguments,
catalog discovery, protocol frames, text and images use bounded admission;
temporary artifacts remain outside the user's workspace. Model arguments do
not expose JavaScript evaluation, arbitrary selectors, upload paths or
upstream launch options. `rook config edit` describes launch argument
limits too.

MCP process admission also reserves bounded cleanup capacity before spawning.
For configured `[[mcp]]` servers, `stdio_max_processes` defaults to 128 and
accepts 1–1024. Its limit applies at that server's admission across the process,
including drivers retained during the 600 ms cleanup grace. Browser equipment
uses the default admission limit. Exhausted capacity refuses the spawn.

## Verification

The ordinary suite exercises policy denial before startup, delegated context
isolation, foreign/stale refs, capacity, argument/output bounds, cancellation,
uncertain recovery and process descendants. Native Chromium checks require the
installation above and are explicit so the normal suite stays offline:

```sh
cargo test -p rook-core --test browser real_chromium_form_and_screenshot_pass_through_the_ordinary_agent_loop -- --ignored --exact --nocapture
cargo test -p rook-core --lib browser::tests::real_chromium_cancellation_and_engine_drop_leave_no_owned_browser -- --ignored --exact --nocapture
```

Optionally set `ROOK_BROWSER_MCP_CLI` to a preinstalled absolute `cli.js` path.
The first check uses the ordinary agent loop against a real local web app:
fill, exactly one submission, visible confirmation, retained PNG, full durable
call history, no pending receipts and an untouched workspace. The second
identifies Chromium through its exact driver ancestry before checking cleanup
after cancelled page calls, cancelled close and engine drop.

Reference: [Playwright MCP](https://github.com/microsoft/playwright-mcp),
with browser interaction ideas from the pinned OpenClaw reference. Rook's
adapter is independently implemented; no upstream subsystem is copied.
