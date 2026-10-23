# Z.ai balance refusals

Z.ai documents HTTP 429 with business code `1113` as an insufficient balance or
missing resource package. That account refusal requires a credential, balance or
package change; the normal short retry backoff cannot resolve it. Rook recognizes
the structured `error.code` together with the documented message and skips the
transient retries. Existing explicitly configured fallback routes remain eligible;
without one, the original provider refusal is returned immediately.

This applies to HTTP 429 JSON bodies up to 16 KiB, with code `1113` as a string
or integer and a message containing `insufficient balance or no resource package`
(case-insensitive). Requiring both fields avoids guessing from an arbitrary
mention of balance. Other business codes, rolling usage windows, overloads,
malformed bodies and ambiguous messages retain the existing retry classification.
The provider's original error body is preserved. Partial response content keeps
the existing no-replay rule.

Verification uses an owned local HTTP endpoint. A complete-request regression
reproduced four requests before this change. Afterward, tests cover one attempt
for complete requests and stream opening in both the OpenAI-compatible and
Anthropic transports. Unit controls preserve transient eligibility for usage-window
codes, missing/mismatched fields, an oversized body and a different HTTP status.
No cloud key or paid API call is used by these tests.

Sources: [Z.ai's error contract](https://docs.z.ai/api-reference/api-code) and
Hermes's separate payment/rate-limit handling at
[`22063fc099ec9ad8357d7f8f6e6248a0400680d3`](https://github.com/NousResearch/hermes-agent/blob/22063fc099ec9ad8357d7f8f6e6248a0400680d3/agent/auxiliary_client.py).
The Rook implementation was written independently.
