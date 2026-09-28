---
name: refactoring
description: Use when improving code structure while preserving observable behavior.
---

# Refactoring

Define the structural problem and the behavior that must remain invariant. A
feature change mixed into the same patch is not behavior-preserving refactoring;
separate the intent and verification when both are requested.

Find callers, extension points, serialization formats and implicit contracts
before moving or renaming code. Check reflection, configuration strings, generated
bindings and plugins where ordinary reference search may miss a consumer.

Establish existing behavior with focused tests or characterization checks at
public boundaries. Avoid snapshots of unstable details and tests that simply
repeat the implementation.

Make small transformations: extract a responsibility, reduce a dependency,
introduce an explicit boundary or consolidate genuinely shared behavior. Keep the
project buildable between meaningful steps and run the checks that can detect
behavior changes after each risky transformation.

Prefer local simplification over a new hierarchy. Duplication is not sufficient
reason to merge code whose responsibilities will evolve differently. Do not
combine broad formatting, unrelated renaming and dependency upgrades with the
structural change.

Inspect the resulting dependency flow and public diff. Confirm that errors,
ordering, resource ownership and performance-sensitive behavior remain compatible.
If behavior must change, name and justify it rather than describing the whole
patch as a refactor.

Report the structural improvement, preserved invariants and verification evidence.
