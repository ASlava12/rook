---
name: software-development
description: Use when implementing application behavior within an established codebase.
---

# Software development

Confirm the behavior to change, its entry point, callers, project conventions and
acceptance checks. Read a nearby implementation before introducing a new pattern.

Implement a coherent vertical slice, including error paths and affected consumers.
Use idiomatic facilities supported by the project's actual language and runtime
versions. Follow existing ownership and dependency boundaries.

- Make invalid states difficult to represent where that simplifies callers.
- Keep side effects at explicit boundaries. Define ownership and cleanup for
  files, connections, tasks and other resources.
- Preserve error context and distinguish absence, invalid input, cancellation and
  external failure. Do not turn every failure into a successful empty value.
- Bound growing inputs, collections and background work. Apply limits while data
  arrives, not after allocating the entire input.
- Reuse established code without coupling unrelated concepts merely because two
  snippets look similar. Apply SOLID, DRY and KISS as tradeoff guides, not quotas
  for interfaces, classes or abstractions.
- Evaluate algorithmic cost against realistic input sizes; measure uncertain hot
  paths before optimizing them.

Make the smallest change that completes the behavior, with tests at the boundary
where a regression would be observable. Run relevant project checks and inspect
the final diff. Report the changed behavior, evidence and unresolved limitations;
do not equate compilation with a completed feature.
