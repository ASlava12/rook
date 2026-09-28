---
name: problem-analysis
description: Use before a code change when the goal, current behavior or constraints need to be established.
---

# Problem analysis

Turn the request into a testable problem before editing. Scale the analysis to
the task: a small correction needs a few notes, not a design document.

Establish these facts from the request and available evidence:

- **Goal:** the observable outcome and who needs it.
- **Constraints:** scope, compatibility, resources and explicit non-goals.
- **Existing behavior:** what happens now, with a source location or reproduction.
- **Unknowns:** unanswered questions; distinguish assumptions from observations.
- **Affected components:** entry points, state, callers and external contracts.
- **Edge cases:** boundaries and failure paths that could change the result.
- **Risks:** concrete ways this change could break existing behavior.
- **Verification strategy:** how each important outcome will be demonstrated.

Inspect available project evidence before asking the user for facts the repository
can answer. Ask only when an unresolved choice materially changes the outcome;
continue independent work while it is pending. For a reversible implementation
choice, state a reasonable assumption and proceed within the requested scope.

Do not treat the user's proposed implementation as proof of the cause. Separate
the desired behavior from the suggested mechanism. Revise the problem statement
when evidence contradicts it.

Finish with a short problem statement, acceptance checks, and any consequential
unknowns. Carry those checks into implementation and final verification.
