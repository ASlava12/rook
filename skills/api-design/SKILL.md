---
name: api-design
description: Use when defining or changing a public API, RPC contract or externally consumed interface.
---

# API design

Identify consumers, operations, ownership, latency constraints and existing
conventions. Choose REST, RPC or GraphQL from interaction needs and ecosystem
constraints, not fashion. Keep the contract independent of internal storage shapes.

Specify request/response schemas, validation, authorization scope, error semantics
and cancellation. Distinguish omitted, null, empty and default values. Bound payloads,
page sizes, query complexity and batch work where they can accumulate.

For mutations, define idempotency scope, key lifetime, conflicting reuse and the
response to an uncertain outcome. Do not assume a timeout means no work occurred.
Specify which errors are retryable and how clients learn retry timing.

For lists, choose stable ordering and pagination behavior under concurrent changes.
For long operations, define lifecycle, status retrieval and cancellation. Keep
error codes stable and useful without leaking internal or sensitive information.

Check schema evolution against existing clients: unknown fields, enum additions,
required fields, numeric ranges and generated bindings can all affect compatibility.
Follow the project's versioning and deprecation policy; do not introduce a new
version merely to avoid designing a compatible extension.

Produce the contract in the project's native format, examples for success and
important errors, and consumer-facing compatibility notes. Validate producers and
consumers with contract or integration checks, including duplicate requests,
invalid input and permission failures where relevant.
