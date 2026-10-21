# Owned native CLI dialog probe

This uses a scripted local HTTP model adapter, a local page and the pinned
Playwright MCP driver. It exercises the actual `rook` executable and Chromium
without a real model or the installed daemon:

```sh
python3 xtask/probes/browser-dialogs/run.py \
  --binary /path/to/extracted/bin/rook \
  --mcp-cli /path/to/node_modules/@playwright/mcp/cli.js \
  --output /path/to/new-output-directory
```

Install the pinned browser equipment as described in [browser control](../../../docs/browser.md).
The output directory must be new. It retains the scratch config/store, native
answer, stderr, browser results and JSON report, including the executable hash.
The process has a five-minute ceiling and cleanup is restricted to owned state.

Seven cases cover alert acceptance, confirm dismissal, repeated confirm acceptance,
answered/dismissed prompts and an explicitly empty prompt answer. The page reports
each resolved value independently to the local server; all seven events must
match in order. The probe also requires native exit, ordinary end of turn,
consumption of all scripted calls, no unknown outcomes/cycle refusal and an
untouched workspace. Scripted provider behavior does not establish model quality
or independent model verification.

The GitHub-hosted Windows 0.21.0 package passed all ten checks, including the seven
exact page events. Native execution on other packaged targets is not claimed.
