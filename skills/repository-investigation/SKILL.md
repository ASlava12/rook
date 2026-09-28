---
name: repository-investigation
description: Use when entering an unfamiliar repository or tracing the code and tests affected by a change.
---

# Repository investigation

Build an evidence-backed map of the relevant behavior, not an inventory of every
file. Before proposing edits:

1. Read applicable project instructions and identify build and test entry points.
2. Find the feature's entry point: command, route, event handler, exported API or
   scheduled job. Follow its configuration and data to the implementation.
3. Find a similar implementation and its conventions: error handling, naming,
   ownership, dependency direction and test placement.
4. Locate callers, tests and external contracts. Trace at least the dependency
   path that the proposed change would affect.
5. Read the actual source spans that support the map. A name match or inferred
   graph edge is a lead, not evidence of runtime behavior.

Use available structural indexes when useful, checking their freshness against
the working tree. Otherwise use bounded file discovery and symbol searches such
as `rg --files` and `rg`. Exclude generated and vendor trees unless they own the
behavior. Do not install indexing tools or scan unrelated projects just to orient.

Stop exploration when the entry point, implementation, consumers, conventions
and verification path are known well enough for the task. If a link is unresolved,
name it instead of filling it in from assumptions.

Return relevant paths and symbols, the observed flow, likely edit locations and
the checks that exercise them. Do not modify code during a read-only inquiry.
