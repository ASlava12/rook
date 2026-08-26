# 0003 — Adopt the Agent Skills format, extend it in free keys

**Status:** accepted

## Context

Skills need a format. Inventing one is tempting because the requirements here —
version constraints on OS, language and tooling — are not in any existing spec.

## Decision

Read the Agent Skills format (`SKILL.md`, YAML frontmatter, `name` and
`description` required) unchanged. Add `requires`, `variants` and `supersedes` in
keys the specification leaves free. Preserve unknown frontmatter fields verbatim.

## Why

The format is settled: open-sourced by Anthropic in December 2025, adopted by
OpenAI and Google, governed under the Agentic AI Foundation, and deferred to by
Agent Plugins 1.0 for the skill format itself. goose has an
[open issue to adopt Agent Plugins](https://github.com/aaif-goose/goose/issues/11043).
A fourth format would strand every skill written for Rook.

Preserving unknown fields is what makes this two-way: a skill carrying another
agent's keys round-trips through Rook unchanged. There is a test for it.

## The extensions, and why they are not in the spec

`requires` gates a whole skill on the environment; `variants` swaps only the body.
Both exist because a local agent runs on the user's actual machine, where GNU
versus BSD `sed`, Rust 1.75 versus 1.85, and Docker 24 versus 27 are real and
silent failure modes. A hosted agent in a fixed container does not have this
problem, which is presumably why the spec does not address it.

## Cost

Another agent reading a Rook skill ignores `requires` and gets the default body —
which may be the wrong one for its platform. That is strictly better than a skill
it cannot parse at all, and it is the reason the extensions sit in free keys rather
than changing required ones.
