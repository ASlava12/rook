# Native browser task probe

Install the pinned browser as described in [browser control](../../../docs/browser.md),
build `rook-cli`, then run once with a new output directory:

```sh
python3 xtask/probes/browser/run.py --binary /absolute/path/to/rook --output /tmp/rook-browser-check
```

`--endpoint`, `--model` and `--mcp-cli /absolute/path/to/cli.js` select trusted
equipment for the scratch configuration. Defaults use the available LAN model.
The runner hosts a local form, submits random text through the actual agent,
and checks the returned random confirmation, exact submission count, native
exit and an untouched workspace. It retains failures without retrying. Inspect
the native transcript and image receipts separately; the summary does not
certify every required tool action or an automatic checker's verdict by itself.

The [verification note](../../../docs/research/browser-control-20261011.md)
records the fixed limits and outcomes. Successful deterministic Chromium
fixtures establish protocol/engine behavior; native model failures remain
distinct evidence.
