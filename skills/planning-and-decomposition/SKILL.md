---
name: planning-and-decomposition
description: Use when a multi-step task needs ordered, independently verifiable implementation slices.
---

# Planning and decomposition

Define the outcome and completion checks before splitting the work. Discover the
relevant code and constraints sufficiently to avoid a plan based on guessed files.

Decompose by observable behavior or a necessary dependency boundary. Each step
should have a concrete output, prerequisites and a way to verify it. Prefer
slices that leave the project usable over separate large phases of all interfaces,
all implementations and all tests.

Place high-impact unknowns early as bounded investigations or experiments. Give
each experiment a question, stopping condition and decision it will inform. Do
not convert all possible risks into mandatory work.

Order dependent changes explicitly. For data or public contract changes, account
for mixed versions and expand/migrate/contract phases. Keep independently useful
work separate from optional cleanup.

Identify parallelizable steps only when their inputs and file ownership permit
it; a plan is not permission to delegate or perform external actions. Include
integration verification after independently produced pieces meet.

Track completed, active and blocked steps with evidence. Update the plan when
new facts change scope rather than finishing obsolete steps for consistency.
Do not stop at the plan when implementation is authorized.

Return a short sequence with acceptance checks and consequential dependencies.
For a one-step change, execute directly instead of creating planning ceremony.
