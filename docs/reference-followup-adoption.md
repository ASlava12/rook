# Reference follow-up adoption — 2026-10-16

All eight candidates in [the source review](research/reference-review-20261015.md)
were authorized for implementation. This document records the implementation and
its limits. Reference snapshots remain those recorded in that review; no release,
local-agent replacement or production daemon restart is part of this task.

| Capability | Implementation | Acceptance evidence |
|---|---|---|
| Source-bound skill updates | Implemented and verified | `skill_updates`: reordered/colliding sources, missing origin, explicit migration, personal edits, failed preparation |
| Scoped skill resources | Implemented and verified | `skill_updates`, agent-loop resource replay, CLI/API and actual Chrome stale-receipt check |
| Model-scoped reasoning replay | Implemented and verified | Agent-loop same-model continuation, model switch, legacy reasoning and retained transcript |
| Summarizer request budgeting | Implemented and verified | Large Unicode material fitted to auxiliary windows; existing ACP compaction lifecycle coverage |
| Prompt/session presets | Implemented and verified | Core CAS/persistence/bounds, browser save/load/restore/decline, TUI picker/undo and native PTY |
| Responsive saved-source diffs | Implemented and verified | Frozen-source API, split/unified parser, terminal resize/navigation and actual Chrome narrow/wide layouts |
| Browser diagnostics/viewports | Implemented and verified | Argument/redaction checks, ordinary approval/isolation tests, explicit native Chromium diagnostic probe |
| Optional MCP Apps | Implemented and verified | Core consent/owner/generation/hook checks; actual Chrome initialization, decline, approved call and iframe/network/navigation isolation |

## Skill origins and replacement

`rook skills update` matches the captured installation's source identity **and**
name. Changing configured source order cannot substitute another author's skill.
An absent or unreachable origin leaves the installed copy alone. Local edits
still produce `Edited`; authored skills remain outside updater ownership.
The updater selects the personal installation even when a project skill masks it.
Reinstalling without an explicit source retains the recorded origin.

An intentional migration is explicit:

```sh
rook skills install docx --source /path/to/reviewed/optional-skills
```

The source must already be configured. CLI and HTTP installation use the same
selection. Source notes and existing content-addressed skill captures retain
origin/history; no store-format change is required.

Replacement copies into a sibling temporary directory, validates `SKILL.md`, then
renames the installed directory aside before publication. Preparation failure
cannot delete the current skill. Failed publication restores the old directory;
if restoration also fails, the error names the retained recovery directory.
Capability-relative no-follow reads reject links/special files while copying;
files and bytes are bounded while read, and executable permissions are preserved.
Existing captures provide the version/undo history. Uncoordinated external file
writers are not locked by this installation protocol.

## Bundled skill resources

Loading a skill with bundled files includes a `resource_identity` in the existing
source envelope. The receipt covers the selected skill, canonical root and a
bounded file-to-content-hash catalog. It changes when resource bytes change even
if the author keeps the manifest version unchanged.

The agent uses the existing tool rather than another always-advertised schema:

```json
{"name":"fixture-skill","resource":{"path":"references/data.txt","identity":"receipt-from-load","offset":0,"limit":8000}}
```

`offset` and `next` are UTF-8 byte offsets. Pages contain at most 16 KiB, files
at most 8 MiB, and the catalog uses the existing skill capture bounds. Reads use
a retained no-follow directory boundary, reject unknown/traversing paths and
binary text, and recheck the receipt before returning. They do not execute scripts
or expand access to the agent's home directory. Results are ordinary durable tool
data, not another `SkillLoaded` instruction grant, and survive session replay.

Operator surfaces:

```sh
rook skills resource fixture-skill references/data.txt --json
rook skills resource fixture-skill references/data.txt --identity RECEIPT --offset 8000
```

The browser Skills view lists bundled resources with paged reads. HTTP exposes
`GET /api/skills/{name}/resources` and `POST /api/skills/{name}/resource`, using
the same optional workspace selection as skill resolution. Filesystem hashing
runs on bounded blocking workers, outside the daemon's async I/O workers.

## Reasoning and compaction

