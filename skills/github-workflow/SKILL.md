---
name: github-workflow
description: Use when working on GitHub issues, PRs, checks or releases.
version: 1.0.0
license: MIT
requires:
  tool:
    gh: ">=2.30"
---

# GitHub workflow

Identify the repository, requested operation and exact branch or PR. Use `git`
for local history and `gh` for GitHub state. Read repository instructions and
release/landing conventions before changing remote state.

Check `gh auth status` and repository access. A configured account may be able to
read a public repository but lack write access. Use the intended existing account
for the individual command; avoid switching the global account underneath other
sessions. Keep tokens out of arguments, logs, patches and PR bodies. An absent
or declined credential is an ordinary unavailable result, not a reason to ask
repeatedly. See the examples below for commands.

For CI failures, establish the run's `headSha`, job ID and failed step. Retrieve
logs from completed jobs even when another job keeps the workflow running.
Separate compiler/test failures, unavailable infrastructure and model-quality
smoke failures. Fix the cause within the task's scope; preserve assertions that
establish a bound or a prohibited effect. Verify the resulting commit, rather
than citing a green check from its parent.

For a PR, inspect the base and head, diff, discussions and required checks. Write
the description around the final behavior and verification. Put multiline text
in a file and pass `--body-file`. Honor the user's existing authorization for
creating, commenting, pushing, merging or publishing; request new scope only
when the necessary external action was not authorized.

For an authorized merge or release, continue until the remote result is known.
Queued, draft, uploaded and published are different states. Verify the merged
commit or release tag, visibility, required checks and expected assets. Verify
asset checksums and an installation where applicable. After an ambiguous write,
read the remote state before retrying so a lost reply cannot duplicate an action.

Finish with the issue/PR/release URL, exact result, checks and any remaining
blocker. Pending checks are pending work; optional failures remain visible even
when required checks pass.

## Commands to adapt to the repository

Use explicit `--repo owner/repo` when the working directory is ambiguous. These
are examples, not permission to perform every operation listed.

```sh
gh auth status
gh repo view owner/repo --json nameWithOwner,viewerPermission
gh issue view 42 --repo owner/repo --json title,body,comments,state
gh pr view 55 --repo owner/repo --json baseRefName,headRefOid,state,mergeable,reviewDecision
gh pr diff 55 --repo owner/repo
gh pr checks 55 --repo owner/repo
gh run view RUN_ID --repo owner/repo --json headSha,status,conclusion,jobs
gh api repos/owner/repo/actions/jobs/JOB_ID/logs
```

A running job may not have downloadable logs yet. Read completed failed jobs,
then wait with a bounded polling interval; do not repeatedly fetch a known 404.
Use the fields reported by the installed `gh` if a JSON field is unsupported.

Prepare the final title and body before an authorized write:

```sh
gh pr create --repo owner/repo --title 'Fix the concrete behavior' --body-file /path/to/body.md
gh release create v1.2.3 /path/to/assets/* --repo owner/repo --verify-tag --draft --notes-file /path/to/notes.md
gh release edit v1.2.3 --repo owner/repo --draft=false --latest
gh release view v1.2.3 --repo owner/repo --json tagName,isDraft,isPrerelease,assets,url
gh pr view 55 --repo owner/repo --json state,mergedAt,mergeCommit
```

Check tag-to-source identity separately: a release's target branch name is not
proof of the commit behind its tag. For annotated tags, inspect the peeled ref.
The draft, upload and publication steps above apply only when publishing was
requested. Use the repository's merge queue and protection rules when merging.

When another configured GitHub account is needed, resolve its token privately
and pass it through a child process environment. Do not print it or embed it in
a shell command. Do not turn a read-only review into a write operation.
