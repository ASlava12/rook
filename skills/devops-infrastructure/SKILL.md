---
name: devops-infrastructure
description: Use when changing containers, host configuration, networking infrastructure or infrastructure as code.
---

# DevOps and infrastructure

Identify the environment, state owner, deployment model and resources affected.
Inspect existing IaC, container and orchestration conventions. Use actual tool
and provider versions when consulting documentation or validating manifests.

Keep desired state reviewable and reproducible. Separate configuration from
secrets, use the platform's secret delivery mechanism and scope privileges to
the workload. Avoid mutable artifact tags where identity matters.

For containers, account for the runtime user, writable paths, image contents,
signal handling and graceful termination. For orchestration, distinguish startup,
readiness and liveness: a dependency outage should not cause endless restarts.

Define resource requests/limits, storage lifecycle, routing, DNS/TLS assumptions
and failure domains. Check stateful workloads and disruption behavior before
assuming a rolling replacement is safe.

Run available format, schema and configuration validation. Produce a plan/diff
and inspect replacement, deletion, privilege and network exposure before applying.
A review or proposed configuration is not authorization to mutate live resources;
respect the task's existing authorization and named target environment.

Verify the changed workload's health and recovery in an appropriate environment.
Document the artifact/configuration, observed effect and rollback or repair path.
Treat actual infrastructure drift and plan uncertainty as unresolved evidence,
not as permission to recreate resources blindly.
