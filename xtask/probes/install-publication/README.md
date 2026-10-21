# Independent installer processes

Run without Python optimization, against a built or extracted CLI and a fresh
output path:

```sh
python3 xtask/probes/install-publication/run.py --bin-dir /path/to/package/bin \
  --output /fresh/install-publication.json
```

Two ordinary CLI processes share an owned cache and local release HTTP fixture.
Both asset responses remain held until both are requested. Same-version and
different-version cases require both CLI processes to succeed, their own release
results, and final payload bytes matching the final tag. A digest-refusal case
must leave no cache, including coordination metadata. Eight flags are bound to
the CLI's SHA256, which must remain unchanged. Require exit zero and all flags.

Payloads are synthetic and never executed. No language server, system package
manager, real release download, model, installed daemon or user state is used.
Processes are stopped and reaped before owned temporary files are cleaned up.
Archive versions and provenance remain separate checks.

The published 0.21.2 Windows CLI baseline makes two downloads and one caller
fails with sharing error32 in three controlled same-version runs. The cache
writer lease fixes publication among cooperating updated installers. It does
not deduplicate downloads across processes, coordinate legacy/noncooperating
writers, or make multi-file publication atomic for unrelated readers.
