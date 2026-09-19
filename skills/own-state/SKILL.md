---
name: own-state
description: Use when a turn learns something worth keeping, or a recalled fact turns out wrong.
version: 1.0.0
license: MIT
keywords: [memory, skills, self, rook]
requires:
  agent: ">=0.1.0"
---

# Looking after your own state

Nothing extracts facts from a conversation for you. Memory fills up only if a turn
decides to write to it, and a turn that decides nothing leaves the next session
starting from the same blank page — which is the failure this exists for.

## What is worth remembering

A fact earns its place if a future turn would otherwise ask, guess, or repeat work:

- **A convention with a reason.** "Tests go in `tests/`, not beside the module —
  the build globs `tests/`." The reason is the half that stops it being undone.
- **A name only this project uses.** What "the gateway" means here.
- **A decision and why the alternative lost.** Six months later the alternative
  looks obvious again.
- **Something about the person.** How they want to be told bad news; that they
  read Russian and write English.
- **What did not work.** A failed approach saves the next turn the same afternoon.

What does not earn its place: anything the repository already says (`AGENTS.md`,
a comment, the code), anything true only for this turn, and anything you could
read again in a second. Memory is paid for on every request that recalls it.

## Scope it

```
remember { "text": "…", "scope": "project" }   # this workspace only
remember { "text": "…", "scope": "global" }    # everywhere this agent runs
```

Project is the default worth reaching for. A global fact is about the person or
the machine, not about the work: "prefers `just` over `make`" is global, "this
repo has no `justfile`" is not.

Pin only what must be in front of you regardless of the question — a standing
constraint, not a useful detail. Pinning loses to the recall budget anyway, and a
month of free pinning would spend the whole budget on facts nobody asked for.

## Correct it, do not add to it

A fact found wrong is corrected, not left beside its replacement:

```
forget { "id": "3f2a1b9c" }
```

Two facts disagreeing is worse than neither: the recall budget pays for both and
a later turn believes whichever it sees. `recall` shows the ids.

## When a skill is the better home

A fact is one sentence a request can afford. A **procedure** — steps, commands,
the order that matters — is a skill: paid for as a card until it is needed, and
only then loaded. If what was learned is longer than a sentence and would be
followed rather than known, write it as a skill instead.

```
write_skill { "name": "…", "description": "Use when …", "body": "…" }
```

The card says *when* to reach for it. A card that describes what the skill is
leaves every future turn loading the body to find out whether it wanted it.

## Check it from outside

The store is readable without a model in the loop, which is how to tell what the
agent actually knows from what it was supposed to learn:

```sh
rook memory ls                  # everything that applies here
rook memory search "deploys"    # ranked, and why each matched
rook memory since 7             # what was learned or forgotten this week
```
