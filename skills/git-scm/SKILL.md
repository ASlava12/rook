---
name: git-scm
description: Use when managing commits, branches, conflicts, history investigation or a requested rebase.
---

# Git and source control

Inspect status, current branch, worktrees, relevant history and remotes before
mutating repository state. Distinguish user edits, generated files and your own
changes. Follow the project's branch and commit conventions.

For commits, stage the intended paths and inspect the staged diff. Keep a commit
coherent and explain the behavior and reason. Do not include unrelated edits or
credentials. Commit, push and history rewrite are separate actions; use the
authorization actually given for each.

For conflicts, understand both changes and their combined intent. Resolve at the
behavioral level, check generated/lockfile sources, remove conflict markers and
run checks for the combined result. Taking one side wholesale is not a general
resolution strategy.

For bisect, establish known good/bad revisions and a reliable predicate. Use an
isolated worktree when existing edits need protection. Distinguish a build that
cannot run from a reproduction that passed, and restore the bisect/worktree state
when finished.

Before a rebase or other rewrite, identify shared history and preserve a recovery
reference. Do not use reset/clean or forced checkout to discard work as routine
cleanup. Do not force-push shared history without authorization for that effect.

Report resulting branch/commit state, checks and any unresolved conflicts. If an
operation stops partway through, name its actual state and recovery options rather
than pretending the repository is clean.
