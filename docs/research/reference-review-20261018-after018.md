# Reference review after 0.18.0 — 2026-10-18

Baseline: `9aa294152519dd297a8aea1e319c79f68960aaf2`, published as
[0.18.0](https://github.com/ASlava12/rook/releases/tag/v0.18.0).
All eight required CI jobs and all six release jobs passed. Five public archives,
their checksums, all 50 skills (52 files per archive) and the published Apple
Silicon CLI/daemon/Chrome/PTY probe were verified. Windows skill text differs only
by checkout CRLF conversion. Owned build/tmp files were removed, reclaiming
14.113 GiB; compact evidence remains outside the checkout.

All ten references were fetched once after that delivery. Four pointers advance,
with 18 incoming commits; the other six match their fetched tips. Reference
sources were not edited. A bounded CodeGraph query on `crates/rook-core` exceeded
25 seconds; the structural findings below use the cited source paths instead.

| Reference | Previous pin | Frozen pin | Incoming |
|---|---|---|---|
| acp | `191987a032da60ee91b86f85476491fb7e6452c5` | `2c8270462725d07d9d7cd845beb21dc99c9eae68` | 1 |
| cline | `a42df8c8d5fd9d9f9a9073589baffe32ea9bb2f0` | `a42df8c8d5fd9d9f9a9073589baffe32ea9bb2f0` | 0 |
| codex | `8c076942cb70fba8e5c4732c23f1faabc4aed484` | `8c076942cb70fba8e5c4732c23f1faabc4aed484` | 0 |
| goose | `34a222e3f2b95d8b4a43482b53c2cc957ecce26d` | `34a222e3f2b95d8b4a43482b53c2cc957ecce26d` | 0 |
| hermes | `9e01b44dd23978f9ff40b7837467f3a9f1582130` | `22063fc099ec9ad8357d7f8f6e6248a0400680d3` | 2 |
| openclaw | `6913563f69d92ff17fd5c677950c687a41582dd9` | `e1e4a0dffa2bdb930f8fa2ea65e425ba0fd9c354` | 14 |
| opencode | `da9d436c36e0145110531ffcd7b15e50016fdd82` | `802f2d8e75098ae0b45ad839ce98a31589ea8102` | 1 |
| openhands | `852132d38dbc15e03d6825bbacbeb971c72575be` | `852132d38dbc15e03d6825bbacbeb971c72575be` | 0 |
| openresearch | `6285fee7f42bc8ad21500b84278d38679f1ac01b` | `6285fee7f42bc8ad21500b84278d38679f1ac01b` | 0 |
| pi | `482c1381a00d8d00bc81697e4a980c593db6803e` | `482c1381a00d8d00bc81697e4a980c593db6803e` | 0 |

## Accepted: durable deferred continuation of managed work

Pi `f75b324`, corrected by `ec1be23` and `00bf719`, stores a sleeping phase and
its checkpoint instead of retaining an invocation. Its scheduler checks due
work on every pass, including recovery after a lost process. Read paths:
`packages/durable/README.md` (Sleeping and Waking Up),
`packages/durable/src/harness/scheduler.ts`, its sleep tests and correction diff.

Rook already persists provider retry deadlines, runs ordinary scheduled goals
and has generation-bound pause/resume. It lacks a model-requested wait that ends
the current managed stage and resumes the same transcript later. A shell sleep
holds the executor; a new scheduled goal loses the current continuation. Relevant
paths: `work/managed.rs`, `agent/lifecycle.rs`, `agent/tools.rs`, `rookd/src/work.rs`
and `rook-proto/src/{work,schedule}.rs`.

Implement one complete capability, provisionally named `defer_work`:

- Root managed work may request 1..86,400 seconds and an optional bounded reason.
  Ordinary turns, checkers and delegated children cannot schedule a parent.
  Recipe and policy restrictions remain effective; `tool_status` explains absence.
- Commit the wait/checkpoint and tool completion atomically, bound to the current
  run generation and execution/call identity. Retrying the same operation cannot
  extend its original deadline. A crash on either side of commit must neither
  lose the continuation nor replay earlier tools. A committed intentional yield
  is not another interrupted stage for the restart guard.
- End the physical turn at a safe boundary, retain the same session/transcript,
  release executor capacity and persist the absolute wake time. The scheduler
  checks due state after startup and on every pass; no blocking sleep is added.
- Keep intentional waits distinct from provider failures and their backoff.
  Existing token/time/iteration budgets still apply; deferral cannot extend a
  goal lifetime or evade iteration admission. Due times beyond a known remaining
  lifetime fail explicitly.
- Pause/cancel/correction and explicit resume keep their generation/retry rules.
  Pause or cancellation prevents automatic wake. Explicit resume may end a wait
  early. A late old owner cannot recreate a wait for a changed goal. Declining or
  ignoring human input never grants permission or triggers a new action.
- CLI/API/TUI/browser expose the saved wait reason and deadline, separately from
  current executor ownership. Preserve existing JSON/postcard compatibility;
  add optional JSON state rather than altering stored postcard layouts.
- Verify actual reached bounds, exact retries, concurrent ownership, crash/reopen
  before and after checkpoint, due/not-due scheduling, budget priority, steering,
  pause/cancel/resume and real tool dispatch. The native daemon probe must show a
  deferred run releases capacity, survives restart and resumes its transcript
  once without repeating a previous file/tool effect.

Status: shared engine, scheduler and operator paths are implemented. Managed-work,
clippy, frontend/browser checks and owned native daemon/restart/conversation probes
pass, including chat observer replacement and capacity-bound early resume. Audit
checks cover exact retries, hook effects, context restrictions and older JSON;
the complete local `cargo xtask ci` passes. Required platform CI is checked on
the exact revision before tagging. The
[implementation contract](../deferred-work.md) records current behavior and proof.
Freeze these reference pointers through implementation and release.

## Other directions

OpenClaw `1b61085679` distinguishes validation failures from policy blocks in
warning prose. Rook already has separate `ToolError::Invalid` and `Denied` and
validates unusable arguments before approval in `agent/tools.rs`; do not invent
a port merely to add another label. `b0f778e76d` preserves a cancellation across
repeated terminal callbacks. Rook's scoped nursery collects each future into a
single retained slot before yielding (`agent/delegation.rs`); its lifetime model
differs from a durable external callback registry.

Codex `8c076942` lets late agent mail wake a completed root behind an opt-in flag.
Rook drains its scoped children before ending their parent turn. A flag alone
would promise a lifecycle that does not exist: independent durable child owners
and a persisted mailbox require a separate design before such wake behavior.

OpenClaw's required-sandbox filesystem image projection, incognito diagnostics,
API-key account catalog publication, gateway breaker cleanup and OKF wiki decoder
are additional audit references, not claimed runtime ports. Rook's tool image
retrieval uses saved session/event references, not arbitrary assistant media
paths; it has no equivalent incognito or OKF wiki mode. The incoming ACP commit
updates the registry, OpenCode changes CI deployment runners, and Hermes adds a
plugin catalog listing/disclosure; none justifies a runtime dependency.

The R17-7 summarizer experiment remains inconclusive and separate. Preserve its
2048-token default until a successful measured comparison justifies adoption;
provider unavailability never blocks this independently available implementation.
