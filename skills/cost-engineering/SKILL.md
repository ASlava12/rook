---
name: cost-engineering
description: Use when measuring or reducing cloud, compute, memory, storage or network spending.
---

# Cost engineering

Establish the accounting boundary, time window, currency and actual cost source.
Separate billed cost, allocated/shared cost and modeled estimates. Normalize by a
useful unit such as request, active user, processed byte or completed job.

Identify dominant drivers: utilization, idle capacity, peak provisioning, retention,
replication, egress, retries or per-operation charges. Use current provider prices
for projections and record region, tier and pricing assumptions; do not present
remembered prices as a quote.

Compare options against latency, reliability, security and operational constraints.
Consider rightsizing, autoscaling, lifecycle/retention, batching, data locality and
algorithmic efficiency before introducing a new platform.

Include migration effort, maintenance and commitment risk in savings estimates.
Distinguish reversible usage reductions from reserved capacity or contractual
commitments. Analysis or implementation authorization does not imply permission
to purchase a commitment or delete retained data.

Test representative load and recovery before reducing capacity or redundancy.
Measure actual utilization and cost after an authorized rollout over a comparable
window, accounting for workload differences.

Deliver a ranked set of changes with baseline, expected/observed savings, assumptions,
service tradeoffs and verification. If billing data is missing, provide a bounded
model and identify the measurements needed instead of claiming realized savings.
