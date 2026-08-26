# Platforms

Linux, macOS, Windows and FreeBSD are supported targets. `cargo xtask targets`
prints the current matrix.

| target | platform | tier |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Linux | 1 — built + tested |
| `aarch64-unknown-linux-gnu` | Linux | 1 |
| `x86_64-unknown-linux-musl` | Linux (static) | 1 |
| `x86_64-apple-darwin` | macOS | 1 |
| `aarch64-apple-darwin` | macOS | 1 |
| `x86_64-pc-windows-msvc` | Windows | 1 |
| `x86_64-unknown-freebsd` | FreeBSD | 1 — tested in a VM |
| `aarch64-pc-windows-msvc` | Windows | 2 — built only |
| `aarch64-unknown-freebsd` | FreeBSD | 2 — built only |

## What actually constrains portability

Not Rust. The survey found FreeBSD support in other agents breaking through
*distribution*, not language: Codex's CLI is Rust and its
[FreeBSD break](https://github.com/openai/codex/issues/13802) came from the npm
wrapper restricting the platform list.

Two constraints follow, and both are architectural.

**No runtime.** Rook ships two static binaries. There is no Node, Bun, Python or
Docker to be missing or to segfault on a platform its maintainers do not test.

**No build-time toolchain beyond Rust and a C compiler.** The web UI is one
hand-written HTML file with no bundler, because adding a JavaScript toolchain makes
`npm` a prerequisite for building on every platform — and FreeBSD is exactly where
that goes wrong.

## The two C dependencies

Measured, not assumed. Cross-compiling the workspace to
`x86_64-unknown-freebsd` from macOS: every pure-Rust dependency compiles, and
exactly two fail:

- **`zstd-sys`** — the vendored zstd C sources.
- **`ring`** — the TLS crypto provider, C plus assembly.

Both fail for the same reason: the host `cc` has no FreeBSD sysroot. Neither is a
FreeBSD problem — both build fine *natively* on FreeBSD, which has a working
compiler.

That is why CI tests FreeBSD in a real VM
([`vmactions/freebsd-vm`](https://github.com/vmactions/freebsd-vm)) instead of
cross-checking it. A cross-check would be cheaper and would skip the only two
dependencies where an "unsupported platform" regression could actually land.

**Why keep zstd**, given the cost: dictionary compression is where the storage
ratio comes from — 20.7× against 4.3× for the pure-Rust alternatives that offer no
dictionary support. Losing that would gut the central claim of the design.

**Why `ring` rather than `aws-lc-rs`**: `rustls` defaults to `aws-lc-rs`, which
needs cmake and a full C toolchain and is the single most common cross-compilation
blocker in the ecosystem. `ring` is smaller and cross-compiles far more readily.
This is set explicitly in `rook-llm`'s dependency features, not left to defaults.

## Platform-specific behaviour

Where behaviour genuinely differs, it is handled in one place rather than sprinkled
through the code.

**Paths.** `$ROOK_HOME`, else `~/.rook` — `%USERPROFILE%\.rook` on Windows, with a
`HOMEDRIVE`/`HOMEPATH` fallback. See [`paths.rs`](../crates/rook-core/src/paths.rs).

**Shell.** `/bin/sh -c` on Unix, `cmd /C` on Windows. `cmd` rather than PowerShell
because it is always present; a skill that needs PowerShell invokes it explicitly.

**Userland.** Derived from the OS: `gnu` on Linux, `bsd` on macOS and the BSDs,
`msvc` on Windows. It is exposed to skills as a `requires`/`variants` predicate and
stated in the system prompt, because GNU-versus-BSD tool differences are the most
common cross-platform failure in agent transcripts. macOS and FreeBSD share a
variant automatically, which is the point.

**Path containment** is lexical — `..` is normalised without touching the
filesystem — so it behaves identically on case-insensitive filesystems and on
Windows, and works for paths that do not exist yet.

## Building on FreeBSD

```sh
pkg install -y rust
cargo build --release
```
