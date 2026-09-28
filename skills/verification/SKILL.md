---
name: verification
description: Use before claiming a task is complete to check the implementation against its acceptance criteria.
---

# Verification

Collect evidence for the requested outcome. A successful edit, build or test run
alone does not establish that the user's task is complete.

1. Map each acceptance criterion to the implementation and an observable check.
   Include promised edge cases, compatibility and failure behavior.
2. Run the smallest meaningful checks first, then the project's required gate.
   Choose tests, static analysis, runtime exercise or artifact inspection according
   to the change. A prose-only edit usually needs link and content validation.
3. Check that the test setup reaches the changed path. A regression test should
   fail for the original defect when it is practical to demonstrate that safely.
4. Inspect the final diff for omissions, accidental changes, temporary diagnostics,
   stale documentation and generated artifacts that disagree with their sources.
5. After a fix, rerun affected checks. Broaden testing for unresolved risks rather
   than repeatedly running the same passing suite.

Record the command or procedure, environment, result and relevant evidence. Keep
passed, failed, skipped and blocked checks distinct. A tool that could not start
did not pass. Do not invent output, hide failures or weaken assertions to get green.

Use isolated data for runtime checks. If a required environment is unavailable,
perform the useful checks that remain and state exactly which claim is unverified.

Finish with the outcome, verification evidence and remaining limitations. Do not
claim production readiness from local tests or full correctness from partial
coverage. An unresolved acceptance criterion means the work is incomplete.
