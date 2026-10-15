---
name: clean-room-reimplementation
description: Use when recreating software behavior through a clean-room specification and independent implementation.
version: 1.0.0
license: MIT
---

# Clean-room reimplementation

Recreate the requested behavior through an explicit information boundary between
reference research and implementation. Establish the target version, compatibility
surface and permitted inputs before opening original code. Record the current role
and known exposure. A context that examined original implementation details remains
on the research/specification side; changing its role does not make it independent.

Define the contract: public API/ABI, CLI, file format, protocol, UI interactions or
other observable behavior. Specify required quirks, intentional differences and
out-of-scope features. Separate compatibility from copying original source,
private architecture or assets. Record provenance/reuse conditions for inputs and
dependencies. Process records do not remove licensing or other obligations.

## Research and specification

Use allowed documentation and controlled observations. Keep binaries,
source/decompilation, private names, research notes and tool databases on the
research side. Record any permitted source inspection. Express observable
requirements rather than translating internal code into prose instructions.

Describe inputs, outputs, types, byte layout, errors, state transitions, ordering,
numeric boundaries, encodings and relevant side effects. Distinguish observations,
documented rules and untested hypotheses. Retain public identifiers and wire
constants needed for interoperability; do not invent different API names just to
make source look unlike the reference. Do not prescribe private classes,
algorithms or control flow merely because the researcher saw them.

Create ordinary, boundary, invalid and stateful cases, using synthetic inputs where
possible. Record reference build/hash, setup, input, expected result, nondeterminism
and evidence ID. Exclude credentials, unrelated data, implementation text and
nonessential original assets from the handoff. For UI specify interactions/layout
and create independent assets; transfer screenshots only when the contract permits.
Retain restricted raw evidence on the research side.

Freeze a reviewed handoff: specification, approved fixtures, provenance and open
questions. Manifest exact relative paths, hashes and revision. Link requirements
to evidence/cases without internal implementation excerpts. Clarifications update
the specification and manifest, preserving previous revisions.

## Independent implementation

Use a fresh implementer context with only the approved handoff, the new project's
own code and allowed toolchain/dependency documentation. Verify the actual context
and tool/file reachability. Do not inherit research messages, source-bearing memory,
retrieval indexes, terminal logs or the original repository and its `.git` history.
A branch, copied directory, subagent label or instruction to ignore files is not
access isolation. Use an actual available isolation mechanism; do not invent a Rook
flag or assume that delegation omits history. Delegate only within existing authority.

If the boundary cannot be established, deliver the handoff and mark independent
implementation unverified. A same-context rewrite is not a verified clean-room
result; skill instructions do not enforce isolation. On discovered exposure,
quarantine affected work, retain the audit record and resume from the last clean
handoff in a fresh implementation context. Preserve exposure history.

Implement from the contract with independently chosen structure and algorithms.
Track requirements/gaps and build runnable slices with reproducible commands.
Route questions through the recorded handoff channel; return clarified contracts
and synthetic cases, not original source fragments. Do not browse the reference
implementation to fix a failing test.

## Verification and delivery

Run reference and candidate on the same inputs through a separate comparison
harness. Keep execution state separate. Compare contract-relevant outputs, errors
and side effects; normalize only declared nondeterminism. Include held-out cases
and report failures/unknowns. Passing cases establishes tested compatibility, not
universal equivalence or independent provenance.

Deliver source, build commands, frozen handoff, provenance/exposure record and
requirement-to-test results. Report isolation and compatibility evidence separately,
including unknowns. Preserve scope and authorization; missing/declined access leaves
a partial specification rather than repeated requests or invented observations.

Background: [ReactOS contribution/provenance guidelines](https://reactos.org/intellectual-property-guideline/).
