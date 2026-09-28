---
name: code-review
description: Use when reviewing a diff for actionable correctness, security, compatibility or test gaps.
---

# Code review

Determine the intended behavior and review range. Read the diff, surrounding
implementation, affected callers and relevant tests. Respect a read-only review
request; do not start fixing findings unless that work is authorized.

Follow changed data and control paths. Prioritize:

1. Violated invariants, wrong results, lost data and unhandled failure paths.
2. Authorization and trust boundaries, resource exhaustion and exposed secrets.
3. Concurrency, cancellation, lifecycle and partial-failure behavior.
4. Public API, configuration, persisted data and mixed-version compatibility.
5. Measurable performance risks and checks missing for important behavior.

Use a minimal reproduction or existing test where it can resolve uncertainty.
Do not report a merely possible issue without showing a feasible triggering
condition and the path to the consequence. Separate pre-existing defects from
regressions introduced by the patch.

For each finding, give severity, precise location, triggering conditions, user
impact and a concise correction direction. Mark uncertainty explicitly. Avoid
style preferences already handled by tooling and unrelated refactors masquerading
as blockers.

Lead the result with actionable findings, ordered by impact. If none are found,
say so and name the inspected scope and verification gaps. Do not convert limited
review evidence into a guarantee that the patch is safe or fully tested.
