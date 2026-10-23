# Local Rust API identity

`crate_api` selects a package name from the nearest Cargo.lock. It stops at that
lockfile: an absent package, unreadable/invalid lock or unsupported source does
not fall back to an unrelated ancestor workspace. Use the lockfile package name,
not a renamed Cargo.toml dependency alias.

When the lock contains several versions, the tool refuses to pick the first.
Supply `version` with an exact locked version. A missing requested version does
not fall back to another. Multiple records for the same name/version remain
ambiguous, including records from different registries. Path and git dependencies
are refused rather than matched to a same-name registry package.

The local lookup pairs `registry/src/<registry>/<name>-<version>` with the
corresponding `registry/cache/<registry>/<name>-<version>.crate`. Its full archive
SHA256 must match the selected lock record before signatures are scanned. A
wrong cached archive is skipped; a later matching cache may be used. This checks
the cached archive identity, **not** the integrity of every extracted source
file. The signatures reflect the current local source, including local edits.
No network request, fetch, compilation or model request is launched by the tool.

Lock admission reads at most 4 MiB plus one refusal byte. Registry enumeration
admits at most 256 entries. Archive hashing streams with a 16 KiB buffer, at most
64 MiB plus one byte per candidate and a shared 256 MiB read budget. These are
reader bounds, not a whole-process memory or elapsed-time guarantee. Cargo home
layout is an implementation detail and unsupported layouts are refused.

The existing source-signature heuristic now admits at most 4,096 walker entries,
512 Rust files and sixteen levels. Source reads share 8 MiB, with 256 KiB per
file and one refusal byte inside the shared allowance. Links are not followed
by the walker. Unreadable, oversized or invalid UTF-8 source is omitted explicitly.
At most 201 matching signatures are retained to detect the existing 200-item
display cap. A signature over 4 KiB or a 256 KiB signature-output limit stops the
scan. The `scan_incomplete` flag and notice identify incomplete coverage;
`truncated` also identifies the item cap. An empty incomplete result is not a
claim that the package exposes no API. This remains a line scanner, without
rustdoc type resolution or conditional-compilation evaluation.

Real tool tests use child processes with private Cargo homes. The exact current
fixture produces nine failures and two controls on the unchanged selector; the
candidate passes all eleven. Three existing API tests still use the ordinary
local Cargo cache. The [CLI/daemon probe](../xtask/probes/crate-identity/README.md)
checks model-visible ambiguity, explicit selection, a wrong same-version cached
archive and unchanged workspaces/caches. These controlled results are not a
model-quality measurement.

The audit followed Haico's error-aware build documentation hints at
`5b352f3c7e3b608e68ec49af65a66feb098e07d4`, `src/tools/build_notes.rs`.
Trustworthy existing API lookup is a prerequisite for that next feature; this
change does not add automatic hints. No Haico implementation was copied.

Cargo documents [lock identities](https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html)
and [its local archive/source cache](https://doc.rust-lang.org/cargo/guide/cargo-home.html).
The TOML parser is already in the workspace lockfile; this adds a direct use
without changing any resolved external package version.
