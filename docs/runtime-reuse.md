# Rooted runtime reuse

Each frontend owns expensive equipment across turns: its language-server pool,
MCP session, job registry and approval policy. Shared-workspace delegates reuse
that equipment. A worktree delegate builds local tools, language servers and
jobs for its checkout; parent-only tools and editor-owned file/terminal bridges
do not transfer into that execution root. A shared store does not make filesystem
claims or equipment interchangeable between roots.

## Preparing a daemon project

The daemon canonicalizes the requested directory and admits one cache entry
before copying discovery inputs. Both ready projects and preparations count
toward `server.max_projects`. Requests for the same root await the same entry.
Plugin and skill discovery runs on a blocking worker without a project-map or
engine lock. Cancellation of an opening request leaves the admitted preparation
running, so a subsequent request can use its result.

An idle project can be evicted only when no requester owns its publication entry
and no caller owns its engine. This includes the interval between publication
and a waiting caller cloning the engine; eviction there would create separate
writing registries for the same root. A failed preparation is removed by identity
when its result is consumed, allowing retry without removing a replacement.

The prepared engine receives the daemon's current configuration at publication.
Coherent configuration refresh defers while a project is preparing or an engine
is busy. Existing live instructions, notices and health/context requests can
continue during disk discovery. `Rook::for_workspace` retains its synchronous
contract; `workspace_seed` lets a frontend move discovery off its engine lock.

## MCP generations

Reconnect is explicit and uses the selected root's trusted declarations. A
candidate publishes only after discovery succeeds. In-flight calls and old
tool snapshots retain their original connection; later snapshots use the new
generation. A failed or cancelled candidate preserves the working catalog.
Calls already sent are never replayed through a replacement connection.

## Evidence

`actual_delegates_reuse_rooted_lsp_jobs_and_run_approvals_while_worktrees_rebuild_and_drop_parent_tools`
executes real delegated tool calls and mock LSP processes: shared children use
one initialization, the same jobs context and one run approval; the worktree
starts a different process rooted in its checkout and rejects a parent-only tool.

Daemon API fixtures hold discovery on an owned worker, cancel its first requester,
exercise live instruction/notice delivery and engine access, then verify one
published engine with the current configuration. A reached project-capacity
fixture protects ready engines while publication waiters still own them.
`mcp_connections` uses actual HTTP discovery and held calls to verify generation
replacement, cancellation and admission bounds. These are controlled fixtures,
not measurements of model quality or production startup speed.
