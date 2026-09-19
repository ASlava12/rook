# Platforms

[English](platforms.md) · [Русский](ru/platforms.md)

Linux, macOS, Windows and FreeBSD are supported targets. `cargo xtask targets`
prints the current matrix.

| target | platform | ci |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Linux | tested on `ubuntu-latest` |
| `aarch64-apple-darwin` | macOS | tested on `macos-latest` |
| `x86_64-pc-windows-msvc` | Windows | tested on `windows-latest` |
| `x86_64-unknown-freebsd` | FreeBSD | tested in a VM |
| `aarch64-unknown-linux-gnu` | Linux | compiled |
| `x86_64-unknown-linux-musl` | Linux (static) | compiled |
| `aarch64-unknown-linux-musl` | Linux (static) | compiled |
| `x86_64-apple-darwin` | macOS | compiled |
| `aarch64-pc-windows-msvc` | Windows | best effort |
| `aarch64-unknown-freebsd` | FreeBSD | best effort |

*Tested* means a CI job runs the whole suite there. *Compiled* means a job builds
it and nothing more — a cross-check never links against the target's libc, which
is the entire reason FreeBSD gets a VM instead. *Best effort* means no hosted
runner offers it; the code is written for it, and that is the extent of the claim.

Each row is backed by a string in [`ci.yml`](../.github/workflows/ci.yml) that backs
it, and `cargo test -p xtask` fails if that string is missing — so a row cannot
outlive the job that justified it, which is how this table would otherwise rot.

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
ratio comes from — 37.1× against 8.4× for the pure-Rust alternatives that offer no
dictionary support. Losing that would gut the central claim of the design.

**Why `ring` rather than `aws-lc-rs`**: `rustls` defaults to `aws-lc-rs`, which
needs cmake and a full C toolchain and is the single most common cross-compilation
blocker in the ecosystem. `ring` is smaller and cross-compiles far more readily.
This is set explicitly in `rook-llm`'s dependency features, not left to defaults.

## Platform-specific behaviour

Where behaviour genuinely differs, it is handled in one place rather than sprinkled
through the code.

**Paths.** `$ROOK_HOME`, else `~/.rook` on unix and `%LOCALAPPDATA%\rook` on
Windows. Local rather than Roaming: a roaming profile is copied to a server at
every logon, and this directory holds sessions, caches and downloaded language
servers — gigabytes nobody asked to have synchronised.

It was `%USERPROFILE%\.rook`, which is a unix habit wearing a Windows path and
was reported as one. An install that already has that directory keeps using it:
moving gigabytes under somebody to tidy a path is not a trade worth making, and
pointing them at an empty new one would read as the agent having forgotten
everything. `rook doctor` says which of the two is in use.

`ROOK_CONFIG_DIR` moves `config.toml` alone, for a configuration kept in a
dotfiles repository. `secrets.toml` deliberately does not follow it — it is
nothing but credentials, and the directory somebody points this at is by
construction one they sync or commit. See
[`paths.rs`](../crates/rook-core/src/paths.rs).

**Shell.** `/bin/sh -c` on Unix, `cmd /C` on Windows. `cmd` rather than PowerShell
because it is always present; a skill that needs PowerShell invokes it explicitly.

**Pasting.** A terminal delivers a pasted newline as the Enter key, so a pasted
paragraph used to go to the agent one line at a time. On unix both front ends
ask the terminal to bracket a paste, and the whole of it arrives as one piece.
On Windows the libraries reading the console have never heard of the bracket,
so the same paste arrives as keystrokes, and the tell is what is queued: a hand
puts tens of milliseconds between keys, a paste is queued whole. The window
reads the keys that arrived together as one run and judges the run — a marker
at the front, or a newline with text after it, makes it a paste that goes into
the box and is not sent, and anything else is the typing it was. The line
editor asks, when Enter arrives, whether typing is already queued behind it,
and takes the Enter as a newline when it is; the last newline of a paste has
nothing behind it and sends, as a shell would. See
[`paste.rs`](../crates/rook-cli/src/paste.rs) and `typing_is_already_queued`
in [`rook-contain`](../crates/rook-contain/src/lib.rs).

**Userland.** Derived from the OS: `gnu` on Linux, `bsd` on macOS and the BSDs,
`msvc` on Windows. It is exposed to skills as a `requires`/`variants` predicate and
stated in the system prompt, because GNU-versus-BSD tool differences are the most
common cross-platform failure in agent transcripts. macOS and FreeBSD share a
variant automatically, which is the point.

**The local network, on macOS.** Access to it is granted one application at a
time, and an application that has not been granted it is refused with
`EHOSTUNREACH` — the error a missing route gives, immediately, with no packet
sent and nothing written to any log. A model server on the same subnet is
therefore unreachable in a way that looks exactly like a network fault, and the
obvious check makes it worse: `curl` is Apple's own binary and is not subject to
the rule, so it reaches the address from the same shell in the same second.

The permission belongs to the application the process is attributed to — the
terminal, not the binary — and it is decided when that application starts, so
turning it on does not affect a terminal that is already running. It is under
System Settings → Privacy & Security → Local Network, and it takes a restart of
the terminal. The advice for that error says so where the address is on this
network; `advice` in [`rook-llm`](../crates/rook-llm/src/lib.rs) is where.

**Path containment** is lexical — `..` is normalised without touching the
filesystem — so it behaves identically on case-insensitive filesystems and on
Windows, and works for paths that do not exist yet.

## Building on FreeBSD

```sh
pkg install -y rust
cargo build --release
```

## Containing a command

`run_command` is contained by the platform where the platform can, and the
result of every command says what was applied.

| Platform | Containment | Limits |
|---|---|---|
| macOS | Seatbelt (`sandbox-exec`) | none known; deprecated by Apple, present in every release |
| Linux | Landlock, unprivileged | network only from kernel 6.7, and TCP only — never UDP, so never DNS |
| FreeBSD | none yet | Capsicum's capability mode breaks a shell; jails need root |
| Windows | a low integrity level, through a launcher that is `rook` itself | the network is never restrained; the workspace and a scratch directory of rook's own are labelled low, which persists |

`[sandbox] isolate = "required"` refuses to run a command where the table says
none; `auto`, the default, runs it as it is and says so.
