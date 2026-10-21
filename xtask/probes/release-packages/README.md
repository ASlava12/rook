# Release archive verification

Requires Python 3.11 or newer and no third-party packages. Run against a fresh
download directory and the skills from the immutable tagged source:

```sh
python3 xtask/probes/release-packages/verify.py /path/to/downloads \
  --skills /path/to/tagged-source/skills --version X.Y.Z \
  --source-commit SOURCE_SHA --recipe-commit RECIPE_SHA \
  --run-url https://github.com/OWNER/REPO/actions/runs/RUN_ID
```

The default checks all five targets. `--target TARGET` checks a selected subset.
`--host` additionally extracts the Windows package into a new temporary directory
and runs both binaries with `--version`; it requires Windows. The JSON result
reports the retained extraction path for subsequent owned runtime checks.

The verifier checks checksums, portable paths and duplicate/case aliases, regular
archive entries, file layout, package version and build identity, binary
architecture, Linux ELF linkage and Windows CRT imports. It compares every skill
file, including variants/resources; Markdown comparison normalizes CRLF/LF.
Unexpected artifact files fail verification. Python optimization is refused so
`-O` or `PYTHONOPTIMIZE` cannot silently remove validation.
Portable-name checks follow the
[Win32 naming rules](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file),
including the reserved superscript COM/LPT device aliases.

Tagged release jobs run the verifier before creating a draft. Downloaded-draft
verification, native runtime probes and public-download hash comparison still
follow the [release procedure](../../../docs/releasing.md).

Run the negative fixtures without a compiler, browser or model:

```sh
python3 xtask/probes/release-packages/check.py
```

They recompute checksums for missing/modified resources, duplicate and aliased
entries, links and incorrect provenance. These are deliberate malformed fixtures,
not runnable binary packages. Positive package evidence comes from actual release
archives; the five published 0.21.0 archives passed the repo verifier on Windows.
