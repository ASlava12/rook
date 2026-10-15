---
name: workspace-cleanup
description: Use when reclaiming disk space from build outputs, caches and task artifacts.
version: 1.0.0
license: MIT
---

# Workspace cleanup

Measure the requested filesystem and its largest immediate children first. Use
the platform's disk tools and avoid scanning unrelated mounts. Distinguish
apparent size, allocated disk use and hard-linked/shared data. A decrease in a
directory's apparent size does not necessarily increase free space by that amount.

Classify each candidate as regenerable build output/cache, active state, model
weights shared by consumers, or user-authored data. Inspect ownership, symlinks
and active compiler/download/service processes. A directory named `cache` may
contain keys, patches and reports. Preserve these and source worktrees. Use the
native cache manager where it can distinguish live entries from stale ones.

Work within the user's requested cleanup scope. Record each exact candidate,
why it is disposable and the evidence that no live task owns it. Prefer a dry
run when the native command offers one. Do not follow a link and recursively
delete its target. For content-addressed models, account for every retained
snapshot/reference before removing shared blobs.

Delete reviewed targets in bounded batches, then measure free space again.
Keep reports and final deliverables. Remove temporary virtual environments,
downloads and generated test workspaces owned by this task after verification.
Do not immediately recreate a large cache by rerunning a cold full build when
the requested change has already passed its required checks.

For a Rook store, use its `store stat`, `store prune --dry-run`, `store gc
--dry-run` and `store verify` commands; never remove indexed objects manually.
Report what was removed, what was retained and measured free space before/after.
Identify files still held open or snapshots retaining blocks when reclamation
does not match the estimate.
