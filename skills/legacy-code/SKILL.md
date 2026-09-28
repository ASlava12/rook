---
name: legacy-code
description: Use when changing poorly understood code with weak tests, implicit contracts or obsolete dependencies.
---

# Legacy code

Find the behavior's entry points, consumers, state and operational constraints.
Treat comments and old documentation as leads; confirm them against observed
behavior, source and history. Identify implicit file, timing and error contracts.

Build a narrow characterization harness around the boundary being changed. Capture
representative successful and failing cases. Distinguish behavior that users rely
on from a known defect that the task is intended to correct; do not preserve the
defect merely because a characterization test records it.

Introduce seams only where they make the requested change testable: adapters for
external systems, explicit inputs for hidden state or a small interface around an
unstable dependency. Avoid a comprehensive rewrite before value can be verified.

For larger replacement, move one flow at a time behind a stable boundary using a
strangler approach when appropriate. Compare outputs or shadow safely, ensuring
shadow paths cannot duplicate external side effects. Define ownership while old
and new code coexist and conditions for removing the old path.

Keep environment and dependency upgrades separate when possible, since simultaneous
behavior and toolchain changes complicate diagnosis. Record unavoidable coupling.

Verify the requested outcome and characterized compatibility. Deliver the change,
new understanding, supporting checks and residual uncertainty. Leave a concise
explanation of non-obvious contracts near their maintenance boundary.
