---
name: testing
description: Use when selecting or implementing tests for behavior, regressions or cross-component contracts.
---

# Testing

Start with a behavioral claim and a failure it should catch. Choose the cheapest
test boundary that can detect that failure:

- Unit tests for deterministic rules and boundary conditions.
- Integration tests for persistence, wiring and real component interactions.
- Contract tests for independently evolving producers and consumers.
- End-to-end tests for essential user flows that lower levels cannot establish.
- Property or fuzz tests for broad input spaces with explicit invariants and
  reproducible failing examples.

Balance the portfolio by confidence, runtime and maintenance cost. A test pyramid
is a heuristic, not a target percentage. Do not add tests for trivial reversible
edits when direct inspection supplies adequate evidence.

Assert externally meaningful results. Include setup assertions when a bound,
timeout, overflow or failure path is the point of the test: verify that the input
actually reaches the threshold and the intended configuration is active.

Control clocks, randomness and concurrency using project facilities. Prefer
events and deterministic scheduling over arbitrary sleeps. Keep hang guards
generous; a test about timeouts should set the timeout it is testing explicitly.
Isolate data and clean up resources even on failure.

Mock at genuine external boundaries without mocking away the behavior under test.
For a regression, demonstrate that the check detects the original bug where
practical. Treat flaky failures as evidence to investigate, not reasons to remove
assertions or retry until green.

Run targeted checks and the required project gate. Report what they prove, any
skips and gaps; coverage counts alone do not establish correctness.
