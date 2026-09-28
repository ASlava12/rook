---
name: production-readiness
description: Use when assessing whether a service or release is ready for production rollout.
---

# Production readiness

Assess a specific artifact, environment, workload and rollout. This review does
not itself authorize deployment. Reuse existing evidence; do not rerun every
specialist audit for every change.

Mark each relevant area as supported by evidence, blocked, or not applicable
with a reason:

| Area | Evidence to seek |
|---|---|
| Correctness | Acceptance criteria, tests, integration and runtime checks |
| Security | Trust boundaries, authorization, input limits, secrets, dependencies |
| Capacity | Expected load, latency, CPU, memory, storage, connections and quotas |
| Concurrency | Ownership, ordering, cancellation and bounded background work |
| Failure handling | Useful errors, deadlines, safe retries, degradation and recovery |
| Operations | Logs, metrics, traces, actionable alerts and an owner/runbook |
| Configuration | Validated defaults, environment differences and secret delivery |
| Compatibility | Old/new clients and instances, schemas and persisted data |
| Change safety | Migration/backfill evidence, staged rollout and stop conditions |
| Recovery | Rollback or forward repair, backups and demonstrated restore path |
| Delivery | Identified artifact, reproducible checks, documentation and provenance |

For each gap, identify a concrete failure scenario and its impact. Distinguish
release blockers from accepted limitations and optional improvements. Do not
invent approvals, SLOs or operational ownership that the project has not supplied.

Define rollout signals, observation period, stop thresholds and the recovery
action. A binary rollback is not sufficient if data has changed incompatibly.

Return a readiness verdict bounded by the evidence, unresolved blockers, accepted
risks and rollout/recovery instructions. If production checks were unavailable,
say so; local success alone is not a production verdict.
