---
name: solution-design
description: Use when a nontrivial implementation has competing approaches with meaningful tradeoffs.
---

# Solution design

Start from the problem, constraints, existing architecture and verification needs.
Do not manufacture alternatives for a straightforward change.

Compare the viable approaches, including extending the existing implementation
and doing nothing when those are real options. Eliminate anything that violates
a hard requirement before comparing preferences. For each remaining approach,
record:

- What changes, including data and control flow and public contracts.
- Benefits and costs tied to the actual workload and team constraints.
- Failure modes, compatibility effects and migration or rollback needs.
- Unknowns that could reverse the decision, and a bounded experiment to resolve
  them when evidence is worth the cost.

Prefer the least complex approach that meets the requirements. Do not assume
that a new service, abstraction or dependency is necessary for future growth.
Separate measured facts from estimates; a numeric score does not remove uncertainty.

Choose an approach with the reasons that decided it and the conditions under
which it should be reconsidered. Within the user's authorized task, proceed on
ordinary reversible engineering decisions. Ask only for unresolved product or
operational tradeoffs that need the user's judgment.

Produce an implementation sequence with independently checkable steps, a
verification plan and any rollout constraints. Use an ADR only when the decision
is durable enough to justify maintaining one.
