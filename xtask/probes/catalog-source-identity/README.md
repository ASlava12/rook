# Owned remote skill-source identity fixture

Requires Python 3.11+, Git with `http-backend`, and an actual CLI binary. Run
without Python optimization, with a fresh report path:

```sh
python3 xtask/probes/catalog-source-identity/run.py --bin-dir /path/to/package/bin \
  --output /path/to/catalog-identity.json --tmp-root /path/to/owned-temp
```

Three temporary Git repositories expose skills at loopback smart-HTTP
paths `/a/b`, `/a-b` and `/a/b.git`. The first and third also offer a same-named
skill with distinct manifest/resource bytes. The fixture seeds a legacy lossy-name cache from the
first repository, then invokes `rook skills search` for a cold search, a warm
search and an explicit refresh. It records actual HTTP paths, offered
name/source pairs and the origins of the new Git cache entries. It then installs
from the explicitly selected `.git` endpoint, advances that owned repository
and runs `skills update` to verify the recorded origin remains authoritative.

All ten conditions must pass: correct source attribution; all physical
fetches; separate correct cache origins; correct warm reuse without HTTP;
correct refresh of all repositories; unchanged legacy cache; unchanged
workspace; unchanged CLI SHA256; exact selected install payload; and exact
recorded-origin update payload. The legacy comparison hashes every file
before and after, including Git metadata. A known-bad package can fail those
identity conditions while retaining the independent workspace/binary controls.
A cache-only candidate passes the first eight conditions and fails the two
installation/update conditions, proving that cache separation alone is insufficient.

Every home/config/store/cache/repository belongs to the fresh fixture directory.
Git system/global configuration and interactive prompts are disabled for these
owned operations. All fixture files and its private caches are retained for
inspection, including successful runs. No existing cache is removed or moved;
no user daemon, installation or user state is changed. The report identifies
the tested CLI and owns bounded request/response diagnostics. Release provenance
and model execution of an installed skill are checked separately.
