# 0001 — Rust for every component

**Status:** accepted

## Context

The requirement said Rust, then floated Go for the agent itself on the grounds of
compactness. Both are defensible. The reference implementations are split: Codex's
CLI and goose are Rust; opencode, cline, OpenClaw are TypeScript; hermes, OpenHands,
Agent Zero are Python.

## Decision

Rust for everything: store, engine, tools, daemon, CLI and TUI.

## Why

**A long-lived daemon is the wrong place for a GC.** Rook is meant to sit resident
holding an index and a skill catalog. The clearest storage failure in the survey is
[OpenCode's memory megathread](https://github.com/anomalyco/opencode/issues/20695) —
RSS reaching 1–2 GB, with the maintainers shipping automatic heap snapshots to
diagnose it. Predictable memory is worth more here than fast compiles.

**One toolchain, not two.** A Rust core with a Go agent means duplicated types, an
FFI or IPC boundary in the hottest path, and two build matrices across four
platforms. The compactness argument for Go does not survive that.

**The ecosystem fits.** `redb`, `zstd`, `blake3`, `ratatui`, `axum` are all
first-rate and mostly pure Rust — which is what makes the FreeBSD target realistic
(see [0007](0007-no-js-build-step.md) and [platforms.md](../platforms.md)).

**Go's real advantage was not the deciding one.** Compile speed and simplicity are
genuine, and cross-compilation to FreeBSD is easier in Go — but the actual FreeBSD
breakages in the survey came from runtime distribution, not from the language.

## Cost

Slower compiles. A steeper contribution curve. Cross-compiling to a foreign OS
needs a sysroot for the two C dependencies, which Go would have avoided.
