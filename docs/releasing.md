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
When ACP payload handling changes, run the
[owned ACP null/default probe](../xtask/probes/acp-null/README.md) on the extracted
CLI. Require all twenty-eight flags, including the independent read/write
capability matrix, malformed/error controls, required
read/output content, terminal exit evidence, single normal cleanup and durable
uncertain-effect recovery.
Its editor buffers and provider are
local fixtures; it does not establish compatibility with a particular editor.
When credit-limited provider recovery changes, run the
[owned credit-output probe](../xtask/probes/credit-output/README.md) against the
extracted CLI. Require all ten flags: one useful smaller-budget retry, bounded
repeated/bad-budget refusals, unchanged owned workspace and unchanged binary.
When retry scheduling changes, run the
[owned concurrent retry probe](../xtask/probes/retry-jitter/README.md).
Require all eight functional flags and retain the physical request timings.
Arrival spread is observational, not a statistical pass gate; deterministic
injected-sampler tests establish jitter's wait bounds. Use the extracted CLI and
preserve its SHA256 before and after this probe.
  When provider retry observations change, run the
  [owned provider-wait probe](../xtask/probes/provider-wait/README.md).
  Require all eight flags in both main and auxiliary modes on the extracted host CLI and
  daemon; retain the actual request times, status snapshots and binary hashes.
  When Retry-After interpretation changes, run the
[owned HTTP-date probe](../xtask/probes/retry-after-date/README.md) against the
extracted CLI. Require all eight flags, including a physical retry no earlier
than the specified date, numeric-delay preservation and bounded invalid/distant
date fallbacks.
When checker history acquisition changes, run the
[owned checker-history probe](../xtask/probes/checker-history/README.md) against
the extracted CLI. Require all ten flags, including an actual stage exceeding
the retention limit, explicit omitted-range coverage and old proof read back by
the checker through `session_recall`.
When version discovery changes, run the
[owned environment-probe fixture](../xtask/probes/environment-startup/README.md).
Require all eight flags on the extracted host CLI: exact maps, one execution per
inventory entry, finished probes, observed parallel execution, the four-probe
cap, no model calls and unchanged workspace/binary. Record timing as observation,
without a speed threshold.
When version output/deadline handling changes, run the
[owned version-capture fixture](../xtask/probes/version-capture/README.md).
Require all eight flags on the extracted host CLI, including the thirty-second
inherited-pipe control, accepted stdout/stderr versions, rejected combined/
individual output floods, exact calls, owned holder retirement and unchanged
workspace/binary.
When language-server startup changes, run the
[owned daemon LSP probe](../xtask/probes/lsp-startup/README.md) against the extracted
daemon as well. It checks shared-pool behavior through ordinary chat sockets.
When PATH language-server discovery changes, also run the
[owned LSP detection fixture](../xtask/probes/lsp-detection/README.md) on the
extracted host CLI. Require all eleven flags, including the still-running
admission and descendant retirement after both successful and refused parents.
When workspace admission for language-server discovery changes, run the
[owned workspace fixture](../xtask/probes/lsp-workspace-detection/README.md) on
the extracted host CLI. Require all twelve conditions, including no probes for
notes-only folders, exact per-language calls, mixed ordering, explicit overrides
and refusal. Preserve the separate eleven status/descendant-retirement checks.
When skill-source caching changes, run the
[owned source identity fixture](../xtask/probes/catalog-source-identity/README.md)
on the extracted host CLI. Require all ten flags, including three physical
remote fetches, correct warm reuse/refresh, the unchanged legacy cache and exact
manifest/resource bytes after explicit-origin installation and saved-origin update.
When cache-install ownership changes, run the
[owned background-install probe](../xtask/probes/lsp-install/README.md) on the
extracted daemon. Two ordinary workspace chats must share the physical download
while retaining their own finished background receipts; require all ten flags.
When cache writer coordination changes, also run the
[independent CLI publication probe](../xtask/probes/install-publication/README.md)
against the extracted CLI. Both same-version and different-version callers must
succeed, with checked final payload/tag and no cache mutation after digest refusal;
require all eight flags.
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
