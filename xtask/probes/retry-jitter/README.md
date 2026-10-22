# Owned concurrent retry probe

Run Python 3.11 or newer without optimization against an actual CLI:

```sh
python3 xtask/probes/retry-jitter/run.py --bin-dir /path/to/package/bin \
  --output /fresh/path/retry-jitter.json
```

Require exit zero and all eight flags. Four isolated CLI processes reach a local
model API together. A barrier synchronizes their first HTTP 429 refusals without
Retry-After; each must retry exactly once, retain the ordinary one-second minimum,
finish its own reply and complete one scripted classifier request. The fixture
records refusal/retry timestamps and the physical retry-arrival spread. The
refusal timestamp precedes sending the response, so transport and scheduling
overhead are included in the measured gap.

No random spread threshold is a gate: a statistical assertion would intermittently
fail valid sampling. A release without jitter can pass these functional controls.
Deterministic injected-sampler Rust tests prove the added wait and its ceiling
through complete, stream-open and pre-first-delta physical retry paths. This
probe complements them with concurrent actual-CLI execution and observable
timing; it is not a production throughput or model-quality benchmark.

Request bodies are admitted at 2 MiB and generation requests at six per case.
Each CLI has a 60-second deadline, bounded drained capture at 128 KiB and an
isolated home/config/workspace. Barrier admission has its own 30-second ceiling.
Sentinels, workspace contents and executable SHA256 must remain unchanged.
No installed daemon, real model or tool command is used. Only owned temporary
state is cleaned up.
