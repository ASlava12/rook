---
name: documentation
description: Use when writing or updating READMEs, API guides, ADRs, architecture docs or runbooks.
---

# Documentation

Identify the reader, task and source of truth. Inspect nearby documentation for
structure, language and terminology, and confirm behavior from source or runnable
examples. Do not turn an implementation guess into a documented guarantee.

Choose the smallest suitable document:

- README or tutorial for a successful first use and prerequisites.
- Reference for exact options, defaults, contracts and errors.
- ADR for context, chosen decision, alternatives and consequences.
- Architecture note for responsibilities, boundaries and important flows.
- Runbook for symptoms, diagnosis, authorized actions, stop conditions and recovery.

Make commands copyable and distinguish literal values from placeholders. State
working directory, required environment and consequential side effects. Use fake
credentials and safe example data. Do not execute destructive examples just to
check their formatting.

Explain why a non-obvious constraint exists. Keep volatile numbers, command syntax
and supported versions tied to a reproducible source instead of duplicating them
throughout the documentation.

Validate local links, paths and harmless examples. Check that the narrative
matches actual configuration and output; note examples not executed. Update
maintained translations or parallel references when the changed information is
shared, following project conventions.

Deliver documentation in the requested location and format with verification
evidence. Do not add a new documentation hierarchy for a small correction.
