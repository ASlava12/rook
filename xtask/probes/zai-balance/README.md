# Z.ai balance classification

Run `python3 xtask/probes/zai-balance/run.py --bin-dir target/debug --output
/owned/path/report.json` on a built CLI. `--stage downloaded-release` and
`--expect-version` bind a later downloaded-package execution.

Four owned local HTTP servers exercise OpenAI-compatible and Anthropic request
paths. Code1113 returns the original refusal after one physical request even
with Retry-After120; ordinary code1302 still receives four attempts. The probe
checks actual model/path fields, CLI errors, unchanged workspace/binary, retired
fixture servers and no fixture protocol errors: 19 conditions in total. No
cloud account is contacted. Temporary files and captured outputs are retained.
