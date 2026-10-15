---
name: experiment-reporting
description: Use when comparing measured experiment runs or preparing their figures and verdict.
version: 1.0.0
license: MIT
---

# Experiment reporting

Fix the decision, hypotheses, variants, representative cases and acceptance
criteria before evaluating results. Identify the source/configuration of each
run and whether the evaluation contract changed. Use Rook's existing `eval`
scorecards or experiment sessions where appropriate; a skill does not create a
new experiment engine.

Read the actual metrics and retained logs. A completed process without a final
metric is not a measured success. Record failures, missing runs and skipped
checks rather than excluding them from the denominator. Keep source paths and
the evaluated commit or input hash with the results.

For model/provider comparisons keep prompts, tool schema, input order, generation
limits and grading stable. Compare quality, valid tool use, latency, token usage
and cost where observed. Separate cold and warm cache trials; counterbalance run
order. Record refusals and unavailable credentials as outcomes. Do not install
large model weights for a small API comparison.

Use matched tasks and repeated runs when possible. Report sample counts, seeds,
variation and uncertainty. One run may find a defect but does not establish a
general performance improvement. A difference within measurement noise is an
inconclusive result. Retain the baseline when the proposed change has no useful
demonstrated benefit.

Choose one primary table or figure for each conclusion. Every plotted value
must come from a cited measurement. Label units, aggregation, missing data and
error bars. Export vector graphics where useful; use installed plotting tools,
or deliver a checked table when they are unavailable. See
the examples below for a clearly synthetic
schema fixture; never present its numbers as a real result.

Deliver the evidence, conclusion, limits and decision separately. Preserve the
frozen run inputs and source data needed to reproduce the comparison.


## Example record

```csv
variant,case,repeat,quality_pass,elapsed_ms,input_tokens,output_tokens,evidence
EXAMPLE_BASELINE,fixture-only,1,true,120,20,5,synthetic-not-a-measurement
EXAMPLE_CANDIDATE,fixture-only,1,false,110,20,5,synthetic-not-a-measurement
```
