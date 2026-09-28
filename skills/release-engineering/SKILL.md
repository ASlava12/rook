---
name: release-engineering
description: Use when preparing versioned artifacts, release notes or a staged rollout and recovery plan.
---

# Release engineering

Identify the release scope, source revision, target platforms, versioning policy
and consumers. Use ecosystem-specific release procedures already maintained by
the project; this skill supplies the cross-platform release reasoning.

Determine version changes from public compatibility, including runtime/toolchain
requirements and persisted data. Use SemVer only where it is the project's policy.
Write release notes around user-visible behavior, breaking changes and migration
actions rather than dumping commit messages.

Build from a known revision with locked inputs. Verify package contents, checksums,
signatures/provenance where used, installation and a representative smoke flow.
Test the packaged artifact, not only an unbundled development executable.

Plan publication order when artifacts depend on one another. Distinguish prepared,
uploaded, available and deployed states. A partial release needs an explicit
recovery decision; do not repeat a non-idempotent publish blindly.

For service rollout, choose an appropriate canary, rolling or blue/green strategy.
Define health signals, observation windows and stop criteria. Confirm that old
and new versions can coexist and that rollback remains compatible with data
already written. Otherwise define a forward-repair plan.

Prepare all reviewable artifacts before any approval required for publication.
Do not infer push, publish or deploy authorization from a request for a plan.
Report artifact identity, validation, actual publication/rollout state and recovery
instructions without claiming actions that were only prepared.
