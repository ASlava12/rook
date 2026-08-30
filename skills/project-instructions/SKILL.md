---
name: project-instructions
description: Write or revise a project's AGENTS.md — what belongs in standing instructions, what does not, and how to keep one worth its cost.
version: 1.0.0
license: MIT
keywords: [agents-md, conventions, onboarding, context]
requires:
  agent: ">=0.1.0"
---

# Writing a project's AGENTS.md

`AGENTS.md` in the workspace is read into the system prompt on every request.
That is the whole trade: it is the one place to say something once instead of
every turn, and it is paid for every turn whether or not it is needed. A long one
is worse than none — it crowds out the conversation and teaches whoever reads it
to skim.

## What belongs in it

Only what a competent newcomer would get wrong and cannot read off the code.

- **How to build, test and check.** The one command that gates a change, not a
  list of every command that exists.
- **Rules with a reason.** "Do not add a dependency edge that reverses the
  layering" is a rule; "we use Rust 2024" is in `Cargo.toml`.
- **Traps.** The thing that looks right, compiles, and is wrong — and what
  happens when someone does it. These are worth their weight because nothing else
  records them.
- **Where the non-obvious decisions are written down.** A pointer to the ADRs
  beats a summary of them that will drift.

## What does not

- What the code says. File layout, type names, what a function does.
- What a tool prints. Versions, dependency lists, the test count — a number in
  prose rots, and a wrong one is worse than no number.
- General advice about writing software. The model has that.
- Anything that is true today because of a task in flight.

## Writing one

1. Look before writing. Read the build files, the test entry points, the CI
   workflow and the top-level directories. `list_dir` and `read_file` first; do
   not open with a template.
2. Find the traps by looking for where the project defends itself: assertions
   with long messages, comments that explain why rather than what, tests named
   after a failure. Those are the sentences worth carrying forward.
3. Write it as claims a reader can check, not as aspirations. "Every crate is a
   library, so `pub` turns off the dead-code lint" is checkable; "we value clean
   code" is not.
4. Keep it under what `[agent] max_instructions_bytes` allows — 32 KB by
   default, and most projects want a small fraction of that. If it is cut, the
   prompt says so, which means the end of your file was not read.

## Where it goes

```
<workspace>/AGENTS.md   this project's, and it has the last word
~/.rook/AGENTS.md       yours, wherever you work
```

Both are read, most general first. Put in the personal one what is true of you
rather than of the project — a preferred shell, a habit about commits — and
nothing a collaborator would need.

## Check it

```sh
rook doctor
```

The "standing instructions" section names each file it read and its size, and
says when one was cut. A file that is there and not listed is in the wrong place
or is empty.

## Revising one

Prefer deleting to adding. An instruction that has never changed an outcome is
costing tokens on every request to say nothing; an instruction that is followed
without being read is already in the code. When something goes wrong twice, that
is the sentence to add — and say what went wrong, not just what to do.
