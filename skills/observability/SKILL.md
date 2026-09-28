---
name: observability
description: Use when adding or correcting logs, metrics, traces, service indicators or operational alerts.
---

# Observability

Start with an operational question: which user-visible failure or performance
change must an operator detect and explain? Inspect existing telemetry conventions
and OpenTelemetry integration before adding another stack.

Use logs for discrete contextual events, metrics for bounded aggregate signals
and traces for causality and latency across operations. Keep correlation across
async and service boundaries without making correlation IDs metric labels.

Define metric units, type, aggregation and label cardinality. Avoid user IDs,
request IDs, arbitrary URLs and error messages as labels. Bound logs and trace
volume with appropriate sampling and retention; do not drop all failure evidence.

Redact secrets and personal data at collection boundaries. Log actionable context
without dumping full payloads by default. Avoid duplicate logging of the same
error at every stack layer.

Define SLIs from user outcomes, including the numerator, denominator, window and
exclusions. Take SLO targets from agreed requirements. Alerts need an actionable
failure, useful time window and recovery guidance; a dashboard alone is not an
alerting strategy.

Verify emitted telemetry in a representative request and failure, including
context propagation and exporter behavior. Ensure a telemetry outage cannot
block the primary service or grow buffers without bound.

Deliver the signals, their operational interpretation, queries or dashboards
when requested, and evidence that the instrumentation works.
