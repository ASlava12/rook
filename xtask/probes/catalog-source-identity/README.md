# Owned remote skill-source identity fixture

Requires Python 3.11+, Git with `http-backend`, and an actual CLI binary. Run
without Python optimization, with a fresh report path:

```sh
python3 xtask/probes/catalog-source-identity/run.py --bin-dir /path/to/package/bin \
  --output /path/to/catalog-identity.json --tmp-root /path/to/owned-temp
```

Two temporary Git repositories expose distinct skills at loopback smart-HTTP
paths `/a/b` and `/a-b`. The fixture seeds a legacy lossy-name cache from the
first repository, then invokes `rook skills search` for a cold search, a warm
search and an explicit refresh. It records actual HTTP paths, offered
name/source pairs and the origins of the new Git cache entries.

All eight conditions must pass: correct source attribution; both physical
fetches; separate correct cache origins; correct warm reuse without HTTP;
correct refresh of both repositories; unchanged legacy cache; unchanged
workspace; and unchanged CLI SHA256. The legacy comparison hashes every file
before and after, including Git metadata. A known-bad package can fail those
identity conditions while retaining the independent workspace/binary controls.

Every home/config/store/cache/repository belongs to the fresh fixture directory.
Git system/global configuration and interactive prompts are disabled for these
owned operations. All fixture files and its private caches are retained for
inspection, including successful runs. No existing cache is removed or moved;
no user daemon, installation or user state is changed. The report identifies
the tested CLI and owns bounded request/response diagnostics. Release provenance
and installed-skill execution are checked separately.
