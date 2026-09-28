---
name: minimal-change
description: Use when scoping or reviewing an implementation diff to keep it limited to the requested outcome.
---

# Minimal change

Prefer the smallest coherent change that fully solves the problem. Minimize
unrelated behavior and review burden, not merely the number of changed lines.

Identify the behavior to change and the invariants to preserve. Reuse existing
facilities and established patterns when they satisfy those requirements.

- Do not rename unrelated symbols, reformat neighboring files or upgrade unrelated
  dependencies.
- Do not introduce a framework, configuration switch or generic abstraction for
  a single speculative use case.
- Do not keep a known broken path merely to make the patch shorter. All affected
  consumers and necessary regression coverage belong in the change.
- Preserve pre-existing user edits. Inspect the diff before deciding what is yours
  to remove; never reset the whole working tree to clean up your patch.

If the existing boundary prevents a safe fix, explain the specific obstruction
and the smallest wider change that resolves it. Include required migrations,
compatibility adjustments and tests. Defer independent cleanup explicitly.

Before finishing, inspect the entire diff, including generated files and lockfiles.
For each hunk, identify the requirement, defect or necessary integration it serves.
Remove only your own accidental edits. Report why any substantial expansion of
scope was necessary and how the preserved behavior was checked.
