# Publishing the binary workspace

Rook releases GitHub binary archives, not crates.io packages. Follow
`skills/rust-release/SKILL.md` for the source preflight and version/lockfile
update. Keep the English and Russian README introductions and versioned notes
in `docs/releases/` consistent with the package version.

The source must pass formatting, workspace/all-target clippy and workspace
tests. Inspect the eight required platform CI jobs on the release revision;
the optional model smoke is separate evidence, including its failures.

An annotated stable `vX.Y.Z` tag triggers `.github/workflows/release.yml`.
Five target builds check binary runtime linkage, package the binaries and
bundled skills, and create a **draft** GitHub release containing five archives
and `SHA256SUMS`. A successful build does not publish that draft.

Manual workflow dispatch builds downloadable workflow artifacts only. Its
optional `source_tag` rebuilds an existing stable tag without moving it.
Every archive's `BUILD-INFO.txt` records the checked-out source commit, workflow
recipe commit, target, flags, compiler and run URL. Retain both commit identities
when the recipe differs from the immutable tagged source.

Download the GitHub-hosted draft assets into an isolated verification directory:

```sh
gh release download vX.Y.Z --dir /path/to/release-verification
```

Verify all five archive checksums against `SHA256SUMS`, safe archive paths,
expected binary architectures, source/recipe provenance, package versions and
the exact bundled skills from the tagged source. Linux binaries must have no
ELF interpreter or shared-library dependencies; Windows binaries must not
import the dynamic C runtime. Both host binaries must report the package
version and pass the owned runtime checks for the changed behavior. Record
which targets ran natively and which received archive/linkage checks.

Use the extracted binaries and isolated test state for runtime checks. The
installer changes a user's installation and is not the artifact test runner.
Retain the verified SHA256s and native probe results in the release evidence.

After the source gates and downloaded-draft checks pass, publish that same
draft, without rebuilding or replacing its tested archives:

```sh
gh release edit vX.Y.Z --draft=false --latest
```

Download the public assets once more and confirm their SHA256s match the
verified draft. Record the public URL, source and recipe commits, workflow run
and verification limits in the release notes or linked verification ledger.
