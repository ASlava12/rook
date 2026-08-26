# 0002 — redb + content addressing + zstd dictionaries, not SQLite

**Status:** accepted

## Context

Agent history is enormously redundant and grows without limit unless something
stops it. SQLite is the obvious default and what Codex uses.

## Decision

A redb index holding metadata, session logs, refs and inlined small objects; a
content-addressed object store keyed by blake3; zstd compression with dictionaries
trained per object kind.

## Alternatives rejected

**SQLite + FTS5.** The most tooling, the most familiar. Rejected on the evidence:
Codex's SQLite path produced [a 138 MB database with an 80 MB WAL from trace logs
alone](https://github.com/openai/codex/issues/30236), and a companion thread
[extrapolated the write rate to ~640 TB/year](https://github.com/openai/codex/issues/28224).
None of that is inherent to SQLite — but a design whose safety depends on getting
WAL configuration and log rotation right is a design that will be got wrong. redb
is a single-writer embedded B-tree with no separate WAL to grow unbounded.

**A git-like object store.** Native versioning, easy sync. Rejected as too much
code before the first working result: a custom index and a garbage collector to
write and debug, for benefits that refs over a CAS already provide.

## Why dictionaries specifically

A 400-byte JSON message compressed alone barely shrinks; zstd never sees enough
context to model it. Trained on a few hundred messages of the same shape it becomes
a few dozen bytes. Measured end to end: **20.7× with dictionaries against 4.3×
without** (`cargo xtask compaction`). Every object records the codec it was written
with, so retraining never invalidates history.

## Cost

- One writer process at a time — see [0006](0006-single-writer-store.md).
- No SQL. Ad-hoc queries need `rook … --json` piped into `jq`.
- Fewer third-party inspection tools, which is part of why the CLI, TUI and web UI
  all exist.
- zstd vendors C, which constrains cross-compilation. Judged worth it: dropping
  dictionaries would cost roughly 5× the ratio.
