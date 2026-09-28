---
name: data-engineering
description: Use when building or changing ingestion, ETL, batch jobs, queues or stream-processing pipelines.
---

# Data engineering

Define sources, sinks, schemas, ownership, volume, freshness and quality requirements.
Trace lineage and identify the durable source of truth. Distinguish event time
from processing time and document ordering assumptions.

Specify delivery semantics and commit boundaries. Decide how retries, duplicates,
late/out-of-order records and partial batch failures are handled. An acknowledgment
must not get ahead of durable effects unless the loss window is explicit.

Design transformations with explicit types and schema evolution rules. Preserve
provenance needed to explain outputs. Route malformed records to a bounded,
observable quarantine/dead-letter path with a repair or replay procedure; do not
silently drop them or retry poison records forever.

Bound memory, batch sizes, queue depth and concurrency. Use backpressure and
checkpointing appropriate to the source. Partitioning must account for skew and
hot keys, not just nominal parallelism.

Make reprocessing/backfills resumable and safe against duplicate side effects.
Define reconciliation checks such as counts, aggregates, checksums or domain
invariants. Protect sensitive fields throughout storage, logs and retention.

Validate with representative data, schema changes, duplicates, late events,
interruption/resume and sink failures. Measure freshness and throughput where
required. Deliver the pipeline, data contracts, recovery/replay steps and quality
evidence; successful job exit alone does not prove complete or correct data.
