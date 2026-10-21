# Owned CLI release-metadata probe

Requires Python 3.11 or newer and a directory containing the CLI to test.
Run without Python optimization; choose a fresh output file.

```sh
python3 xtask/probes/release-metadata/run.py --bin-dir /path/to/extracted/package/bin \
  --output /path/to/release-metadata-report.json
```

Four CLI invocations use isolated state and a local release API: `lsp install
rust-analyzer` and `update --check`, each with a traversal tag and a Windows
device name. All must fail with a portable-directory refusal after exactly one
metadata request. No asset body may be requested and owned sentinel files must
remain unchanged. Six flags and the CLI's SHA256 are recorded; successful exit
and all flags are required.

This exercises metadata validation through the packaged CLI. It does not install
a real server, perform a real self-update or exercise a previously constructed
`Check`; the latter mutation boundary has separate native Rust coverage.
