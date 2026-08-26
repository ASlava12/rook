---
name: rust-release
description: Cut a release of a Rust workspace — version bump, changelog, tag, and the pre-flight checks that catch a bad release before it is published.
version: 1.0.0
license: MIT
keywords: [rust, cargo, release, versioning]
requires:
  language:
    rust: ">=1.85"
  tool:
    cargo: ">=1.85"
    git: ">=2.30"
---

# Cutting a Rust release

Requires Rust 1.85 or newer: `edition = "2024"` and workspace inheritance are
assumed throughout.

## 1. Pre-flight, before touching a version number

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo publish --dry-run -p <crate>     # per publishable crate, in dependency order
```

Confirm the working tree is clean and you are on the release branch. A dirty tree
at this point means the tag will not describe what was published.

## 2. Version

Bump `[workspace.package] version` in the root `Cargo.toml` when crates version
together. Then:

```sh
cargo update --workspace     # refresh Cargo.lock with the new versions
```

Commit `Cargo.lock`. For a workspace that produces binaries, the lockfile is part
of the release.

## 3. Changelog

Group by what changed for a user, not by commit:

```sh
git log --oneline --no-merges <last-tag>..HEAD
```

Call out breaking changes explicitly, including an MSRV bump — `rust-version` in
`Cargo.toml` moving is a breaking change for someone.

## 4. Tag and publish

```sh
git tag -a v<x.y.z> -m "v<x.y.z>"
git push origin main --follow-tags
cargo publish -p <crate>     # dependency order; each must be live before its dependents
```

## Traps

- **Publishing out of order fails** and leaves a partial release. Publish leaves
  first and wait for the index between crates.
- **`cargo publish` ignores your `.gitignore`** and uses `include`/`exclude` in
  `Cargo.toml`. Check `cargo package --list` before publishing something large.
- **Yanking is not deleting.** A yanked version stays resolvable for existing
  lockfiles. Fix forward with a patch release.
