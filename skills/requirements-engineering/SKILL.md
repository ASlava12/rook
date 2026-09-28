---
name: requirements-engineering
description: Use when a feature request needs explicit behavior, scope and acceptance criteria before design.
---

# Requirements engineering

Identify the actor, goal, triggering event and observable result. Separate user
needs from proposed implementation details and from assumptions.

Specify the normal path, alternative paths and meaningful boundaries: absent or
invalid input, permission differences, repeated actions, partial failure and
state transitions. Include measurable nonfunctional constraints only where the
request or evidence supports them. Do not invent latency targets or compliance
requirements.

Write acceptance criteria that can be checked from outside the implementation.
Use concrete examples of initial state, action and expected result when prose
could admit incompatible interpretations. Name non-goals and compatibility
obligations so implementation does not quietly grow the feature.

Resolve contradictions by consulting project behavior and the user request.
Ask about unresolved product decisions that materially change the result;
record reversible assumptions for ordinary details and continue useful work.
Do not use an exhaustive questionnaire when a small decision is missing.

Keep each requirement traceable to its source and intended check. For a changing
request, update affected criteria and identify which completed work needs revision.

Deliver a concise behavioral specification with acceptance criteria, constraints,
non-goals and open decisions. Keep the document proportional to the task; a small
feature can fit in a few examples.
