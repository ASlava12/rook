---
name: database-engineering
description: Use when changing schemas, SQL, indexes, transactions or database access behavior.
---

# Database engineering

Identify the actual database engine/version, schema, data volume, read/write
patterns and invariants. Read existing migrations and transaction conventions.
Use engine-specific documentation for uncertain locking or isolation behavior.

Choose types, keys, nullability and constraints from domain invariants. Preserve
data ownership and avoid encoding a relational rule only in one application caller.
Consider index selectivity, ordering, covering behavior and write/storage cost.

Inspect query plans and representative cardinalities before optimizing. A tiny
fixture or warm-cache run does not establish production performance. Bound result
sets, avoid accidental N+1 queries and use parameter binding for values.

Define transaction boundaries from the invariant. Analyze concurrent updates,
lost writes, lock order, deadlocks and retry behavior under the configured isolation
level. Keep external side effects out of a transaction unless their consistency
protocol is explicit. Retry complete transactions only when safe.

For schema changes, evaluate lock duration, rewrite cost, index build options and
old/new application compatibility. Separate schema expansion, resumable backfill
and contraction when mixed versions must operate. A reverse migration may not
restore deleted data; identify the actual recovery method.

Verify on the supported engine with realistic edge cases, constraints, concurrency
and plans where relevant. Deliver schema/query changes, migration and recovery
notes, and evidence. Do not run destructive operations on live data merely to
validate a proposed migration.
