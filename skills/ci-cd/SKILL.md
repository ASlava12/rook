---
name: ci-cd
description: Use when changing build, test or deployment pipelines and their trust boundaries.
---

# CI/CD

Map triggers, jobs, dependencies, permissions, caches, artifacts and deployment
environments. Distinguish trusted branch builds from untrusted contributions.
Check the repository's actual runner and pipeline syntax/version.

Ensure required checks execute for the events and paths they claim to cover.
Inspect conditional jobs, skipped results and failure propagation. A successful
wrapper job must not hide a failed subprocess or a matrix member.

Use locked dependency resolution and explicit toolchains. Key caches from relevant
inputs and keep them disposable; a warm cache must not be required for correctness.
Build an identified artifact once and promote that artifact across environments.

Restrict token permissions and secret availability. Do not run untrusted code in
a privileged workflow context. Pin external automation by the project's provenance
policy, and preserve signing/attestation boundaries when publishing artifacts.

Define concurrency and cancellation carefully: canceling a test is different from
interrupting a migration. Deployment jobs need clear sequencing, health checks,
stop conditions and a recovery path. Pipeline changes do not themselves authorize
a production release.

Validate syntax and exercise representative triggers, including an intentional
failure in an isolated check when needed to verify gating. Check cache misses and
artifact consumption. Report which paths ran and which still require CI evidence;
local parsing alone does not prove runner behavior.
