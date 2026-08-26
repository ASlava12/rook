# 0010 — A line in the prompt instead of a checklist tool

**Status:** accepted

## Context

Every comparable agent has one. codex shipped Plan Mode after a 503-reaction
request, goose has a todo extension enabled by default, opencode has plan and
build modes, Claude Code has a todo tool. The obvious move is to add a
`todo_write` tool and instruct the model to keep a checklist.

Before doing that, [goose#11172](https://github.com/aaif-goose/goose/issues/11172)
is worth reading: its author benchmarked the todo extension on and off as a
single-variable A/B across four models.

| model | pass rate off → on | cost with todo |
|---|---|---|
| haiku-4.5 | 62.8% → 71.8% | +18% |
| sonnet-5 | 78.2% → 80.8% (noise) | +17% |
| opus-5 | 88.5% → 88.5% | +45% |
| gpt-5.6-sol | 79.5% → 85.9% | +49% |

Three findings matter more than the deltas:

1. **The cost is induced turns and per-turn context, not the tool calls.** Lazy
   loading the schema would recover none of it.
2. **On hard tasks the tool bought nothing and cost a "closure loop":** +39%
   spent re-verifying finished work to check off remaining items, because the
   instructions say to verify everything is complete.
3. **Usage was near-universal and uncorrelated with success.** What drives it is
   the reminder to fill an empty list, not any judgement about whether a plan
   helps.

The author then tested two variants. Keeping the tool but reframing its prompts
as optional notes held the pass-rate gain at *below* todo-off cost — and the
models nearly stopped calling the tool. Removing the tool entirely and adding one
system-prompt line asking for a brief plan first did the same, and on haiku beat
the stock tool outright (70.5% against 65.4%).

## Decision

No planning tool. One line in the system prompt, on by default:

> For anything that takes more than one step, say the plan in a sentence or two
> before acting, and say so when it changes. Do not keep a checklist.

Separately, the *user* may set a goal on a session. The agent is told it and
never asked to maintain it.

## Why this shape

**The instructions are the active ingredient**, by the reference's own
measurement. A tool adds a schema to every request, a call to every plan, and a
reminder that induces bookkeeping — for a benefit the sentence already delivers.

**"Do not keep a checklist" is in the line on purpose.** The closure loop is the
specific pathology being avoided, and a model that has been asked to plan will
otherwise invent the bookkeeping the tool would have imposed.

**The goal is the user's, not the agent's.** It gives the same "are we still on
track" visibility with no model overhead at all, because nothing has to be
maintained. It also survives compaction, which a checklist in the transcript
would not.

## Cost

- No structured progress for a UI to render. `session context` and the transcript
  are what there is.
- Sub-agents get the plan line but not the parent's plan; a delegated task states
  its own.
- If a future model genuinely needs structured plan state, this is worth
  re-measuring rather than assumed — the benchmark above is k=3 on one harness.
