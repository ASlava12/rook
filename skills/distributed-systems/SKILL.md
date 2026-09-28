---
name: distributed-systems
description: Use when behavior spans services or replicas and must survive partial failure or message redelivery.
---

# Distributed systems

Draw the participants, state owners, messages and failure boundaries. State the
invariant and the consistency each operation actually needs. Avoid claims of
exactly-once effects without explaining the durable protocol and its scope.

Analyze timeouts, dropped or duplicated messages, reordering, process crashes,
network partitions and recovery. A caller timeout does not establish whether the
callee committed. Wall clocks do not provide a reliable total order.

Define idempotency keys and deduplication lifetime, durable commit points and
acknowledgment order. Use an outbox/inbox or another explicit protocol when a
database update and message publication must agree. Describe replay and retention.

Apply bounded retries only to safe operations, with backoff, jitter, a retry budget
and an overall deadline appropriate to the operation. Avoid retries at every layer
that multiply load. Specify backpressure and bounded queues.

For replication or leadership, use established implementations where possible.
Reason about quorum, stale reads, fencing of former leaders and recovery from
partitions. Do not add consensus for a problem solved by a single state owner.

Exercise duplicate delivery, crash around commit/acknowledgment, stale state and
partial availability in controlled tests. Validate convergence or compensation,
not only the happy path.

Return the protocol, invariants, failure matrix and observed recovery evidence.
Name residual windows and assumptions about the broker, database and network.
