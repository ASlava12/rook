# Upstream reference review, November 12, 2026

Unlike the November 11 pinned-source follow-up, this cycle fetched all ten
configured upstream branches and advanced their working-tree gitlinks. No
upstream code was copied, no reference source was edited, and nothing was staged
or committed automatically. Checkouts used no force and preserved dirty-file
protection.

Exact previous/new SHAs and available incoming subjects are in
[the frozen snapshot manifest](reference-snapshots-20261112.json). Fetches are
200 commits deep; the manifest captures at most 200 non-merge subjects per
repository. A capped/shallow history is a sample, not a complete accounting of
every intervening commit. Review covered incoming subjects, selected patches
below and their corresponding Rook paths. Subject review is sampled from the
captured manifest, not an exhaustive triage of every captured subject. Older/unexamined changes remain a
backlog; advancing a snapshot is not a claim that every upstream feature landed.

| Reference | Previous → reviewed tip | Selected source and decision |
|---|---|---|
| Codex | `8c076942cb70` → `392ced7d3a91` | Adopt ideas from `95dcd1e8` (full Unicode caseless history search) and `367c6756` (choose excerpts before display whitespace normalization). Rook implements its own streaming, original-coordinate matcher and hit-centered previews. `392ced7d` OAuth stale-write protection prompted a check: Rook already holds a file lock across refresh and revalidates credential epochs; save/logout use the same lock. |
| OpenClaw | `e1e4a0dffa2b` → `9c2e60f7d003` | Adopt the credential-safe quoted compaction idea from `4a47d02c6`, using Rook's registered-value and escaped-form masking instead of introducing credential-shape heuristics. `d54e62414` per-result summary shortening has an equivalent in the November 11 Rook work. |
| OpenCode | `802f2d8e7509` → `745636aee37c` | `e56eff2b` adds fork awareness; defer a Rook version until ancestry, original job ownership and context-cost projection are delivered together. Background subagent completion/wake changes must retain Rook's ordinary nursery and budgets rather than adding a second coordinator. |
| Goose | `34a222e3f2b9` → `0c5b5e2f2e79` | `71dd9a9` resumes completed delegates with a follow-up. Useful, but Rook currently closes completed family recipients deliberately. Defer until explicit follow-up admission has generation, quota, cancellation and policy contracts. Existing terminal-completion admission already rejects premature provider streams (`0f8a0b9`). |
| Cline | `a42df8c8d5fd` → `32075f111084` | `4a4c26f` distinguishes a summarizer preference from explicit reasoning disable. Rook's summarizer requests low effort, with existing provider/capability mapping; do not introduce an unconditional `none`. Desktop/cloud artifact flows and provider ranking are outside this local engine change. |
| Hermes | `22063fc099ec` → `6a0d2d12815c` | `d1db1bc6` moves a blocking readiness probe off the API event loop. Rook's health endpoint deliberately avoids the store lock and has a regression for answering while it is held. Profile routing and shell-launcher guard changes need their own model of Rook's command admission; do not paste lexical rules. |
| OpenHands | `852132d38dbc` → `b759f8a7f4cf` | `b759f8a` fixes timestamp-tied history pages. Rook already pages by monotonic event sequence with frozen `through` bounds, not timestamps. Keep those ordering/source guarantees. |
| OpenResearch | `6285fee7f42b` → `cd2cae9d487d` | `35f5422` asks before quitting/restarting active work. Rook already separates UI observer lifetime from daemon work and has explicit drain/force controls. Do not make closing a browser observer cancel work or introduce a new confirmation flow. |
| Pi | `482c1381a00d` → `ec566e6b1826` | `3345c29` preserves restored MCP tools when an extension hides only its loader. Rook has source/generation-bound catalogs and recipe equipment instead of this extension replacement API; retain its current admission path. Room replication, Radius and experimental shared-state APIs are outside native family mailboxes and authenticated remote-task transport. |
| ACP | `2c8270462725` → `f3ed0fca2fd6` | `67cde2d` stabilizes client system prompts; current unstable session options/fork additions are separate negotiations. Record client-prompt support as a follow-up with explicit authority/size/capability checks. This cycle changes no ACP wire schema or advertised capabilities. |

## Implemented

1. Full default Unicode case folding in global and paged transcript search,
   including `STRASSE`/`Straße`, Greek sigma variants and expanded query
   characters across byte-page boundaries. Original source offsets remain UTF-8
   byte offsets. Matching uses query-sized state, not a position table for the
   entire body. `caseless` 0.2.2 (MIT) supplies Unicode folding tables; the
   matching/projection code is independently implemented in Rook.
2. Global history previews center on an actual matching term in a long line.
   They preserve original casing and normalize display whitespace after source
   selection. Literal transcript search retains exact whitespace and quoted
   source bytes. Global queries now share the 1,024-byte input ceiling already
   used by literal transcript search.
3. Secret-safe compaction input, previous-summary fallback, file-request
   companions and generated prose, using bounded registered-value masking.
   Redaction expansion cannot bypass the configured output admission. Original
   journal records remain unchanged. This is not general credential discovery
   or a rewrite of ordinary historical replay.

See [Unicode history search](../history-search.md) and
[the compaction contract](../long-task-compaction.md) for bounds and limitations.
All behavior is in shared core paths used by CLI, TUI, daemon/API and the web
workspace. No additional tool executor, UI bundle or automatic remote action.

## Verification

Targeted core tests cover actual compaction requests and saved records, quoted
known values, failed/new summaries, legacy file metadata and redaction expansion.
Search controls exercise literal tabs/newlines, UTF-8 offsets, fold expansion,
query admission and match-centered previews. Existing agent, provider-history,
compaction-observation and transcript tests are included in the focused run.

The initial broad `cargo check` exhausted Windows commit/pagefile capacity while
other projects were building; it did not establish a source-code failure. Later
checks use fewer compilation jobs. The first complete gate then exhausted Z
during test compilation. Package-scoped `cargo clean -p rook-core` removed
rebuildable outputs (Cargo reported 57.5 GiB), restoring sufficient free space.
No system pagefile configuration was changed.
Artifacts and temporary projects stay on Z. The final Windows x86_64 run of
`xtask/cargo.cmd xtask ci` passed with exit status 0 and `ci: ok`: formatting,
workspace/all-target Clippy with warnings denied, fresh CLI/daemon builds and
the complete workspace test run with `--no-fail-fast`. It used
`CARGO_BUILD_JOBS=4` and `RUST_TEST_THREADS=4` without narrowing the gate. Web
runtime syntax checks and the separate Node suite passed, and the final diff
passes whitespace checks. Scripted providers do not establish real-model
summary quality, and local verification does not establish remote multi-platform
CI success.
