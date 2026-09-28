---
name: software-architecture
description: Use when changing module boundaries, dependency direction or service and data ownership.
---

# Software architecture

Map the current responsibilities, dependencies, data owners and public contracts
from source evidence. Identify the change pressure: what concrete operation is
unsafe, costly or difficult under the current structure?

Evaluate boundaries using cohesion, coupling, invariants, change frequency and
operational ownership. Keep domain rules independent of delivery and storage
details where that makes change easier. Use DDD or clean/hexagonal patterns only
where the domain and dependency pressure justify them.

Before adding a service boundary, account for deployment, latency, partial failure,
data consistency, observability and on-call cost. A module boundary may solve the
same ownership problem without a distributed protocol. Conversely, independent
scaling or isolation requirements may justify a service when evidenced.

Describe contracts and dependency direction explicitly. Trace a representative
request and failure through the proposed design. Check for cycles, shared mutable
state, duplicate sources of truth and responsibilities with no clear owner.

Plan an incremental transition that keeps existing callers working. Identify any
temporary adapters, their removal conditions and the checks that enforce the new
boundary. Do not rewrite working subsystems solely to match a named architecture.

Deliver the chosen structure, alternatives that mattered, consequences and
migration/verification plan. Add a concise diagram or ADR when it will help future
maintainers understand a durable decision.
