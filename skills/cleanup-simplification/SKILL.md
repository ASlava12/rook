---
name: cleanup-simplification
description: Use when implementation is complete and temporary code or unnecessary complexity needs removal.
---

# Cleanup and simplification

Review the implementation against the original outcome and the current diff.
Identify temporary diagnostics, experimental branches, unused imports, abandoned
helpers and scaffolding introduced during this task.

Remove your own leftovers when they no longer support a requirement. Consolidate
duplication only when it represents the same responsibility. Replace unnecessary
indirection or configuration with the existing straightforward mechanism when
behavior and compatibility are preserved.

Check references before deleting apparently unused code. Public exports, reflection,
plugins, generated code, configuration strings and external consumers may not
appear in ordinary local call searches. Uncertain reachability is not proof that
an API is dead.

Keep cleanup within the requested scope. Do not rename unrelated symbols, reformat
whole files or remove user-created scratch work to make the repository look tidy.
Do not delete comments that preserve a non-obvious invariant or compatibility
reason. Track independent improvement opportunities separately.

After cleanup, run checks for any changed behavior and inspect the final diff.
Verify that temporary feature paths are either removed or have a concrete lifecycle
reason to remain. Do not weaken error handling or testing to reduce line count.

Report material simplifications and the evidence that the requested behavior
remains intact. A smaller diff is useful only if it still completely solves the
task.
