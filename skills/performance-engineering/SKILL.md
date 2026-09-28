---
name: performance-engineering
description: Use when a measured latency, throughput or resource target requires optimization or benchmarking.
---

# Performance engineering

Define the workload and metric: latency distribution, throughput, memory, CPU,
I/O or cost per operation. Identify the required improvement and correctness
constraints. Do not invent targets or optimize a guessed hot path.

Establish a baseline with representative data, concurrency, build mode and runtime.
Record hardware, versions, warmup, cache state and measurement variability. Keep
setup work outside the timed region unless users actually pay for it.

Profile to distinguish computation, allocation, contention, queues and external
waiting. Check algorithmic growth across realistic input sizes. Optimize the
dominant contributor before micro-optimizing incidental code.

For caching, define keys, invalidation, consistency, capacity and stampede behavior.
For pooling or batching, account for added queueing, memory and tail latency.
Measure tradeoffs rather than assuming fewer calls always mean better behavior.

Compare the change against the same baseline with repeated measurements. Include
tails and saturation, not just average time; check memory and correctness regressions.
Avoid performance gates below the noise of the available runner.

Retain reproducible benchmark commands and raw measurements sufficient to assess
the claim. Report the measured improvement, variance, workload and limitations.
Do not extrapolate a synthetic microbenchmark into a production capacity claim.
