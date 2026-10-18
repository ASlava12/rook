# Reference cycle — 2026-10-18

The active goal requests batches of roughly ten useful reference ports, followed
by refactoring, security/contract/completeness audit, fixes and a release.
The working tree starts from v0.16.0 plus the seven implemented R17 runtime
items. Three additional capabilities below bring this delivery batch to ten.
The inconclusive R17 summarizer experiment remains pending with its default
unchanged; it is not counted as a completed capability.

## Reference snapshots

All ten reference worktrees were clean before fetching their configured branches.
Every previous pin is a proven ancestor of its fetched replacement. All ten
worktrees were checked out detached and their gitlinks staged. No reference
source files were modified. The incoming ranges contain 3117 commit subjects.
Triage prioritized feature, lifecycle, provider, catalog and failure-path
subjects; the selected source changes were read. This is not an exhaustive audit
of those commits or an assertion that upstream tests pass. A whole-repository
CodeGraph query was stopped after failing to return a bounded result; targeted
source inspection supplies the evidence below.

| Reference | Previous R17 snapshot | New reviewed snapshot | Incoming commits |
|---|---|---|---|
| acp | `00c5a02d7ab96b9d6aa214002d408ce9d972b4e4` | `3be9cfc8ea2dfe5a28973b461fd956b4045f24d3` | 6 |
| codex | `b5115cdd669d37a8635a9742851e97ba4fb1d42c` | `7b44ac1661433ae3a6f375fde873fa7123c935fe` | 30 |
| goose | `fe55e842fb8d92f16c45d04c83f07120cc4fa258` | `bfcc6ee6f1ee18d0c4c852a65c49b25f87ee40dc` | 6 |
| opencode | `ed772f5009755b5262805a7ba07e66e1417f140a` | `8f7b0120eb6b4aa3da1f82b1a9094a9fb4db2d06` | 37 |
| cline | `01b8a9724da905423e636396d7fb4acdf47796bb` | `a42df8c8d5fd9d9f9a9073589baffe32ea9bb2f0` | 7 |
| hermes | `f974134f367bc0a4c4cd24afd90494bbceaa86ee` | `e6cf2a9b1e8f98d40408b09a9a55993c8459b4b4` | 1653 |
| openhands | `9d02893bc96aa256a4c0ece316bdb992247bc918` | `852132d38dbc15e03d6825bbacbeb971c72575be` | 30 |
| openclaw | `58d7e878feb0b27f85c4ecb48595c38b534553ab` | `00adf856e25545861a26b106ee3ddad6cdbcdbfa` | 1333 |
| openresearch | `8fc90cf355cda1e89a6aabc02d49a098f159f463` | `6285fee7f42bc8ad21500b84278d38679f1ac01b` | 2 |
| pi | `4dd2b149d155d7369de16ea4dc77c84e9f5ffb24` | `00bf7193d6ba0f13c01bbb5666a6e72505683f87` | 13 |

## Additional delivery scope

| ID | Capability / source | Existing Rook gap | Acceptance |
|---|---|---|---|
| C18-1 | Live MCP tool catalog invalidation — Codex `0df4aa67781ae45a3c97befa4db48b734afe46d9`, `codex-rs/codex-mcp/src/client_tool_catalog.rs`, `connection_manager/tool_catalog.rs` and `rmcp-client/src/tool_list_changes.rs` | Connections and advertised catalogs are frozen; tool-list notifications do not update subsequent discovery/binding within the same turn. | Explicitly enabled bounded refresh; coalesced generations; invalidation during listing/pagination; failed refresh preserves the previous catalog and reports stale status with cooldown; current calls keep their original descriptor/permission binding; subsequent model requests and discovery agree. Check native stdio and HTTP delivery, cancellations and all operator surfaces. |
| C18-2 | Recover Gemini malformed generations — OpenCode `180fd9d622c6855ef6ea36fd81314de6406af054`, `packages/ai/src/protocols/gemini.ts` | `MALFORMED_RESPONSE` becomes an ordinary terminal result; buffered output and failed frames can expose tool calls without a valid generation. | Reject the failed frame before exposing its content; use the shared bounded retry policy only before semantic/usage/completion delivery; never replay partial text, reasoning or calls; keep reported failed-attempt counters; omitted counters remain unknown; respect cancellation and never retry refusals or invalid function-call finishes; reject calls carried by refused generations. Applies through the ordinary shared provider in CLI/API/TUI/browser. |
| C18-3 | Session-family background-work overview — OpenClaw `d6affe9629b280068840e61275b3dd10270280e0`, `ui/src/pages/chat/components/chat-details-background.tsx` | Tree, jobs and per-turn activity exist separately; no one bounded session-family view correlates child progress and background work when the original chat is lost. | Shared bounded reader with lineage/source IDs, live versus saved/unverified status, jobs, managed owners and last-progress observations; no payload/secret copies, no liveness inference from saved markers; pagination/omission notices and CLI/API/TUI/browser reachability. Preserve draft/current turn during inspection. |

The Gemini finish reason is confirmed in the primary
[GenerateContent API reference](https://ai.google.dev/api/generate-content#FinishReason).
The retry decision is Rook's own conservative policy, not a claim that all
malformed generations are transient.

## Other decisions

ACP incoming changes are registry updates; keep negotiated v1 semantics.
Cline's GPT-6 output parameter mapping already exists in Rook. Its OTLP desktop
exporter and OpenHands account/UI onboarding are architecture-specific.
Goose's provider catalog/ACP discovery and runtime changes do not justify
embedding a JavaScript engine. OpenResearch's two incoming subjects and Pi's
SQLite/workerd hosting remain separate product/runtime directions. Hermes's
canonical queue ownership, retained refusals, source-bound secrets and browser
redaction limits inform the audit of existing Rook mechanisms; they do not add
a second queue, memory service or messaging runtime. OpenClaw's mixed database
engines, avatars, paired-device folders and native decision APIs are deferred;
Rook retains its bounded local store and existing browser/tool policy.

A durable sleep/wake state for managed goals is a useful separate direction
(Pi `f75b324`), requiring generation-safe wake/cancel/restart semantics and clear
front-end countdowns before adoption. It is not silently substituted with a long
blocking sleep or counted in this batch. Other deferred directions remain in
the existing roadmap/reviews.

Delivery, audit and release evidence: [cycle tracker](../reference-cycle-20261018.md).
