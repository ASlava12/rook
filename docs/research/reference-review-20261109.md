# Reference review — November 9, 2026

Scope: the ten pinned submodules in `references/`, the existing adoption ledger,
and Rook's current delegation, equipment, context and recovery paths. Submodules
were initialized at their recorded commits; no upstream pointer was advanced.
Ideas were independently implemented; no source was copied.

The most useful gap is coordination between existing native agents. Rook already
has asynchronous children, worktree isolation, structured results, shared family
budgets, checkpoint-aware undo, MCP, ACP executors and durable managed work.
Adding another scheduler or replacing these mechanisms would duplicate working
capabilities. This change extends their shared engine.

| Reference / pinned commit | Inspected source and finding | Decision |
|---|---|---|
| Codex `8c076942cb70` | `codex-rs/core/src/tools/handlers/multi_agents_v2/{message_tool,send_message}.rs` separates queued messages from turn-triggering follow-ups; `multi_agents/send_input.rs` targets existing agents. | Adopt direct addressing and queued delivery. Keep completed agents closed; do not introduce automatic wake or extra model budgets. |
| OpenClaw `e1e4a0dffa2b` | `src/agents/tools/sessions-send-tool.{input,a2a}.ts` distinguishes admission, reply flow and delivery ownership. | Adopt explicit delivery states and engine-assigned sender identity. Omit channel routing and automatic reply chains. |
| Hermes `22063fc099ec` | `tools/delegate_tool_toolsets.py` intersects child toolsets with parent tools and strips blocked tools. | Add per-task `tools` intersected with Rook's existing recipe equipment, enforced at catalog and dispatch. Retain Rook's existing policy, depth and cost limits. |
| Goose `34a222e3f2b9` | `crates/goose/src/agents/subagent_task_config.rs` bundles parent session, workspace, extension selection and turn limits. | Retain Rook's shared Crew/Bounds path; carry task equipment and family mailboxes through it rather than a second executor. |
| OpenCode `802f2d8e7509` | `packages/core/src/tool/plugin/subagent.ts`, `test/subagent-lifetime.test.ts` distinguish foreground/background work and test completion/cancellation ownership. | Keep existing `delegate`/`subagents`; use a membership guard for mailbox cleanup and retain result collection semantics. |
| Pi `482c1381a00d` | `packages/durable/src/harness/{context,scheduler}.ts` separates committed history and live/sleeping task ownership. | Persist only admitted recipient context; keep mailboxes bounded and runtime-local. Do not turn volatile messages into durable scheduled work. |
| ACP `2c8270462725` | `agent-client-protocol-schema/src/v1/client.rs` describes announced child identity and independent capabilities. | Preserve current editor/executor boundaries. Native mailboxes do not pretend to be cross-process ACP or network A2A. |
| Cline `a42df8c8d5fd` | `apps/vscode/src/sdk/sdk-compaction-coordinator.ts` serializes read/compact/persist and retains the replacement context. | Include admitted messages in Rook's existing replay and compaction projection; do not add a separate summary store. |
| OpenResearch `6285fee7f42b` | `agent-skills/orx-agent-delegation/SKILL.md` distinguishes independent chats from subordinate agents and forbids accidental worktree integration. | Local mailboxes stay within a native family. Independent remote tasks retain their own conversations, workspace and operator policy. |
| OpenHands `852132d38dbc` | `src/api/agent-server-compatibility.ts` and `src/services/agent-state-service.ts` expose server compatibility and agent state through frontend boundaries. | No frontend-specific coordinator or replacement runtime. CLI/TUI/daemon/editor use the shared core; remote work uses the existing supervisor. |

Implemented features and usage are described in
[agent-messaging.md](../agent-messaging.md). The mailbox implementation is
`rook-core/src/agent/messaging.rs`; child equipment is wired into both delegation
paths. Received Notes retain source identity through replay and bounded checker
history. Existing limits, output contracts, provider billing and worktree
recovery stay on their current paths.

The user's additional cross-computer requirement is implemented independently in
[remote-agents.md](../remote-agents.md): configured HTTP(S) peers, separately
authenticated requests and replies, durable task submission, scoped live messages,
status/result pages and generation-bound cancellation. The implementation reuses
Rook's own inbound source/receipt transactions and ordinary work supervisor.
It adds neither an arbitrary remote shell nor a replacement agent loop. Remote
work has its own operator budgets and does not pretend to be included in the
sender's local model-cost accounting. The local volatile mailbox and remote
durable task transport have different lifetimes deliberately.

Validation on Windows x86_64: `cargo xtask ci` passed, including formatting,
workspace/all-target Clippy with warnings denied, fresh CLI/daemon builds and
`cargo test --workspace --no-fail-fast`. The final run used
`CARGO_BUILD_JOBS=8` and `RUST_TEST_THREADS=8` without narrowing the gate.

Native-loop integration tests verify sibling/parent delivery with attributed
replay and task-tool restrictions on both delegation paths. Transport tests cover
bounded arriving responses, authenticated request/reply binding, route isolation,
lost-ack retries, redacted result pages, original-generation controls and the
shared remote-task allowance. The CLI integration test runs two independent
`rookd` processes with separate stores/workspaces, submits actual managed work,
delivers a live message, checks its result after restart and retries admission
without duplicating the task.

Those daemons ran on one host over real TCP; physically separate computers and
the documented SSH/HTTPS deployment were not exercised here. Scripted providers
prove engine delivery and dispatch behavior; they cannot prove that a real model
will choose good assignments or act correctly on a peer's findings.