New readable reasoning events carry a scoped producer receipt in their existing
label: configured source/endpoint identity and actual dispatch identity where
known. Same-model replay keeps shortened reasoning. A different or unknown
producer excludes that reasoning from automatic requests and compaction input;
ordinary assistant text, tool pairs and the inspectable original history remain.
Existing signed/opaque provider-state protections remain separate. Context
inspection derives static identity without invoking credential helpers.

Compaction prepares the **actual** summarizer request, including instructions,
source envelope and carried summary. Input plus reserved output fits 90% of that
provider's advertised window by Rook's estimator. The usual 2,048-token output
allowance remains when it fits; smaller windows reduce it as needed. Bounded
head/tail elision states omissions and keeps the original journal available.
An unusably small window fails explicitly instead of looping over the same
elision marker. This is an estimate-based admission guard, not a claim that all
provider tokenizers agree or that a live-model quality gain was measured.

## Presets

The browser composer has **Prompt presets** for saving the current draft,
loading an editable draft, explicit replacement/deletion and restoring prior
text. Including model/effort is optional; applying settings asks explicitly.
No action submits the draft or broadens permission stance. Declining or ignoring
a confirmation leaves settings unchanged.

In the TUI, **F8** (named, remappable `presets` action) opens the picker without
clearing the current draft. Enter loads it; Ctrl+Z restores the prior draft.
Model/effort are displayed before selection. Active work or uncertain saved
submission prevents applying a preset's settings. The palette also exposes the
picker. CLI and slash commands expose the shared bounded action interface:

```sh
rook presets
rook presets '{"action":"save","name":"review","preset":{"prompt":"Review these changes","model":null,"effort":"low"},"expected":null}'
rook presets '{"action":"get","name":"review"}'
```

`/presets` lists/manages presets; `/preset NAME` loads a draft in the TUI and
inspects the saved request in the line REPL. `POST /api/presets` shares the same
request shape. Replacement/deletion requires the current revision returned by
inspection. A single bounded catalog uses optimistic store transactions so stale
clients cannot overwrite another window's changes. Limits: 32 entries, 32 KiB
per prompt, 256 KiB total. Unknown fields, including permission settings, are
rejected; no credential values are included in settings snapshots.

## Saved diffs

The existing review panel now includes file navigation, source/hash attribution,
paged unified patches and paired split rows. Narrow browser layouts use unified
view; wide layouts can use split view. Clicking an anchored line selects the
existing saved-source comment target. A page starting mid-hunk cannot invent line
anchors; original source reads remain available for precise comments.

`review` actions add:

```json
{"action":"diff","review":"REVIEW_ID","path":"src/main.rs","offset":0}
```

CLI/API return the saved patch page. In the TUI the same `/review` action opens
a resize-aware viewer: arrows navigate saved files/scroll, `n` reads the next
page, `a/u/s` select auto/unified/split, Esc closes. It reads the frozen review
snapshot rather than current workspace bytes. Existing comment identities,
review execution and selective undo are unchanged. This viewer extends the
existing bounded review-capture contract. Binary changes show original content
hashes and an explicit omission notice, without decoding bytes into source text.
They cannot receive invented line comments or independent content certification;
select text paths for an independent review. Binary status derives from the existing retained-text/hash fields, so the stored
snapshot shape and historical text-only source identities remain compatible.

## Browser observations

The optional native `browser` tool adds:

```json
{"action":"console","level":"error"}
{"action":"requests"}
{"action":"resize","width":375,"height":812}
```

Console levels: error, warning, info, debug. Dimensions: 240–3,840 CSS pixels.
Request observations omit static resources. Diagnostic URLs lose user-info,
query and fragment; credential-bearing header lines are redacted, in addition
to ordinary vault redaction. Output follows existing tool byte limits; the
conversation-owned browser, cancellation, driver lifetime and ordinary external
call approval remain. No arbitrary JavaScript execution surface was added.
Legacy drivers supporting the original actions can still connect; optional
unsupported commands fail through their normal tool result.

## Optional MCP Apps

Explicit opt-in:

```toml
[mcp_connections]
apps_enabled = true
```

