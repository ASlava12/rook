# Reference follow-up — 2026-10-18, after 0.17.0

The ongoing improvement goal continues from clean commit `a0403d5`.
All ten configured reference branches were fetched once. Incoming commit subjects
were inspected; selected implementation paths are read before adoption. Seven
pointers advance; three already match their tips. Reference sources remain unchanged.
A bounded CodeGraph query did not return in a useful interval and was terminated;
structural evidence below comes from source inspection, not an inferred graph.

| Reference | Previous pin | Frozen pin | Incoming commits |
|---|---|---|---|
| acp | `3be9cfc8ea2dfe5a28973b461fd956b4045f24d3` | `191987a032da60ee91b86f85476491fb7e6452c5` | 1 |
| codex | `7b44ac1661433ae3a6f375fde873fa7123c935fe` | `8c076942cb70fba8e5c4732c23f1faabc4aed484` | 1 |
| goose | `bfcc6ee6f1ee18d0c4c852a65c49b25f87ee40dc` | `34a222e3f2b95d8b4a43482b53c2cc957ecce26d` | 1 |
| opencode | `8f7b0120eb6b4aa3da1f82b1a9094a9fb4db2d06` | `da9d436c36e0145110531ffcd7b15e50016fdd82` | 17 |
| cline | `a42df8c8d5fd9d9f9a9073589baffe32ea9bb2f0` | `a42df8c8d5fd9d9f9a9073589baffe32ea9bb2f0` | 0 |
| hermes | `e6cf2a9b1e8f98d40408b09a9a55993c8459b4b4` | `9e01b44dd23978f9ff40b7837467f3a9f1582130` | 159 |
| openhands | `852132d38dbc15e03d6825bbacbeb971c72575be` | `852132d38dbc15e03d6825bbacbeb971c72575be` | 0 |
| openclaw | `00adf856e25545861a26b106ee3ddad6cdbcdbfa` | `6913563f69d92ff17fd5c677950c687a41582dd9` | 157 |
| openresearch | `6285fee7f42bc8ad21500b84278d38679f1ac01b` | `6285fee7f42bc8ad21500b84278d38679f1ac01b` | 0 |
| pi | `00bf7193d6ba0f13c01bbb5666a6e72505683f87` | `482c1381a00d8d00bc81697e4a980c593db6803e` | 7 |

## Accepted work

| ID | Source and gap | Acceptance / current state |
|---|---|---|
| F18-1 | OpenClaw `f427a05291`: session-specific tool availability diagnostics. Existing request catalogs show only advertised tools, without explaining configuration/input/depth/recipe restrictions or MCP overflow. | Shared bounded inspection tool, current request scope, explicit deferred-MCP route and permission caveat; no probing/installation/secret requests; deterministic pagination and real engine-dispatch checks. Implemented; targeted checks pass. |
| F18-2 | Hermes `35948f22a`, corrected by `ed9fb3cf5` and `a31975222`: post-compaction observers belong after a committed record, including fallback paths. | Metadata-only callback after commit; no callback for cancelled/uncommitted candidates; observer replies cannot inject context/UI or veto; timeout/failure isolation. Implemented; initial native shell/engine tests pass. |
| F18-3 | Hermes session-deletion hooks (`ee3ce9a4d`, `aab0dc72c`, `627ce0355`). | Explicit CLI/daemon/browser deletion invokes observers once, including empty sessions; atomic missing-session detection prevents duplicate callbacks on retry. Retention/internal cleanup never run callbacks. No stored layout change or interactive authority. Implemented; initial native shell/engine tests pass; operator paths need verification. |
| F18-4 | OpenCode `49e9786b`: bounded skill discovery. Rook already uses two supported directory levels, but total entries/manifests/errors and bundled-resource enumeration were unlimited. | Bound root/entry/skill/read-byte/diagnostic admission, preserve source priority, surface omissions/errors, fail visibly on oversized resource listings; test exact reached bounds, malformed files, version directories and existing shipped skills. Implemented; targeted checks pass. |
| F18-5 | Pi `a30c2a8`, `440a5fc`, `5ba1cba`, `91b33ca`: persistent crash-loop guard. Rook bounds handled provider retries, but process death can repeatedly reopen an unfinished managed stage without a committed recovery counter. | Persist stage ownership/recovery attempts, park at bounded restart limit, explicit resume resets the guard, generation-safe controls, no confusion with provider retry counters; fixtures for reopen/drop/cancel/pause/concurrency and frontend status. Implemented; targeted checks pass. |

## Other reviewed directions

OpenCode `60a5a6ca` changes nullable Responses fields. Rook's decoder already
uses optional Value access for many fields and requires an authoritative terminal
response before releasing calls. Inspect the actual missing/null cases before
claiming a port or weakening malformed identity/terminal checks.
Goose's Bedrock-only block mapping does not apply to Rook's adapters. ACP's
incoming commit changes its registry; keep negotiated v1 semantics. Three other
references have no incoming changes. Hermes plugin catalogs and OpenClaw hosted
PR/account runtimes are product-specific and do not justify new dependencies.
Pi failed-input policy, typed fault attribution and blocked task graph are useful
audit references for the existing durable queue/background view; equivalent behavior
is not counted as a new feature. Durable sleep/wake and idle-root message wakes
remain separate lifecycle work; do not implement them as a blocking sleep.

## Delivery

All five accepted capabilities are implemented and audited. The final frozen-source
`cargo xtask ci`, browser module suite and native CLI/daemon/Chrome/PTY probe passed.
The native follow-up verifies explicit deletion, actual tool dispatch and two
physical isolated-daemon crashes followed by a parked recovery. The
[delivery record](../reference-followup-20261018.md) contains contracts,
corrections and validation scope. Exact-commit platform CI is required before
tagging; published archives receive their own checksum and native verification.
No model-quality improvement is claimed and the existing summarizer experiment's
default remains unchanged.
