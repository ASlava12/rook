# ADR-0016: Separate the web application from the daemon

## Status

Accepted. Supersedes the asset-embedding part of
[ADR-0012](0012-hand-written-modules-no-bundler.md) and
[ADR-0007](0007-no-js-build-step.md). The buildless ES module decision remains.

## Context

The interactive workspace now includes session navigation, project selection,
source-addressed review and automation integration. The user explicitly requested
an API-only daemon and a separately delivered interface, so frontend changes
should not require rebuilding the native executable. The CLI and server must
retain their existing Rust-only build path on supported native targets.

## Decision

`rookd` owns agent execution, durable state, HTTP/WebSocket APIs and scoped HTTP
MCP. It no longer links or serves `web/dist`. The interface is a separate
HTML/CSS/JavaScript directory. `web/serve.mjs` is an optional, dependency-free
Node host for that directory, with a same-origin, streaming API/WebSocket proxy
to a loopback daemon. A hosted installation can use its own authenticated reverse
proxy instead. Neither Node nor frontend files are prerequisites for Rust builds.

The local UI proxy validates Host and Origin before forwarding and preserves that
validated authority for the OAuth callback contract. Its asset handler confines
real paths to the UI directory, bounds file size and returns genuine missing
module errors instead of HTML in place of JavaScript. It does not introduce a
permissive CORS policy on the daemon.

HTTP automation and MCP share the core's existing source/receipt admission and
managed-work supervisor. Explicit operator routes define credentials and finite
worker scope. The MCP transport is a projection of the same operations, not a
second agent runtime. The existing stdio MCP tool server remains available.

## Consequences

- Frontend releases and reloads are independent of native compilation.
- Headless clients can install and run only the native agent components.
- UI hosting has its own process and runtime requirements; removing embedding is
  a packaging boundary, not a claim of reduced model latency or total process RAM.
- Frontend syntax, import/asset integrity and behavior are checked in Node and in
  a real browser against a scratch daemon. Rust tests cover the API contracts.
