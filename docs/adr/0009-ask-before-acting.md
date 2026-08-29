# 0009 — Ask before acting, and never let an approval beat a denial

**Status:** accepted

## Context

Rook runs shell commands and edits files on the user's machine. Until now the
only guard was a deny list checked inside the exec tool, and the `allow` list in
the config did nothing at all — a setting that promised something it did not
deliver.

The references converge on the same three-way answer. goose has
`AlwaysAllow | AskBefore | NeverAllow` per tool and, in its own words, defaults
to needing approval "for safety". codex carries an `approval_policy` on every
turn. goose also has an open request for regex-based allow/ask/deny rules on
shell commands specifically, because one switch for "may it run commands" is too
coarse: `git status` and `rm -rf` are not the same request.

## Decision

A policy with three modes — `auto`, `ask`, `readonly` — and three rule lists
matched against what a call would actually do. Default mode is `ask`.

Order of resolution, and the order matters more than the lists:

1. Read-only calls are always allowed and never reach the rest.
2. **Deny wins outright**, before mode and before any prompt.
3. `readonly` mode refuses anything that would change the machine.
4. An approval granted for this run.
5. `allow` rules.
6. `ask` rules — these prompt *even in `auto` mode*.
7. Otherwise: `auto` allows, anything else asks.

A rule is a substring, or a regular expression when written `/…/`.

## Why these specifics

**Asking is the default.** goose's comment is the argument: when nothing matched,
the safe answer is to ask. An agent given a tool it was not configured for should
stop, not improvise.

**Nothing can override a denial.** If an approval prompt could unlock a denied
command, the deny list would be decorative — and the one moment a user is most
likely to click through a prompt is the moment they should not.

**A question shows what it is asking about.** An approval that names only a path
is an approval of something the person cannot see, and the one moment they are
most likely to click through is the moment they should not. A tool that can say
what it would change says it — a diff for a write or an edit, the body for a
skill being authored — and the same string reaches the terminal, the TUI, the
browser and the editor, because a question that is answerable in one front end
and blind in another is the same defect as a capability reachable from one.

It is built only when someone is actually going to be asked: a diff of a call the
policy already allows is work nobody reads.

**No approver means refuse.** An unattended run — a script, a cron job, the
daemon — gets an approver that always declines, with a message naming `--yes` and
the config key. Silently allowing would put the least-supervised runs at the
greatest risk.

**Deny rules are anchored regexes, not substrings.** The obvious `"rm -rf /"`
substring also blocks `rm -rf /tmp/scratch` and `rm -rf ./target`. A deny list
that cries wolf gets turned off, so the shipped rules are tested in both
directions: what they must stop, and what they must leave alone.

## What "for this run" means

An approval granted for the run lives on the policy, and the policy's lifetime is
the front end's, not the turn's. In an interactive session — `rook chat`, the
TUI, one websocket connection — it is built once and shared by every turn, so
"always" means until the session ends. In `rook run` a turn *is* the run.

Getting this wrong is invisible from the code and obvious in use: the user says
"always" and is asked again on the next turn, which is precisely what they
declined.

## Hooks fit under the same rule

A `pre_tool` hook may answer `allow`, `ask` or `deny`, and it is consulted after
the policy rather than before, so it can loosen an `ask` but never a `deny`. A
hook that fails to run is treated as a denial for the call it was guarding: a
guard that could not run is not an approval.

## Cost

- `rook run` no longer writes files unattended without `--yes`. That is a
  behaviour change, and the refusal says so.
- Rules match text, not intent. `curl … | sh` is one obfuscation away from any
  pattern. This raises the floor; it is not a sandbox, and
  [the roadmap](../roadmap.md) says where a real one would go.
