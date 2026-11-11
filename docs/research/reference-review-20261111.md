# Reference review, November 11, 2026

This is a follow-up review of the ten checked-out reference snapshots, compared
with Rook's current implementation. It is not a fetch or a review of newer
upstream commits. Gitlinks remain at the same pins; no reference source was
modified. Ideas were independently implemented in Rook.

| Reference pin | Source inspected | Finding and decision |
|---|---|---|
| Pi `482c1381a00d` | `packages/coding-agent/src/core/compaction/{compaction,utils}.ts`, `packages/durable/src/harness/context.ts` | Separate file-operation metadata survives model-written summaries; summary serialization bounds each tool result. Adopt bounded file-request metadata and per-result shortening, with source event numbers, explicit omissions and whole-render JSON accounting. Rook distinguishes requests from successful changes. |
| Cline `a42df8c8d5fd` | `apps/vscode/src/sdk/sdk-compaction-coordinator.ts` | Read/compact/persist belongs to one owned session; a local command must not masquerade as a model prompt. Rook already has real core compaction and committed-boundary observers. This prompted an independent fallback audit: preserve bounded previously committed prose when a later summary fails. This is a Rook correction, not a claim that Cline uses the same fallback. |
| Codex `8c076942cb70` | `codex-rs/core/src/tools/handlers/multi_agents_v2/{message_tool,send_message}.rs` | Queued peer messages and turn-triggering follow-ups have different lifetimes. Rook already implements queued native-family messages and closed-recipient receipts. Keep these semantics; do not add implicit wakeups or a second agent executor. |
| OpenClaw `e1e4a0dffa2b` | `src/agents/tools/sessions-send-tool.a2a.ts` | Delivery is scoped to a run and a session generation, with idempotent send keys. Rook's existing cross-computer transport already binds durable messages/controls to admitted route/task/generation. No additional channel-routing subsystem. |
| Hermes `22063fc099ec` | `tools/delegate_tool_toolsets.py`, `agent/tool_guardrails.py` | Children intersect inherited equipment; loop checks must distinguish semantic results from changing bookkeeping. Rook already enforces equipment at catalog/dispatch and has durable bounded cycle receipts. Keep those mechanisms and their tool-specific result normalization. |
| Goose `34a222e3f2b9` | `crates/goose/src/agents/{subagent_task_config,agent}.rs` | Parent workspace/session identity and bounded subtask execution are explicit; permission/security inspection precedes repetition inspection. Existing Rook Crew/Bounds and tool admission cover these needs. No new parallel permission engine. |
| OpenCode `802f2d8e7509` | `packages/core/src/tool/plugin/subagent.ts` | Background tasks have owned completion and separate session identity. Rook already has nursery collection, progress coalescing, equipment restrictions and worktree recovery. Keep the shared engine instead of adding a frontend-specific coordinator. |
| OpenHands `852132d38dbc` | `src/api/agent-server-compatibility.ts`, `src/services/agent-state-service.ts` | Separate backend compatibility and agent state from the UI. Rook already has a separately served web workspace, API health/version checks and explicit connection recovery. Preserve that boundary. |
| ACP `2c8270462725` | `agent-client-protocol-schema/src/v1/client.rs` | Child notifications and unstable protocol additions need explicit negotiation. Preserve the current negotiated adapter; native compaction enhancements do not require a wire-schema or ACP-version change. |
| OpenResearch `6285fee7f42b` | `agent-skills/orx-agent-delegation/SKILL.md` | Independent project chats differ from subordinate native agents; worktree and compute authority must not be inferred. Existing Rook native mailboxes and operator-scoped remote work keep those lifetimes separate. No automatic worktree integration. |

## Delivered changes

All three changes use the shared core; CLI, TUI, web and API turns receive the
same behavior without frontend-specific implementations:

1. Structured bounded file-request continuity across compactions, failed
   summaries, restart and fork. The model can omit filenames without erasing the
   admitted native-tool/path references. Failed/refused requests are explicitly
   not evidence of modifications.
2. Summary/checker input gives individual tool results a bounded head/tail
   representation and enforces the complete rendered span's estimated-token
   admission after JSON quoting. Even one oversized entry cannot bypass it.
3. Failed subsequent summaries retain a bounded portion of the prior committed
   prose and a distinct unsummarized-source warning. Repeated failures cannot
   grow carried fallback context indefinitely.

Implementation: `rook-core/src/compaction_files.rs`, `agent/compaction.rs`, and
the existing `Rook::last_compaction` replay projection. Optional JSON companions
extend the current compaction event; no postcard/store schema or tool wire enum
changes, UI embedding, new runtime dependency, or automatic network call.

The exact contract and limitations are in
[long-task-compaction.md](../long-task-compaction.md). In particular, path
tracking is not a shell/MCP/child inventory, and paths in argument bodies already
truncated by transcript admission are omitted instead of reconstructed.

## Verification

Focused tests exercise actual AgentLoop file tools, a failed read, two
compactions (including a failed summary), ordinary subsequent replay, fork and
store reopening. Unit controls reach entry/encoded-byte admission, lower the
configured allowance on reload, preserve source-data quoting and challenge the
rendered-span limit with large control-character bodies. Repeated failed
summaries check bounded growth and retained earlier constraints.

Native verification passed on Windows x86_64: `xtask/cargo.cmd xtask ci` exited
with status 0 and `ci: ok`. This includes formatting, workspace/all-target Clippy
with warnings denied, fresh CLI/daemon builds and the complete workspace test
run with `--no-fail-fast`. The final run used `CARGO_BUILD_JOBS=8` and
`RUST_TEST_THREADS=8` without narrowing the gate. Web runtime syntax checks and
the separate Node test suite also passed; the final diff passes whitespace checks.

Build artifacts stayed in the physical `Z:\Projects\rook\target` and temporary
files in `Z:\RookBuildTemp`. Scripted providers establish engine behavior, not
real-model summary quality.
No live provider prompts, remote GitHub CI, or physical two-computer deployment
are implied by these checks.
