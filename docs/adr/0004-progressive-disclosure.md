# 0004 — Skill cards and tool stubs by default

**Status:** accepted

## Context

The straightforward implementation puts every enabled tool's JSON Schema and every
skill's body into every request.

## Decision

Send a *catalog* of skill cards (name, version, description) and *stubs* of tool
schemas (name, description). Full bodies and schemas arrive on demand, via the
`load_skill` pseudo-tool and a schema fetch. This is the default; config can turn
it off.

## Why

[hermes-agent#6839](https://github.com/NousResearch/hermes-agent/issues/6839)
quantifies it: with 50+ tools, full schemas cost ~3,500–5,000 tokens on every call
regardless of need. The benchmark in that thread is the sharper point — on local
models, tool-formatted prompts processed at **134 tok/s against 1,230 tok/s** for
plain text with 8 tools. Roughly a 10× slowdown, paid every turn, to advertise
tools the turn will not use.

Since local models are a first-class target here, that is not a rounding error.

The test `the_catalog_is_one_card_per_name_and_stays_small` asserts a full catalog
costs under 100 tokens.

## Cost

An extra round trip when the model does need a body or a schema. Weaker models may
pick worse from a one-line description than from a full schema — which is why
`lazy_tools = false` exists, and why descriptions are held to describing the
*trigger* rather than the implementation.
