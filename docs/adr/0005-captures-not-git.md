# 0005 — Content-addressed captures instead of git snapshots

**Status:** accepted

## Context

Checkpoint-and-rewind is one of the most requested agent features:
[codex `/rewind`](https://github.com/openai/codex/issues/11626) has 207 reactions,
[`/undo`](https://github.com/openai/codex/issues/9203) has 451. The obvious
implementation is to shell out to git.

## Decision

Capture an explicit file set into the content-addressed store, under a declared
budget. Never touch the user's git repository, index or working tree.

## Why

[opencode#3176](https://github.com/anomalyco/opencode/issues/3176) is the argument.
Session snapshots ran `git add .` over the working directory; on a 45 GB,
54,000-file data-science workspace that pinned the CPU and staged datasets nobody
asked to version, with "no warning, no configuration, no permission". A checkpoint
feature must not be able to do that.

Three properties follow:

- **A declared budget**, checked before the work: file count, total bytes, per-file
  bytes. Exceeding it is an error naming the limit, not a slow path.
- **Exclusions applied before the counters**, so `target/` and `node_modules/`
  never consume budget, with `.gitignore` honoured.
- **Content addressing**, so unchanged files across captures cost nothing — the
  property that made git attractive in the first place, without the side effects.

The same mechanism versions skills, which is where [hermes-agent#12238](https://github.com/NousResearch/hermes-agent/issues/12238)
wanted per-skill history and rollback.

## Cost

No git history, blame, or `git diff` on a capture. Restore writes files but does not
delete, so it reports what it left behind rather than silently producing a hybrid
of two versions.
