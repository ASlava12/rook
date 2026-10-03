# Pi adoption scope audit

Scope: [the original review](pi-reference-review-20260930.md) and
[the implementation tracker](pi-adoption-20260930.md), with Pi pinned to
`ee602414c703be8da722ec56de7f2399e62581ac`. This audit covers the multi-day
transfer, including the queue and session-inspection follow-ups from the user.
The tracker retains the sequence of failures, fixes and actual gate outcomes;
old "next" paragraphs describe their historical block, not the current backlog.

## Main capabilities

| Capability | Original requirement and retained evidence |
|---|---|
| Bounded delivery/recovery | Byte and frame admission before allocation, paced slow clients, atomic replay/subscription, bounded current controls and snapshot recovery; see tracker **Snapshot recovery completed** and current daemon/replay tests. |
| Steering/follow-up queue | Shared durable IDs, latest pending revision, edit/withdraw-to-draft, immutable acceptance, restart/retry and ordinary versus whole-goal boundaries; see **Named follow-up action and fresh managed-goal boundary checks** and its requirement matrix. Fresh local/shared TUI and browser interactions verify pause/continue/completion and exactly one accepted follow-up. |
| Keyboard actions/prompt undo | Shared registry, remapping/config/help, bounded undo with Unicode, paste, mentions and external-editor replacement; the earlier PTY evidence and current editor tests cover the requested transfer. Kill-ring expansion is a possible subsequent enhancement in the review, not a pending undo requirement. |
| Branch navigation/summary | Existing IDs, tree/names/bookmarks, historical-event forks and optional reviewed/model-assisted carry; see **Branch switches during model output and ordinary prompt admission** and [branch semantics](../conversation-branches.md). Source event scope, historical file/test disclaimer, local busy refusal and shared live switches are verified. Navigation does not rewind workspace files. |
| Inline tool cards | Live/saved compact and expanded results, measured errors/duration, saved diffs, command/search/MCP facts, bounded explicit image retrieval and terminal text fallback; see **Explicit retained image presentation** and the preceding browser/native result-navigation blocks. |
| Context provenance | Saved request-specific sources, instruction scope, advertised versus loaded skills, deferred tools, provider delivery and volatile additions in CLI/API/TUI/browser; see **Browser context provenance view** and source/catalog compatibility tests. The inspector is not added to model context. |
| Local HTML export | Explicit saved event scope, escaped content, bounded body/event admission, expandable details and source links, no overwrite/publication; see **Actual HTML download and native export interactions**. Real browser file download/render and local/shared TUI/CLI output parity passed. |
| Opt-in phase routing | Static named-model policy, branch-owned transitions, selected/physical/reported dispatch, frozen turn target, compatible images/schema/reasoning, native continuation/fallback/reopen/fork, physical/auxiliary/delegated cost coverage and live frontend observations; see [phase routing](../phase-routing.md) and **Live phase-routing frontend evidence**. Real task comparison is completed with negative candidate findings in the [comparison note](pi-phase-comparison-20261003.md). |
| Declarative extension UI | Opt-in bounded saved status/progress/forms/result/clear with source attribution, text fallback, persistent native/browser panels, typed answers and cancellation/reconnect/reopen/fork; see **Persistent live extension reports completed**, [contract](../extension-ui.md) and repository probes. No arbitrary extension runtime was added. |

The comparison includes independent quality, observed wall time and actual
per-dispatch-model token usage including auxiliary requests. The committed
[1536-token summary](pi-phase-comparison-20261003.json) and
[4096-token summary](pi-phase-comparison-4096-20261003.json) preserve every arm
and native exit. Both suite runners exited **1** due invalid planning cases.
The higher-budget eligible pairs failed routed correctness; Gemma is therefore
not adopted as an automatic implementation default. Configured prices are
absent, so USD remains unknown and monetary savings are unmeasured. Comparing
the observed failures fulfils the requested evaluation; it does not establish
equivalent completed work, lower monetary cost or a general recommendation.
No retry-to-pass benchmark series is pending.

