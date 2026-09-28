---
name: compatibility
description: Use when a change may affect supported clients, platforms, runtimes, formats or mixed versions.
---

# Compatibility

Build the relevant support matrix from project policy and actual consumers.
Separate source, binary/ABI, wire, persisted-data, configuration and behavioral
compatibility. An unchanged function signature does not guarantee unchanged behavior.

Identify public and implicit contracts: defaults, error codes, ordering, enum
values, serialization, paths, exit codes and CLI output consumed by scripts.
Determine how old/new readers and writers interact during a staged rollout.

For platforms, inspect filesystem case/encoding, path rules, shell utilities,
line endings, process/signal behavior and native dependencies as relevant. For
browsers/runtimes, check supported features against the project's minimum versions.
Prefer capability detection over guessed platform labels when feasible.

Use additive evolution, adapters or a deprecation period where they preserve
supported consumers. Do not retain unlimited obsolete paths without a policy or
remove a compatibility branch merely because the current machine does not use it.

Verify representative matrix edges, especially minimum supported versions and
old/new interoperability. Use fixtures produced by older releases for persisted
formats where available. Cross-compilation proves less than runtime testing;
record the distinction.

Deliver the affected contracts, compatibility measures, breaking changes if
authorized, migration guidance and test matrix results. Mark unsupported or
untested environments explicitly rather than claiming universal portability.
