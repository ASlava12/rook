---
name: migration-engineering
description: Use when moving data, schemas, APIs or implementations while old and new versions must coexist.
---

# Migration engineering

Define source and target states, data invariants, consumers, traffic and allowable
interruption. Identify which stages are reversible and which destroy information.
Do not promise zero downtime without a compatible deployment sequence.

Prefer expand → migrate → verify → contract when mixed versions must operate:

1. Add compatible structures or adapters; keep existing readers/writers working.
2. Deploy compatible code in an explicit order. Define the source of truth during
   dual operation and how divergent writes are detected or reconciled.
3. Backfill in bounded, resumable batches with checkpoints, idempotency, throttling
   and progress reporting. Account for concurrent updates during the backfill.
4. Reconcile counts, checksums or domain invariants and compare old/new behavior.
   Switch reads/traffic with explicit stop and recovery conditions.
5. Remove old paths only after supported consumers have moved and the recovery
   window permits contraction.

Test interruption and resume, duplicate processing, malformed records and mixed
versions. Estimate locks, storage, load and duration from representative data.
Keep production execution separate from preparing scripts and a runbook unless
execution in that environment is authorized.

Describe rollback at each stage. If old software cannot read new data, rolling
back a binary is not recovery; use restoration, reconciliation or forward repair
with the actual data-loss window stated.

Deliver executable steps where requested, invariant checks, progress/stop signals,
recovery instructions and evidence from rehearsal. Never silently discard records
that could not be transformed.
