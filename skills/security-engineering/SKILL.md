---
name: security-engineering
description: Use when reviewing security or changing trust boundaries, authorization or sensitive data handling.
---

# Security engineering

Define assets, actors, entry points and trust boundaries for the actual change.
Trace untrusted data to privileged operations and identify concrete abuse cases.
Use the project's threat model and applicable current official guidance; do not
declare compliance from a generic checklist.

Examine relevant paths:

- Authentication versus authorization, including object and tenant scope and
  authorization after state changes.
- Injection, unsafe deserialization, path traversal, SSRF, command execution and
  resource exhaustion at the boundary that accepts input.
- Browser/session protections when applicable: output encoding, CSRF, cookie
  policy, redirects and origin handling.
- Secret lifecycle, redaction, least privilege and storage/transport protections.
- Dependency provenance, update paths and externally supplied plugins or builds.

Use maintained cryptographic and authentication libraries. Do not invent crypto
protocols or weaken certificate verification to make a test pass. Validate input
with an appropriate parser and enforce limits before expensive work or allocation.

Demonstrate findings with a bounded local test or source-level causal path. Do
not attack third-party or production systems as an assumed part of a review.
Distinguish a reachable issue from a hypothetical one and record prerequisites,
impact and uncertainty. Never include live credentials in reports or test fixtures.

Fix the trust boundary that owns the rule and add meaningful negative coverage.
Return prioritized findings, evidence, remediation and residual risks; a clean
review means no issues found in the inspected scope, not proof of security.