## Additional requests and optional experiments

The next queued message is pinned to the bottom while LLM output and a
multiline draft grow. Unicode ordering/counts, actual native queue interactions
and the integrated `queued_message_stays_at_the_bottom_as_output_and_draft_grow`
regression cover the user's request.

Session `01M3T336VB22NEP5AY77T5NYB4` was inspected through the actual daemon.
One valid question at event 1641 timed out after 1800 seconds. Later malformed
ask attempts never reached the TUI. The old 2,000-event Calls pane hid those
attempts; paging and bounded malformed-ask guidance/choice compatibility were
fixed. See **Request-prefix source provenance** and **Recovering from malformed
`ask` calls**. There is no saved evidence of repeated valid question delivery;
the unrelated benchmark's model failures do not establish this session's cause.

Both terminal experiments now have bounded prototypes, native observations and
explicit decisions:

- [Regular scrollback](terminal-scrollback-20261003.md): shrinking the inline
  viewport moves the next-message queue upward; direct production adoption was
  rejected. Native fullscreen shrink was refused by this Win32 transport, so
  its native shrink behavior is not certified by a TestBackend result.
- [Terminal images](terminal-images-20261003.md): ConPTY and isolated WezTerm
  mux probes received DA without a positive Kitty response. The console may
  consume queries before mux parsing; GUI pixels/crop/delete were not verified.
  Keep attributed text/export here. This does not declare WezTerm unsupported.

These were experiments in review section 7. A production scrollback adapter or
rich image renderer requires the retained transport/interaction evidence before
adoption; neither was promised as a mandatory replacement for the existing TUI.

## Bounds, compatibility and final verification

The transfer preserves existing store schemas, postcard event layouts and ID
relationships. New optional wire/JSON fields and bounded companion notes keep
legacy defaults and fallback text. Storage changes add bounded transactional
claims/companions and durable accounting without a format migration. File, body,
queue, snapshot, capture and extension-report bounds are applied before copying;
oversized/refusal tests remain in the integrated gate.

The last audit fixed the empty-prefix zero receipt quota: a new named-session
claim is refused at `work.max_messages=0`; an existing receipt stays readable
at the lowered quota, including daemon restart. Store, core and actual daemon
regressions exited 0 (`target/pi-claim-zero-focused.log`,
`target/pi-claim-zero-core.log`, `target/pi-claim-zero-native.log`). No new model
request, UserMessage or claim is published on refusal.

Node browser and native harness tests are separate from Cargo's gate. The
current UI source was verified by 45 browser tests
(`target/pi-extension-live-all-browser-tests.log`), and the current comparison
runner by 13 Node tests (`target/pi-phase-gemma-runner-final-tests.log`), both
exit 0. Controlled scripted providers prove protocol/UI behavior; only the
separate real-model suites supply quality/timing/token observations.

Final `cargo xtask ci` for the quota fix and combined scope audit exited **0**
in 545.1 seconds (`target/pi-claim-zero-final-ci.log`), including fmt, Clippy,
frontend builds, workspace tests and doctests. Rust sources stayed unchanged
during the gate. `cargo xtask compaction` exited **0**
(`target/pi-claim-zero-compaction.log`): 23.31 MiB logical, 5.29 MiB distinct,
0.14 MiB warm objects, 4.02 MiB disk, 37.1x dictionary and 5.8x end-to-end.
The fixture and published storage claims remain unchanged.

All nine main capabilities, both requested follow-ups and both optional
experiment assessments have reached their defined completion evidence. No
mandatory transfer feature remains pending. Further monetary measurement needs
real configured prices; production terminal adapters need the separate retained
transport/interaction evidence. Neither is represented as a shipped capability
or a measured saving. The completed block is ready for its commit and clean-tree
check; the full transfer goal can then be marked complete.
