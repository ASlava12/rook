---
name: build-systems
description: Use when changing build graphs, toolchains, generated artifacts or build reproducibility.
---

# Build systems

Identify the build entry point, dependency graph, toolchain, targets, generated
sources and external inputs. Read the project's existing Make/CMake/Cargo/npm/
Gradle or other build conventions before adding a parallel path.

Make inputs and outputs explicit. Generated artifacts must depend on their true
sources, and clean/incremental builds must agree. Avoid dependencies on an undeclared
working directory, globally installed package, timestamp, locale or network fetch.

Use the ecosystem's lock and toolchain mechanisms. Distinguish reproducible
dependency selection from bit-for-bit artifact reproducibility. For cross builds,
separate host tools from target artifacts and check target-specific flags and ABI.

Keep cache keys tied to meaningful inputs and make cache misses correct. Do not
delete all caches as the permanent solution to an invalid dependency graph.
Treat dependency build scripts and downloaded generators as code execution.

Validate in an isolated output directory: perform a clean build, an incremental
build after a relevant input changes, and an unchanged build when incremental
behavior matters. Check packaged/generated outputs as well as compilation.
Do not remove user artifacts from shared build directories without need.

Report the corrected dependency/toolchain behavior, reproducible commands and
checks. Claim hermetic or reproducible builds only to the extent that undeclared
inputs and output identity were actually tested.
