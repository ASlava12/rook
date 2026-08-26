# 0007 — The web UI is one hand-written file with no bundler

**Status:** accepted

## Context

The web UI needs to render sessions, transcripts, skills with their version history
and storage statistics. The default answer is React or Svelte with a bundler.

## Decision

One hand-written `web/dist/index.html`: vanilla JavaScript, no dependencies, no
build step, embedded into `rookd` at compile time with `rust-embed`.

## Why

**A JavaScript toolchain would become a build prerequisite on four platforms.**
That is precisely where cross-platform support decays in this ecosystem — Codex's
[FreeBSD break](https://github.com/openai/codex/issues/13802) came from its npm
wrapper, and OpenCode shipped a
[Bun segfault on Windows](https://github.com/anomalyco/opencode/issues/33742) that
forced users to downgrade. `cargo build` must remain the whole story.

**The UI is a read-only viewer.** Lists, tables, a detail pane, a few hundred lines
of DOM construction. A framework would be more code, not less.

**It stays honest about size.** The whole UI is ~12 KB and is visible in one file.

## Cost

No component model, no type checking, no hot reload. If the UI ever becomes
interactive enough to need those, this decision should be revisited — and that is a
better trigger than adopting the toolchain up front.
