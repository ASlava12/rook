# Owned workspace admission fixture

Requires Python 3.11+, Rust compiler and a built CLI. Use unoptimized Python and
a fresh output path:

```sh
python3 xtask/probes/lsp-workspace-detection/run.py --bin-dir /path/to/bin \
  --output /path/to/report.json --tmp-root /path/to/owned-temp
```

Six actual `rook lsp servers` invocations cover notes-only, Rust-only,
Python-only, all five known languages, explicit custom configuration and a
refused applicable Python server. All five PATH server names and all sixteen
environment-inventory names resolve to owned helpers. Each actual version
invocation records its command, stdin EOF and exact `--version` arguments.

Require all twelve conditions: six exact applicable-call sets, unchanged offered
language/refusal results, closed stdin, exact version requests, no model HTTP,
unchanged workspace files and unchanged CLI SHA256. Mixed results preserve known
order and explicit config does not execute a PATH probe. An owned HTTP observer
records unexpected model requests. Timing is retained without a speed gate.

The published 0.21.10 negative control must fail four call-admission conditions
while retaining eight result/isolation controls. All homes/config/workspaces and
helpers are fresh private fixture state and are retained on success or failure.
No user daemon, installation or cache is changed. The separate LSP detection
fixture covers timeout/descendant retirement and installed-payload bypass has a
core regression. This fixture is not a live language-server compatibility test.
