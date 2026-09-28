---
name: debugging-and-profiling
description: Use when reproducing a failure or collecting debugger, trace, CPU, memory or I/O evidence.
---

# Debugging and profiling

Record the symptom, expected result, environment and reproduction input. Choose
an instrument that distinguishes a concrete hypothesis rather than collecting
every available diagnostic.

| Symptom | Useful first evidence |
|---|---|
| Wrong result or crash | Minimal input, stack, breakpoint and state at divergence |
| Hang | Thread/task stacks, wait relationships and progress timestamps |
| High CPU | Representative sampling profile and hot call paths |
| Memory growth | Live allocations, retention owners and workload over time |
| Slow I/O | Request spans, queue time, syscall or network timing |

Check that symbols, build mode and workload represent the problem. Distinguish
wall time from CPU time, allocation from retention, and waiting from computation.
Profiler overhead and debug builds can change the behavior being measured.

Collect bounded traces and scrub secrets and personal data. Prefer local or
staging reproduction. Intrusive production debugging requires authorization for
that action and explicit overhead/duration limits.

Use evidence to narrow the failing boundary. Reproduce under controlled variation
and test the causal hypothesis before changing implementation. Keep temporary
instrumentation isolated and remove it when its purpose is complete.

Repeat the same reproduction or workload after the fix, checking both the symptom
and normal behavior. Report the commands, relevant trace/profile locations,
observations and remaining uncertainty. If several causes remain plausible,
continue with explicit hypothesis testing before claiming a root cause.
