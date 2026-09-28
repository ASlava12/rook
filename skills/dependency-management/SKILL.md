---
name: dependency-management
description: Use when adding, upgrading, removing or auditing direct and transitive dependencies.
---

# Dependency management

Establish why the dependency change is needed and whether existing facilities
satisfy it. Inspect manifests, lockfiles, feature flags, transitive paths and
supported toolchains before selecting a version.

For an update, consult current upstream release notes and authoritative advisories
for the affected version range. Separate an advisory match from demonstrated
reachability; record platform, feature and configuration conditions. Do not dismiss
a vulnerable component solely because one caller appears safe.

Evaluate maintenance, provenance, install/build scripts, package contents and
operational footprint. Check license requirements against the project's actual
distribution model and policy; identify unresolved legal questions without
inventing a compatibility verdict.

Use the ecosystem's package manager to update the lockfile. Keep unrelated updates
out unless resolution requires them, and explain that expansion. Preserve supported
runtime and compiler versions and check duplicate major versions or new native
dependencies where relevant.

Verify locked installation/build, impacted integration behavior and supported
platforms. Review the lockfile diff and generate or refresh an SBOM when the task
or delivery process requires it. An SBOM is an inventory, not a security verdict.

Report changed versions, important transitive effects, compatibility adjustments,
verification and remaining advisory/license uncertainty. Do not claim an audit
is current if its advisory source could not be consulted.
