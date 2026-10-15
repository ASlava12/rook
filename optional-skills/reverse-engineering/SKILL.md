---
name: reverse-engineering
description: Use when decompiling binaries, recovering class names or reconstructing buildable source.
version: 1.0.0
license: MIT
---

# Reverse engineering and source reconstruction

Establish the supplied artifacts, target build, platform and requested recovery:
readable decompilation, restored names/types, a buildable project, or behavioral
replacement. Work on hashed copies and retain the original. Inspect format,
architecture, endianness, runtime, ABI, imports, sections and accompanying assets
before executing anything. Run unfamiliar binaries in an isolated test environment
without user credentials or unrelated network access.

Choose the installed tool for the artifact: Ghidra or another native decompiler
for machine code, ILSpy for .NET assemblies, JADX for Android DEX/APK, and a
matching bytecode tool for other runtimes. Confirm its supported input and local
CLI/API before scripting it. Unity Mono assemblies and native IL2CPP builds need
different workflows; version-matched IL2CPP metadata describes types, not the
original managed method bodies. Missing equipment leaves a concrete analysis
plan and partial results; loading this skill installs no reverse-engineering suite.

Inventory PDB/DWARF/dSYM symbols, map files, source maps, obfuscation mappings,
RTTI, reflection metadata, exports, serializers, resources and embedded strings.
Verify that sidecars match this exact build before applying them. These may
recover original names. Without such evidence, propose meaningful semantic names
and label them inferred; compilation can erase names, comments and source layout.
Do not promise byte-for-byte original source from information that is absent.

Create a persistent analysis project and recovery ledger before large renames.
Identify entrypoints, module boundaries, callers/callees and data dependencies.
Reconstruct function signatures, calling conventions, object layouts, field
offsets, vtables, inheritance, ownership, callbacks and error paths from multiple
uses. Check important decompiler expressions against disassembly: signedness,
integer width, aliasing and optimized control flow can mislead. Fix types and
signatures, then regenerate pseudocode before drawing downstream conclusions.

Rename classes, functions, fields and parameters through the analysis database,
using stable addresses/tokens and aliases instead of text replacement. Distinguish
file offsets, module-relative virtual addresses and relocated runtime addresses.
For each rename/type hypothesis record the build hash, stable symbol ID, old and
new names, supporting references, confidence and unresolved alternatives. Prefer
specific supported names over invented domain stories. Keep the mapping exportable
and reapplicable to the same build; validate it again after an update.

Reconstruct a source tree with explicit interfaces, resources, dependencies and
build commands. Recover one executable slice, then expand across its dependencies.
Keep raw decompiler output separate from corrected source. Mark missing functions
and guessed behavior explicitly; a silent stub that returns success hides a gap.
Preserve ABI/layout, serialization, numeric behavior, error handling and observable
side effects needed for compatibility. A different toolchain may prevent identical
binaries even when behavior matches.

Compare the original and reconstruction on the same retained inputs in the test
environment. Cover ordinary and boundary values, malformed inputs, exceptions,
state transitions and resource loading. Compare outputs, file/protocol formats and
side effects, accounting explicitly for nondeterminism. Compilation alone proves
neither equivalence nor complete recovery. Report checked behavior and outstanding
functions/types/assets with evidence; use measured coverage rather than claiming
full recovery because every function has a guessed name.

Deliver the source/build instructions, analysis database or export, rename/type
ledger, reproducible comparison cases and remaining gaps. Keep essential findings
in workspace artifacts: a resource path returned by `load_skill` does not grant
access to protected Rook state.

Tool references: [Ghidra analysis training](https://ghidra.re/ghidra_docs/GhidraClass/Advanced/improvingDisassemblyAndDecompilation.pdf),
[ILSpy](https://github.com/icsharpcode/ILSpy), [JADX](https://github.com/skylot/jadx).
