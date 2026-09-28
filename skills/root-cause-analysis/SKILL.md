---
name: root-cause-analysis
description: Use when a defect recurs, has an uncertain cause or needs an evidence-backed causal explanation.
---

# Root cause analysis

Follow observation → hypotheses → evidence → experiments → cause → fix → regression
check. Do not edit the implementation until there is a falsifiable hypothesis.
Diagnostic instrumentation is an experiment; an urgent mitigation may precede
diagnosis, but label it as mitigation rather than a proven fix.

Record expected and observed behavior, the earliest known divergence, relevant
versions, inputs and a minimal reproduction. Reduce unrelated variables without
discarding the conditions that trigger the problem.

For each plausible hypothesis, write the predicted observation and an experiment
that distinguishes it from the alternatives. Prefer cheap read-only evidence
before invasive instrumentation. Change one explanatory variable at a time when
possible and preserve negative results.

Trace the causal chain across boundaries: input, configuration, control flow,
state transition and external interaction. Temporal correlation or the last
stack frame is not sufficient proof. Identify both the trigger and the condition
that allowed it to cause the failure.

Fix at the boundary that owns the violated invariant. Avoid unrelated rewrites,
silent error suppression and retries that only conceal the symptom. Demonstrate
the original failure is prevented and nearby valid behavior still works.

Return observations, rejected and supported hypotheses, the causal explanation,
fix and regression evidence. If evidence is insufficient, report the leading
hypothesis and next distinguishing experiment rather than asserting certainty.