The browser conversation's **MCP connections → MCP Apps** lists UI declarations
from already-connected tool catalogs. Opening a declared app fetches only that
server's `ui://` resource. The host implements the
[2026-01-26 MCP Apps protocol](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx)
for initialization, explicit same-server tool requests and text-to-draft requests.
The MCP handshake advertises the extension only when enabled. App-only tools are
kept out of the model catalog. CLI/TUI provide static discovery/descriptions via
`rook mcp-apps SESSION` and `/mcp-apps` in the TUI; interactive rendering requires
the browser, with the daemon retaining stateful leases.

Each HTML view runs in nested opaque-origin sandboxed frames. Parent DOM,
cookies, fetch, external images/scripts, forms, workers, nested frames and
external self-navigation are unavailable under the enforced policies. This
initial host supports self-contained HTML/JS/CSS apps. Server-requested CSP
exceptions, camera/microphone/geolocation/clipboard, external link opening and
automatic model-context updates are not granted. Unsupported methods return an
explicit error rather than appearing to succeed.

`tools/call` prepares one bounded pending request, displayed **outside** the
untrusted frame with exact server/tool/arguments. Only **Allow this call** consumes
its one-shot nonce. Decline, expiry, closing, reconnect, restart and another
session cannot approve it. Decline or expiry stops further tool requests from
that view until the operator explicitly closes and reopens it. Current policy is checked again before execution;
pre-tool hooks cannot be bypassed. Interactive hooks or hooks requiring separate
approval send the operation back to the ordinary agent interaction path. Frozen
experiments and mandatory domain containment refuse this extra call surface.
Vault secrets are never automatically supplied to an app.

Leases bind the owning session, connection generation and exact server instance.
Limits include eight backend views, bounded discovery bytes, 256 KiB HTML,
32 KiB arguments, one pending request/view, two-minute consent expiry, ten-minute
view expiry and a 45-second call deadline. A durable unknown receipt precedes an
external call. Unknown/error outcomes require inspection and closing the view;
confirmations never replay calls. Shared connection shutdown and iframe removal
clean up their owned state. The text-only server tool path remains available.

## Verification record

The native probe is [reference-followup.mjs](../xtask/probes/reference-followup.mjs).
It starts a scratch daemon, a deterministic local provider and owned MCP fixture,
then uses actual Chrome to check draft preservation, initialized app interaction,
absence of calls before consent, decline/one approved call, DOM/fetch/image/
navigation isolation, literal/frozen patches across viewport resize and stale
skill-resource rejection. It cleans successful probe directories and processes.
It does not use the operator's model, credentials or daemon.

Verified on macOS on 2026-10-16:

- `cargo xtask ci`: formatting, workspace Clippy with warnings denied, native
  frontend builds and the complete workspace test suite passed. The full native
  PTY suite includes preset load/undo without submission and session continuation.
- `node --test web/tests/*.mjs`: passed, including preset revision/draft handling,
  a late response after a conversation switch and exact saved diff anchors.
- Explicit installed-Chromium diagnostic test: console error, failed request,
  URL redaction, mobile/desktop resize and close passed. It waits for the page's
  observable resize result rather than assuming synchronous JavaScript painting.
- The owned native Chrome/daemon probe passed all checks, including refusal,
  one approved app call, isolation, frozen diff resize, stale resources and a
  near-capacity preset catalog read through CLI/daemon. This final boundary check
  also passed after reserving transport room for revision/name response overhead;
  the CLI change was rebuilt and Clippy checked again.
- All ten advanced reference worktrees are clean and their staged gitlinks match
  the inspected snapshots. Relative guide links and configuration-help JSON pass
  validation.

The initial integrated run exposed request-budget/catalog regressions, which were
corrected. Its gzip check also hit an already-full external temporary volume;
all subsequent checks used an owned temporary directory on the main disk. The
successful complete run includes that inflation-bound test, daemon address reuse
and the previously interrupted terminal session-continuation scenario.

Cross-platform runtime evidence here is macOS; portable tests remain for other
supported targets. Optional external-fixture tests retain their declared skips;
the new installed-Chromium check was run explicitly. No translation, cache/effort
optimization or live-model quality gain is inferred from these checks. Owned
build and temporary directories are removed after verification; compressed check
logs remain outside the repository.
