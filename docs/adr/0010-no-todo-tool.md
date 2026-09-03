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

## Measured here

The ADR above stood on somebody else's benchmark and said so. `cargo xtask
bench` is ours: three arms differing by one variable, six multi-step tasks —
three of them with somewhere to go wrong — each scored by looking at the
workspace afterwards rather than by reading what the model said about it. Three
runs each, against `qwen3.6-35b-a3b` through LM Studio.

| arm | passed | tokens | steps |
|---|---|---|---|
| plan-line (the decision) | 18/18 | 669,578 | 136 |
| nothing | 18/18 | 643,094 | 141 |
| todo-tool | 18/18 | 1,190,124 | 229 |

Two findings, and the second is the one worth keeping.

**The tool cost 78% more tokens and 68% more steps for no difference in
outcome.** Every arm passed every task, including the three with traps in
them, so what this measures is price rather than quality — which is the same
place the reference's own opus row landed. The plan line costs 4% over saying
nothing at all, which at k=3 is noise.

**Without a per-turn reminder the model does not use the tool at all.** Told
once in the system prompt to write a plan with `plan` before acting, a capable
model made nine tool calls on a three-part task and not one of them was a plan.
The first version of this measurement therefore compared nothing to nothing;
the arm only became real once the turn started carrying "you have no plan yet"
and "mark a step done as soon as it is". That nag is the tool's active
ingredient, and it is also the whole of its cost — which is what the reference
found from the other direction when reframing the prompts as optional notes
made the models stop calling it.

## What would change the decision

Not a better tool: a task set where the arms come apart. Everything here
passed everything, so nothing was learned about hard work, and the honest next
step is harder tasks or a weaker model rather than more repeats of these. The
tool stays behind `[agent] todo_tool`, off, because an arm that cannot be run
is a measurement that cannot be repeated.

## Cost

- No structured progress for a UI to render. `session context` and the transcript
  are what there is.
- Sub-agents get the plan line but not the parent's plan; a delegated task states
  its own.
- If a future model genuinely needs structured plan state, this is worth
  re-measuring rather than assumed — the benchmark above is k=3 on one harness.
