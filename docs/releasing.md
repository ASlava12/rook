# Publishing the binary workspace

Rook releases GitHub binary archives, not crates.io packages. Follow
`skills/rust-release/SKILL.md` for the source preflight and version/lockfile
update. Keep the English and Russian README introductions and versioned notes
in `docs/releases/` consistent with the package version.

The source must pass formatting, workspace/all-target clippy and workspace
tests. Inspect the eight required platform CI jobs on the release revision;
the optional model smoke is separate evidence, including its failures.
Its [preparation step](../xtask/probes/model-smoke/README.md) requires a complete
pull response and confirms the requested model is available. HTTP 200 alone is
insufficient; a preparation failure stops before scenarios and is recorded
separately from model task failures. Offline preparation regressions run in the
required Linux gate.
The smoke's endpoint check, scenario turns and transcript reads set both
`ROOK_HOME` and `ROOK_CONFIG_DIR` to the scenario's temporary directory. An
inherited config override must not replace the requested model, eight-step
limit or disabled server installation. This isolates the scenario config;
provider endpoints and credentials still follow the normal config machinery.
On failure, diagnostics read the latest root session and at most two of its
direct children, including verifier sessions. Each read covers up to 40 newest
events with 1200-byte body previews. The display keeps the beginning and end
within 80 lines for the root and 40 per child, with omitted lines marked. These
are bounded excerpts, not a complete transcript or an independent verdict.
The Linux CI job also runs the pure-Python archive verifier regressions.

An annotated stable `vX.Y.Z` tag triggers `.github/workflows/release.yml`.
Five target builds check binary runtime linkage, package the binaries and
bundled skills, and create a **draft** GitHub release containing five archives
and `SHA256SUMS`. Before draft creation, the collected archives pass the
repo's Python verifier against the source skills and build identity. A
successful build does not publish that draft.

Manual workflow dispatch builds downloadable workflow artifacts only. Its
optional `source_tag` rebuilds an existing stable tag without moving it.
Every archive's `BUILD-INFO.txt` records the checked-out source commit, workflow
recipe commit, target, flags, compiler and run URL. Retain both commit identities
when the recipe differs from the immutable tagged source.

Download the GitHub-hosted draft assets into an isolated verification directory:

```sh
gh release download vX.Y.Z --dir /path/to/release-verification
python3 xtask/probes/release-packages/verify.py /path/to/release-verification \
  --skills /path/to/tagged-source/skills --version X.Y.Z \
  --source-commit SOURCE_SHA --recipe-commit RECIPE_SHA \
  --run-url https://github.com/OWNER/REPO/actions/runs/RUN_ID
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
The [archive verifier](../xtask/probes/release-packages/README.md) can extract and
check Windows host versions with `--host`. Run the
[owned dialog probe](../xtask/probes/browser-dialogs/README.md) on the extracted
CLI when browser behavior changes, alongside the managed-work probe.
When language-server startup changes, run the
[owned daemon LSP probe](../xtask/probes/lsp-startup/README.md) against the extracted
daemon as well. It checks shared-pool behavior through ordinary chat sockets.
When cache-install ownership changes, run the
[owned background-install probe](../xtask/probes/lsp-install/README.md) on the
extracted daemon. Two ordinary workspace chats must share the physical download
while retaining their own finished background receipts; require all ten flags.
When release-tag validation changes, run the
[owned CLI metadata probe](../xtask/probes/release-metadata/README.md) on the
extracted CLI. Its isolated refusals must precede all asset-download requests.
When model-terminal admission changes, run the
[owned daemon terminal probe](../xtask/probes/model-terminal/README.md) against
the extracted daemon. The first two incomplete chats must stop without tool
effects or further model requests while preserving partial text and reported
usage. A third chat must retain its earlier confirmed action while refusing an
unconfirmed forced final answer before writing the output artifact. Require all
twelve check flags.

After the source gates and downloaded-draft checks pass, publish that same
draft, without rebuilding or replacing its tested archives:

```sh
gh release edit vX.Y.Z --draft=false --latest
```

Download the public assets once more and confirm their SHA256s match the
verified draft. Record the public URL, source and recipe commits, workflow run
and verification limits in the release notes or linked verification ledger.
