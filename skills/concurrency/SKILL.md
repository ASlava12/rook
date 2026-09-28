---
name: concurrency
description: Use when shared state, threads, async tasks or actors can race, deadlock or outlive their owner.
---

# Concurrency

Identify execution contexts, shared state, ownership, lifecycle and the invariant
protected by synchronization. Trace task creation, cancellation, joins and cleanup.

Prefer clear ownership or message passing where they simplify the invariant.
When using locks, document lock ordering and the state each lock protects. Avoid
holding a lock across blocking I/O, callbacks or an await unless the protocol
explicitly requires it and its consequences are understood.

Check check-then-act races, publication of partially initialized state, concurrent
close/use, lost wakeups and cancellation between acquisition and cleanup. Atomics
require reasoning about the language's memory model and happens-before relations;
do not substitute a guessed memory order for a proven synchronization protocol.

Bound task counts, queues and outstanding work. Define backpressure, fairness and
shutdown behavior. An async function can still block its executor; inspect CPU
work and blocking library calls on the execution path.

Use deterministic scheduling, barriers, concurrency model checkers or race
detectors where the ecosystem provides them. Stress tests supplement reasoning
but a passing run does not prove a race absent. Prefer synchronization over sleeps
in regression tests and include cancellation and shutdown paths.

Deliver the ownership/synchronization model, fix and evidence for relevant
interleavings. If a hang is under investigation, collect wait relationships before
changing lock or timeout values.
