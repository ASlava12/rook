---
name: reliability-engineering
description: Use when designing failure handling, degradation, recovery or capacity for a running service.
---

# Reliability engineering

Identify critical user journeys, agreed service objectives, dependencies and
capacity constraints. Prioritize failure modes by impact and plausible exposure,
not by an exhaustive list of theoretical outages.

For each relevant failure, specify detection, containment, degraded behavior and
recovery. Include unavailable dependencies, overload, exhausted pools/storage,
partial writes and restart during work.

Bound queues, concurrency and resource growth. Propagate deadlines deliberately,
distinguishing connection, idle and whole-operation limits. For long-lived streams,
decide whether continued progress should keep the operation alive.

Retry only safe operations with bounded attempts, backoff/jitter and a retry
budget. Define circuit breaker recovery/probing and avoid synchronized retry storms.
Use load shedding or backpressure before the service exhausts its own resources.

Define graceful shutdown, drain behavior, cancellation and restart reconciliation.
A health check should express the condition its controller can actually remedy.
Check that redundancy does not share the same failure domain.

Exercise controlled dependency failure and overload in an authorized environment.
Measure recovery and useful service retained, not merely whether a process stayed
up. Validate backups through restoration where data recovery is in scope.

Return the failure model, implemented controls, capacity/recovery evidence and
remaining limitations. Do not infer reliability targets the project has not set.
