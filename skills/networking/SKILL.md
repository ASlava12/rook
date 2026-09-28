---
name: networking
description: Use when diagnosing or changing DNS, TCP/UDP, TLS, HTTP, proxy or load-balancer behavior.
---

# Networking

Map the actual path from client through DNS, proxies, load balancers and TLS
termination to the service. Record endpoint, protocol, environment, timeouts and
whether failures affect all clients or only a particular path.

Work layer by layer: name resolution, route/connectivity, transport establishment,
TLS identity/negotiation, application request and response/body progress. Success
at one layer does not prove the next one works. A successful ping does not prove
an HTTP service is reachable.

Use bounded diagnostics suited to the hypothesis: resolver output, connection
timing, certificate details, request headers, server logs or a packet capture in
an authorized environment. Protect credentials and payloads in traces.

Distinguish connection, read/idle and overall operation timeouts. Check proxy
buffering, keepalive/connection reuse, protocol negotiation, DNS caching and body
limits when relevant. For UDP, account for loss, duplication, ordering and payload
size rather than assuming stream semantics.

Reproduce with the same host name, network path and trust configuration as the
failing application. Do not disable TLS verification or open broad firewall access
as a fix. Verify the certificate/trust or routing cause directly.

After changing configuration, test the affected path and adjacent clients,
including long responses or connection reuse if implicated. Report the failing
layer, evidence, correction and remaining environmental uncertainty.
