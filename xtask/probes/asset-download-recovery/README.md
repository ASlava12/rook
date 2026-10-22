# Owned CLI asset recovery

Requires Python 3.11+ and an actual CLI/daemon pair:

```sh
python3 xtask/probes/asset-download-recovery/run.py --bin-dir /path/to/bin \
  --output /path/to/fresh-report.json --tmp-root /path/to/owned-temp
```

The fixture discovers the actual CLI target through an isolated JSON update
check. It packs the provided executable pair and one owned skill into a local
same-version archive, then runs ordinary `rook update --force` on private copies.
On Linux, the host `strip --strip-debug` removes DWARF sections from replacement
copies so source-build debug data does not exhaust the installer archive budget.
The original updater remains unchanged. Payload versions and separate payload/
original SHA256s are retained; other platforms use direct payload copies.
`--force` makes a same-version reinstall exercise real staging, binary replacement
and previous-file retention. This is a transport/placement check, not evidence
of a version upgrade or live GitHub behavior.

Five owned loopback release cases serve a truncated body followed by an exact
range, an ignored Range/full restart, three successive truncations, a mismatched
range and a deliberately wrong advertised digest. They return real body data
and HTTP framing; a fake provider refusal does not substitute for truncation.
The full payload SHA256 binds both binaries and the new skill.

Require all twelve flags: normal CLI continuation, ignored-range restart,
three-attempt cap, mismatched-range refusal, terminal digest refusal, unchanged
file contents and directory entries on refusal, exact successful payload/tree,
previous-file retention, only owned requests, identity/range headers, unchanged
workspace and unchanged original executable pair. The frozen 0.21.12 tag package
fails seven conditions while retaining five digest/isolation/no-effect controls;
the Windows development correction passes all twelve. Both archive/host-version checks
and source identity are retained separately for that negative pair.

The model endpoint is never used. Each child receives private `ROOK_HOME` and
`ROOK_CONFIG_DIR`, an owned release API/repository, and its own installation
prefix. No user daemon is started/stopped, and no installed user file is replaced.
Refused cases compare the complete private installation tree; successful cases
compare the exact expected current/previous tree. The original provided pair is
checked by SHA256 before and after.

There are at most 32 observed requests, an archive ceiling of 128 MiB, 512-byte
request paths, eight bounded observer errors and 64 KiB command captures per
stream. Each CLI has a ninety-second fixture deadline. Only fixture-owned children
are killed after a deadline/capture refusal. All case directories, archives,
receipts, requests and logs are retained. See the
[reader contract and limits](../../../docs/asset-download-recovery.md).
