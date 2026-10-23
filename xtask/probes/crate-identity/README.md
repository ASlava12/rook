# Owned CLI/daemon crate identity

```
python3 xtask/probes/crate-identity/run.py --bin-dir target/debug --output /owned/report.json
```

Requires Python 3.11+, rustc and Node with built-in WebSocket support. Each
frontend receives its own home, workspace, Cargo cache and model route. It asks
for a two-version package, then explicitly selects 2.0.0. A wrong 2.0.0 archive
in the first cache must be skipped in favor of a later checksum-matching one.
Archives are valid owned gzip/tar fixtures. The version helper replaces PATH
discovery tools and has no application, filesystem or network actions.

Require all ten flags: CLI/daemon ambiguity refusal and explicit-version API,
wrong-cache exclusion, model-visible version argument, both ordinary completions,
exact bounded model requests, unchanged workspace/cache trees and unchanged
binary/helper/script bytes. Expected requests are three main, one claim verifier
and one completion classifier per frontend. The verifier returns unproven and
does not supply an independent factual success claim. Approvals and follow-up
prompts are refused.

The supplied pair performs the actual tool reads. The fixture never calls an
external model, registry or dependency build. Child captures admit 64 KiB per
stream, model requests 2 MiB and twelve total requests, WebSocket input 512 frames
and 1 MiB per frame. Child/daemon/model time limits and private state are retained
in the output. Native package execution must use the extracted verified pair;
development-pair evidence does not substitute for that release check.
